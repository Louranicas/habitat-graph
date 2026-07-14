//! `SSRF` guard (PC-tail) — block `add <URL>` requests that would reach internal services.
//!
//! Before fetching a remote `URL`, the ingest path calls [`resolve_safe_url`] which:
//!
//! 1. Rejects non-`http`/`https` schemes (case-insensitive).
//! 2. Extracts the host (literal `IP` or hostname).
//! 3. For literal `IP` addresses, calls [`ip_is_blocked`] directly.
//! 4. For hostnames, resolves via [`std::net::ToSocketAddrs`] once, rejects if **any**
//!    returned address is blocked, and returns the validated addresses.
//!
//! Classification alone does not defeat `DNS` rebinding: a `TTL`-0 resolver can answer the
//! guard's lookup with a public address and the transport's lookup with `127.0.0.1`. The fetch
//! path must therefore **pin the connection** to the addresses returned here rather than
//! resolving the hostname a second time.
//!
//! [`ip_is_blocked`] is the pure classifier (no `I/O`) — fully testable in isolation.
//!
//! ## `IPv4`-mapped `IPv6` bypass hardening
//!
//! `::ffff:127.0.0.1` and `::ffff:a.b.c.d` in general are `IPv4` addresses dressed as `IPv6`.
//! [`std::net::Ipv6Addr::is_loopback`] returns `false` for `::ffff:127.0.0.1`, so a naïve
//! loopback check would allow it through. [`ip_is_blocked`] detects both `IPv4`-mapped
//! (`::ffff:a.b.c.d`) and `IPv4`-compatible (`::a.b.c.d`) forms via
//! [`std::net::Ipv6Addr::to_ipv4`] and classifies the embedded `IPv4` address instead.

use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};

// ── Internal IPv4 classifier ──────────────────────────────────────────────────

/// Classify a bare `IPv4` address. Extracted so the `IPv6` mapper can reuse the logic
/// without recursion.
fn v4_blocked(v4: Ipv4Addr) -> Option<&'static str> {
    let oct = v4.octets();
    if v4.is_loopback() {
        Some("loopback")
    } else if v4.is_unspecified() {
        Some("unspecified")
    } else if v4.is_private() {
        Some("private")
    } else if v4.is_link_local() {
        Some("link-local")
    } else if oct[0] == 100 && (oct[1] & 0xc0) == 0x40 {
        // RFC 6598 carrier-grade NAT shared address space (`100.64.0.0/10`); `Ipv4Addr::is_private`
        // does not cover it, but it can reach an ISP's internal infrastructure — block it.
        Some("cgnat")
    } else {
        None
    }
}

// ── Public IP classifier ──────────────────────────────────────────────────────

/// Returns `Some(reason)` when `ip` must not be fetched by an untrusted-`URL` ingest path,
/// or `None` when it is a publicly routable address.
///
/// ## Blocked classes
///
/// | Class | Examples |
/// |---|---|
/// | Loopback | `127.0.0.1`, `::1` |
/// | Unspecified | `0.0.0.0`, `::` |
/// | `RFC`-1918 private | `10/8`, `172.16/12`, `192.168/16` |
/// | Link-local `IPv4` | `169.254/16` (includes `169.254.169.254` cloud metadata) |
/// | Link-local `IPv6` | `fe80::/10` |
/// | Unique-local `IPv6` | `fc00::/7` |
/// | `IPv4`-mapped `IPv6` | `::ffff:127.0.0.1`, `::ffff:10.0.0.1` |
/// | `IPv4`-compatible `IPv6` | `::10.0.0.1` |
#[must_use]
pub fn ip_is_blocked(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => v4_blocked(v4),
        IpAddr::V6(v6) => {
            // Standard IPv6 loopback (`::1`) and unspecified (`::`) handled first.
            // `::1.to_ipv4()` returns `Some(0.0.0.1)` which is not in any blocked `IPv4`
            // range, so the ordering here is load-bearing.
            if v6.is_loopback() {
                return Some("loopback");
            }
            if v6.is_unspecified() {
                return Some("unspecified");
            }

            // `IPv4`-mapped (`::ffff:a.b.c.d`) and `IPv4`-compatible (`::a.b.c.d`) bypass.
            // `Ipv6Addr::to_ipv4()` returns `Some` for both forms.  Classify the embedded
            // `IPv4` address.  This is the critical case that `is_loopback()` misses.
            if let Some(v4) = v6.to_ipv4() {
                return v4_blocked(v4);
            }

            let first = v6.segments()[0];
            // Unique-local `fc00::/7` — covers `fc00::` through `fdff::`.
            if (first & 0xfe00) == 0xfc00 {
                return Some("unique-local");
            }
            // Link-local `fe80::/10` — covers `fe80::` through `febf::`.
            if (first & 0xffc0) == 0xfe80 {
                return Some("link-local");
            }

            None
        }
    }
}

