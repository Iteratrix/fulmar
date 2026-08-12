//! XRPC client over reqwest.
//!
//! Thin by design: most methods hand back `serde_json::Value` and the
//! CLI passes server JSON through to the user (`--json` is NDJSON of
//! whatever the server said). Typed structs exist only where fulmar
//! itself must understand the data: session tokens, identity
//! resolution, record refs for chaining writes.
//!
//! ## Auth flow
//!
//! 1. Construction loads the session file (never a password — see
//!    [`crate::session`]).
//! 2. Every authed request sends the in-memory access JWT.
//! 3. On `401`, or `400 ExpiredToken` (the chat service's dialect for
//!    the same condition), the request retries once after
//!    [`Client::refresh`].
//! 4. Refresh is double-checked: serialize in-process behind a tokio
//!    mutex, take the exclusive file lock, re-read the file, and only
//!    spend the refresh token if no other task/process already did.
//!    A refresh rejection surfaces [`ApiError::SessionExpired`] —
//!    terminal, exit code 3, never a prompt.
//!
//! ## Chat routing
//!
//! `chat.bsky.*` calls carry `atproto-proxy:
//! did:web:api.bsky.chat#bsky_chat` AND go directly to the chat
//! service base URL rather than the PDS: `bsky.social` stopped
//! proxying chat methods and returns 501 for them (observed 2026-05).

pub mod helpers;
pub mod identity;

use reqwest::{Client as HttpClient, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, warn};

use crate::identifiers::{Did, Handle};
use crate::session::{SessionError, SessionFile, SessionStore};

/// `atproto-proxy` header value for Bluesky chat (DM) routes.
pub const CHAT_PROXY: &str = "did:web:api.bsky.chat#bsky_chat";

/// Default chat service base URL (`did:web:api.bsky.chat`).
pub const DEFAULT_CHAT_URL: &str = "https://api.bsky.chat";

/// Default entryway for `fulmar login` when the handle doesn't
/// resolve elsewhere.
pub const DEFAULT_LOGIN_SERVICE: &str = "https://bsky.social";

/// Video service base URL and its service DID.
pub const DEFAULT_VIDEO_URL: &str = "https://video.bsky.app";
/// DID of the Bluesky video service (service-auth audience).
pub const VIDEO_SERVICE_DID: &str = "did:web:video.bsky.app";

/// Errors surfaced by the client.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("decoding response: {0}")]
    Decode(#[from] serde_json::Error),
    /// Non-2xx with a structured XRPC error body. `kind` mirrors the
    /// AT Protocol `error` field (e.g. `InvalidRequest`).
    #[error("api {status} {kind}: {message}")]
    Api {
        status: u16,
        kind: String,
        message: String,
    },
    /// The refresh chain is dead. Only re-running `fulmar login`
    /// (with the password) can recover; exit code 3.
    #[error(
        "session expired: the refresh token was rejected — run `fulmar login` (once, by someone with the password) to start a new session"
    )]
    SessionExpired,
    #[error(transparent)]
    Session(#[from] SessionError),
    /// 2xx but the body was missing something we needed.
    #[error("unexpected response shape: {0}")]
    Unexpected(String),
    #[error(transparent)]
    Identifier(#[from] crate::identifiers::IdentifierError),
}

/// Where an XRPC call is routed and which proxy header it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// The user's PDS, no proxy header. Writes, session methods, and
    /// `app.bsky.*` reads (which the PDS service-proxies to the
    /// `AppView` by default).
    Pds,
    /// The chat service directly, with the chat proxy header.
    Chat,
    /// The user's PDS with an explicit `atproto-proxy` header
    /// (labelers, video, `fulmar api --proxy`).
    Proxied(String),
}

/// Connection knobs. Defaults are production; tests point the URLs at
/// wiremock.
#[derive(Debug, Clone)]
pub struct ClientOptions {
    pub chat_url: String,
    pub plc_url: String,
    pub video_url: String,
    pub http_timeout: std::time::Duration,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            chat_url: DEFAULT_CHAT_URL.to_string(),
            plc_url: identity::PLC_DIRECTORY.to_string(),
            video_url: DEFAULT_VIDEO_URL.to_string(),
            http_timeout: std::time::Duration::from_secs(30),
        }
    }
}

