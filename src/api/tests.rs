//! Client behavior tests over wiremock.
//!
//! The refresh-and-retry, ExpiredToken-vs-real-400 discrimination,
//! and session-rotation-persistence tests are ports of the reference
//! suite that caught these bugs in production. The
//! adopt-other-process-refresh test is new: it proves the
//! double-checked locking design (see `crate::session` module docs).

use std::time::Duration;

use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{ApiError, Client, ClientOptions, Route};
use crate::identifiers::{Did, Handle};
use crate::session::{SessionFile, SessionStore};

const DID: &str = "did:plc:testtesttesttest";
const HANDLE: &str = "test.example.com";

fn seeded_store(dir: &tempfile::TempDir, pds_url: &str) -> SessionStore {
    let store = SessionStore::at(dir.path().join("session.json"));
    store
        .save(&SessionFile::new(
            Did::from_trusted(DID),
            Handle::from_trusted(HANDLE),
            pds_url.to_string(),
            "access-1".to_string(),
            "refresh-1".to_string(),
        ))
        .expect("seed session");
    store
}

fn options(chat_url: &str) -> ClientOptions {
    ClientOptions {
        chat_url: chat_url.to_string(),
        plc_url: "http://plc.invalid".to_string(),
        http_timeout: Duration::from_secs(2),
    }
}

fn wire_session(access: &str, refresh: &str) -> serde_json::Value {
    json!({
        "did": DID,
        "handle": HANDLE,
        "accessJwt": access,
        "refreshJwt": refresh,
    })
}

async fn mount_refresh(server: &MockServer, old_refresh: &str, access: &str, refresh: &str) {
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.server.refreshSession"))
        .and(header("authorization", format!("Bearer {old_refresh}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(wire_session(access, refresh)))
        .expect(1)
        .mount(server)
        .await;
}

/// 401 on the first attempt → refresh → retry succeeds, and the
/// rotated pair is persisted to the session file (short-lived
/// processes must leave the fresh chain behind for the next one).
#[tokio::test]
async fn refresh_and_retry_persists_rotated_tokens() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let store = seeded_store(&dir, &server.uri());

    Mock::given(method("GET"))
        .and(path("/xrpc/app.bsky.feed.getTimeline"))
        .and(header("authorization", "Bearer access-1"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "error": "InvalidToken" })))
        .expect(1)
        .mount(&server)
        .await;
    mount_refresh(&server, "refresh-1", "access-2", "refresh-2").await;
    Mock::given(method("GET"))
        .and(path("/xrpc/app.bsky.feed.getTimeline"))
        .and(header("authorization", "Bearer access-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "feed": [] })))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::from_store(store.clone(), &options(&server.uri())).expect("client");
    let value = client
        .get(&Route::Pds, "app.bsky.feed.getTimeline", &[])
        .await
        .expect("call succeeds after refresh");
    assert!(value.get("feed").is_some());

    let on_disk = store.load().expect("reload");
    assert_eq!(on_disk.access_jwt, "access-2");
    assert_eq!(on_disk.refresh_jwt, "refresh-2");
}

/// The chat service surfaces an aged-out access token as
/// `400 ExpiredToken`, not 401. The same retry loop must cover it —
/// a long-running caller once went silent for hours because retry
/// only triggered on 401.
#[tokio::test]
async fn expired_token_400_triggers_refresh_and_retry() {
    let pds = MockServer::start().await;
    let chat = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let store = seeded_store(&dir, &pds.uri());

    Mock::given(method("GET"))
        .and(path("/xrpc/chat.bsky.convo.listConvos"))
        .and(header("authorization", "Bearer access-1"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({ "error": "ExpiredToken", "message": "Token has expired" })),
        )
        .expect(1)
        .mount(&chat)
        .await;
    mount_refresh(&pds, "refresh-1", "access-2", "refresh-2").await;
    Mock::given(method("GET"))
        .and(path("/xrpc/chat.bsky.convo.listConvos"))
        .and(header("authorization", "Bearer access-2"))
        .and(header("atproto-proxy", super::CHAT_PROXY))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "convos": [] })))
        .expect(1)
        .mount(&chat)
        .await;

    let client = Client::from_store(store, &options(&chat.uri())).expect("client");
    let value = client
        .get(&Route::Chat, "chat.bsky.convo.listConvos", &[])
        .await
        .expect("chat call succeeds after refresh");
    assert!(value.get("convos").is_some());
}

