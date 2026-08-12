//! End-to-end tests: the real `fulmar` binary against wiremock
//! servers, including the two-OS-process refresh race that is this
//! tool's reason to exist.

use assert_cmd::cargo::cargo_bin;
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DID: &str = "did:plc:integrationtest";
const HANDLE: &str = "itest.example.com";

fn seed_session(dir: &tempfile::TempDir, pds_url: &str, access: &str, refresh: &str) -> String {
    let path = dir.path().join("session.json");
    let session = json!({
        "version": 1,
        "did": DID,
        "handle": HANDLE,
        "pds_url": pds_url,
        "access_jwt": access,
        "refresh_jwt": refresh,
        "updated_at": "2026-08-11T00:00:00Z",
    });
    std::fs::write(&path, session.to_string()).expect("seed session");
    path.to_string_lossy().to_string()
}

fn fulmar(session_path: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new(cargo_bin("fulmar"));
    cmd.env("FULMAR_SESSION", session_path);
    cmd.env_remove("FULMAR_PASSWORD");
    cmd
}

fn run(cmd: &mut std::process::Command) -> (i32, String, String) {
    let output = cmd.output().expect("run fulmar");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

#[test]
fn missing_session_exits_3_and_names_login() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("absent.json");
    let (code, _, stderr) = run(fulmar(&path.to_string_lossy()).arg("whoami"));
    assert_eq!(code, 3, "stderr: {stderr}");
    assert!(stderr.contains("fulmar login"), "stderr: {stderr}");
}

#[test]
fn usage_error_exits_2() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("absent.json");
    let (code, _, _) = run(fulmar(&path.to_string_lossy()).arg("no-such-command"));
    assert_eq!(code, 2);
}

#[test]
fn help_teaches() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("absent.json");
    for (args, expected) in [
        (vec!["--help"], "Exit codes"),
        (vec!["post", "--help"], "fulmar post \"hello from fulmar\""),
        (vec!["dm", "read", "--help"], "updateRead"),
        (vec!["dm", "log", "--help"], "Poll pattern"),
        (vec!["login", "--help"], "FULMAR_PASSWORD"),
        (vec!["prefs", "--help"], "get"),
        (vec!["api", "--help"], "chat.bsky.convo.getLog"),
    ] {
        let (code, stdout, _) = run(fulmar(&path.to_string_lossy()).args(&args));
        assert_eq!(code, 0, "args: {args:?}");
        assert!(
            stdout.contains(expected),
            "args: {args:?}\nstdout: {stdout}"
        );
    }
}

#[test]
fn completions_generate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("absent.json");
    let (code, stdout, _) = run(fulmar(&path.to_string_lossy()).args(["completions", "bash"]));
    assert_eq!(code, 0);
    assert!(stdout.contains("_fulmar"), "should emit bash completion");
}

#[tokio::test(flavor = "multi_thread")]
async fn timeline_json_emits_ndjson_and_trailing_cursor() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/xrpc/app.bsky.feed.getTimeline"))
        .and(header("authorization", "Bearer access-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "feed": [
                { "post": { "uri": "at://did:plc:a/app.bsky.feed.post/1", "record": { "text": "one" } } },
                { "post": { "uri": "at://did:plc:a/app.bsky.feed.post/2", "record": { "text": "two" } } },
            ],
            "cursor": "next-page",
        })))
        .expect(1)
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let session = seed_session(&dir, &server.uri(), "access-1", "refresh-1");
    let (code, stdout, stderr) =
        tokio::task::spawn_blocking(move || run(fulmar(&session).args(["timeline", "--json"])))
            .await
            .expect("join");

    assert_eq!(code, 0, "stderr: {stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 3, "stdout: {stdout}");
    let first: serde_json::Value = serde_json::from_str(lines[0]).expect("ndjson");
    assert_eq!(first["post"]["record"]["text"], "one");
    let last: serde_json::Value = serde_json::from_str(lines[2]).expect("cursor line");
    assert_eq!(last, json!({ "cursor": "next-page" }));
}

