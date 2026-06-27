//! Lexical URL validation for ingestion (cheap pre-check; full parsing + size/timeout caps live in
//! `habitat-graph-source::ingest`). Pure: no network access.

/// Maximum accepted URL length, in bytes.
pub const MAX_URL_LEN: usize = 2048;

/// Validates a URL string for ingestion.
///
/// Accepts only `http`/`https`, requires a non-empty host, and rejects control characters, spaces,
/// and embedded credentials (`user:pass@host`).
///
/// Returns the validated URL unchanged.
///
/// # Errors
/// Returns [`GraphError::Guard`](crate::GraphError::Guard) describing the first violation.
pub fn validate_url(raw: &str) -> crate::Result<String> {
    let reject = |msg: &str| Err(crate::GraphError::Guard(format!("invalid url: {msg}")));

    if raw.len() > MAX_URL_LEN {
        return reject("exceeds maximum length");
    }
    if raw
        .chars()
        .any(|c| c == ' ' || crate::guard::sanitize::is_render_dangerous(c))
    {
        return reject("contains control, space, or bidi/zero-width character");
    }
    let rest = if let Some(r) = raw.strip_prefix("https://") {
        r
    } else if let Some(r) = raw.strip_prefix("http://") {
        r
    } else {
        return reject("scheme must be http or https");
    };
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() {
        return reject("missing host");
    }
    if host.contains('@') {
        return reject("embedded credentials are not allowed");
    }
    Ok(raw.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_https() {
        assert_eq!(
            validate_url("https://example.com/a").unwrap(),
            "https://example.com/a"
        );
    }

    #[test]
    fn accepts_http_root() {
        assert!(validate_url("http://localhost:8133/health").is_ok());
    }

    #[test]
    fn rejects_other_schemes() {
        assert!(validate_url("ftp://example.com").is_err());
        assert!(validate_url("file:///etc/passwd").is_err());
        assert!(validate_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn rejects_missing_host() {
        assert!(validate_url("https://").is_err());
        assert!(validate_url("https:///path").is_err());
    }

    #[test]
    fn rejects_embedded_credentials() {
        assert!(validate_url("https://user:pass@evil.com/").is_err());
    }

    #[test]
    fn rejects_control_chars_and_spaces() {
        assert!(validate_url("https://example.com/a b").is_err());
        assert!(validate_url("https://exa\u{0007}mple.com").is_err());
        assert!(validate_url("https://example.com/\u{202E}").is_err());
    }

    #[test]
    fn rejects_overlong() {
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_LEN));
        assert!(validate_url(&long).is_err());
    }

    #[test]
    fn returns_input_unchanged_on_success() {
        let u = "https://api.example.com:443/v1/items?x=1";
        assert_eq!(validate_url(u).unwrap(), u);
    }
}
