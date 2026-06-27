//! Source spans locating a node within its file (interface contract §1).

use serde::{Deserialize, Serialize};

/// A byte + line range within a source file.
///
/// Byte offsets drive precise slicing; line numbers (1-based) are for human-facing output.
/// A span is *well-formed* when `end_byte >= start_byte` and `end_line >= start_line`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    /// Inclusive start byte offset.
    pub start_byte: u32,
    /// Exclusive end byte offset.
    pub end_byte: u32,
    /// 1-based start line.
    pub start_line: u32,
    /// 1-based end line.
    pub end_line: u32,
}

impl Span {
    /// Creates a span from raw byte and line ranges.
    #[must_use]
    pub const fn new(start_byte: u32, end_byte: u32, start_line: u32, end_line: u32) -> Self {
        Self {
            start_byte,
            end_byte,
            start_line,
            end_line,
        }
    }

    /// Returns the byte length of the span, saturating at zero for malformed spans.
    #[must_use]
    pub const fn byte_len(self) -> u32 {
        self.end_byte.saturating_sub(self.start_byte)
    }

    /// Returns `true` if the span covers no bytes.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.byte_len() == 0
    }

    /// Returns `true` if byte and line ranges are non-decreasing.
    #[must_use]
    pub const fn is_well_formed(self) -> bool {
        self.end_byte >= self.start_byte && self.end_line >= self.start_line
    }

    /// Returns `true` if `byte` falls within `[start_byte, end_byte)`.
    #[must_use]
    pub const fn contains_byte(self, byte: u32) -> bool {
        byte >= self.start_byte && byte < self.end_byte
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_len_basic() {
        assert_eq!(Span::new(10, 25, 2, 4).byte_len(), 15);
    }

    #[test]
    fn byte_len_saturates_on_malformed() {
        assert_eq!(Span::new(25, 10, 1, 1).byte_len(), 0);
    }

    #[test]
    fn empty_span() {
        assert!(Span::new(5, 5, 1, 1).is_empty());
        assert!(!Span::new(5, 6, 1, 1).is_empty());
    }

    #[test]
    fn well_formed_checks_both_axes() {
        assert!(Span::new(0, 10, 1, 3).is_well_formed());
        assert!(!Span::new(10, 0, 1, 3).is_well_formed());
        assert!(!Span::new(0, 10, 5, 3).is_well_formed());
    }

    #[test]
    fn contains_byte_is_half_open() {
        let s = Span::new(10, 20, 1, 1);
        assert!(s.contains_byte(10));
        assert!(s.contains_byte(19));
        assert!(!s.contains_byte(20));
        assert!(!s.contains_byte(9));
    }

    #[test]
    fn serde_roundtrip() {
        let s = Span::new(1, 2, 3, 4);
        let json = serde_json::to_string(&s).unwrap();
        let back: Span = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn serde_field_names_are_stable() {
        let json = serde_json::to_string(&Span::new(1, 2, 3, 4)).unwrap();
        for field in ["start_byte", "end_byte", "start_line", "end_line"] {
            assert!(json.contains(field), "missing {field} in {json}");
        }
    }

    #[test]
    fn copy_semantics() {
        let a = Span::new(0, 1, 1, 1);
        let b = a;
        assert_eq!(a, b);
    }
}