impl ClientOptions {
    /// Defaults, overridable by `FULMAR_CHAT_URL`, `FULMAR_PLC_URL`,
    /// `FULMAR_VIDEO_URL`, and `FULMAR_TIMEOUT` (seconds). For tests
    /// and self-hosted service deployments; normal use never sets
    /// these.
    #[must_use]
    pub fn from_env() -> Self {
        let mut options = Self::default();
        if let Ok(url) = std::env::var("FULMAR_CHAT_URL") {
            options.chat_url = url;
        }
        if let Ok(url) = std::env::var("FULMAR_PLC_URL") {
            options.plc_url = url;
        }
        if let Ok(url) = std::env::var("FULMAR_VIDEO_URL") {
            options.video_url = url;
        }
        if let Some(secs) = std::env::var("FULMAR_TIMEOUT")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
        {
            options.http_timeout = std::time::Duration::from_secs(secs);
        }
        options
    }
}

/// Build fulmar's HTTP client: rustls with the **bundled** Mozilla
/// root store (`webpki-root-certs`) verified in-process, instead of
/// reqwest 0.13's default `rustls-platform-verifier`. The platform
/// verifier calls the OS trust machinery — on macOS an XPC hop to
/// `trustd` via Security.framework — which sandboxes (Seatbelt
/// profiles, the same lane that breaks `gh`) deny. Bundled roots
/// keep TLS verification entirely inside the process.
///
/// Escape hatch: `FULMAR_NATIVE_ROOTS=1` restores the platform
/// verifier for the one setup bundled roots can't serve — a custom
/// PDS behind a private/enterprise CA (which must then run fulmar
/// unsandboxed on macOS).
///
/// # Errors
///
/// [`ApiError::Http`] if the client cannot be built.
pub fn http_client(timeout: std::time::Duration) -> Result<HttpClient, ApiError> {
    let builder = HttpClient::builder().timeout(timeout);
    if std::env::var("FULMAR_NATIVE_ROOTS").is_ok_and(|v| v == "1") {
        return Ok(builder.build()?);
    }
    let roots = webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .map(|der| reqwest::Certificate::from_der(der))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(builder.tls_certs_only(roots).build()?)
}

/// Wire shape of `createSession` / `refreshSession` responses.
#[derive(Debug, Clone, Deserialize)]
struct WireSession {
    did: Did,
    handle: Handle,
    #[serde(rename = "accessJwt")]
    access_jwt: String,
    #[serde(rename = "refreshJwt")]
    refresh_jwt: String,
    #[serde(rename = "didDoc")]
    did_doc: Option<Value>,
}

/// The authenticated XRPC client. Cheap to share behind a reference;
/// all interior state is locked.
pub struct Client {
    http: HttpClient,
    store: SessionStore,
    session: RwLock<SessionFile>,
    refresh_serial: Mutex<()>,
    chat_url: String,
    video_url: String,
}

impl std::fmt::Debug for Client {
    /// Manual impl so JWTs can never leak through `{:?}`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("store", &self.store)
            .field("chat_url", &self.chat_url)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Construct from a persisted session. This is the normal,
    /// password-free path: every command except `login` starts here.
    ///
    /// # Errors
    ///
    /// [`ApiError::Session`] when the session file is missing or
    /// corrupt (the CLI maps missing to exit code 3), or
    /// [`ApiError::Http`] if the HTTP client cannot be built.
    pub fn from_store(store: SessionStore, options: &ClientOptions) -> Result<Self, ApiError> {
        let session = store.load()?;
        let http = http_client(options.http_timeout)?;
        Ok(Self {
            http,
            store,
            session: RwLock::new(session),
            refresh_serial: Mutex::new(()),
            chat_url: options.chat_url.clone(),
            video_url: options.video_url.clone(),
        })
    }

    /// Log in with a password and persist the session. The ONLY path
    /// that ever sees a password. Resolves the account's real PDS
    /// endpoint from the DID document in the `createSession` response
    /// (falling back to directory resolution) so subsequent calls —
    /// chat included — hit the right host even on a custom PDS.
    ///
    /// # Errors
    ///
    /// [`ApiError::Api`] on bad credentials, [`ApiError::Session`] if
    /// the session file cannot be written, or [`ApiError::Http`] on
    /// network failure.
    pub async fn login(
        store: SessionStore,
        options: &ClientOptions,
        service_url: &str,
        identifier: &str,
        password: &str,
    ) -> Result<Self, ApiError> {
        let http = http_client(options.http_timeout)?;
        let url = xrpc_url(service_url, "com.atproto.server.createSession");
        let body = serde_json::json!({ "identifier": identifier, "password": password });
        let resp = http.post(url).json(&body).send().await?;
        let status = resp.status();
        let bytes = resp.bytes().await?;
        let wire: WireSession = decode_xrpc(status, &bytes)?;

        let pds_url = match wire.did_doc.as_ref().and_then(identity::pds_endpoint) {
            Some(url) => url,
            None => identity::resolve_pds_via_directory(&http, &options.plc_url, &wire.did)
                .await
                .unwrap_or_else(|e| {
                    warn!("DID doc resolution failed ({e}); falling back to {service_url}");
                    service_url.to_string()
                }),
        };

        let session = SessionFile::new(
            wire.did,
            wire.handle,
            pds_url,
            wire.access_jwt,
            wire.refresh_jwt,
        );
        store.save(&session)?;
        Ok(Self {
            http,
            store,
            session: RwLock::new(session),
            refresh_serial: Mutex::new(()),
            chat_url: options.chat_url.clone(),
            video_url: options.video_url.clone(),
        })
    }

