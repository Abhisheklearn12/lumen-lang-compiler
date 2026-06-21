//! The compilation session: the single entry point that drives the phase
//! pipeline end to end.
//!
//! A [`Session`] owns the [`SourceFile`] and the [`Diagnostics`] sink, runs the
//! phases in order, records per-phase timings for observability, and stops
//! before code generation if the front-end reported any error. It returns the
//! [`Artifacts`] each phase produced so the CLI can either run the program or
//! dump an intermediate representation.
//!
//! ```text
//! tokens → ast → resolution → typeck → hir → optimized hir → bytecode
//! ```
//!
//! The session is the only place phases are wired together; each phase remains
//! independently testable in isolation.

use std::time::{Duration, Instant};

use crate::backend::{Program, generate};
use crate::diagnostics::Diagnostics;
use crate::hir::{Hir, lower};
use crate::lexer::{Token, tokenize};
use crate::opt::{OptOptions, OptStats, optimize};
use crate::parser::ast::Ast;
use crate::parser::parse;
use crate::sema::resolve::Resolution;
use crate::sema::typeck::Typeck;
use crate::sema::{check, resolve};
use crate::source::SourceFile;

/// How far to run the pipeline. Earlier stages are useful for `dump`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Tokens,
    Ast,
    Hir,
    /// HIR after optimization.
    OptimizedHir,
    /// Optimized MIR (built from optimized HIR).
    Mir,
    /// Graphviz DOT of the MIR control-flow graph.
    Cfg,
    /// Transpiled C source (built from optimized HIR).
    C,
    /// Bytecode (the full pipeline).
    Bytecode,
    /// The bytecode verifier's report on the final program.
    Verify,
}

/// Options controlling a compilation.
#[derive(Debug, Clone, Copy)]
pub struct PipelineOptions {
    pub stop_after: Stage,
    pub optimize: OptOptions,
}

impl Default for PipelineOptions {
    fn default() -> PipelineOptions {
        PipelineOptions {
            stop_after: Stage::Bytecode,
            optimize: OptOptions::default(),
        }
    }
}

/// The artifacts produced by a run, each present iff its stage was reached and
/// no earlier error stopped the pipeline.
#[derive(Debug, Default)]
pub struct Artifacts {
    pub tokens: Option<Vec<Token>>,
    pub ast: Option<Ast>,
    pub resolution: Option<Resolution>,
    pub typeck: Option<Typeck>,
    pub hir: Option<Hir>,
    pub opt_stats: Option<OptStats>,
    pub program: Option<Program>,
}

/// Wall-clock duration of each phase that ran, in execution order.
#[derive(Debug, Default, Clone)]
pub struct Timings {
    entries: Vec<(&'static str, Duration)>,
}

impl Timings {
    /// The recorded `(phase, duration)` pairs in order.
    pub fn entries(&self) -> &[(&'static str, Duration)] {
        &self.entries
    }

    /// Total time across all recorded phases.
    pub fn total(&self) -> Duration {
        self.entries.iter().map(|(_, d)| *d).sum()
    }
}

/// A single compilation, owning the source and accumulated diagnostics.
#[derive(Debug)]
pub struct Session {
    file: SourceFile,
    diagnostics: Diagnostics,
    timings: Timings,
}

impl Session {
    /// Creates a session for a source file with display `name` and `src` text.
    pub fn new(name: impl Into<String>, src: impl Into<String>) -> Session {
        Session {
            file: SourceFile::new(name, src),
            diagnostics: Diagnostics::new(),
            timings: Timings::default(),
        }
    }

    /// The source file being compiled.
    pub fn file(&self) -> &SourceFile {
        &self.file
    }

    /// The diagnostics accumulated so far.
    pub fn diagnostics(&self) -> &Diagnostics {
        &self.diagnostics
    }

    /// Per-phase timings from the last [`Session::compile`].
    pub fn timings(&self) -> &Timings {
        &self.timings
    }

    /// Renders all diagnostics against the source, in reading order.
    pub fn render_diagnostics(&self) -> String {
        self.diagnostics.render_all(&self.file)
    }

