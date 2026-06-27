//! Label sanitization + render-safety escaping (Trojan-Source / bidi defense).

use std::fmt::Write as _;

/// Maximum stored label length, in `char`s.
pub const MAX_LABEL_LEN: usize = 256;

/// Returns `true` for codepoints that can spoof rendering and must never reach a terminal raw:
/// Unicode bidi overrides/embeds/isolates (Trojan-Source, CVE-2021-42574), directional marks,
/// zero-width characters, the BOM, and any other control character.
#[must_use]
pub(crate) fn is_render_dangerous(ch: char) -> bool {
    matches!(
        u32::from(ch),
        0x202A..=0x202E   // LRE RLE PDF LRO RLO
        | 0x2066..=0x2069 // LRI RLI FSI PDI
        | 0x200E | 0x200F // LRM RLM
        | 0x200B..=0x200D // zero-width space / non-joiner / joiner
        | 0xFEFF          // BOM / zero-width no-break space
    ) || ch.is_control()
}

/// Escapes every render-dangerous codepoint to its `\u{XXXX}` form, leaving ordinary text untouched.
///
/// Apply at **every** output/render boundary (CLI, exporters, TUI, MCP) — a missed boundary is how
/// Trojan-Source escapes leak. Idempotent on already-safe text.
#[must_use]
pub fn display_safe(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if is_render_dangerous(ch) {
            let _ = write!(out, "\\u{{{:04X}}}", u32::from(ch));
        } else {
            out.push(ch);
        }
    }
    out
}

/// Sanitizes a node/edge label for storage: strips control characters and caps length to
/// [`MAX_LABEL_LEN`] `char`s. Rendering still goes through [`display_safe`].
#[must_use]
pub fn sanitize_label(input: &str) -> String {
    input
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_LABEL_LEN)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_untouched() {
        assert_eq!(
            display_safe("fn main() -> Result<()>"),
            "fn main() -> Result<()>"
        );
        assert_eq!(display_safe("habitat-graph"), "habitat-graph");
    }

    #[test]
    fn rtl_override_is_escaped() {
        // The classic Trojan-Source payload: U+202E (RIGHT-TO-LEFT OVERRIDE).
        let evil = "let access = \u{202E}// admin";
        let safe = display_safe(evil);
        assert!(!safe.contains('\u{202E}'), "raw RLO leaked: {safe:?}");
        assert!(safe.contains("\\u{202E}"), "{safe}");
    }

    #[test]
    fn all_bidi_overrides_escaped() {
        for cp in [0x202A_u32, 0x202B, 0x202C, 0x202D, 0x202E] {
            let ch = char::from_u32(cp).unwrap();
            let out = display_safe(&ch.to_string());
            assert_eq!(out, format!("\\u{{{cp:04X}}}"));
        }
    }

    #[test]
    fn bidi_isolates_escaped() {
        for cp in [0x2066_u32, 0x2067, 0x2068, 0x2069] {
            let ch = char::from_u32(cp).unwrap();
            assert!(!display_safe(&ch.to_string()).contains(ch));
        }
    }

    #[test]
    fn directional_marks_escaped() {
        assert_eq!(display_safe("\u{200E}"), "\\u{200E}");
        assert_eq!(display_safe("\u{200F}"), "\\u{200F}");
    }

    #[test]
    fn zero_width_and_bom_escaped() {
        for ch in ['\u{200B}', '\u{200C}', '\u{200D}', '\u{FEFF}'] {
            assert!(!display_safe(&ch.to_string()).contains(ch));
        }
    }

    #[test]
    fn control_chars_escaped() {
        assert_eq!(display_safe("\u{1B}[31m"), "\\u{001B}[31m"); // ANSI ESC neutralized
        assert_eq!(display_safe("\u{7F}"), "\\u{007F}"); // DEL
        assert_eq!(display_safe("\u{0007}"), "\\u{0007}"); // BEL
    }

    #[test]
    fn display_safe_is_idempotent() {
        let once = display_safe("x\u{202E}y");
        assert_eq!(display_safe(&once), once);
    }

    #[test]
    fn sanitize_strips_control_chars() {
        assert_eq!(sanitize_label("ab\u{0007}\ncd\t"), "abcd");
    }

    #[test]
    fn sanitize_caps_length() {
        let long = "a".repeat(1000);
        assert_eq!(sanitize_label(&long).chars().count(), MAX_LABEL_LEN);
    }

    #[test]
    fn sanitize_counts_chars_not_bytes() {
        let s = "é".repeat(300); // 2 bytes each
        assert_eq!(sanitize_label(&s).chars().count(), MAX_LABEL_LEN);
    }

    #[test]
    fn sanitize_keeps_ordinary_unicode() {
        assert_eq!(sanitize_label("café→graph"), "café→graph");
    }

    #[test]
    fn newline_and_tab_are_control_for_labels() {
        assert_eq!(sanitize_label("line1\nline2"), "line1line2");
    }
}