    /// The authenticated account's DID.
    pub async fn did(&self) -> Did {
        self.session.read().await.did.clone()
    }

    /// The authenticated account's handle (as of login/last refresh).
    pub async fn handle(&self) -> Handle {
        self.session.read().await.handle.clone()
    }

    /// The resolved PDS base URL.
    pub async fn pds_url(&self) -> String {
        self.session.read().await.pds_url.clone()
    }

    /// Authed XRPC query (HTTP GET).
    ///
    /// # Errors
    ///
    /// [`ApiError::Api`] for structured server errors,
    /// [`ApiError::SessionExpired`] when refresh fails, or transport
    /// and decode variants.
    pub async fn get(
        &self,
        route: &Route,
        nsid: &str,
        query: &[(&str, String)],
    ) -> Result<Value, ApiError> {
        self.request(route, nsid, query, None).await
    }

    /// Authed XRPC procedure (HTTP POST with JSON body).
    ///
    /// # Errors
    ///
    /// Same as [`Client::get`].
    pub async fn post(&self, route: &Route, nsid: &str, body: &Value) -> Result<Value, ApiError> {
        self.request(route, nsid, &[], Some(body)).await
    }

    /// Upload raw bytes as a blob. Returns the `blob` ref object to
    /// embed in a record (must be referenced within minutes or the
    /// PDS garbage-collects it).
    ///
    /// # Errors
    ///
    /// Same as [`Client::get`].
    pub async fn upload_blob(&self, bytes: Vec<u8>, content_type: &str) -> Result<Value, ApiError> {
        let url = {
            let session = self.session.read().await;
            xrpc_url(&session.pds_url, "com.atproto.repo.uploadBlob")
        };
        let jwt = self.access_jwt().await;
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&jwt)
            .header("content-type", content_type)
            .body(bytes.clone())
            .send()
            .await?;
        let status = resp.status();
        let body = resp.bytes().await?;
        if is_session_expired(status, &body) {
            self.refresh(&jwt).await?;
            let jwt = self.access_jwt().await;
            let resp = self
                .http
                .post(&url)
                .bearer_auth(&jwt)
                .header("content-type", content_type)
                .body(bytes)
                .send()
                .await?;
            let status = resp.status();
            let body = resp.bytes().await?;
            let value: Value = decode_xrpc(status, &body)?;
            return extract_blob(&value);
        }
        let value: Value = decode_xrpc(status, &body)?;
        extract_blob(&value)
    }

    /// Force a session refresh (used by `fulmar session refresh` as a
    /// health check / seeding tool).
    ///
    /// # Errors
    ///
    /// [`ApiError::SessionExpired`] when the chain is dead.
    pub async fn force_refresh(&self) -> Result<(), ApiError> {
        let jwt = self.access_jwt().await;
        self.refresh(&jwt).await
    }

    /// Authed XRPC query returning the raw response body (CAR files,
    /// blobs — anything non-JSON). Same refresh-and-retry discipline
    /// as [`Client::get`].
    ///
    /// # Errors
    ///
    /// Same as [`Client::get`].
    pub async fn get_bytes(
        &self,
        route: &Route,
        nsid: &str,
        query: &[(&str, String)],
    ) -> Result<Vec<u8>, ApiError> {
        let jwt = self.access_jwt().await;
        let (status, bytes) = self.send_once(route, nsid, query, None, &jwt).await?;
        if is_session_expired(status, &bytes) {
            self.refresh(&jwt).await?;
            let jwt = self.access_jwt().await;
            let (status, bytes) = self.send_once(route, nsid, query, None, &jwt).await?;
            return decode_bytes(status, bytes);
        }
        decode_bytes(status, bytes)
    }

    /// Mint a short-lived service JWT for calling another service
    /// directly (`aud` = service DID, `lxm` = the one method it may
    /// call). Used for the video service.
    ///
    /// # Errors
    ///
    /// [`ApiError::Api`] / transport variants.
    pub async fn service_auth(&self, aud: &str, lxm: &str) -> Result<String, ApiError> {
        let value = self
            .get(
                &Route::Pds,
                "com.atproto.server.getServiceAuth",
                &[("aud", aud.to_string()), ("lxm", lxm.to_string())],
            )
            .await?;
        value
            .get("token")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .ok_or_else(|| ApiError::Unexpected("getServiceAuth response missing token".into()))
    }

    /// Upload a video to the video service. Returns the initial job
    /// status (`jobId`, `state`, possibly a `blob` if processing was
    /// instant or the video already exists).
    ///
    /// # Errors
    ///
    /// [`ApiError::Api`] / transport variants.
    pub async fn upload_video(&self, bytes: Vec<u8>, name: &str) -> Result<Value, ApiError> {
        let token = self
            .service_auth(VIDEO_SERVICE_DID, "app.bsky.video.uploadVideo")
            .await?;
        let did = self.did().await;
        let url = xrpc_url(&self.video_url, "app.bsky.video.uploadVideo");
        let resp = self
            .http
            .post(&url)
            .query(&[("did", did.as_str()), ("name", name)])
            .bearer_auth(&token)
            .header("content-type", "video/mp4")
            .body(bytes)
            .send()
            .await?;
        let status = resp.status();
        let body = resp.bytes().await?;
        // The service reports "video already processed" as a 409-ish
        // structured error carrying the finished jobStatus — surface
        // that as success so re-posting the same file works.
        if !status.is_success() {
            let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            if let Some(job) = parsed.get("jobStatus") {
                return Ok(job.clone());
            }
            return decode_xrpc(status, &body);
        }
        let value: Value = decode_xrpc(status, &body)?;
        Ok(value.get("jobStatus").cloned().unwrap_or(value))
    }

    /// Poll a video processing job.
    ///
    /// # Errors
    ///
    /// [`ApiError::Api`] / transport variants.
    pub async fn video_job_status(&self, job_id: &str) -> Result<Value, ApiError> {
        let token = self
            .service_auth(VIDEO_SERVICE_DID, "app.bsky.video.getJobStatus")
            .await?;
        let url = xrpc_url(&self.video_url, "app.bsky.video.getJobStatus");
        let resp = self
            .http
            .get(&url)
            .query(&[("jobId", job_id)])
            .bearer_auth(&token)
            .send()
            .await?;
        let status = resp.status();
        let body = resp.bytes().await?;
        let value: Value = decode_xrpc(status, &body)?;
        Ok(value.get("jobStatus").cloned().unwrap_or(value))
    }

    async fn access_jwt(&self) -> String {
        self.session.read().await.access_jwt.clone()
    }

    async fn request(
        &self,
        route: &Route,
        nsid: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Value, ApiError> {
        let jwt = self.access_jwt().await;
        let (status, bytes) = self.send_once(route, nsid, query, body, &jwt).await?;
        if is_session_expired(status, &bytes) {
            debug!(nsid, "access token stale; refreshing and retrying");
            self.refresh(&jwt).await?;
            let jwt = self.access_jwt().await;
            let (status, bytes) = self.send_once(route, nsid, query, body, &jwt).await?;
            return decode_xrpc(status, &bytes);
        }
        decode_xrpc(status, &bytes)
    }

    async fn send_once(
        &self,
        route: &Route,
        nsid: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
        jwt: &str,
    ) -> Result<(StatusCode, Vec<u8>), ApiError> {
        let base = match route {
            Route::Chat => self.chat_url.clone(),
            Route::Pds | Route::Proxied(_) => self.session.read().await.pds_url.clone(),
        };
        let url = xrpc_url(&base, nsid);
        let mut req = match body {
            Some(body) => self.http.post(&url).json(body),
            None => self.http.get(&url).query(query),
        };
        req = req.bearer_auth(jwt);
        match route {
            Route::Pds => {}
            Route::Chat => req = req.header("atproto-proxy", CHAT_PROXY),
            Route::Proxied(header) => req = req.header("atproto-proxy", header),
        }
        let resp = req.send().await?;
        let status = resp.status();
        let bytes = resp.bytes().await?;
        Ok((status, bytes.to_vec()))
    }

    /// Double-checked refresh. `stale_access` is the access JWT the
    /// caller just saw rejected; if the in-memory or on-disk session
    /// has already moved past it, adopt that instead of spending a
    /// refresh token (a stampede of refreshes on a rotating token
    /// severs the chain).
    async fn refresh(&self, stale_access: &str) -> Result<(), ApiError> {
        let _serial = self.refresh_serial.lock().await;
        if self.session.read().await.access_jwt != stale_access {
            return Ok(());
        }

        let store = self.store.clone();
        let guard = tokio::task::spawn_blocking(move || store.exclusive())
            .await
            .map_err(|e| ApiError::Unexpected(format!("lock task panicked: {e}")))??;
        let on_disk = guard.read()?;
        let current_refresh = {
            let session = self.session.read().await;
            if on_disk.access_jwt != session.access_jwt
                || on_disk.refresh_jwt != session.refresh_jwt
            {
                debug!("adopting session refreshed by another process");
                drop(session);
                *self.session.write().await = on_disk;
                return Ok(());
            }
            session.refresh_jwt.clone()
        };

        debug!("refreshing session");
        let url = {
            let session = self.session.read().await;
            xrpc_url(&session.pds_url, "com.atproto.server.refreshSession")
        };
        let resp = self
            .http
            .post(url)
            .bearer_auth(&current_refresh)
            .send()
            .await?;
        let status = resp.status();
        let bytes = resp.bytes().await?;
        if refresh_rejected(status, &bytes) {
            warn!("refresh token rejected — chain is dead");
            return Err(ApiError::SessionExpired);
        }
        let wire: WireSession = decode_xrpc(status, &bytes)?;
        let new_session = {
            let session = self.session.read().await;
            SessionFile::new(
                wire.did,
                wire.handle,
                session.pds_url.clone(),
                wire.access_jwt,
                wire.refresh_jwt,
            )
        };
        guard.write(&new_session)?;
        *self.session.write().await = new_session;
        Ok(())
    }
}