    /// Runs the pipeline up to `options.stop_after`.
    ///
    /// The error-tolerant front-end (lex → parse → resolve → typeck) always runs
    /// in full so every diagnosable problem is reported at once; lowering and
    /// code generation run only if the program is error-free.
    #[tracing::instrument(level = "info", skip_all, fields(file = self.file.name()))]
    pub fn compile(&mut self, options: PipelineOptions) -> Artifacts {
        let mut artifacts = Artifacts::default();

        // --- Lexing ---
        let tokens = self.timed("lex", |s| tokenize(&s.file, &mut s.diagnostics));
        if options.stop_after == Stage::Tokens {
            artifacts.tokens = Some(tokens);
            return artifacts;
        }

        // --- Parsing ---
        let ast = self.timed("parse", |s| parse(tokens, &mut s.diagnostics));
        if options.stop_after == Stage::Ast {
            artifacts.ast = Some(ast);
            return artifacts;
        }

        // --- Semantic analysis (always run fully for complete diagnostics) ---
        let resolution = self.timed("resolve", |s| resolve(&ast, &mut s.diagnostics));
        let typeck = self.timed("typeck", |s| check(&ast, &resolution, &mut s.diagnostics));

        // Stop here if the program is not well-typed: lowering assumes it is.
        if self.diagnostics.has_errors() {
            artifacts.ast = Some(ast);
            artifacts.resolution = Some(resolution);
            artifacts.typeck = Some(typeck);
            return artifacts;
        }

        // --- Lowering ---
        let mut hir = self.timed("lower", |_| lower(&ast, &resolution, &typeck));
        artifacts.ast = Some(ast);
        artifacts.resolution = Some(resolution);
        artifacts.typeck = Some(typeck);
        if options.stop_after == Stage::Hir {
            artifacts.hir = Some(hir);
            return artifacts;
        }

        // --- Optimization ---
        let stats = self.timed("optimize", |_| optimize(&mut hir, options.optimize));
        artifacts.opt_stats = Some(stats);
        // `Mir` and `OptimizedHir` dumps both stop here with optimized HIR
        // available; the CLI builds MIR from it on demand.
        if matches!(
            options.stop_after,
            Stage::OptimizedHir | Stage::Mir | Stage::Cfg | Stage::C
        ) {
            artifacts.hir = Some(hir);
            return artifacts;
        }

        // --- Code generation ---
        let mut program = self.timed("codegen", |_| generate(&hir));
        // A final bytecode-level cleanup, only when optimizing.
        if options.optimize.enabled {
            self.timed("peephole", |_| {
                crate::backend::peephole::optimize(&mut program);
            });
        }
        artifacts.hir = Some(hir);
        artifacts.program = Some(program);
        artifacts
    }

    /// Runs `phase` named `name`, recording its duration.
    fn timed<T>(&mut self, name: &'static str, phase: impl FnOnce(&mut Session) -> T) -> T {
        let start = Instant::now();
        let result = phase(self);
        let elapsed = start.elapsed();
        self.timings.entries.push((name, elapsed));
        tracing::debug!(
            phase = name,
            micros = elapsed.as_micros() as u64,
            "phase complete"
        );
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_a_valid_program_to_bytecode() {
        let mut session = Session::new("ok.lm", "fn main() { print_int(1 + 1); }");
        let artifacts = session.compile(PipelineOptions::default());
        assert!(!session.diagnostics().has_errors());
        assert!(artifacts.program.is_some());
        // All eight phases recorded a timing (the final one is the bytecode
        // peephole pass, which runs because optimization is on by default).
        assert_eq!(session.timings().entries().len(), 8);
    }

    #[test]
    fn stops_before_codegen_on_type_error() {
        let mut session = Session::new("bad.lm", "fn main() { let x: i64 = true; }");
        let artifacts = session.compile(PipelineOptions::default());
        assert!(session.diagnostics().has_errors());
        assert!(artifacts.program.is_none());
        assert!(artifacts.hir.is_none());
        // The front-end artifacts are still available.
        assert!(artifacts.typeck.is_some());
    }

    #[test]
    fn dump_stages_stop_early() {
        let mut session = Session::new("t.lm", "fn main() {}");
        let opts = PipelineOptions {
            stop_after: Stage::Tokens,
            ..Default::default()
        };
        let artifacts = session.compile(opts);
        assert!(artifacts.tokens.is_some());
        assert!(artifacts.ast.is_none());
    }

    #[test]
    fn records_optimization_stats() {
        let mut session = Session::new("o.lm", "fn main() { print_int(2 * 3); }");
        let artifacts = session.compile(PipelineOptions::default());
        let stats = artifacts.opt_stats.expect("optimizer ran");
        assert!(stats.folded >= 1);
    }
}
