//! Cheap, dependency-free secret screening — run before any content is persisted or exported.
//!
//! This is a *screen*, not a vault scanner: it catches the obvious, high-signal leaks (private keys,
//! cloud tokens, bearer headers) so they never land in a `graph.json`, receipt, or vault note.

use std::borrow::Cow;

use crate::content_id;

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
        let credential = lower[index + "authorization:".len()..].trim_start_matches([' ', '\t']);
        credential
            .strip_prefix("bearer")
            .is_some_and(|rest| rest.starts_with(' ') || rest.starts_with('\t'))
    })
}

/// Screens `text` for obvious secret patterns, returning the kind tags matched (empty = clean).
#[must_use]
pub fn screen_for_secrets(text: &str) -> Vec<&'static str> {
    let mut hits = Vec::new();
    let lower = text.to_ascii_lowercase();

    if text.contains("-----BEGIN") && text.contains("PRIVATE KEY-----") {
        hits.push("private_key");
    }
    if text.contains("AKIA") || text.contains("ASIA") {
        hits.push("aws_access_key_id");
    }
    if lower.contains("cargo_registry_token") {
        hits.push("cargo_registry_token");
    }
    if contains_bearer_authorization(&lower) {
        hits.push("bearer_token");
    }
    if lower.contains("api_key") || lower.contains("apikey") || lower.contains("api-key") {
        hits.push("api_key");
    }
    if lower.contains("xoxb-") || lower.contains("xoxp-") {
        hits.push("slack_token");
    }

    hits
}

fn normalized_secret_tags(input: &str) -> Vec<&'static str> {
    let stripped: String = input
        .chars()
        .filter(|character| {
            !character.is_control() && !matches!(*character, '\u{FFFE}' | '\u{FFFF}')
        })
        .collect();
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

    let mut encountered = std::collections::HashSet::new();
    for candidate in [
        input,
        stripped.as_str(),
        filename_like.as_str(),
        compact.as_str(),
    ] {
        encountered.extend(screen_for_secrets(candidate));
    }
    SECRET_TAG_ORDER
        .iter()
        .copied()
        .filter(|tag| encountered.contains(tag))
        .collect()
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
/// Secret-bearing relations receive a non-secret content-id suffix so distinct relations between
/// the same endpoints remain distinct after redaction. Existing projected relations pass through
/// unchanged.
#[must_use]
pub fn project_public_relation(relation: &str) -> String {
    if let Some((marker, suffix)) = relation.rsplit_once("#r") {
        if is_canonical_redaction_marker(marker)
            && suffix.len() == 8
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
        format!("{redacted}#r{:08x}", content_id(relation))
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
    }
}
