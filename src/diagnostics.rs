//! The diagnostics subsystem: a phase-independent model for errors and
//! warnings, plus rendering on top of [`miette`].
//!
//! Diagnostics are first-class. Every phase builds [`Diagnostic`] values and
//! pushes them into a shared [`Diagnostics`] sink rather than returning `Err`
//! eagerly, which lets phases report *many* problems from one run instead of
//! stopping at the first. Construction is infallible and never panics.
//!
//! Rendering is deterministic (no colour, fixed Unicode theme) so that snapshot
//! tests over diagnostic output are stable.

use crate::errors::DiagCode;
use crate::source::SourceFile;
use crate::span::Span;

/// Whether a diagnostic blocks compilation (`Error`) or is advisory (`Warning`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// A span annotated with a message. The `primary` label points at the root of
/// the problem; secondary labels add supporting context elsewhere in the source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    pub span: Span,
    pub message: String,
    pub primary: bool,
}

/// A single error or warning, with optional labels, notes, and help text.
///
/// Build one with [`Diagnostic::error`] / [`Diagnostic::warning`] and the
/// `with_*` chaining methods.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: DiagCode,
    pub message: String,
    pub labels: Vec<Label>,
    pub notes: Vec<String>,
    pub helps: Vec<String>,
}

impl Diagnostic {
    /// Starts an error diagnostic with the given code and headline message.
    pub fn error(code: DiagCode, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            severity: Severity::Error,
            code,
            message: message.into(),
            labels: Vec::new(),
            notes: Vec::new(),
            helps: Vec::new(),
        }
    }

    /// Starts a warning diagnostic with the given code and headline message.
    pub fn warning(code: DiagCode, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            severity: Severity::Warning,
            code,
            message: message.into(),
            labels: Vec::new(),
            notes: Vec::new(),
            helps: Vec::new(),
        }
    }

    /// Adds the primary label, which carries the caret underline in rendered
    /// output. A diagnostic should have exactly one.
    pub fn with_primary(mut self, span: Span, message: impl Into<String>) -> Diagnostic {
        self.labels.push(Label {
            span,
            message: message.into(),
            primary: true,
        });
        self
    }

    /// Adds a secondary label providing extra context at another location.
    pub fn with_label(mut self, span: Span, message: impl Into<String>) -> Diagnostic {
        self.labels.push(Label {
            span,
            message: message.into(),
            primary: false,
        });
        self
    }

    /// Adds a free-standing note (rendered after the snippet).
    pub fn with_note(mut self, note: impl Into<String>) -> Diagnostic {
        self.notes.push(note.into());
        self
    }

    /// Adds an actionable help line suggesting how to fix the problem.
    pub fn with_help(mut self, help: impl Into<String>) -> Diagnostic {
        self.helps.push(help.into());
        self
    }

    /// The primary span, if any, else the first label's span, else `DUMMY`.
    /// Useful for sorting diagnostics by source order.
    pub fn primary_span(&self) -> Span {
        self.labels
            .iter()
            .find(|l| l.primary)
            .or_else(|| self.labels.first())
            .map(|l| l.span)
            .unwrap_or(Span::DUMMY)
    }

    /// Renders this diagnostic against `file` to a plain (uncoloured) string.
    pub fn render(&self, file: &SourceFile) -> String {
        let adapter = Adapter::new(self, file);
        let mut out = String::new();
        let handler = miette::GraphicalReportHandler::new()
            .with_theme(miette::GraphicalTheme::unicode_nocolor());
        // Rendering writes into a String, whose `fmt::Write` is infallible.
        let _ = handler.render_report(&mut out, &adapter);
        out
    }
}

/// A growable collection of diagnostics shared across phases.
///
/// Phases append to it; the driver inspects [`Diagnostics::has_errors`] at each
/// phase boundary to decide whether to continue.
#[derive(Debug, Default)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
    errors: usize,
    warnings: usize,
}

impl Diagnostics {
    /// An empty sink.
    pub fn new() -> Diagnostics {
        Diagnostics::default()
    }

    /// Records a diagnostic, updating the error/warning tallies.
    pub fn emit(&mut self, diag: Diagnostic) {
        match diag.severity {
            Severity::Error => self.errors += 1,
            Severity::Warning => self.warnings += 1,
        }
        self.items.push(diag);
    }

    /// Number of errors recorded so far.
    pub fn error_count(&self) -> usize {
        self.errors
    }

    /// Number of warnings recorded so far.
    pub fn warning_count(&self) -> usize {
        self.warnings
    }

    /// Whether any error has been recorded.
    pub fn has_errors(&self) -> bool {
        self.errors > 0
    }