// ── Internal URL parser ───────────────────────────────────────────────────────

/// Parses a raw `URL` string into its `(scheme, host)` pair.
///
/// - Strips userinfo (`user:pass@`).
/// - Strips port suffix (`host:port` → `host`).
/// - Unwraps `IPv6` bracket notation (`[::1]` → `::1`).
///
/// # Errors
///
/// Returns `Err` when the `URL` is structurally invalid: missing `://`, empty authority,
/// or an unclosed `[` for an `IPv6` literal.
fn parse_url_parts(url: &str) -> Result<(&str, String), String> {
    // Locate the scheme separator "://".
    let scheme_end = url
        .find("://")
        .ok_or_else(|| format!("missing '://' in URL: {url:?}"))?;
    let scheme = &url[..scheme_end];
    // Bounds: `find` guarantees `scheme_end + 3 <= url.len()` because `"://"` occupies 3 bytes.
    let after_scheme = &url[scheme_end + 3..];

    // Authority = everything up to the first path/query/fragment delimiter. NB: `\` is included —
    // WHATWG-compliant fetchers treat a backslash as `/`, so terminating the authority here keeps
    // the guard's host extraction aligned with what the fetcher actually connects to
    // (parser-divergence SSRF bypass: `http://8.8.8.8\@127.0.0.1/` must not slip an internal host).
    let authority_end = after_scheme
        .find(['/', '?', '#', '\\'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];

    if authority.is_empty() {
        return Err(format!("URL has no host: {url:?}"));
    }

    // Strip userinfo: everything up to and including the last `@`.
    let host_and_port = match authority.rfind('@') {
        Some(at) => &authority[at + 1..],
        None => authority,
    };

    // Distinguish `IPv6` literal (`[::1]` / `[::1]:port`) from bare host or `host:port`.
    let host = if host_and_port.starts_with('[') {
        let close = host_and_port
            .find(']')
            .ok_or_else(|| format!("unclosed '[' in URL: {url:?}"))?;
        &host_and_port[1..close]
    } else {
        // Strip trailing `:port`; `rfind` is safe because unbracketed `IPv6` is invalid here.
        match host_and_port.rfind(':') {
            Some(colon) => &host_and_port[..colon],
            None => host_and_port,
        }
    };

    if host.is_empty() {
        return Err(format!("URL has empty host: {url:?}"));
    }

    Ok((scheme, host.to_owned()))
}

fn validate_url_parts(url: &str) -> Result<(&str, String), String> {
    let (scheme, host) = parse_url_parts(url)?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(format!(
            "scheme {scheme:?} is not permitted; only 'http' and 'https' are allowed"
        ));
    }
    Ok((scheme, host))
}

/// Validates the structure and scheme of an untrusted ingest URL without resolving its host.
///
/// # Errors
///
/// Returns `Err(reason)` when the URL cannot be parsed or does not use `http` or `https`.
#[must_use = "ignoring the URL validation result defeats the security purpose"]
pub fn validate_url_syntax(url: &str) -> Result<(), String> {
    validate_url_parts(url).map(|_| ())
}

