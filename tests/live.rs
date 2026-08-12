//! Live end-to-end tests against the real Bluesky network. Opt-in:
//!
//! ```sh
//! fulmar login you.bsky.social            # once, anywhere
//! FULMAR_LIVE_SESSION=~/.local/state/fulmar/session.json cargo test --test live
//! ```
//!
//! Every test is silently skipped unless `FULMAR_LIVE_SESSION` points
//! at a seeded session file. All tests in the default tier are
//! READ-ONLY (they may refresh the session's tokens — that's the
//! session file working as designed).
//!
//! The write tier additionally requires `FULMAR_LIVE_WRITE=1` and
//! MUST only be pointed at a dedicated test account: it creates a
//! real post, likes/unlikes it, and deletes it.
//!
//! Call volume is kept tiny (a handful of requests per run) — these
//! are wiring checks against real servers, not load tests. The mocked
//! integration suite in `tests/cli.rs` owns behavioral coverage.

use assert_cmd::cargo::cargo_bin;

fn live_session() -> Option<String> {
    std::env::var("FULMAR_LIVE_SESSION")
        .ok()
        .filter(|s| !s.is_empty())
}

fn write_tier_enabled() -> bool {
    std::env::var("FULMAR_LIVE_WRITE").is_ok_and(|v| v == "1")
}

fn fulmar(session: &str, args: &[&str]) -> (i32, String, String) {
    let output = std::process::Command::new(cargo_bin("fulmar"))
        .env("FULMAR_SESSION", session)
        .args(args)
        .output()
        .expect("run fulmar");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn require_ok(session: &str, args: &[&str]) -> String {
    let (code, stdout, stderr) = fulmar(session, args);
    assert_eq!(code, 0, "fulmar {args:?} failed:\n{stderr}");
    stdout
}

macro_rules! skip_without_session {
    () => {
        match live_session() {
            Some(session) => session,
            None => {
                eprintln!("skipped: set FULMAR_LIVE_SESSION to run live tests");
                return;
            }
        }
    };
}

#[test]
fn live_whoami_verifies_against_pds() {
    let session = skip_without_session!();
    let stdout = require_ok(&session, &["whoami", "--verify", "--json"]);
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    let did = value["did"].as_str().expect("did");
    assert!(did.starts_with("did:"), "got {did}");
}

#[test]
fn live_timeline_pages_with_real_cursor() {
    let session = skip_without_session!();
    let stdout = require_ok(&session, &["timeline", "--limit", "3", "--json"]);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(!lines.is_empty(), "timeline returned nothing");
    for line in &lines {
        let _valid: serde_json::Value = serde_json::from_str(line).expect("NDJSON line");
    }
    let last: serde_json::Value = serde_json::from_str(lines[lines.len() - 1]).expect("json");
    let Some(cursor) = last["cursor"].as_str() else {
        return;
    };
    require_ok(
        &session,
        &["timeline", "--limit", "3", "--cursor", cursor, "--json"],
    );
}

#[test]
fn live_resolve_own_identity_round_trips() {
    let session = skip_without_session!();
    let whoami = require_ok(&session, &["whoami", "--json"]);
    let whoami: serde_json::Value = serde_json::from_str(&whoami).expect("json");
    let handle = whoami["handle"].as_str().expect("handle");
    let did = whoami["did"].as_str().expect("did");

    let resolved = require_ok(&session, &["resolve", handle, "--json"]);
    let resolved: serde_json::Value = serde_json::from_str(&resolved).expect("json");
    assert_eq!(resolved["did"].as_str(), Some(did));
    assert!(
        resolved["pdsUrl"]
            .as_str()
            .is_some_and(|u| u.starts_with("https://")),
        "PDS should resolve from the DID document"
    );
}

#[test]
fn live_notifs_count() {
    let session = skip_without_session!();
    let stdout = require_ok(&session, &["notifs", "count", "--json"]);
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert!(value["count"].is_u64(), "got {value}");
}

/// The one that catches routing regressions no mock can: real
/// `api.bsky.chat` with the proxy header, where `bsky.social` would
/// 501.
#[test]
fn live_dm_unread_reaches_chat_service() {
    let session = skip_without_session!();
    let stdout = require_ok(&session, &["dm", "unread", "--json"]);
    let _valid: serde_json::Value = serde_json::from_str(&stdout).expect("json");
}

/// Authed search — anonymous search is edge-blocked from datacenter
/// IPs, so this proves our route goes through the PDS.
#[test]
fn live_search_is_authed() {
    let session = skip_without_session!();
    let stdout = require_ok(&session, &["search", "bluesky", "--limit", "2", "--json"]);
    assert!(!stdout.trim().is_empty(), "search returned nothing at all");
}

#[test]
fn live_prefs_read() {
    let session = skip_without_session!();
    let stdout = require_ok(&session, &["prefs", "get"]);
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert!(value["preferences"].is_array(), "got {value}");
}

/// WRITE TIER — `FULMAR_LIVE_WRITE=1`, dedicated test account only.
/// Full self-cleaning cycle: post (with a facet), view, like, unlike,
/// delete. Leaves the account exactly as it found it.
#[test]
fn live_write_post_lifecycle() {
    let session = skip_without_session!();
    if !write_tier_enabled() {
        eprintln!("skipped: set FULMAR_LIVE_WRITE=1 (dedicated test account only!)");
        return;
    }

    let marker = format!(
        "fulmar live test {} #fulmarSelfTest",
        chrono_free_timestamp()
    );
    let created = require_ok(&session, &["post", &marker, "--json"]);
    let created: serde_json::Value = serde_json::from_str(&created).expect("json");
    let uri = created["uri"].as_str().expect("post uri").to_string();

    let view = require_ok(&session, &["view", &uri, "--json"]);
    assert!(
        view.contains("fulmarSelfTest"),
        "posted text should be visible"
    );

    require_ok(&session, &["like", &uri]);
    require_ok(&session, &["unlike", &uri]);
    require_ok(&session, &["delete", &uri]);
}

/// Seconds since epoch without pulling chrono into the test — just
/// enough uniqueness for a test post marker.
fn chrono_free_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
