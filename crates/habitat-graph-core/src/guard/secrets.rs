//! Cheap, dependency-free secret screening — run before any content is persisted or exported.
//!
//! This is a *screen*, not a vault scanner: it catches the obvious, high-signal leaks (private keys,
//! cloud tokens, bearer headers) so they never land in a `graph.json`, receipt, or vault note.

use std::borrow::Cow;
use std::collections::HashSet;

/// Secret tags in the canonical order used by public redaction markers.
pub const SECRET_TAG_ORDER: &[&str] = &[
    "private_key",
    "aws_access_key_id",
    "cargo_registry_token",
    "bearer_token",
    "api_key",
    "slack_token",
];

/// Returns whether `input` is an exact canonical `[REDACTED:<ordered-tags>]` marker.
#[must_use]
pub fn is_canonical_redaction_marker(input: &str) -> bool {
    let Some(tags) = input
        .strip_prefix("[REDACTED:")
        .and_then(|rest| rest.strip_suffix(']'))
    else {
        return false;
    };
    if tags.is_empty() {
        return false;
    }

    let mut previous_index: Option<usize> = None;
    for tag in tags.split(',') {
        let Some(index) = SECRET_TAG_ORDER
            .iter()
            .position(|candidate| *candidate == tag)
        else {
            return false;
        };
        if previous_index.is_some_and(|previous| index <= previous) {
            return false;
        }
        previous_index = Some(index);
    }
    true
}

fn contains_bearer_authorization(lower: &str) -> bool {
    lower.match_indices("authorization:").any(|(index, _)| {
        let credential =
            lower[index + "authorization:".len()..].trim_start_matches(char::is_whitespace);
        credential
            .strip_prefix("bearer")
            .is_some_and(|rest| rest.chars().next().is_some_and(char::is_whitespace))
    })
}

fn is_format_character(character: char) -> bool {
    matches!(
        u32::from(character),
        0x00AD
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    )
}

fn is_noncharacter(character: char) -> bool {
    let value = u32::from(character);
    (0xFDD0..=0xFDEF).contains(&value) || value & 0xFFFE == 0xFFFE
}

fn normalize_for_screening(input: &str, formats_as_space: bool) -> String {
    let mut normalized = String::with_capacity(input.len());
    for character in input.chars() {
        if character.is_whitespace() {
            normalized.push(' ');
        } else if character.is_control()
            || is_format_character(character)
            || is_noncharacter(character)
        {
            if formats_as_space {
                normalized.push(' ');
            }
        } else {
            normalized.push(character);
        }
    }
    normalized
}

fn screen_candidate(text: &str, hits: &mut HashSet<&'static str>) {
    let lower = text.to_ascii_lowercase();

    if text.contains("-----BEGIN") && text.contains("PRIVATE KEY-----") {
        hits.insert("private_key");
    }
    if text.contains("AKIA") || text.contains("ASIA") {
        hits.insert("aws_access_key_id");
    }
    if lower.contains("cargo_registry_token") {
        hits.insert("cargo_registry_token");
    }
    if contains_bearer_authorization(&lower) {
        hits.insert("bearer_token");
    }
    if lower.contains("api_key") || lower.contains("apikey") || lower.contains("api-key") {
        hits.insert("api_key");
    }
    if lower.contains("xoxb-") || lower.contains("xoxp-") {
        hits.insert("slack_token");
    }
}

/// Screens `text` for obvious secret patterns, returning the kind tags matched (empty = clean).
#[must_use]
pub fn screen_for_secrets(text: &str) -> Vec<&'static str> {
    let stripped = normalize_for_screening(text, false);
    let separated = normalize_for_screening(text, true);
    let filename_like: String = stripped
        .chars()
        .map(|character| {
            if character == '/' || character.is_whitespace() {
                '_'
            } else {
                character
            }
        })
        .collect();
    let compact: String = stripped
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect();

    let mut encountered = HashSet::new();
    for candidate in [
        text,
        stripped.as_str(),
        separated.as_str(),
        filename_like.as_str(),
        compact.as_str(),
    ] {
        screen_candidate(candidate, &mut encountered);
    }
    SECRET_TAG_ORDER
        .iter()
        .copied()
        .filter(|tag| encountered.contains(tag))
        .collect()
}

fn normalized_secret_tags(input: &str) -> Vec<&'static str> {
    screen_for_secrets(input)
}

/// Replaces obvious secret-bearing public text with a deterministic canonical marker.
///
/// Clean values are borrowed unchanged. Canonical markers pass through unchanged, making the
/// projection idempotent. Graph assembly remains responsible for retaining raw values internally;
/// public renderers apply this projection only when producing display artifacts.
#[must_use]
pub fn redact_public_text(input: &str) -> Cow<'_, str> {
    if is_canonical_redaction_marker(input) {
        return Cow::Borrowed(input);
    }
    let hits = normalized_secret_tags(input);
    if hits.is_empty() {
        Cow::Borrowed(input)
    } else {
        Cow::Owned(format!("[REDACTED:{}]", hits.join(",")))
    }
}