/// A real 400 (not `ExpiredToken`) must NOT trigger a refresh — it
/// surfaces as a typed API error and the refresh token stays
/// unspent.
#[tokio::test]
async fn real_400_does_not_refresh() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let store = seeded_store(&dir, &server.uri());

    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.repo.createRecord"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({ "error": "InvalidRequest", "message": "bad record" })),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.server.refreshSession"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let client = Client::from_store(store.clone(), &options(&server.uri())).expect("client");
    let err = client
        .post(&Route::Pds, "com.atproto.repo.createRecord", &json!({}))
        .await
        .expect_err("must fail");
    let ApiError::Api { status, kind, .. } = err else {
        panic!("expected Api error, got {err:?}");
    };
    assert_eq!(status, 400);
    assert_eq!(kind, "InvalidRequest");
    let on_disk = store.load().expect("reload");
    assert_eq!(
        on_disk.refresh_jwt, "refresh-1",
        "refresh token must be unspent"
    );
}

/// When the refresh token itself is rejected, the chain is dead:
/// surface `SessionExpired` (exit code 3 at the CLI), never retry
/// further, never prompt.
#[tokio::test]
async fn dead_refresh_chain_is_session_expired() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let store = seeded_store(&dir, &server.uri());

    Mock::given(method("GET"))
        .and(path("/xrpc/app.bsky.feed.getTimeline"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "error": "InvalidToken" })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.server.refreshSession"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({ "error": "ExpiredToken", "message": "Token has expired" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::from_store(store, &options(&server.uri())).expect("client");
    let err = client
        .get(&Route::Pds, "app.bsky.feed.getTimeline", &[])
        .await
        .expect_err("must fail");
    let ApiError::SessionExpired = err else {
        panic!("expected SessionExpired, got {err:?}");
    };
}

/// The double-checked lock: if another process already refreshed (the
/// on-disk tokens differ from the pair we hold), adopt the disk
/// tokens and retry — `refreshSession` must NOT be called, because
/// spending the stale refresh token would sever the chain.
#[tokio::test]
async fn adopts_other_process_refresh_without_spending_token() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let store = seeded_store(&dir, &server.uri());

    let client = Client::from_store(store.clone(), &options(&server.uri())).expect("client");

    store
        .save(&SessionFile::new(
            Did::from_trusted(DID),
            Handle::from_trusted(HANDLE),
            server.uri(),
            "access-9".to_string(),
            "refresh-9".to_string(),
        ))
        .expect("simulate another process's refresh");

    Mock::given(method("GET"))
        .and(path("/xrpc/app.bsky.feed.getTimeline"))
        .and(header("authorization", "Bearer access-1"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "error": "InvalidToken" })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/xrpc/app.bsky.feed.getTimeline"))
        .and(header("authorization", "Bearer access-9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "feed": [] })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.server.refreshSession"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    client
        .get(&Route::Pds, "app.bsky.feed.getTimeline", &[])
        .await
        .expect("succeeds with adopted tokens");
}

/// 2xx with a body that isn't JSON must surface the typed decode
/// error, not a panic or a silent success.
#[tokio::test]
async fn malformed_2xx_body_is_decode_error() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let store = seeded_store(&dir, &server.uri());

    Mock::given(method("GET"))
        .and(path("/xrpc/app.bsky.feed.getTimeline"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>not json</html>"))
        .mount(&server)
        .await;

    let client = Client::from_store(store, &options(&server.uri())).expect("client");
    let err = client
        .get(&Route::Pds, "app.bsky.feed.getTimeline", &[])
        .await
        .expect_err("must fail");
    let ApiError::Decode(_) = err else {
        panic!("expected Decode, got {err:?}");
    };
}

/// A hung server must produce a timeout error within the configured
/// client timeout, not hang the CLI.
#[tokio::test]
async fn hung_server_times_out() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let store = seeded_store(&dir, &server.uri());

    Mock::given(method("GET"))
        .and(path("/xrpc/app.bsky.feed.getTimeline"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "feed": [] }))
                .set_delay(Duration::from_secs(30)),
        )
        .mount(&server)
        .await;

    let opts = ClientOptions {
        http_timeout: Duration::from_millis(200),
        ..options(&server.uri())
    };
    let client = Client::from_store(store, &opts).expect("client");
    let err = client
        .get(&Route::Pds, "app.bsky.feed.getTimeline", &[])
        .await
        .expect_err("must time out");
    let ApiError::Http(inner) = err else {
        panic!("expected Http, got {err:?}");
    };
    assert!(inner.is_timeout(), "expected timeout, got {inner:?}");
}

