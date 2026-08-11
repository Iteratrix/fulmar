//! Public types returned by `BlueskyClient` methods.
//!
//! Kept narrow on purpose: AT Protocol responses have *many* fields
//! we don't care about today, and leaking the full shape would couple
//! callers to the wire format. Each struct here exposes just the
//! fields the agent loop or tools actually consume.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::identifiers::{AtUri, Cid, Did, Handle};

/// Active session returned by `createSession` / `refreshSession`.
/// Held inside the client; not exposed to tools.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Session {
    pub did: Did,
    pub handle: Handle,
    #[serde(rename = "accessJwt")]
    pub access_jwt: String,
    #[serde(rename = "refreshJwt")]
    pub refresh_jwt: String,
}

/// Result of publishing a `WhiteWind` blog entry.
///
/// `url` is the human-readable page (`https://whtwnd.com/<handle>/<rkey>`)
/// — `WhiteWind` routes by record key, never by title slug, so this
/// is the only shareable link. `uri` is the underlying AT record.
#[derive(Debug, Clone)]
pub struct WhitewindEntry {
    pub uri: String,
    pub url: String,
}

/// Trimmed view of a feed post returned by `getTimeline` /
/// `getPostThread` / `getAuthorFeed` / `searchPosts`. AT Protocol
/// includes embeds, labels, viewer state, etc.; we surface only what
/// the agent reads.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PostView {
    /// AT URI of the post (`at://did:plc:xxx/app.bsky.feed.post/yyy`).
    pub uri: AtUri,
    /// Content-addressed CID. Internal use only — not surfaced to the
    /// model in read-tool output. The daemon resolves CIDs internally
    /// for write operations (reply/like/repost).
    pub cid: Cid,
    /// Author handle (`@example.bsky.social`-shaped, minus the @).
    pub author_handle: Handle,
    /// Author DID (`did:plc:...`).
    pub author_did: Did,
    /// Plain text content. Facets/links/mentions are not surfaced
    /// here; the agent reads the rendered text.
    pub text: String,
    /// Reply parent URI, if this is a reply.
    pub reply_parent: Option<AtUri>,
    /// Timestamp from the post record. Wire format is RFC3339;
    /// chrono's default serde for `DateTime<Utc>` round-trips that
    /// shape unchanged.
    pub indexed_at: DateTime<Utc>,
    /// Engagement counts. Present when the upstream call returns
    /// them (`getAuthorFeed`, `searchPosts`, `getTimeline`); absent
    /// from sources that don't include them (e.g. some `getPostThread`
    /// shapes). Rendered as `♡↻💬` when nonzero.
    #[serde(default)]
    pub like_count: u64,
    #[serde(default)]
    pub repost_count: u64,
    #[serde(default)]
    pub reply_count: u64,
}

/// Rich profile shape returned by `getProfile`. Surfaced to the agent
/// via `bluesky_read_profile`; smaller `ProfileRelationship` lives
/// alongside for the reply-gate path that only needs the viewer
/// booleans.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProfileView {
    pub did: Did,
    pub handle: Handle,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub followers_count: u64,
    pub follows_count: u64,
    pub posts_count: u64,
    pub follows_me: bool,
    pub followed_by_me: bool,
    /// Mutual followers (`viewer.knownFollowers`). Total count plus
    /// up to ~5 handle samples — the wire format caps the sample
    /// itself, so we just pass through whatever Bluesky returns.
    pub known_followers_count: u64,
    pub known_followers_sample: Vec<DmMember>,
}

/// One entry in `getTimeline`'s `feed` array. Includes the post and
/// reply context (parent/root) when the entry is a reply.
#[derive(Debug, Clone)]
pub struct TimelinePost {
    pub post: PostView,
    /// If the timeline entry is a reply, this is the post being
    /// replied to (one hop up).
    pub reply_parent: Option<PostView>,
}

/// Thread tree returned by `getPostThread`. Flat list — the daemon
/// flattens the AT Protocol nested structure into oldest-first order
/// for easy LLM consumption. Caller receives `[root, reply1, reply2, ...]`.
#[derive(Debug, Clone)]
pub struct PostThread {
    pub posts: Vec<PostView>,
}

/// Viewer-state-centric profile relationship — derived from the
/// `viewer` block in `getProfile`'s response. Both fields are AT URIs
/// of the underlying follow records when set; we only surface the
/// presence/absence as bools because that's all the reply gate needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileRelationship {
    /// Does the target follow Lumen?
    pub follows_me: bool,
    /// Does Lumen follow the target?
    pub followed_by_me: bool,
}

/// A reference to an AT Protocol record we just created — both URI
/// and CID. Returned by [`crate::BlueskyClient::compose_post`] /
/// [`crate::BlueskyClient::reply_to_post`] so the caller can chain
/// further operations (threads need the parent URI+CID to reply
/// against; reposts need both to reference the subject).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PostRef {
    pub uri: AtUri,
    pub cid: Cid,
}

/// One image to attach to a post. `url` may be any HTTP(S) URL the
/// client can reach; the client downloads it, uploads to Bluesky's
/// blob store, and embeds the resulting blob ref in the post record.
/// `alt` is required by the AT lex (empty string is accepted but
/// discouraged for accessibility reasons).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ImageAttachment {
    pub url: String,
    pub alt: String,
}

/// Embed options for [`crate::BlueskyClient::compose_post`] /
/// [`crate::BlueskyClient::reply_to_post`]. `Default::default()` means
/// "plain text, no embed" — the common case. Pass a `quote` to make
/// the post a quote-post; pass `images` to attach up to four images;
/// pass both to use Bluesky's `recordWithMedia` combined embed.
#[derive(Debug, Default, Clone)]
pub struct ComposeOptions {
    pub quote: Option<PostRef>,
    pub images: Vec<ImageAttachment>,
}