#[tokio::test(flavor = "multi_thread")]
async fn post_sends_correct_facet_byte_offsets() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/xrpc/com.atproto.identity.resolveHandle"))
        .and(query_param("handle", "alice.test"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "did": "did:plc:alicealice" })),
        )
        .mount(&server)
        .await;
    // The body matcher IS the assertion: multibyte text before the
    // mention, byte (not char) offsets expected. "✨ " is 4 bytes.
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.repo.createRecord"))
        .and(body_partial_json(json!({
            "collection": "app.bsky.feed.post",
            "record": {
                "text": "✨ @alice.test hello",
                "facets": [{
                    "index": { "byteStart": 4, "byteEnd": 15 },
                    "features": [{
                        "$type": "app.bsky.richtext.facet#mention",
                        "did": "did:plc:alicealice",
                    }],
                }],
            },
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "uri": format!("at://{DID}/app.bsky.feed.post/3abc"),
            "cid": "bafyreianewpost",
        })))
        .expect(1)
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let session = seed_session(&dir, &server.uri(), "access-1", "refresh-1");
    let (code, stdout, stderr) = tokio::task::spawn_blocking(move || {
        run(fulmar(&session).args(["post", "✨ @alice.test hello"]))
    })
    .await
    .expect("join");

    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("at://"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn dm_send_routes_to_chat_service_with_proxy_header() {
    let pds = MockServer::start().await;
    let chat = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/xrpc/com.atproto.identity.resolveHandle"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "did": "did:plc:bobbobbob" })),
        )
        .mount(&pds)
        .await;
    Mock::given(method("GET"))
        .and(path("/xrpc/chat.bsky.convo.getConvoForMembers"))
        .and(header("atproto-proxy", "did:web:api.bsky.chat#bsky_chat"))
        .and(query_param("members", "did:plc:bobbobbob"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "convo": { "id": "convo-77" } })),
        )
        .expect(1)
        .mount(&chat)
        .await;
    Mock::given(method("POST"))
        .and(path("/xrpc/chat.bsky.convo.sendMessage"))
        .and(header("atproto-proxy", "did:web:api.bsky.chat#bsky_chat"))
        .and(body_partial_json(json!({
            "convoId": "convo-77",
            "message": { "text": "hello bob" },
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "id": "msg-1", "text": "hello bob" })),
        )
        .expect(1)
        .mount(&chat)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let session = seed_session(&dir, &pds.uri(), "access-1", "refresh-1");
    let chat_url = chat.uri();
    let (code, _, stderr) = tokio::task::spawn_blocking(move || {
        run(fulmar(&session).env("FULMAR_CHAT_URL", chat_url).args([
            "dm",
            "send",
            "bob.test",
            "hello bob",
        ]))
    })
    .await
    .expect("join");

    assert_eq!(code, 0, "stderr: {stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn not_found_exits_4() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/xrpc/app.bsky.feed.getPostThread"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "NotFound",
            "message": "Post not found",
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let session = seed_session(&dir, &server.uri(), "access-1", "refresh-1");
    let (code, _, stderr) = tokio::task::spawn_blocking(move || {
        run(fulmar(&session).args(["view", "at://did:plc:x/app.bsky.feed.post/gone"]))
    })
    .await
    .expect("join");

    assert_eq!(code, 4, "stderr: {stderr}");
}

/// THE test: two real OS processes, one session file, an expired
/// access token, and a server that permanently kills the chain if
/// the same refresh token is ever spent twice. Without the flock +
/// double-checked re-read this fails; with it, both processes exit 0
/// no matter who wins the race.
#[tokio::test(flavor = "multi_thread")]
async fn two_processes_racing_one_session_never_double_spend_a_refresh_token() {
    let server = MockServer::start().await;

    let wire = |access: &str, refresh: &str| json!({ "did": DID, "handle": HANDLE, "accessJwt": access, "refreshJwt": refresh });
    // Each refresh token works exactly once; a reuse gets the real
    // server's ExpiredToken, which fulmar treats as a dead chain
    // (exit 3) — so a double-spend fails the assertions below.
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.server.refreshSession"))
        .and(header("authorization", "Bearer refresh-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(wire("access-2", "refresh-2")))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.server.refreshSession"))
        .and(header("authorization", "Bearer refresh-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(wire("access-3", "refresh-3")))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/xrpc/com.atproto.server.refreshSession"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({ "error": "ExpiredToken", "message": "reused token" })),
        )
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let session = seed_session(&dir, &server.uri(), "access-1", "refresh-1");

    let (children, results) = tokio::task::spawn_blocking(move || {
        let children: Vec<_> = (0..2)
            .map(|_| {
                fulmar(&session)
                    .args(["session", "refresh"])
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .expect("spawn fulmar")
            })
            .collect();
        let results: Vec<_> = children
            .into_iter()
            .map(|child| child.wait_with_output().expect("wait"))
            .collect();
        ((), results)
    })
    .await
    .expect("join");
    let () = children;

    for (i, output) in results.iter().enumerate() {
        assert!(
            output.status.success(),
            "process {i} failed (code {:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let final_session: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("session.json")).expect("read final"),
    )
    .expect("parse final");
    let final_refresh = final_session["refresh_jwt"].as_str().unwrap_or_default();
    assert!(
        final_refresh == "refresh-2" || final_refresh == "refresh-3",
        "chain must have advanced cleanly, got {final_refresh:?}"
    );
}
