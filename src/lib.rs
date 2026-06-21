//! # Lumen
//!
//! Lumen is a small, statically-typed programming language and the compiler
//! that implements it. The compiler is organised as an explicit pipeline of
//! phases, each with a narrow public API, its own diagnostics, and no hidden
//! state shared with its neighbours:
//!
//! ```text
//! source ─▶ lexer ─▶ parser ─▶ AST
//!                                │  name resolution
//!                                ▼  type checking
//!                               HIR (lowering)
//!                                │  optimizer passes
//!                                ▼
//!                            bytecode ─▶ VM
//! ```
//!
//! Phases communicate through distinct data types  [`lexer::Token`],
//! [`parser::ast`], [`hir`], and [`backend::bytecode`]  so that no phase can
//! reach into another's representation. Cross-cutting infrastructure
//! ([`span`], [`source`], [`diagnostics`], [`errors`]) is shared by all.
//!
//! The [`Session`] type ties the phases together and is the entry point most
//! callers want; see its documentation for the end-to-end flow.

// The compiler holds itself to a high lint bar. These are denied rather than
// warned so regressions fail the build, matching the project's acceptance
// criteria.
#![deny(rust_2018_idioms)]
#![warn(missing_debug_implementations)]

pub mod backend;
pub mod diagnostics;
pub mod errors;
pub mod explain;
pub mod format;
pub mod hir;
pub mod lexer;
pub mod mir;
pub mod opt;
pub mod parser;
pub mod sema;
pub mod session;
pub mod source;
pub mod span;
pub mod suggest;

pub use session::{Artifacts, PipelineOptions, Session, Stage};

#[cfg(test)]
mod foundation_tests {
    //! Cross-module smoke tests for the shared foundation, exercising the way
    //! spans, source files, and diagnostics compose  the contract every later
    //! phase relies on.

    use crate::diagnostics::{Diagnostic, Diagnostics};
    use crate::errors::DiagCode;
    use crate::source::SourceFile;
    use crate::span::Span;

    #[test]
    fn diagnostic_points_at_correct_location() {
        let src = "fn main() {\n    let x = bogus;\n}\n";
        let file = SourceFile::new("main.lm", src);
        // Span of `bogus` on line 2.
        let start = src.find("bogus").unwrap() as u32;
        let span = Span::new(start, start + 5);
        let loc = file.location(span.lo);
        assert_eq!(loc.line, 2);

        let mut diags = Diagnostics::new();
        diags.emit(
            Diagnostic::error(DiagCode::UnresolvedName, "cannot find value `bogus`")
                .with_primary(span, "not found in this scope"),
        );
        let rendered = diags.render_all(&file);
        assert!(rendered.contains("E0200"));
        assert!(rendered.contains("bogus"));
    }
}
