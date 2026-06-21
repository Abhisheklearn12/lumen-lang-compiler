//! Source positions and spans.
//!
//! A [`Span`] is a half-open byte range `[lo, hi)` into a single
//! [`SourceFile`](crate::source::SourceFile). Lumen compiles one root file per
//! invocation (the language has no module/import system), so spans do not need
//! to carry a file id  the active file is known to the [`Session`].
//!
//! Byte offsets are stored as `u32`. A 4 GiB source-file limit is more than
//! sufficient and halves the size of `Span` compared to `usize`, which matters
//! because spans are embedded in every AST and HIR node.

use std::fmt;

/// A half-open byte range `[lo, hi)` into the source file.
///
/// Spans are cheap (`Copy`, 8 bytes) and are attached to virtually every
/// compiler artifact so diagnostics can point back at the originating source.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    /// Inclusive start byte offset.
    pub lo: u32,
    /// Exclusive end byte offset.
    pub hi: u32,
}

impl Span {
    /// Creates a span from raw byte offsets.
    ///
    /// `lo` must not exceed `hi`; this is debug-asserted because an inverted
    /// span signals a bug in a phase that constructs it.
    #[inline]
    pub fn new(lo: u32, hi: u32) -> Span {
        debug_assert!(lo <= hi, "inverted span: {lo}..{hi}");
        Span { lo, hi }
    }

    /// A zero-length sentinel span, used where a real location is unavailable
    /// (e.g. compiler-synthesised nodes). Never points into real source text.
    pub const DUMMY: Span = Span { lo: 0, hi: 0 };

    /// The number of bytes covered by the span.
    #[inline]
    pub fn len(&self) -> u32 {
        self.hi - self.lo
    }

    /// Whether the span covers no bytes.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.lo == self.hi
    }

    /// Returns the smallest span covering both `self` and `other`.
    #[inline]
    pub fn to(self, other: Span) -> Span {
        Span::new(self.lo.min(other.lo), self.hi.max(other.hi))
    }

    /// A zero-length span pointing at the start of `self`.
    #[inline]
    pub fn shrink_to_lo(self) -> Span {
        Span::new(self.lo, self.lo)
    }

    /// A zero-length span pointing at the end of `self`.
    #[inline]
    pub fn shrink_to_hi(self) -> Span {
        Span::new(self.hi, self.hi)
    }

    /// The byte range as a `usize` pair, for slicing into source text.
    #[inline]
    pub fn range(&self) -> std::ops::Range<usize> {
        self.lo as usize..self.hi as usize
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.lo, self.hi)
    }
}

/// A value paired with its source span.
///
/// Used where a node is otherwise just data (e.g. an identifier string) but
/// still needs a location for diagnostics.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Spanned<T> {
    pub node: T,
    pub span: Span,
}

impl<T> Spanned<T> {
    #[inline]
    pub fn new(node: T, span: Span) -> Spanned<T> {
        Spanned { node, span }
    }

    /// Applies `f` to the contained value, preserving the span.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Spanned<U> {
        Spanned {
            node: f(self.node),
            span: self.span,
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for Spanned<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}@{:?}", self.node, self.span)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_covers_both() {
        let a = Span::new(2, 5);
        let b = Span::new(8, 10);
        assert_eq!(a.to(b), Span::new(2, 10));
        assert_eq!(b.to(a), Span::new(2, 10));
    }

    #[test]
    fn len_and_emptiness() {
        assert_eq!(Span::new(3, 7).len(), 4);
        assert!(Span::DUMMY.is_empty());
        assert!(!Span::new(0, 1).is_empty());
    }

    #[test]
    fn shrink_endpoints() {
        let s = Span::new(4, 9);
        assert_eq!(s.shrink_to_lo(), Span::new(4, 4));
        assert_eq!(s.shrink_to_hi(), Span::new(9, 9));
    }
}
