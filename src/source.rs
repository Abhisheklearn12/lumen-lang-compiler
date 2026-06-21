//! The source file and line-index used for diagnostics.
//!
//! A [`SourceFile`] owns the program text and a precomputed table of line-start
//! byte offsets. The table is built once and lets us translate a byte offset to
//! a human `line:column` in `O(log n)` via binary search, which keeps
//! diagnostic rendering cheap even for large inputs.

use crate::span::Span;

/// A 1-based line/column location within a source file.
///
/// Columns are counted in Unicode scalar values (`char`s), not bytes, so a
/// multi-byte character advances the column by one  matching what a user sees
/// in an editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Location {
    /// 1-based line number.
    pub line: u32,
    /// 1-based column number, counted in characters.
    pub column: u32,
}

/// An owned source file: its display name, full text, and line index.
#[derive(Debug, Clone)]
pub struct SourceFile {
    name: String,
    src: String,
    /// Byte offset of the first character of each line. Always starts with `0`.
    line_starts: Vec<u32>,
}

impl SourceFile {
    /// Builds a source file from a display `name` and its `src` text.
    pub fn new(name: impl Into<String>, src: impl Into<String>) -> SourceFile {
        let src = src.into();
        let line_starts = line_starts(&src);
        SourceFile {
            name: name.into(),
            src,
            line_starts,
        }
    }

    /// The display name (typically a path) used in diagnostics.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The full source text.
    pub fn text(&self) -> &str {
        &self.src
    }

    /// The text covered by `span`.
    ///
    /// Returns `""` if the span lies outside the source (which only happens for
    /// synthetic spans). This never panics, satisfying the rule that diagnostic
    /// machinery must be infallible.
    pub fn snippet(&self, span: Span) -> &str {
        let range = span.range();
        self.src.get(range).unwrap_or("")
    }

    /// Translates a byte offset to a 1-based [`Location`].
    ///
    /// Offsets past the end of the file clamp to the final position, so callers
    /// (e.g. an EOF span) always get a sensible answer.
    pub fn location(&self, offset: u32) -> Location {
        let offset = offset.min(self.src.len() as u32);
        // `line_starts` is sorted; find the last start <= offset.
        let line_idx = match self.line_starts.binary_search(&offset) {
            Ok(idx) => idx,
            Err(idx) => idx - 1,
        };
        let line_start = self.line_starts[line_idx];
        // Column = number of chars between the line start and the offset, + 1.
        let column = self.src[line_start as usize..offset as usize]
            .chars()
            .count() as u32
            + 1;
        Location {
            line: line_idx as u32 + 1,
            column,
        }
    }

    /// Returns the full text of the 1-based `line`, without its terminator.
    pub fn line_text(&self, line: u32) -> &str {
        if line == 0 || line as usize > self.line_starts.len() {
            return "";
        }
        let start = self.line_starts[line as usize - 1] as usize;
        let end = self
            .line_starts
            .get(line as usize)
            .map(|&s| s as usize)
            .unwrap_or(self.src.len());
        self.src[start..end].trim_end_matches(['\n', '\r'])
    }

    /// The number of lines in the file (always at least 1).
    pub fn line_count(&self) -> u32 {
        self.line_starts.len() as u32
    }
}

/// Computes the byte offset of the start of each line.
fn line_starts(src: &str) -> Vec<u32> {
    let mut starts = vec![0u32];
    for (idx, byte) in src.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(idx as u32 + 1);
        }
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line_locations() {
        let f = SourceFile::new("t.lm", "let x = 1;");
        assert_eq!(f.location(0), Location { line: 1, column: 1 });
        assert_eq!(f.location(4), Location { line: 1, column: 5 });
    }

    #[test]
    fn multi_line_locations() {
        let f = SourceFile::new("t.lm", "a\nbb\nccc");
        assert_eq!(f.location(0), Location { line: 1, column: 1 });
        assert_eq!(f.location(2), Location { line: 2, column: 1 });
        assert_eq!(f.location(5), Location { line: 3, column: 1 });
        assert_eq!(f.location(7), Location { line: 3, column: 3 });
    }

    #[test]
    fn unicode_columns_count_chars() {
        // 'é' is two bytes; the column after it must be 2, not 3.
        let f = SourceFile::new("t.lm", "é=1");
        assert_eq!(f.location(2), Location { line: 1, column: 2 });
    }

    #[test]
    fn line_text_strips_terminator() {
        let f = SourceFile::new("t.lm", "first\nsecond\n");
        assert_eq!(f.line_text(1), "first");
        assert_eq!(f.line_text(2), "second");
        assert_eq!(f.line_text(99), "");
    }

    #[test]
    fn snippet_and_offset_clamp_never_panic() {
        let f = SourceFile::new("t.lm", "abc");
        assert_eq!(f.snippet(Span::new(0, 2)), "ab");
        assert_eq!(f.snippet(Span::new(10, 20)), "");
        assert_eq!(f.location(999), Location { line: 1, column: 4 });
    }
}
