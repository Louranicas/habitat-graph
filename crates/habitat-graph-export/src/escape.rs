//! Public-output safety helpers shared by every exporter.
//!
//! Knowledge-graph node labels and source paths are attacker-influenced data (they come from
//! arbitrary parsed source). Before a value is escaped for its destination format, obvious secret
//! patterns are replaced with one deterministic marker by [`redact_public_text`]. Structured
//! exporters then use [`xml_escape`] or [`cypher_escape`] to prevent injection/tampering (STRIDE-T).
//! Keeping redaction in one shared policy and destination escaping in this tested surface prevents
//! format drift such as JSON being redacted while SVG or generated Markdown still exposes the
//! original label.

pub(crate) use habitat_graph_core::project_public_relation as project_relation;
pub use habitat_graph_core::redact_public_text;

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
    use super::{cypher_escape, redact_public_text, xml_escape};

    #[test]
    fn redaction_leaves_clean_text_borrowed_and_unchanged() {
        let input = "normal_function_name";
        let output = redact_public_text(input);
        assert!(matches!(output, std::borrow::Cow::Borrowed(_)));
        assert_eq!(output, input);
    }

    #[test]
    fn redaction_replaces_api_key_identifier() {
        assert_eq!(
            redact_public_text("api_key_assignment_refused"),
            "[REDACTED:api_key]"
        );
    }

    #[test]
    fn redaction_replaces_high_confidence_secret_forms() {
        assert_eq!(
            redact_public_text("AKIAIOSFODNN7EXAMPLE"),
            "[REDACTED:aws_access_key_id]"
        );
        assert_eq!(
            redact_public_text("Authorization: Bearer token"),
            "[REDACTED:bearer_token]"
        );
        assert_eq!(
            redact_public_text("xoxb-123-secret"),
            "[REDACTED:slack_token]"
        );
    }

    #[test]
    fn redaction_handles_bearer_header_optional_whitespace() {
        assert_eq!(
            redact_public_text("Authorization:  \t Bearer\t token"),
            "[REDACTED:bearer_token]"
        );
    }

    #[test]
    fn redaction_uses_stable_multi_tag_order() {
        assert_eq!(
            redact_public_text("AKIAIOSFODNN7EXAMPLE api_key=x xoxb-123"),
            "[REDACTED:aws_access_key_id,api_key,slack_token]"
        );
    }

    #[test]
    fn redaction_screens_after_control_character_normalization() {
        assert_eq!(
            redact_public_text("api_\u{0007}key=SECRET"),
            "[REDACTED:api_key]"
        );
        assert_eq!(
            redact_public_text("Authorization:\u{0007} Bearer token"),
            "[REDACTED:bearer_token]"
        );
    }

    #[test]
    fn redaction_screens_filename_and_link_normalizations() {
        assert_eq!(redact_public_text("api/key=SECRET"), "[REDACTED:api_key]");
        assert_eq!(redact_public_text("api:key=SECRET"), "[REDACTED:api_key]");
    }

    #[test]
    fn canonical_single_tag_marker_is_idempotent() {
        let once = redact_public_text("api_key=x");
        let twice = redact_public_text(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn canonical_multi_tag_marker_is_idempotent() {
        let marker = "[REDACTED:aws_access_key_id,api_key,slack_token]";
        assert_eq!(redact_public_text(marker), marker);
    }

    #[test]
    fn noncanonical_marker_does_not_hide_trailing_secret() {
        let input = "[REDACTED:unknown] api_key=still-present";
        assert_eq!(redact_public_text(input), "[REDACTED:api_key]");
    }

    #[test]
    fn duplicate_or_out_of_order_marker_is_recanonicalized_when_screened() {
        assert_eq!(
            redact_public_text("[REDACTED:slack_token,api_key]"),
            "[REDACTED:api_key]"
        );
        assert_eq!(
            redact_public_text("[REDACTED:api_key,api_key]"),
            "[REDACTED:api_key]"
        );
    }

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
