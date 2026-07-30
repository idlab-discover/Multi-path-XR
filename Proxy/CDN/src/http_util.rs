use axum::http::{HeaderMap, HeaderName};
use std::collections::HashSet;

/// Removes hop-by-hop headers that must not be forwarded by proxies.
pub fn strip_hop_by_hop_headers(headers: &mut HeaderMap) {
    // RFC 7230 hop-by-hop headers
    const HOP: &[&str] = &[
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ];

    let mut remove: HashSet<HeaderName> = HashSet::new();
    for h in HOP {
        if let Ok(name) = HeaderName::from_bytes(h.as_bytes()) {
            remove.insert(name);
        }
    }

    // Also remove headers named in "Connection: ..."
    if let Some(conn) = headers.get("connection").and_then(|v| v.to_str().ok()) {
        for token in conn.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            if let Ok(name) = HeaderName::from_bytes(token.as_bytes()) {
                remove.insert(name);
            }
        }
    }

    for name in remove {
        headers.remove(name);
    }
}