/// Query parameters (including repeated keys) must reach the wire.
#[tokio::test]
async fn query_params_are_sent() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let store = seeded_store(&dir, &server.uri());

    Mock::given(method("GET"))
        .and(path("/xrpc/app.bsky.feed.getAuthorFeed"))
        .and(query_param("actor", DID))
        .and(query_param("limit", "30"))
        .and(query_param("cursor", "abc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "feed": [] })))
        .expect(1)
        .mount(&server)
        .await;

    let client = Client::from_store(store, &options(&server.uri())).expect("client");
    client
        .get(
            &Route::Pds,
            "app.bsky.feed.getAuthorFeed",
            &[
                ("actor", DID.to_string()),
                ("limit", "30".to_string()),
                ("cursor", "abc".to_string()),
            ],
        )
        .await
        .expect("call succeeds");
}

/// `login` persists a session whose PDS is resolved from the DID doc
/// in the createSession response — the custom-PDS path.
#[tokio::test]
async fn login_resolves_pds_from_did_doc() {
    let entryway = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let store = SessionStore::at(dir.path().join("session.json"));

    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.server.createSession"))
        .and(body_partial_json(json!({ "identifier": HANDLE })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "did": DID,
            "handle": HANDLE,
            "accessJwt": "access-1",
            "refreshJwt": "refresh-1",
            "didDoc": {
                "service": [{
                    "id": "#atproto_pds",
                    "type": "AtprotoPersonalDataServer",
                    "serviceEndpoint": "https://real-pds.example.com"
                }]
            }
        })))
        .expect(1)
        .mount(&entryway)
        .await;

    Client::login(
        store.clone(),
        &options("http://chat.invalid"),
        &entryway.uri(),
        HANDLE,
        "app-password",
    )
    .await
    .expect("login");

    let session = store.load().expect("persisted");
    assert_eq!(session.pds_url, "https://real-pds.example.com");
    assert_eq!(session.did.as_str(), DID);
}

/// Bad credentials surface the server's structured error.
#[tokio::test]
async fn login_bad_credentials_is_api_error() {
    let entryway = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let store = SessionStore::at(dir.path().join("session.json"));

    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.server.createSession"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": "AuthenticationRequired",
            "message": "Invalid identifier or password"
        })))
        .mount(&entryway)
        .await;

    let err = Client::login(
        store,
        &options("http://chat.invalid"),
        &entryway.uri(),
        HANDLE,
        "wrong",
    )
    .await
    .expect_err("must fail");
    let ApiError::Api {
        status: 401, kind, ..
    } = err
    else {
        panic!("expected Api 401, got {err:?}");
    };
    assert_eq!(kind, "AuthenticationRequired");
}
