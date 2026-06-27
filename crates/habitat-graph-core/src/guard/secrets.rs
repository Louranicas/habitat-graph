//! Cheap, dependency-free secret screening — run before any content is persisted or exported.
//!
//! This is a *screen*, not a vault scanner: it catches the obvious, high-signal leaks (private keys,
//! cloud tokens, bearer headers) so they never land in a `graph.json`, receipt, or vault note.

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
    if lower.contains("authorization: bearer ") || lower.contains("authorization:bearer ") {
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
}
