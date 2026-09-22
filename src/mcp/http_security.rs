use axum::{
    body::Body,
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::net::SocketAddr;

/// Explicit upper bound for MCP HTTP request bodies.
///
/// Doge-Code's MCP tools never need huge payloads; this keeps a misbehaving
/// or malicious client from forcing unbounded buffering in the rmcp transport
/// (which collects the body without its own strict limit).
///
/// Enforced by `tower_http::limit::RequestBodyLimitLayer` in `server.rs`
/// (covers both `Content-Length` and chunked bodies).
pub const MAX_MCP_REQUEST_BODY_BYTES: usize = 2 * 1024 * 1024;

/// Validate that a local MCP bind address is loopback-only.
///
/// Allowed:
/// - `127.0.0.1:<port>` (including port `0` for tests)
/// - `[::1]:<port>`
/// - `localhost:<port>`
///
/// Rejected: `0.0.0.0`, `::`, LAN/public IPs, arbitrary hostnames (even if
/// they currently DNS-resolve to loopback — DNS rebinding must not turn an
/// attacker hostname into an allowed bind).
pub fn validate_local_bind_address(address: &str) -> anyhow::Result<()> {
    // Fast path: literal socket address (covers 127.0.0.1 and [::1]).
    if let Ok(sock) = address.parse::<SocketAddr>() {
        if sock.ip().is_loopback() {
            // Enforce the explicit allowlist (127.0.0.1 / ::1) rather than the
            // whole 127/8 range: the documented boundary is loopback-only with
            // named hosts.
            let ip_str = sock.ip().to_string();
            if ip_str == "127.0.0.1" || ip_str == "::1" {
                return Ok(());
            }
            anyhow::bail!(
                "Remote MCP binding is disabled until spec-compliant authorization is configured. Use a loopback address (127.0.0.1, ::1, localhost)."
            );
        }
        anyhow::bail!(
            "Remote MCP binding is disabled until spec-compliant authorization is configured. Use a loopback address (127.0.0.1, ::1, localhost)."
        );
    }

    // Hostname path: only `localhost` (with explicit port) is allowed.
    // Split host/port at the last ':' to avoid hand-rolled IPv6 splitting;
    // bracketed IPv6 literals were already handled by the SocketAddr path.
    let (host, port_str) = address.rsplit_once(':').ok_or_else(|| {
        anyhow::anyhow!(
            "Invalid MCP bind address '{address}': expected host:port. Use a loopback address (127.0.0.1, ::1, localhost)."
        )
    })?;
    if host.is_empty() || port_str.is_empty() {
        anyhow::bail!(
            "Invalid MCP bind address '{address}': expected host:port. Use a loopback address."
        );
    }
    // Reject bracketed hosts that failed SocketAddr parsing (malformed IPv6).
    if host.contains('[') || host.contains(']') || host.contains(':') {
        anyhow::bail!(
            "Remote MCP binding is disabled until spec-compliant authorization is configured. Use a loopback address (127.0.0.1, ::1, localhost)."
        );
    }
    port_str.parse::<u16>().map_err(|_| {
        anyhow::anyhow!(
            "Invalid MCP bind address '{address}': invalid port. Use a loopback address."
        )
    })?;
    if host.eq_ignore_ascii_case("localhost") {
        return Ok(());
    }
    anyhow::bail!(
        "Remote MCP binding is disabled until spec-compliant authorization is configured. Use a loopback address (127.0.0.1, ::1, localhost)."
    )
}

/// Check whether a `Host` header value targets this local listener.
///
/// Allowed hostnames (any port): `localhost`, `127.0.0.1`, `::1`.
/// Parsing uses `http::uri::Authority` so IPv6 `[::1]:port` is handled
/// without hand-rolled `split(':')`. Userinfo (`user@host`) is rejected
/// outright so `evil@127.0.0.1` cannot smuggle an attacker label past the
/// allowlist.
pub fn is_allowed_host(host_value: &str) -> bool {
    if host_value.contains('@') {
        return false;
    }
    let authority: Result<axum::http::uri::Authority, _> = host_value.parse();
    let Ok(authority) = authority else {
        return false;
    };
    let host = authority.host().to_ascii_lowercase();
    // Authority::host() strips brackets for IPv6 on most versions, but accept
    // the bracketed form too for robustness.
    matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1" | "[::1]")
}

/// Check whether an `Origin` header value is a loopback origin.
///
/// - No `Origin` header -> allowed (non-browser MCP clients don't send one).
/// - Present -> only `http://localhost:*`, `http://127.0.0.1:*`,
///   `http://[::1]:*` are allowed. Scheme must be `http`; ports are not
///   compared (the hostname is the security boundary). URLs with userinfo
///   (`http://evil@localhost`) are rejected.
pub fn is_allowed_origin(origin_value: &str) -> bool {
    let Ok(url) = url::Url::parse(origin_value) else {
        return false;
    };
    if url.scheme() != "http" {
        return false;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    // url crate keeps IPv6 brackets in host_str() for some versions.
    matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1" | "[::1]")
}

/// Axum middleware enforcing local-only HTTP security.
///
/// Order: Host validation -> Origin validation. Request-body size is enforced
/// by the inner `RequestBodyLimitLayer` (covers both `Content-Length` and
/// chunked bodies). Rejections are plain `403` responses issued before the
/// MCP handler runs; no host filesystem or secret details are included.
/// `X-Forwarded-Host` and friends are deliberately ignored (no reverse-proxy
/// trust).
pub async fn mcp_security_middleware(
    headers: HeaderMap,
    request: axum::http::Request<Body>,
    next: Next,
) -> Response {
    // Host validation (fail closed on missing/invalid).
    let host_ok = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .is_some_and(is_allowed_host);
    if !host_ok {
        tracing::warn!("MCP request rejected: untrusted Host");
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }

    // Origin validation (absent -> allow for non-browser clients).
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok())
        && !is_allowed_origin(origin)
    {
        tracing::warn!("MCP request rejected: untrusted Origin");
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }

    // Only `Host` and `Origin` drive security decisions; forwarded headers
    // are ignored.
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_loopback_bind_allowed() {
        for addr in ["127.0.0.1:8000", "127.0.0.1:0", "[::1]:8000", "[::1]:0"] {
            assert!(
                validate_local_bind_address(addr).is_ok(),
                "should allow {addr}"
            );
        }
        // localhost forms (any casing) are allowed.
        for addr in ["localhost:8000", "localhost:0", "LOCALHOST:8000"] {
            assert!(
                validate_local_bind_address(addr).is_ok(),
                "should allow {addr}"
            );
        }
    }

    #[test]
    fn test_non_loopback_ipv4_bind_rejected() {
        assert!(validate_local_bind_address("0.0.0.0:8000").is_err());
    }

    #[test]
    fn test_unspecified_ipv6_bind_rejected() {
        assert!(validate_local_bind_address("[::]:8000").is_err());
    }

    #[test]
    fn test_arbitrary_hostname_bind_rejected() {
        // Even a hostname that *might* resolve to loopback must not be trusted.
        assert!(validate_local_bind_address("attacker.example:8000").is_err());
        assert!(validate_local_bind_address("example.com:8000").is_err());
        assert!(validate_local_bind_address("192.168.1.10:8000").is_err());
    }

    #[test]
    fn test_host_allowlist() {
        for host in [
            "127.0.0.1:8000",
            "127.0.0.1:49321",
            "localhost:8000",
            "LOCALHOST:8000",
            "[::1]:8000",
        ] {
            assert!(is_allowed_host(host), "should allow {host}");
        }
        for host in ["evil.example", "evil.example:8000", "example.com:8000"] {
            assert!(!is_allowed_host(host), "should reject {host}");
        }
        // Userinfo must not smuggle an attacker label past the allowlist.
        for host in ["evil@127.0.0.1:8000", "user@localhost:8000", "a@b"] {
            assert!(!is_allowed_host(host), "should reject userinfo {host}");
        }
        assert!(!is_allowed_host("not a host!!"));
    }

    #[test]
    fn test_origin_policy() {
        // Absent origin is handled by the middleware (allowed); here we test
        // present-origin values.
        assert!(is_allowed_origin("http://localhost:12345"));
        assert!(is_allowed_origin("http://127.0.0.1:8000"));
        assert!(is_allowed_origin("http://[::1]:8000"));
        assert!(!is_allowed_origin("https://evil.example"));
        assert!(!is_allowed_origin("http://evil.example"));
        assert!(!is_allowed_origin("null"));
        assert!(!is_allowed_origin("not-a-url"));
        // Local server is plain HTTP; https origins are rejected.
        assert!(!is_allowed_origin("https://localhost:8000"));
        // Userinfo must not smuggle an attacker label past the allowlist.
        assert!(!is_allowed_origin("http://evil@localhost:8000"));
        assert!(!is_allowed_origin("http://user:pass@127.0.0.1:8000"));
    }
}
