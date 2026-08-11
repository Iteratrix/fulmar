//! Bluesky client configuration.

use std::time::Duration;

/// Login + connection settings.
#[derive(Debug, Clone)]
pub struct BlueskyConfig {
    /// PDS URL (e.g. `https://bsky.social`). Trailing slash optional.
    pub service_url: String,
    /// Chat service URL. The official chat service lives at
    /// `https://api.bsky.chat`; `bsky.social` stopped proxying chat
    /// lex methods in 2026-05 and now returns 501 for them, so the
    /// client must hit chat directly. Overridable so tests can point
    /// it at a wiremock server.
    pub chat_service_url: String,
    /// Handle or DID to log in as. App password expected for
    /// `password`; OAuth/DPoP would mean a different code path.
    pub identifier: String,
    /// App password. Production deployments source this from the
    /// secrets file in `runtime/deploy/etc/env`.
    pub password: String,
    /// Per-call HTTP timeout. AT Protocol calls are usually fast;
    /// the default is generous to handle PDS slowness without false
    /// positives.
    pub http_timeout: Duration,
}

impl Default for BlueskyConfig {
    fn default() -> Self {
        Self {
            service_url: "https://bsky.social".to_string(),
            chat_service_url: "https://api.bsky.chat".to_string(),
            identifier: String::new(),
            password: String::new(),
            http_timeout: Duration::from_secs(30),
        }
    }
}