    /// Whether nothing has been recorded at all.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// All recorded diagnostics, in emission order.
    pub fn items(&self) -> &[Diagnostic] {
        &self.items
    }

    /// Renders every diagnostic in source order, separated by blank lines.
    ///
    /// Sorting by primary span gives stable, reading-order output regardless of
    /// the order phases happened to emit problems in.
    pub fn render_all(&self, file: &SourceFile) -> String {
        let mut order: Vec<&Diagnostic> = self.items.iter().collect();
        order.sort_by_key(|d| d.primary_span().lo);
        let mut out = String::new();
        for diag in order {
            out.push_str(&diag.render(file));
            out.push('\n');
        }
        out
    }
}

/// Bridges a [`Diagnostic`] to the [`miette::Diagnostic`] trait for rendering.
///
/// Kept private: the rest of the compiler never depends on miette directly,
/// so the diagnostic model and its presentation stay decoupled.
#[derive(Debug)]
struct Adapter {
    severity: Severity,
    code: DiagCode,
    message: String,
    labels: Vec<miette::LabeledSpan>,
    help: Option<String>,
    source: miette::NamedSource<String>,
}

impl Adapter {
    fn new(diag: &Diagnostic, file: &SourceFile) -> Adapter {
        let labels = diag
            .labels
            .iter()
            .map(|l| {
                let span: miette::SourceSpan = (l.span.lo as usize, l.span.len() as usize).into();
                if l.primary {
                    miette::LabeledSpan::new_primary_with_span(Some(l.message.clone()), span)
                } else {
                    miette::LabeledSpan::new_with_span(Some(l.message.clone()), span)
                }
            })
            .collect();

        // miette exposes a single help/footer block; fold notes and helps into
        // it, tagging notes so they remain distinguishable from suggestions.
        let mut footer = Vec::new();
        footer.extend(diag.notes.iter().map(|n| format!("note: {n}")));
        footer.extend(diag.helps.iter().cloned());
        let help = if footer.is_empty() {
            None
        } else {
            Some(footer.join("\n"))
        };

        Adapter {
            severity: diag.severity,
            code: diag.code,
            message: diag.message.clone(),
            labels,
            help,
            source: miette::NamedSource::new(file.name(), file.text().to_owned()),
        }
    }
}

impl std::fmt::Display for Adapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Adapter {}

impl miette::Diagnostic for Adapter {
    fn code(&self) -> Option<Box<dyn std::fmt::Display + '_>> {
        Some(Box::new(self.code.as_str()))
    }

    fn severity(&self) -> Option<miette::Severity> {
        Some(match self.severity {
            Severity::Error => miette::Severity::Error,
            Severity::Warning => miette::Severity::Warning,
        })
    }

    fn help(&self) -> Option<Box<dyn std::fmt::Display + '_>> {
        self.help
            .as_ref()
            .map(|h| Box::new(h.clone()) as Box<dyn std::fmt::Display>)
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        Some(&self.source)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        Some(Box::new(self.labels.iter().cloned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sink_counts_and_flags() {
        let mut diags = Diagnostics::new();
        assert!(!diags.has_errors());
        diags.emit(Diagnostic::warning(DiagCode::MissingReturn, "w"));
        diags.emit(Diagnostic::error(DiagCode::TypeMismatch, "e"));
        assert_eq!(diags.warning_count(), 1);
        assert_eq!(diags.error_count(), 1);
        assert!(diags.has_errors());
    }

    #[test]
    fn renders_with_caret_and_code() {
        let file = SourceFile::new("main.lm", "let x: i32 = \"hello\";");
        let diag = Diagnostic::error(DiagCode::TypeMismatch, "mismatched types")
            .with_primary(Span::new(13, 20), "expected i32, found str")
            .with_help("change the literal to an integer");
        let out = diag.render(&file);
        assert!(out.contains("E0300"), "code missing:\n{out}");
        assert!(out.contains("mismatched types"), "message missing:\n{out}");
        assert!(
            out.contains("expected i32, found str"),
            "label missing:\n{out}"
        );
        assert!(out.contains("help:"), "help missing:\n{out}");
    }

    #[test]
    fn render_all_is_source_ordered() {
        let file = SourceFile::new("main.lm", "aaaa\nbbbb\n");
        let mut diags = Diagnostics::new();
        diags.emit(
            Diagnostic::error(DiagCode::UnresolvedName, "second")
                .with_primary(Span::new(5, 9), "here"),
        );
        diags.emit(
            Diagnostic::error(DiagCode::UnresolvedName, "first")
                .with_primary(Span::new(0, 4), "here"),
        );
        let out = diags.render_all(&file);
        let first = out.find("first").unwrap();
        let second = out.find("second").unwrap();
        assert!(first < second, "diagnostics not source-ordered:\n{out}");
    }
}