/// Returns the deterministic public identity for an edge relation.
///
/// Secret-bearing relations receive a non-secret full content-digest suffix so distinct relations
/// between the same endpoints remain distinct after redaction. Existing projected relations pass
/// through unchanged.
#[must_use]
pub fn project_public_relation(relation: &str) -> String {
    if let Some((marker, suffix)) = relation.rsplit_once("#r") {
        if is_canonical_redaction_marker(marker)
            && suffix.len() == 64
            && suffix
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        {
            return relation.to_owned();
        }
    }

    let redacted = redact_public_text(relation);
    if redacted.as_ref() == relation {
        relation.to_owned()
    } else {
        format!("{redacted}#r{}", blake3::hash(relation.as_bytes()).to_hex())
    }
}

/// Returns `true` if `text` appears clean (no screened secret patterns).
#[must_use]
pub fn is_clean(text: &str) -> bool {
    screen_for_secrets(text).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_text_has_no_hits() {
        assert!(screen_for_secrets("fn main() { println!(\"hi\"); }").is_empty());
        assert!(is_clean("a normal node label about authentication"));
    }

    #[test]
    fn detects_private_key_block() {
        let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----";
        assert!(screen_for_secrets(pem).contains(&"private_key"));
    }

    #[test]
    fn detects_aws_key() {
        assert!(screen_for_secrets("AKIAIOSFODNN7EXAMPLE").contains(&"aws_access_key_id"));
    }

    #[test]
    fn detects_cargo_token() {
        assert!(screen_for_secrets("CARGO_REGISTRY_TOKEN=abc123").contains(&"cargo_registry_token"));
    }

    #[test]
    fn detects_bearer_header() {
        assert!(screen_for_secrets("Authorization: Bearer eyJhbGci").contains(&"bearer_token"));
    }

    #[test]
    fn detects_bearer_header_with_http_optional_whitespace() {
        for header in [
            "Authorization:  Bearer eyJhbGci",
            "Authorization:\tBearer eyJhbGci",
            "Authorization: \t Bearer\t eyJhbGci",
        ] {
            assert!(screen_for_secrets(header).contains(&"bearer_token"));
        }
    }

    #[test]
    fn detects_bearer_header_with_unicode_whitespace_and_format_characters() {
        for header in [
            "Authorization:\u{00a0}Bearer eyJhbGci",
            "Authorization:\u{200b}Bearer eyJhbGci",
            "Author\u{2060}ization: Bearer eyJhbGci",
            "Authorization: Bearer\u{200b}eyJhbGci",
        ] {
            assert!(screen_for_secrets(header).contains(&"bearer_token"));
            assert_eq!(redact_public_text(header), "[REDACTED:bearer_token]");
        }
    }

    #[test]
    fn bearer_match_requires_scheme_separator() {
        assert!(!screen_for_secrets("Authorization: Bearerish token").contains(&"bearer_token"));
    }

    #[test]
    fn detects_api_key_variants() {
        assert!(screen_for_secrets("api_key=zzz").contains(&"api_key"));
        assert!(screen_for_secrets("ApiKey: zzz").contains(&"api_key"));
        assert!(screen_for_secrets("x-api-key: zzz").contains(&"api_key"));
    }

    #[test]
    fn detects_slack_token() {
        assert!(screen_for_secrets("xoxb-123-456-abc").contains(&"slack_token"));
    }

    #[test]
    fn is_clean_is_inverse_of_hits() {
        assert!(!is_clean("AKIAIOSFODNN7EXAMPLE"));
        assert!(is_clean("nothing to see here"));
    }

    #[test]
    fn multiple_secrets_all_reported() {
        let hits = screen_for_secrets("api_key=x and xoxb-1 token");
        assert!(hits.contains(&"api_key"));
        assert!(hits.contains(&"slack_token"));
    }

    #[test]
    fn canonical_redaction_marker_requires_known_strictly_ordered_tags() {
        assert!(is_canonical_redaction_marker("[REDACTED:api_key]"));
        assert!(is_canonical_redaction_marker(
            "[REDACTED:aws_access_key_id,api_key,slack_token]"
        ));
        assert!(!is_canonical_redaction_marker("[REDACTED:unknown]"));
        assert!(!is_canonical_redaction_marker(
            "[REDACTED:slack_token,api_key]"
        ));
        assert!(!is_canonical_redaction_marker("[REDACTED:api_key,api_key]"));
    }

    #[test]
    fn public_relation_projection_is_stable_and_idempotent() {
        let alpha = project_public_relation("api_key=alpha");
        let beta = project_public_relation("api_key=beta");
        assert_ne!(alpha, beta);
        assert_eq!(project_public_relation(&alpha), alpha);
        assert_eq!(alpha.rsplit_once("#r").unwrap().1.len(), 64);
        assert!(alpha.ends_with(blake3::hash("api_key=alpha".as_bytes()).to_hex().as_str()));
    }
}
