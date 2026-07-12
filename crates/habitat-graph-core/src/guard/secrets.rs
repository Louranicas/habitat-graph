//! Cheap, dependency-free secret screening — run before any content is persisted or exported.
//!
//! This is a *screen*, not a vault scanner: it catches the obvious, high-signal leaks (private keys,
//! cloud tokens, bearer headers) so they never land in a `graph.json`, receipt, or vault note.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use crate::NodeId;

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
            | 0x034F
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x115F..=0x1160
            | 0x17B4..=0x17B5
            | 0x180B..=0x180F
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x206F
            | 0x3164
            | 0xFE00..=0xFE0F
            | 0xFEFF
            | 0xFFA0
            | 0xFFF0..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0000..=0xE0FFF
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

fn leading_canonical_redaction_marker(input: &str) -> Option<&str> {
    let end = input.find(']')?;
    let marker = input.get(..=end)?;
    is_canonical_redaction_marker(marker).then_some(marker)
}

/// Returns the safe public text shared by all projections of an edge relation.
///
/// A canonical marker at the start of a previously projected value is reduced to the marker itself;
/// any trailing discriminator or attacker-controlled content is discarded and regenerated by
/// [`PublicRelationProjector`] when structural edge identity is required.
#[must_use]
pub fn project_public_relation(relation: &str) -> String {
    if let Some(marker) = leading_canonical_redaction_marker(relation) {
        return marker.to_owned();
    }

    redact_public_text(relation).into_owned()
}

/// Stateful public edge-relation projection using endpoint-local occurrence ordinals.
///
/// Secret-bearing relations are rendered as a canonical marker followed by `#eN`, where `N` is
/// their fixed-width, zero-padded occurrence among edges with the same endpoints and marker. The
/// discriminator depends only on public graph structure, so it preserves parallel edges without
/// exposing a digest of the original relation.
#[derive(Debug, Default)]
pub struct PublicRelationProjector {
    occurrences: HashMap<(NodeId, NodeId, String), usize>,
}

impl PublicRelationProjector {
    /// Creates an empty projector.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Projects one relation using its edge endpoints as structural context.
    #[must_use]
    pub fn project(&mut self, source: NodeId, target: NodeId, relation: &str) -> String {
        let projected = project_public_relation(relation);
        if !is_canonical_redaction_marker(&projected) {
            return projected;
        }

        let occurrence = self
            .occurrences
            .entry((source, target, projected.clone()))
            .or_default();
        let result = format!("{projected}#e{occurrence:020}");
        *occurrence = occurrence.saturating_add(1);
        result
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
            "Authorization: Bearer\u{fe0f} eyJhbGci",
            "Author\u{034f}ization: Bearer eyJhbGci",
            "Author\u{2065}ization: Bearer eyJhbGci",
            "Authorization: Be\u{e0080}arer eyJhbGci",
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
    fn public_relation_projection_uses_structural_identity() {
        let mut projector = PublicRelationProjector::new();
        let alpha = projector.project(NodeId::new(1), NodeId::new(2), "api_key=alpha");
        let beta = projector.project(NodeId::new(1), NodeId::new(2), "api_key=beta");
        assert_ne!(alpha, beta);
        assert_eq!(alpha, "[REDACTED:api_key]#e00000000000000000000");
        assert_eq!(beta, "[REDACTED:api_key]#e00000000000000000001");

        let mut replay = PublicRelationProjector::new();
        assert_eq!(
            replay.project(NodeId::new(1), NodeId::new(2), &alpha),
            alpha
        );
    }

    #[test]
    fn projected_relation_discards_untrusted_trailing_content() {
        let relation = "[REDACTED:bearer_token]#r0123456789abcdefsecret";
        assert_eq!(project_public_relation(relation), "[REDACTED:bearer_token]");

        let mut projector = PublicRelationProjector::new();
        assert_eq!(
            projector.project(NodeId::new(3), NodeId::new(4), relation),
            "[REDACTED:bearer_token]#e00000000000000000000"
        );
    }

    #[test]
    fn projected_relation_ordinals_remain_sorted_and_stable_after_replay() {
        let mut projector = PublicRelationProjector::new();
        let projected: Vec<String> = (0..12)
            .map(|index| {
                projector.project(
                    NodeId::new(1),
                    NodeId::new(2),
                    &format!("api_key=value{index}"),
                )
            })
            .collect();
        let mut sorted = projected.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, projected);

        let mut replay = PublicRelationProjector::new();
        let replayed: Vec<String> = sorted
            .iter()
            .map(|relation| replay.project(NodeId::new(1), NodeId::new(2), relation))
            .collect();
        assert_eq!(replayed, projected);
    }
}