fn xrpc_url(base: &str, nsid: &str) -> String {
    let base = base.trim_end_matches('/');
    format!("{base}/xrpc/{nsid}")
}

fn extract_blob(value: &Value) -> Result<Value, ApiError> {
    value
        .get("blob")
        .cloned()
        .ok_or_else(|| ApiError::Unexpected("uploadBlob response missing `blob`".into()))
}

/// True when the response means "access token needs a refresh". Two
/// wire shapes exist: the classic `401`, and the chat service's
/// `400 ExpiredToken` (observed 2026-06-08 — a long-running process
/// got `400 ExpiredToken` on every DM poll because its retry path
/// only triggered on 401; this check covers both).
fn is_session_expired(status: StatusCode, body: &[u8]) -> bool {
    if status == StatusCode::UNAUTHORIZED {
        return true;
    }
    if status == StatusCode::BAD_REQUEST {
        let Ok(v) = serde_json::from_slice::<Value>(body) else {
            return false;
        };
        return v.get("error").and_then(Value::as_str) == Some("ExpiredToken");
    }
    false
}

/// True when `refreshSession` itself rejected the refresh token —
/// the chain is dead. Servers surface this as `401`, or as `400`
/// with `ExpiredToken`/`InvalidToken`.
fn refresh_rejected(status: StatusCode, body: &[u8]) -> bool {
    if status == StatusCode::UNAUTHORIZED {
        return true;
    }
    if status == StatusCode::BAD_REQUEST {
        let Ok(v) = serde_json::from_slice::<Value>(body) else {
            return false;
        };
        let kind = v.get("error").and_then(Value::as_str);
        return kind == Some("ExpiredToken") || kind == Some("InvalidToken");
    }
    false
}

fn decode_bytes(status: StatusCode, bytes: Vec<u8>) -> Result<Vec<u8>, ApiError> {
    if status.is_success() {
        return Ok(bytes);
    }
    let err: Result<Value, ApiError> = decode_xrpc(status, &bytes);
    match err {
        Err(e) => Err(e),
        Ok(_) => Err(ApiError::Unexpected(format!("unexpected status {status}"))),
    }
}

fn decode_xrpc<T: for<'de> Deserialize<'de>>(
    status: StatusCode,
    body: &[u8],
) -> Result<T, ApiError> {
    if !status.is_success() {
        let parsed: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        let kind = parsed
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let message = parsed
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        return Err(ApiError::Api {
            status: status.as_u16(),
            kind,
            message,
        });
    }
    if body.is_empty() {
        return Ok(serde_json::from_slice::<T>(b"null")?);
    }
    Ok(serde_json::from_slice::<T>(body)?)
}

#[cfg(test)]
mod tests;
