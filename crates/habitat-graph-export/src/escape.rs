//! Output-escaping helpers shared by the structured exporters (`svg`/`graphml`/`cypher`).
//!
//! Knowledge-graph node labels and source paths are attacker-influenced data (they come from
//! arbitrary parsed source). Embedding them unescaped into XML or Cypher is an injection/tampering
//! vector (STRIDE-T): a label like `</text><script>` or `' DETACH DELETE n //` must be neutralised.
//! These helpers are the single, tested escaping surface every structured exporter routes through.

/// Escapes a string for safe embedding inside XML text or a double-quoted XML attribute.
///
/// Replaces the five XML metacharacters (`&`, `<`, `>`, `"`, `'`) with their entities and **drops
/// characters that are illegal in XML 1.0** (the C0 control range except tab/newline/carriage-return,
/// plus the non-characters `U+FFFE`/`U+FFFF`), so the output is always well-formed regardless of input.
///
/// `&` is handled first by construction (each branch emits a complete entity), so no double-escaping
/// occurs.
#[must_use]
pub fn xml_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + input.len() / 8);
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Legal XML 1.0 whitespace controls — keep.
            '\t' | '\n' | '\r' => out.push(ch),
            // Illegal C0 controls and non-characters — drop entirely.
            c if (c as u32) < 0x20 => {}
            '\u{FFFE}' | '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
    out
}

/// Escapes a string for safe embedding inside a single-quoted Cypher string literal.
///
/// Backslash and single-quote are escaped (`\` → `\\`, `'` → `\'`); literal control characters are
/// rewritten to Cypher escape sequences (`\n`, `\r`, `\t`) and other C0 controls are dropped, so a
/// label can never terminate the literal early or inject a clause.
#[must_use]
pub fn cypher_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + input.len() / 8);
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{cypher_escape, xml_escape};

    #[test]
    fn xml_escapes_all_five_metacharacters() {
        assert_eq!(xml_escape("&<>\"'"), "&amp;&lt;&gt;&quot;&apos;");
    }

    #[test]
    fn xml_ampersand_not_double_escaped() {
        assert_eq!(xml_escape("a & b"), "a &amp; b");
        assert_eq!(xml_escape("&amp;"), "&amp;amp;");
    }

    #[test]
    fn xml_neutralises_tag_injection() {
        let evil = "</text><script>alert(1)</script>";
        let safe = xml_escape(evil);
        assert!(!safe.contains('<'), "no raw '<' may survive: {safe}");
        assert!(!safe.contains('>'), "no raw '>' may survive: {safe}");
    }

    #[test]
    fn xml_keeps_legal_whitespace_controls() {
        assert_eq!(xml_escape("a\tb\nc\rd"), "a\tb\nc\rd");
    }

    #[test]
    fn xml_drops_illegal_c0_controls() {
        let s = format!("a{}b{}c", '\u{0}', '\u{7}');
        assert_eq!(xml_escape(&s), "abc");
    }

    #[test]
    fn xml_drops_noncharacters() {
        let s = format!("x{}{}y", '\u{FFFE}', '\u{FFFF}');
        assert_eq!(xml_escape(&s), "xy");
    }

    #[test]
    fn xml_passes_unicode_text() {
        assert_eq!(xml_escape("café — Ω"), "café — Ω");
    }

    #[test]
    fn xml_empty_is_empty() {
        assert_eq!(xml_escape(""), "");
    }

    #[test]
    fn cypher_escapes_backslash_and_quote() {
        assert_eq!(cypher_escape(r"a\b'c"), r"a\\b\'c");
    }

    #[test]
    fn cypher_neutralises_clause_injection() {
        let evil = "' DETACH DELETE n //";
        let safe = cypher_escape(evil);
        assert!(
            !safe.starts_with('\''),
            "leading quote must be escaped: {safe}"
        );
        assert_eq!(safe, "\\' DETACH DELETE n //");
    }

    #[test]
    fn cypher_rewrites_control_chars() {
        assert_eq!(cypher_escape("a\nb\tc\rd"), "a\\nb\\tc\\rd");
    }

    #[test]
    fn cypher_drops_other_controls() {
        let s = format!("a{}b", '\u{0}');
        assert_eq!(cypher_escape(&s), "ab");
    }

    #[test]
    fn cypher_empty_is_empty() {
        assert_eq!(cypher_escape(""), "");
    }
}