// ── Public URL guard ──────────────────────────────────────────────────────────

/// Validates that `url` is safe to fetch from an untrusted ingest path.
///
/// Three checks are performed in order:
///
/// 1. **Scheme gate** — only `http` and `https` are permitted (case-insensitive).
/// 2. **Literal `IP` gate** — if the host is an `IP` literal, [`ip_is_blocked`] is called
///    with no `I/O`.
/// 3. **`DNS` resolution gate** — if the host is a hostname, the system resolver is invoked;
///    if **any** returned address is blocked, the `URL` is rejected (`DNS`-rebinding defence).
///
/// # Errors
///
/// Returns `Err(reason)` when:
///
/// - The `URL` cannot be parsed (missing `://`, empty host, unclosed `[`).
/// - The scheme is not `http` or `https`.
/// - The host is a blocked `IP` literal (loopback, private, link-local, etc.).
/// - The hostname resolves to at least one blocked address.
/// - The hostname fails to resolve.
#[must_use = "ignoring the SSRF check result defeats the security purpose"]
pub fn is_safe_url(url: &str) -> Result<(), String> {
    resolve_safe_url(url).map(|_| ())
}

/// Validates `url` like [`is_safe_url`] and returns the validated host with the exact addresses
/// the guard classified.
///
/// Callers performing the fetch must pin the connection to the returned addresses instead of
/// resolving the hostname again: a second lookup can be answered differently (`DNS` rebinding)
/// and reach an internal service the guard never saw.
///
/// # Errors
///
/// Returns `Err(reason)` in every case [`is_safe_url`] does, plus when the hostname resolves to
/// an empty address set.
#[must_use = "ignoring the SSRF check result defeats the security purpose"]
pub fn resolve_safe_url(url: &str) -> Result<(String, Vec<IpAddr>), String> {
    let (_, host) = validate_url_parts(url)?;

    // Literal-IP gate (no I/O).
    if let Ok(ip) = host.parse::<IpAddr>() {
        return match ip_is_blocked(ip) {
            Some(reason) => Err(format!("IP {host} is blocked ({reason})")),
            None => Ok((host, vec![ip])),
        };
    }

    // Hostname — resolve via the system resolver exactly once; check every returned address.
    let socket_addrs = (host.as_str(), 80_u16)
        .to_socket_addrs()
        .map_err(|e| format!("host {host:?} did not resolve: {e}"))?;

    let mut addresses = Vec::new();
    for addr in socket_addrs {
        let ip = addr.ip();
        if let Some(reason) = ip_is_blocked(ip) {
            return Err(format!(
                "host {host:?} resolves to blocked IP {ip} ({reason})"
            ));
        }
        if !addresses.contains(&ip) {
            addresses.push(ip);
        }
    }
    if addresses.is_empty() {
        return Err(format!("host {host:?} did not resolve to any address"));
    }

    Ok((host, addresses))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use super::{ip_is_blocked, is_safe_url, validate_url_syntax};

    // ── helpers ───────────────────────────────────────────────────────────────

    #[must_use]
    fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    /// Build an `IPv4`-mapped `IPv6` address: `::ffff:a.b.c.d`.
    #[must_use]
    fn v6_mapped(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V6(Ipv6Addr::new(
            0,
            0,
            0,
            0,
            0,
            0xffff,
            u16::from(a) << 8 | u16::from(b),
            u16::from(c) << 8 | u16::from(d),
        ))
    }

    /// Build an `IPv4`-compatible `IPv6` address: `::a.b.c.d` (deprecated but still a bypass
    /// vector).
    #[must_use]
    fn v6_compat(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V6(Ipv6Addr::new(
            0,
            0,
            0,
            0,
            0,
            0,
            u16::from(a) << 8 | u16::from(b),
            u16::from(c) << 8 | u16::from(d),
        ))
    }

    // ── ip_is_blocked: IPv4 loopback ──────────────────────────────────────────

    #[test]
    fn v4_loopback_127_0_0_1() {
        assert_eq!(ip_is_blocked(v4(127, 0, 0, 1)), Some("loopback"));
    }

    #[test]
    fn v4_loopback_127_0_0_2() {
        assert_eq!(ip_is_blocked(v4(127, 0, 0, 2)), Some("loopback"));
    }

    #[test]
    fn v4_loopback_127_255_255_255() {
        assert_eq!(ip_is_blocked(v4(127, 255, 255, 255)), Some("loopback"));
    }

    // ── ip_is_blocked: IPv4 unspecified ───────────────────────────────────────

    #[test]
    fn v4_unspecified_0_0_0_0() {
        assert_eq!(ip_is_blocked(v4(0, 0, 0, 0)), Some("unspecified"));
    }

    // ── ip_is_blocked: IPv4 private ───────────────────────────────────────────

    #[test]
    fn v4_private_10_start() {
        assert_eq!(ip_is_blocked(v4(10, 0, 0, 0)), Some("private"));
    }

    #[test]
    fn v4_private_10_mid() {
        assert_eq!(ip_is_blocked(v4(10, 10, 10, 10)), Some("private"));
    }

    #[test]
    fn v4_private_10_end() {
        assert_eq!(ip_is_blocked(v4(10, 255, 255, 255)), Some("private"));
    }

    #[test]
    fn v4_private_172_16_start() {
        assert_eq!(ip_is_blocked(v4(172, 16, 0, 0)), Some("private"));
    }

    #[test]
    fn v4_private_172_31_end() {
        assert_eq!(ip_is_blocked(v4(172, 31, 255, 255)), Some("private"));
    }

    #[test]
    fn v4_private_192_168_start() {
        assert_eq!(ip_is_blocked(v4(192, 168, 0, 0)), Some("private"));
    }

    #[test]
    fn v4_private_192_168_end() {
        assert_eq!(ip_is_blocked(v4(192, 168, 255, 255)), Some("private"));
    }

    // ── ip_is_blocked: IPv4 link-local ────────────────────────────────────────

    #[test]
    fn v4_link_local_start() {
        assert_eq!(ip_is_blocked(v4(169, 254, 0, 1)), Some("link-local"));
    }

    #[test]
    fn v4_link_local_aws_metadata_endpoint() {
        // 169.254.169.254 is the cloud-provider instance-metadata endpoint on all major clouds.
        assert_eq!(ip_is_blocked(v4(169, 254, 169, 254)), Some("link-local"));
    }

    #[test]
    fn v4_link_local_end() {
        assert_eq!(ip_is_blocked(v4(169, 254, 255, 255)), Some("link-local"));
    }

    // ── ip_is_blocked: IPv4 public (allowed) ──────────────────────────────────

    #[test]
    fn v4_public_google_dns() {
        assert_eq!(ip_is_blocked(v4(8, 8, 8, 8)), None);
    }

    #[test]
    fn v4_public_cloudflare_dns() {
        assert_eq!(ip_is_blocked(v4(1, 1, 1, 1)), None);
    }

    #[test]
    fn v4_public_example_dot_com() {
        assert_eq!(ip_is_blocked(v4(93, 184, 216, 34)), None);
    }

    // ── ip_is_blocked: IPv4 boundary — just outside blocked ranges ────────────

    #[test]
    fn v4_just_above_10_slash_8_is_public() {
        assert_eq!(ip_is_blocked(v4(11, 0, 0, 0)), None);
    }

    #[test]
    fn v4_just_below_172_16_slash_12_is_public() {
        assert_eq!(ip_is_blocked(v4(172, 15, 255, 255)), None);
    }

    #[test]
    fn v4_just_above_172_31_slash_12_is_public() {
        assert_eq!(ip_is_blocked(v4(172, 32, 0, 0)), None);
    }

    #[test]
    fn v4_just_below_link_local_is_public() {
        assert_eq!(ip_is_blocked(v4(169, 253, 255, 255)), None);
    }

    #[test]
    fn v4_just_above_link_local_is_public() {
        assert_eq!(ip_is_blocked(v4(169, 255, 0, 0)), None);
    }

    #[test]
    fn v4_lookalike_168_254_is_not_link_local() {
        // Common off-by-one: 168.254.x.x is NOT in 169.254/16.
        assert_eq!(ip_is_blocked(v4(168, 254, 0, 1)), None);
    }

    // ── ip_is_blocked: IPv6 loopback / unspecified ────────────────────────────

    #[test]
    fn v6_loopback_is_blocked() {
        assert_eq!(
            ip_is_blocked(IpAddr::V6(Ipv6Addr::LOCALHOST)),
            Some("loopback")
        );
    }

    #[test]
    fn v6_unspecified_is_blocked() {
        assert_eq!(
            ip_is_blocked(IpAddr::V6(Ipv6Addr::UNSPECIFIED)),
            Some("unspecified")
        );
    }

    // ── ip_is_blocked: IPv6 unique-local (fc00::/7) ───────────────────────────

    #[test]
    fn v6_unique_local_fc00() {
        let ip = IpAddr::V6(Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 1));
        assert_eq!(ip_is_blocked(ip), Some("unique-local"));
    }

    #[test]
    fn v6_unique_local_fd00() {
        let ip = IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1));
        assert_eq!(ip_is_blocked(ip), Some("unique-local"));
    }

    #[test]
    fn v6_unique_local_fdff_upper_bound() {
        let ip = IpAddr::V6(Ipv6Addr::new(
            0xfdff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff,
        ));
        assert_eq!(ip_is_blocked(ip), Some("unique-local"));
    }

    #[test]
    fn v6_fe00_is_not_unique_local() {
        // fe00:: is above fc00::/7 and is NOT unique-local.
        let ip = IpAddr::V6(Ipv6Addr::new(0xfe00, 0, 0, 0, 0, 0, 0, 1));
        assert_eq!(ip_is_blocked(ip), None);
    }

    // ── ip_is_blocked: IPv6 link-local (fe80::/10) ───────────────────────────

    #[test]
    fn v6_link_local_fe80() {
        let ip = IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1));
        assert_eq!(ip_is_blocked(ip), Some("link-local"));
    }

    #[test]
    fn v6_link_local_febf_upper_bound() {
        // fe80::/10 ends at febf:ffff:…
        let ip = IpAddr::V6(Ipv6Addr::new(
            0xfebf, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff,
        ));
        assert_eq!(ip_is_blocked(ip), Some("link-local"));
    }

    #[test]
    fn v6_fec0_is_not_link_local() {
        // fec0:: (deprecated site-local) is outside fe80::/10 and not blocked by this guard.
        let ip = IpAddr::V6(Ipv6Addr::new(0xfec0, 0, 0, 0, 0, 0, 0, 1));
        assert_eq!(ip_is_blocked(ip), None);
    }

    // ── ip_is_blocked: IPv4-mapped IPv6 bypass (critical) ────────────────────

    #[test]
    fn v6_mapped_loopback_must_be_blocked() {
        // ::ffff:127.0.0.1 — the canonical bypass vector.
        // Ipv6Addr::is_loopback() returns false for this address; our guard must not.
        assert_eq!(ip_is_blocked(v6_mapped(127, 0, 0, 1)), Some("loopback"));
    }

    #[test]
    fn v6_mapped_loopback_127_0_0_2_also_blocked() {
        assert_eq!(ip_is_blocked(v6_mapped(127, 0, 0, 2)), Some("loopback"));
    }

    #[test]
    fn v6_mapped_private_10_blocked() {
        assert_eq!(ip_is_blocked(v6_mapped(10, 0, 0, 1)), Some("private"));
    }

    #[test]
    fn v6_mapped_private_192_168_blocked() {
        assert_eq!(ip_is_blocked(v6_mapped(192, 168, 1, 1)), Some("private"));
    }

    #[test]
    fn v6_mapped_private_172_16_blocked() {
        assert_eq!(ip_is_blocked(v6_mapped(172, 16, 0, 1)), Some("private"));
    }

    #[test]
    fn v6_mapped_link_local_blocked() {
        assert_eq!(ip_is_blocked(v6_mapped(169, 254, 1, 1)), Some("link-local"));
    }

    #[test]
    fn v6_mapped_unspecified_blocked() {
        assert_eq!(ip_is_blocked(v6_mapped(0, 0, 0, 0)), Some("unspecified"));
    }

    #[test]
    fn v6_mapped_public_is_allowed() {
        // ::ffff:8.8.8.8 is publicly routable even in mapped form.
        assert_eq!(ip_is_blocked(v6_mapped(8, 8, 8, 8)), None);
    }

    // ── ip_is_blocked: IPv4-compatible bypass ────────────────────────────────

    #[test]
    fn v6_compat_private_10_must_be_blocked() {
        // ::10.0.0.1 (IPv4-compatible, deprecated) is still a bypass vector.
        assert_eq!(ip_is_blocked(v6_compat(10, 0, 0, 1)), Some("private"));
    }

    // ── ip_is_blocked: IPv6 public ───────────────────────────────────────────

    #[test]
    fn v6_public_google_dns() {
        // 2001:4860:4860::8888
        let ip = IpAddr::V6(Ipv6Addr::new(0x2001, 0x4860, 0x4860, 0, 0, 0, 0, 0x8888));
        assert_eq!(ip_is_blocked(ip), None);
    }

    #[test]
    fn v6_public_cloudflare_dns() {
        // 2606:4700:4700::1111
        let ip = IpAddr::V6(Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111));
        assert_eq!(ip_is_blocked(ip), None);
    }

    // ── is_safe_url: scheme gate ──────────────────────────────────────────────

    #[test]
    fn scheme_http_public_literal_ok() {
        assert!(is_safe_url("http://8.8.8.8/").is_ok());
    }

    #[test]
    fn scheme_https_public_literal_ok() {
        assert!(is_safe_url("https://8.8.8.8/").is_ok());
    }

    #[test]
    fn scheme_http_uppercase_ok() {
        // Scheme matching is case-insensitive per RFC 3986 §3.1.
        assert!(is_safe_url("HTTP://8.8.8.8/").is_ok());
    }

    #[test]
    fn scheme_https_mixed_case_ok() {
        assert!(is_safe_url("Https://8.8.8.8/").is_ok());
    }

    #[test]
    fn scheme_ftp_rejected() {
        let err = is_safe_url("ftp://8.8.8.8/file.txt").unwrap_err();
        assert!(err.contains("ftp"), "error should mention scheme: {err}");
    }

    #[test]
    fn scheme_file_rejected() {
        let err = is_safe_url("file:///etc/passwd").unwrap_err();
        assert!(err.contains("file"), "error should mention scheme: {err}");
    }

    #[test]
    fn scheme_dict_rejected() {
        let err = is_safe_url("dict://8.8.8.8/d:word").unwrap_err();
        assert!(err.contains("dict"), "error should mention scheme: {err}");
    }

    #[test]
    fn scheme_gopher_rejected() {
        let err = is_safe_url("gopher://8.8.8.8/").unwrap_err();
        assert!(err.contains("gopher"), "error should mention scheme: {err}");
    }

    #[test]
    fn scheme_javascript_no_sep_err() {
        // `javascript:` has no `://` so parse fails before scheme check.
        assert!(is_safe_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn scheme_data_no_sep_err() {
        // `data:` has no `://`.
        assert!(is_safe_url("data:text/plain,hello").is_err());
    }

    #[test]
    fn syntax_validation_does_not_require_dns() {
        assert!(validate_url_syntax("https://unresolvable.invalid/file.rs").is_ok());
        assert!(validate_url_syntax("ftp://unresolvable.invalid/file.rs").is_err());
    }

    // ── is_safe_url: literal IP gate ─────────────────────────────────────────

    #[test]
    fn literal_loopback_rejected() {
        let err = is_safe_url("http://127.0.0.1/").unwrap_err();
        assert!(err.contains("loopback"), "expected 'loopback' in: {err}");
    }

    #[test]
    fn literal_private_10_rejected() {
        assert!(is_safe_url("http://10.0.0.1/").is_err());
    }

    #[test]
    fn literal_private_192_168_rejected() {
        assert!(is_safe_url("http://192.168.1.1/").is_err());
    }

    #[test]
    fn literal_link_local_aws_metadata_rejected() {
        // 169.254.169.254 is the cloud-metadata endpoint; must never be reachable.
        assert!(is_safe_url("http://169.254.169.254/latest/meta-data/").is_err());
    }

    #[test]
    fn literal_unspecified_rejected() {
        assert!(is_safe_url("http://0.0.0.0/").is_err());
    }

    #[test]
    fn literal_v6_loopback_bracket_rejected() {
        assert!(is_safe_url("http://[::1]/").is_err());
    }

    #[test]
    fn literal_v6_mapped_loopback_bracket_rejected() {
        // [::ffff:127.0.0.1] — bracket notation for the mapped loopback.
        assert!(is_safe_url("http://[::ffff:127.0.0.1]/").is_err());
    }

    #[test]
    fn literal_v6_mapped_private_bracket_rejected() {
        assert!(is_safe_url("http://[::ffff:10.0.0.1]/").is_err());
    }

    #[test]
    fn literal_v6_unique_local_rejected() {
        assert!(is_safe_url("http://[fc00::1]/").is_err());
    }

    #[test]
    fn literal_v6_link_local_bracket_rejected() {
        assert!(is_safe_url("http://[fe80::1]/").is_err());
    }

    #[test]
    fn literal_public_ipv4_allowed() {
        assert!(is_safe_url("http://8.8.8.8/").is_ok());
    }

    #[test]
    fn literal_public_ipv6_bracket_allowed() {
        // 2001:4860:4860::8888 — Google public DNS.
        assert!(is_safe_url("http://[2001:4860:4860::8888]/").is_ok());
    }

    // ── is_safe_url: malformed URL ────────────────────────────────────────────

    #[test]
    fn empty_url_is_err() {
        assert!(is_safe_url("").is_err());
    }

    #[test]
    fn no_scheme_separator_is_err() {
        assert!(is_safe_url("http8.8.8.8").is_err());
    }

    #[test]
    fn no_host_after_scheme_is_err() {
        assert!(is_safe_url("http://").is_err());
    }

    #[test]
    fn empty_host_before_path_is_err() {
        // `http:///path` → authority is empty.
        assert!(is_safe_url("http:///path").is_err());
    }

    #[test]
    fn unclosed_ipv6_bracket_is_err() {
        assert!(is_safe_url("http://[::1/").is_err());
    }

    // ── is_safe_url: URL structural variants ─────────────────────────────────

    #[test]
    fn url_with_explicit_port_public_ok() {
        assert!(is_safe_url("http://8.8.8.8:8080/").is_ok());
    }

    #[test]
    fn url_with_path_and_query_public_ok() {
        assert!(is_safe_url("http://8.8.8.8/foo/bar?q=1#frag").is_ok());
    }

    #[test]
    fn url_with_userinfo_public_ok() {
        assert!(is_safe_url("http://user:pass@8.8.8.8/").is_ok());
    }

    #[test]
    fn url_with_userinfo_does_not_bypass_blocked_host() {
        // Userinfo must not confuse host extraction; 10.0.0.1 is still the host.
        assert!(is_safe_url("http://user@10.0.0.1/").is_err());
    }

    #[test]
    fn url_no_trailing_slash_ok() {
        assert!(is_safe_url("http://8.8.8.8").is_ok());
    }

    #[test]
    fn url_private_host_with_port_rejected() {
        // Port must not rescue a blocked IP.
        assert!(is_safe_url("http://10.0.0.1:8080/").is_err());
    }

    // ── is_safe_url: DNS resolution gate ─────────────────────────────────────

    #[test]
    fn localhost_hostname_always_rejected() {
        // `localhost` resolves to 127.0.0.1 / ::1 on any conforming host.
        // If resolution itself fails that is also Err (safe-fail: deny on uncertainty).
        assert!(
            is_safe_url("http://localhost/").is_err(),
            "'localhost' must never be reachable via the SSRF guard"
        );
    }

    // ── Regression: parser-divergence (backslash) SSRF bypass ────────────────

    #[test]
    fn backslash_authority_terminator_blocks_internal_bypass() {
        // A WHATWG-compliant fetcher treats `\` as `/`, so `http://127.0.0.1\@8.8.8.8/` connects to
        // 127.0.0.1. A naive parser that splits authority only on `/?#` and then on `@` would read
        // the host as 8.8.8.8 (public) and wave it through — the classic parser-divergence bypass.
        // The guard must terminate the authority at `\` and see the real internal host.
        assert!(
            is_safe_url("http://127.0.0.1\\@8.8.8.8/").is_err(),
            "backslash must not smuggle an internal host past the SSRF guard"
        );
    }

    #[test]
    fn backslash_before_path_keeps_real_host() {
        // `http://127.0.0.1\\evil.example/` -> a fetcher connects to 127.0.0.1; must be blocked.
        assert!(is_safe_url("http://127.0.0.1\\evil.example/").is_err());
    }

    // ── Regression: RFC 6598 carrier-grade NAT shared space ──────────────────

    #[test]
    fn cgnat_shared_space_blocked() {
        assert_eq!(ip_is_blocked(v4(100, 64, 0, 1)), Some("cgnat"));
        assert_eq!(ip_is_blocked(v4(100, 100, 50, 25)), Some("cgnat"));
        assert_eq!(ip_is_blocked(v4(100, 127, 255, 255)), Some("cgnat"));
    }

    #[test]
    fn cgnat_boundaries_outside_range_allowed() {
        // 100.0.0.0/10 boundaries: 100.63.x and 100.128.x are NOT in 100.64.0.0/10.
        assert_eq!(ip_is_blocked(v4(100, 63, 255, 255)), None);
        assert_eq!(ip_is_blocked(v4(100, 128, 0, 1)), None);
    }

    // ── resolve_safe_url: address pinning contract ────────────────────────────

    #[test]
    fn resolve_safe_url_pins_a_public_literal_ip() {
        let (host, addrs) = super::resolve_safe_url("https://8.8.8.8/x").expect("public literal");
        assert_eq!(host, "8.8.8.8");
        assert_eq!(addrs, vec![v4(8, 8, 8, 8)]);
    }

    #[test]
    fn resolve_safe_url_unwraps_ipv6_brackets() {
        let (host, addrs) =
            super::resolve_safe_url("http://[2001:4860:4860::8888]/").expect("public v6 literal");
        assert_eq!(host, "2001:4860:4860::8888");
        assert_eq!(addrs.len(), 1);
    }

    #[test]
    fn resolve_safe_url_rejects_blocked_literal() {
        assert!(super::resolve_safe_url("http://127.0.0.1/").is_err());
        assert!(super::resolve_safe_url("http://169.254.169.254/latest/").is_err());
    }
}
