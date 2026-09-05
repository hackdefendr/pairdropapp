//! Resolves a user-typed server address into the URLs PairDrop actually uses.
//!
//! The web client derives its WebSocket endpoint from `location.host + location.pathname`
//! unless `/config` names a separate `signalingServer`. Both are reproduced here.

use serde::{Deserialize, Deserializer};
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerEndpoint {
    /// Normalized base, always with a scheme and a trailing slash.
    base: Url,
}

impl ServerEndpoint {
    /// Returns `None` for anything that isn't a usable http(s) address.
    pub fn parse(address: &str) -> Option<Self> {
        let mut text = address.trim().to_string();
        if text.is_empty() {
            return None;
        }

        if !text.contains("://") {
            // Bare host: assume TLS unless it looks like something on the local network.
            text = if looks_local(&text) {
                format!("http://{text}")
            } else {
                format!("https://{text}")
            };
        }
        if !text.ends_with('/') {
            text.push('/');
        }

        let base = Url::parse(&text).ok()?;
        if !matches!(base.scheme(), "http" | "https") || base.host_str().is_none() {
            return None;
        }
        Some(Self { base })
    }

    pub fn is_secure(&self) -> bool {
        self.base.scheme() == "https"
    }

    pub fn base(&self) -> &Url {
        &self.base
    }

    pub fn config_url(&self) -> Url {
        self.base.join("config").expect("base always ends in a slash")
    }

    /// `host + path` — what the web client passes as the WebSocket domain when the
    /// instance does not override it.
    pub fn ws_domain(&self) -> String {
        let mut host = self.base.host_str().unwrap_or_default().to_string();
        // `Url::port` is None when the port is the scheme default, which is what we
        // want: the web client reads `location.host`, which omits it too.
        if let Some(port) = self.base.port() {
            host.push_str(&format!(":{port}"));
        }
        let mut path = self.base.path().to_string();
        if path.is_empty() {
            path.push('/');
        }
        if !path.ends_with('/') {
            path.push('/');
        }
        host + &path
    }

    /// `signaling_server` is the `/config` value for instances that delegate signaling
    /// elsewhere; upstream sends it with a trailing slash.
    pub fn websocket_url(
        &self,
        signaling_server: Option<&str>,
        peer_id: Option<&str>,
        peer_id_hash: Option<&str>,
    ) -> Option<Url> {
        let mut domain = signaling_server
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.ws_domain());
        if !domain.ends_with('/') {
            domain.push('/');
        }

        let scheme = if self.is_secure() { "wss" } else { "ws" };
        let mut url = Url::parse(&format!("{scheme}://{domain}server")).ok()?;

        {
            let mut query = url.query_pairs_mut();
            query.append_pair("webrtc_supported", "true");
            // The server only reuses an identity when both halves are present.
            if let (Some(id), Some(hash)) = (peer_id, peer_id_hash) {
                query.append_pair("peer_id", id);
                query.append_pair("peer_id_hash", hash);
            }
        }
        Some(url)
    }
}

fn looks_local(host: &str) -> bool {
    let name = host.split('/').next().unwrap_or(host);
    let bare = name.split(':').next().unwrap_or(name);

    if bare == "localhost" || bare.ends_with(".local") {
        return true;
    }
    if bare.starts_with("192.168.") || bare.starts_with("10.") || bare.starts_with("127.") {
        return true;
    }
    // 172.16.0.0/12 — the second octet decides, so parse it rather than prefix-match.
    if let Some(rest) = bare.strip_prefix("172.") {
        if let Some(octet) = rest.split('.').next().and_then(|o| o.parse::<u8>().ok()) {
            return (16..=31).contains(&octet);
        }
    }
    false
}

/// Response body of `GET /config`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceConfig {
    #[serde(default, deserialize_with = "string_or_false")]
    pub signaling_server: Option<String>,
}

/// The server sends `signalingServer: false` when unset, not a string — and older
/// builds omit the key entirely.
fn string_or_false<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::String(s) if !s.is_empty() => Some(s),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_host_defaults_to_https() {
        let endpoint = ServerEndpoint::parse("drop.example.com").unwrap();
        assert!(endpoint.is_secure());
        assert_eq!(endpoint.ws_domain(), "drop.example.com/");
    }

    #[test]
    fn lan_address_defaults_to_http() {
        let endpoint = ServerEndpoint::parse("192.168.1.50:3000").unwrap();
        assert!(!endpoint.is_secure());
        assert_eq!(endpoint.ws_domain(), "192.168.1.50:3000/");
    }

    #[test]
    fn local_forms_are_recognised() {
        for address in ["localhost:3000", "nas.local", "10.0.0.5", "127.0.0.1", "172.16.0.1", "172.31.255.1"] {
            assert!(!ServerEndpoint::parse(address).unwrap().is_secure(), "{address} should be http");
        }
        // Just outside 172.16.0.0/12, so it is a public address.
        for address in ["172.15.0.1", "172.32.0.1", "example.com"] {
            assert!(ServerEndpoint::parse(address).unwrap().is_secure(), "{address} should be https");
        }
    }

    /// The web client builds `protocol://host+pathname + "server"`, so a subpath
    /// deployment has to keep its prefix.
    #[test]
    fn websocket_url_includes_subpath() {
        let endpoint = ServerEndpoint::parse("https://example.com/pairdrop").unwrap();
        let url = endpoint.websocket_url(None, None, None).unwrap();
        assert_eq!(url.scheme(), "wss");
        assert_eq!(url.path(), "/pairdrop/server");
        assert_eq!(url.query(), Some("webrtc_supported=true"));
    }

    #[test]
    fn websocket_url_carries_peer_identity() {
        let endpoint = ServerEndpoint::parse("http://localhost:3000").unwrap();
        let url = endpoint.websocket_url(None, Some("abc"), Some("def")).unwrap();
        assert_eq!(url.scheme(), "ws");
        let query = url.query().unwrap();
        assert!(query.contains("peer_id=abc"), "{query}");
        assert!(query.contains("peer_id_hash=def"), "{query}");
    }

    /// Half an identity is no identity: the server would reject it.
    #[test]
    fn websocket_url_omits_partial_identity() {
        let endpoint = ServerEndpoint::parse("http://localhost:3000").unwrap();
        let url = endpoint.websocket_url(None, Some("abc"), None).unwrap();
        assert_eq!(url.query(), Some("webrtc_supported=true"));
    }

    #[test]
    fn signaling_server_override_wins() {
        let endpoint = ServerEndpoint::parse("https://example.com").unwrap();
        let url = endpoint
            .websocket_url(Some("signal.example.net/"), None, None)
            .unwrap();
        assert_eq!(url.host_str(), Some("signal.example.net"));
        assert_eq!(url.path(), "/server");
    }

    #[test]
    fn rejects_garbage() {
        assert!(ServerEndpoint::parse("").is_none());
        assert!(ServerEndpoint::parse("   ").is_none());
        assert!(ServerEndpoint::parse("ftp://example.com").is_none());
    }

    #[test]
    fn config_decodes_false_as_absent() {
        let config: InstanceConfig =
            serde_json::from_str(r#"{"signalingServer":false,"buttons":{}}"#).unwrap();
        assert!(config.signaling_server.is_none());

        let config: InstanceConfig = serde_json::from_str(r#"{"buttons":{}}"#).unwrap();
        assert!(config.signaling_server.is_none());

        let config: InstanceConfig =
            serde_json::from_str(r#"{"signalingServer":"signal.example.net/"}"#).unwrap();
        assert_eq!(config.signaling_server.as_deref(), Some("signal.example.net/"));
    }
}