/// One notification from `listNotifications`. AT Protocol calls these
/// "reasons": `mention`, `reply`, `quote`, `like`, `repost`, `follow`.
/// We surface the raw reason string so the agent can filter; tools
/// usually just pass `reason=mention` or `reason=reply`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Notification {
    pub uri: AtUri,
    pub cid: Cid,
    pub author_handle: Handle,
    pub author_did: Did,
    /// `mention`, `reply`, `quote`, `like`, `repost`, `follow`,
    /// `starterpack-joined`, ...
    pub reason: String,
    pub is_read: bool,
    /// Wire format is RFC3339; chrono's default serde matches.
    pub indexed_at: DateTime<Utc>,
    /// For mention/reply/quote, the post text that triggered the
    /// notification. Empty for like/follow/repost.
    pub text: String,
}

/// One participant in a DM conversation. Lumen herself is filtered
/// out by [`crate::BlueskyClient::list_convos`] before returning, so
/// this is always "the other side."
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DmMember {
    pub did: Did,
    pub handle: Handle,
}

/// A single DM. `sent_at` is RFC3339 from the chat service.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DmMessage {
    pub id: String,
    pub sender_did: Did,
    pub text: String,
    pub sent_at: DateTime<Utc>,
}

/// A DM conversation. `id` is the chat service's opaque convo
/// identifier — needed for `getMessages` / `sendMessage` / `updateRead`
/// but kept internal to the client; tools resolve via DID lookup.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DmConvo {
    pub id: String,
    pub members: Vec<DmMember>,
    pub last_message: Option<DmMessage>,
    pub unread_count: u32,
}

/// Compare two RFC3339 timestamps as actual instants, not strings.
///
/// `a > b` lexically only equals `a > b` chronologically inside a
/// single canonical RFC3339 shape — Bluesky has historically returned
/// both `Z` and `+00:00`, and fractional precision can vary across
/// endpoints. Lexical compare across those shapes silently lies.
/// Parses both sides via `chrono`; on parse failure of either side,
/// falls back to lexical compare so we degrade to the previous
/// behavior rather than dropping data.
#[must_use]
pub fn rfc3339_strictly_after(a: &str, b: &str) -> bool {
    use chrono::DateTime;
    match (
        DateTime::parse_from_rfc3339(a),
        DateTime::parse_from_rfc3339(b),
    ) {
        (Ok(da), Ok(db)) => da > db,
        _ => a > b,
    }
}

#[cfg(test)]
mod rfc3339_tests {
    use super::rfc3339_strictly_after;

    #[test]
    fn equal_instants_are_not_strictly_after() {
        assert!(!rfc3339_strictly_after(
            "2026-05-09T10:00:00Z",
            "2026-05-09T10:00:00Z"
        ));
    }

    #[test]
    fn later_instant_is_strictly_after() {
        assert!(rfc3339_strictly_after(
            "2026-05-09T10:00:01Z",
            "2026-05-09T10:00:00Z"
        ));
    }

    /// The bug this fix exists for: lexical compare lies when one
    /// timestamp uses `Z` and the other uses `+00:00`. They represent
    /// the same instant, but `Z` (0x5a) sorts after `+` (0x2b).
    #[test]
    fn equal_instants_with_different_offset_shapes_are_equal() {
        assert!(!rfc3339_strictly_after(
            "2026-05-09T10:00:00Z",
            "2026-05-09T10:00:00+00:00"
        ));
        assert!(!rfc3339_strictly_after(
            "2026-05-09T10:00:00+00:00",
            "2026-05-09T10:00:00Z"
        ));
    }

    /// Non-UTC offsets compared against UTC: `12:00 in +05:00` is
    /// the same instant as `07:00Z`. Lexical compare would call the
    /// `+05:00` one "later"; chronological compare gets it right.
    #[test]
    fn non_utc_offsets_compare_chronologically() {
        // 12:00+05:00 == 07:00Z. They're equal — neither strictly
        // after the other.
        assert!(!rfc3339_strictly_after(
            "2026-05-09T12:00:00+05:00",
            "2026-05-09T07:00:00Z"
        ));
        // 13:00+05:00 == 08:00Z, which IS strictly after 07:00Z.
        assert!(rfc3339_strictly_after(
            "2026-05-09T13:00:00+05:00",
            "2026-05-09T07:00:00Z"
        ));
        // But lexical compare on the same pair would NOT find this:
        // "2026-05-09T13:00:00+05:00" > "2026-05-09T13:00:01Z" lexically
        // (the Z shape wins on offset bytes), but chronologically
        // 13:00+05:00 = 08:00Z is BEFORE 13:00:01Z.
        assert!(!rfc3339_strictly_after(
            "2026-05-09T13:00:00+05:00",
            "2026-05-09T13:00:01Z"
        ));
    }

    /// On parse failure, fall back to lexical compare so callers
    /// degrade to the previous behavior rather than losing data.
    #[test]
    fn unparseable_input_falls_back_to_lexical() {
        assert!(rfc3339_strictly_after("zzz", "aaa"));
        assert!(!rfc3339_strictly_after("aaa", "zzz"));
    }

    /// Fractional precision drift: `.000Z` vs `.000000Z` are the
    /// same instant. Lexical compare considers them different.
    #[test]
    fn fractional_precision_drift_is_equal() {
        assert!(!rfc3339_strictly_after(
            "2026-05-09T10:00:00.000Z",
            "2026-05-09T10:00:00.000000Z"
        ));
    }
}
