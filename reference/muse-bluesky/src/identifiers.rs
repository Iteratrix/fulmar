//! Typed AT Protocol identifiers.
//!
//! See `docs/os/rust-rewrite/types.md` and the audit report at
//! `crates/muse-eval/reports/v4/bluesky-identifier-types-audit.md`
//! for the design rationale. Short version: the AT Protocol surface
//! has five distinct identifier kinds — DID, handle, AT URI, CID,
//! rkey — that collapse to opaque-looking strings on the wire. Wake
//! #3 on 2026-06-08 burned 33k tokens because the model substituted
//! an rkey (`3mnsgtqsess26`) for a CID (`bafyrei...`); a `String`
//! field couldn't tell them apart.
//!
//! These newtypes catch wrong-kind-of-string at serde deserialization.
//! Production callers that take a `Cid` cannot accidentally receive a
//! rkey — the rkey's missing `bafy` prefix fails validation at the
//! tool-dispatch boundary, before any XRPC call leaves the daemon.
//!
//! Wire format is unchanged: each type round-trips through serde as
//! a single string. The newtype is the Rust-side discipline.
//!
//! Validation is intentionally lightweight — prefix checks, not full
//! grammar. The AT Protocol's rules for handles in particular are
//! intricate (RFC1035 labels, length limits, IDN); we don't want
//! this layer to be the authority. The wire is the authority; these
//! types catch the obvious wrong-kind errors that make agentic
//! confusion easy.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// One typed identifier. The `validate` function gates construction
/// from a `String`; serde defers to it on deserialization.
macro_rules! string_newtype {
    (
        $(#[$attr:meta])*
        $name:ident { $validate:expr, $expected:expr $(,)? }
    ) => {
        $(#[$attr])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(String);

        impl $name {
            /// Construct without validation. Use only for tests or for
            /// values produced by code that has already validated
            /// (e.g. extracting from a parsed AT URI).
            #[must_use]
            pub fn from_trusted(s: impl Into<String>) -> Self {
                Self(s.into())
            }

            /// Construct from a string, validating the prefix shape.
            /// Returns `Err(IdentifierError)` for the wrong kind.
            ///
            /// # Errors
            ///
            /// Returns `IdentifierError::WrongKind` when the input
            /// doesn't match this identifier's expected shape.
            pub fn parse(s: impl Into<String>) -> Result<Self, IdentifierError> {
                let s = s.into();
                let validator: fn(&str) -> bool = $validate;
                if validator(&s) {
                    Ok(Self(s))
                } else {
                    Err(IdentifierError::WrongKind {
                        expected: $expected,
                        got: truncated(&s),
                    })
                }
            }

            /// Borrow as `&str`.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl From<$name> for String {
            fn from(v: $name) -> String {
                v.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
                self.0.serialize(ser)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
                let s = String::deserialize(de)?;
                Self::parse(s).map_err(serde::de::Error::custom)
            }
        }
    };
}

/// Errors from typed-identifier construction.
#[derive(Debug, Clone, thiserror::Error)]
pub enum IdentifierError {
    /// Input didn't match the expected identifier shape. `got` is
    /// truncated for log hygiene (full string may include user
    /// content via tool calls).
    #[error("identifier wrong kind: expected {expected}, got {got:?}")]
    WrongKind { expected: &'static str, got: String },
}

/// Truncate a string to ~60 chars for error display. Keeps logs
/// readable when the offending input is unexpectedly long (e.g. a
/// model emits a full prose paragraph where an identifier was
/// expected).
fn truncated(s: &str) -> String {
    let cap = 60;
    if s.chars().count() <= cap {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(cap).collect();
        out.push('…');
        out
    }
}

string_newtype! {
    /// Stable per-actor identity. Either `did:plc:<base32>` (the
    /// common case, produced by the PLC directory) or `did:web:<host>`.
    /// Canonical for storage / lookup; handles change, DIDs don't.
    Did {
        |s: &str| s.starts_with("did:plc:") || s.starts_with("did:web:"),
        "did:plc:* or did:web:*",
    }
}

string_newtype! {
    /// Human-readable handle like `alice.bsky.social`. NOT canonical
    /// — Bluesky permits handle portability, so a stored handle can
    /// later resolve to a different account. Anywhere we need
    /// stability, resolve to `Did` first.
    Handle {
        |s: &str| !s.is_empty() && s.contains('.') && !s.starts_with("did:") && !s.starts_with("at://"),
        "handle (e.g. alice.bsky.social)",
    }
}

string_newtype! {
    /// AT Protocol resource URI: `at://<did>/<collection>/<rkey>`.
    /// The full record reference; contains DID + collection + rkey.
    AtUri {
        |s: &str| s.starts_with("at://"),
        "at://...",
    }
}

string_newtype! {
    /// Content-addressed identifier — hash of a specific record
    /// version. Anti-poisoning: write tools (reply/like/repost)
    /// require a CID so they target the record version the caller
    /// observed, not a later edit. Most commonly base32-multibase
    /// `CIDv1` (`bafyrei...`); the older base58 form (`Qm...`) is
    /// theoretically possible but Bluesky doesn't produce it.
    Cid {
        |s: &str| s.starts_with("bafy"),
        "CID (e.g. bafyrei... base32-multibase CIDv1)",
    }
}

string_newtype! {
    /// Per-collection record key. Appears as the tail segment of an
    /// AT URI. Opaque-looking (e.g. `3kxxxxx`); easy to confuse with
    /// CID at a glance. Never canonical alone — always paired with
    /// a DID + collection.
    RKey {
        |s: &str| !s.is_empty() && !s.contains('/') && !s.contains(':'),
        "rkey (per-collection record key, no slashes or colons)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn did_accepts_plc_and_web() {
        assert!(Did::parse("did:plc:a3nr3jzwxvmwgmbx7rhptcms").is_ok());
        assert!(Did::parse("did:web:lumen.example.com").is_ok());
        assert!(Did::parse("alice.bsky.social").is_err());
        assert!(Did::parse("bafyrei...").is_err());
    }

    #[test]
    fn handle_accepts_dotted_no_did_prefix() {
        assert!(Handle::parse("alice.bsky.social").is_ok());
        assert!(Handle::parse("lumen.example.com").is_ok());
        assert!(Handle::parse("did:plc:abc").is_err());
        assert!(Handle::parse("at://did:plc:abc").is_err());
        assert!(Handle::parse("").is_err());
        assert!(Handle::parse("noseparator").is_err());
    }

    #[test]
    fn at_uri_requires_at_scheme() {
        assert!(AtUri::parse("at://did:plc:abc/app.bsky.feed.post/3kxxxxx").is_ok());
        assert!(AtUri::parse("https://bsky.app/profile/alice/post/3kxxxxx").is_err());
        assert!(AtUri::parse("3kxxxxx").is_err());
    }

    #[test]
    fn cid_requires_bafy_prefix() {
        assert!(Cid::parse("bafyreidkxk2yqg7tw3eqqfd54x6sxbiz5jb6ohh5d6h7s3pcvqfvw3umm4").is_ok());
        // The wake #3 substitution: an rkey is NOT a CID.
        assert!(Cid::parse("3mnsgtqsess26").is_err());
    }

    #[test]
    fn rkey_excludes_uris_and_dids() {
        assert!(RKey::parse("3mnsgtqsess26").is_ok());
        assert!(RKey::parse("at://did:plc:abc/...").is_err());
        assert!(RKey::parse("did:plc:abc").is_err());
        assert!(RKey::parse("").is_err());
    }

    /// **Regression for wake #3**: deserializing a `Cid` from an
    /// rkey string is the exact bug class that took down Lumen.
    /// The typed boundary must refuse it.
    #[derive(Debug, serde::Deserialize)]
    struct CidWrapper {
        #[allow(dead_code)]
        cid: Cid,
    }

    #[test]
    fn cid_deserialize_rejects_rkey_substitution() {
        let payload = r#"{"cid":"3mnsgtqsess26"}"#;
        let err =
            serde_json::from_str::<CidWrapper>(payload).expect_err("serde must reject rkey as Cid");
        let msg = err.to_string();
        assert!(
            msg.contains("CID") || msg.contains("bafy"),
            "error should name the expected shape; got {msg:?}"
        );
    }

    #[test]
    fn round_trips_through_serde() {
        let cid = Cid::parse("bafyreidkxk2yqg7tw3eqqfd54x6sxbiz5jb6ohh5d6h7s3pcvqfvw3umm4")
            .expect("parse");
        let json = serde_json::to_string(&cid).expect("serialize");
        let back: Cid = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(cid, back);
    }

    #[test]
    fn from_trusted_skips_validation() {
        // Useful when extracting a known-good value from an already
        // parsed structure (e.g. the rkey segment of an AT URI).
        let _wrong = Cid::from_trusted("3mnsgtqsess26");
        // No panic; trusted constructors bypass validation.
    }

    #[test]
    fn truncated_keeps_short_strings_whole() {
        assert_eq!(truncated("hello"), "hello");
        let long: String = "x".repeat(80);
        let t = truncated(&long);
        assert!(t.len() <= 80, "truncation should cap length");
        assert!(t.ends_with('…'), "should mark truncation");
    }

    #[test]
    fn display_returns_raw_string() {
        let did = Did::parse("did:plc:abc").expect("parse");
        assert_eq!(format!("{did}"), "did:plc:abc");
    }
}
