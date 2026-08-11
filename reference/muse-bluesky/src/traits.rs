//! `Bluesky` — the trait tools see, with two implementations:
//!
//! 1. [`super::BlueskyClient`] — the real HTTP client (production).
//! 2. `FixtureBluesky` in `muse-eval` — seeded world for read methods,
//!    in-memory action recorder for write methods, deterministic fake
//!    return values. Used by the benchmarking harness so we can drive
//!    Lumen through canned scenarios without touching real Bluesky.
//!
//! Surface mirrors every `BlueskyClient` method actually called from
//! outside this crate. Internal helpers (`upload_blob`, `build_embed`,
//! `delete_record_at`) stay private to the concrete client. The trait
//! is dyn-compatible via `async-trait` so callers can hold
//! `Arc<dyn Bluesky>`.
//!
//! No type changes in the trait surface — same arg types and return
//! types as the inherent impls, so the 45 call sites in muse-tools
//! don't change.
//!
//! ## Two-impl symmetry
//!
//! The trait is the *only* surface tool code sees. New `BlueskyClient`
//! methods should land on the trait first (with a stub fixture impl)
//! and then on the concrete client. The other direction — adding a
//! method to the concrete client without putting it on the trait —
//! defeats the swap and forces callers to know which impl they have.

use async_trait::async_trait;
use serde_json::Value;

use crate::error::BlueskyError;
use crate::identifiers::{Cid, Did};
use crate::types::{
    ComposeOptions, DmConvo, DmMessage, Notification, PostRef, PostThread, PostView,
    ProfileRelationship, ProfileView, TimelinePost, WhitewindEntry,
};

/// All Bluesky operations Lumen's tools issue. See module docs for
/// the rationale and two-impl symmetry.
#[async_trait]
pub trait Bluesky: Send + Sync {
    // ─── session ───────────────────────────────────────────────

    /// Authenticate. Real client calls `createSession`; fixture is a no-op.
    async fn login(&self) -> Result<(), BlueskyError>;

    /// The DID of the currently-authenticated account, or `None` if
    /// not logged in.
    async fn current_did(&self) -> Option<Did>;

    // ─── writes (mutate the network) ───────────────────────────

    async fn compose_post(
        &self,
        text: &str,
        opts: &ComposeOptions,
    ) -> Result<PostRef, BlueskyError>;

    async fn reply_to_post(
        &self,
        text: &str,
        parent_uri: &str,
        parent_cid: &str,
        root_uri: &str,
        root_cid: &str,
        opts: &ComposeOptions,
    ) -> Result<PostRef, BlueskyError>;

    async fn compose_thread(&self, segments: &[String]) -> Result<Vec<PostRef>, BlueskyError>;

    async fn repost(&self, post_uri: &str, post_cid: &str) -> Result<String, BlueskyError>;

    async fn like(&self, post_uri: &str, post_cid: &str) -> Result<String, BlueskyError>;

    async fn follow(&self, did_to_follow: &str) -> Result<String, BlueskyError>;

    async fn unfollow(&self, did_to_unfollow: &str) -> Result<(), BlueskyError>;

    async fn block(&self, did_to_block: &str) -> Result<String, BlueskyError>;

    async fn delete_post(&self, post_uri: &str) -> Result<(), BlueskyError>;

    async fn publish_whitewind_entry(
        &self,
        title: &str,
        content_md: &str,
    ) -> Result<WhitewindEntry, BlueskyError>;

    async fn upload_blob(&self, bytes: &[u8], content_type: &str) -> Result<Value, BlueskyError>;

    // ─── reads (observe the network) ───────────────────────────

    /// Resolve the current CID for a post URI. Calls
    /// `getPostThread(depth=0)` to fetch just the post head.
    /// Used internally by write operations (reply/like/repost) so the
    /// model doesn't have to supply CIDs.
    async fn resolve_post_cid(&self, uri: &str) -> Result<Cid, BlueskyError>;

    async fn get_timeline(&self, limit: u32) -> Result<Vec<TimelinePost>, BlueskyError>;

    async fn get_post_thread(&self, uri: &str, depth: u32) -> Result<PostThread, BlueskyError>;

    async fn get_profile(&self, actor: &str) -> Result<ProfileView, BlueskyError>;

    async fn get_profile_relationship(
        &self,
        actor: &str,
    ) -> Result<ProfileRelationship, BlueskyError>;

    async fn get_author_feed(
        &self,
        actor: &str,
        limit: u32,
        filter: &str,
    ) -> Result<Vec<TimelinePost>, BlueskyError>;

    async fn search_posts(
        &self,
        query: &str,
        author: Option<&str>,
        sort: &str,
        limit: u32,
    ) -> Result<Vec<PostView>, BlueskyError>;

    async fn thread_includes_did(
        &self,
        parent_uri: &str,
        me_did: &str,
    ) -> Result<bool, BlueskyError>;

    async fn list_notifications(&self, limit: u32) -> Result<Vec<Notification>, BlueskyError>;

    // ─── DMs (chat.bsky.* — proxied through user's PDS) ────────

    async fn list_convos(&self, limit: u32) -> Result<Vec<DmConvo>, BlueskyError>;

    async fn get_convo_for_members(&self, member_dids: &[String]) -> Result<DmConvo, BlueskyError>;

    async fn get_dm_messages(
        &self,
        convo_id: &str,
        limit: u32,
    ) -> Result<Vec<DmMessage>, BlueskyError>;

    async fn send_dm(&self, convo_id: &str, text: &str) -> Result<DmMessage, BlueskyError>;

    async fn update_dm_read(
        &self,
        convo_id: &str,
        message_id: Option<&str>,
    ) -> Result<(), BlueskyError>;
}

/// Delegating impl — every trait method forwards to the inherent
/// method of the same name on the concrete client. Behavior parity is
/// total; the trait exists for swap-ability, not for adding behavior.
#[async_trait]
impl Bluesky for crate::BlueskyClient {
    async fn login(&self) -> Result<(), BlueskyError> {
        Self::login(self).await
    }

    async fn current_did(&self) -> Option<Did> {
        Self::current_did(self).await
    }

    async fn compose_post(
        &self,
        text: &str,
        opts: &ComposeOptions,
    ) -> Result<PostRef, BlueskyError> {
        Self::compose_post(self, text, opts).await
    }

    async fn reply_to_post(
        &self,
        text: &str,
        parent_uri: &str,
        parent_cid: &str,
        root_uri: &str,
        root_cid: &str,
        opts: &ComposeOptions,
    ) -> Result<PostRef, BlueskyError> {
        Self::reply_to_post(self, text, parent_uri, parent_cid, root_uri, root_cid, opts).await
    }

    async fn compose_thread(&self, segments: &[String]) -> Result<Vec<PostRef>, BlueskyError> {
        Self::compose_thread(self, segments).await
    }

    async fn repost(&self, post_uri: &str, post_cid: &str) -> Result<String, BlueskyError> {
        Self::repost(self, post_uri, post_cid).await
    }

    async fn like(&self, post_uri: &str, post_cid: &str) -> Result<String, BlueskyError> {
        Self::like(self, post_uri, post_cid).await
    }

    async fn follow(&self, did_to_follow: &str) -> Result<String, BlueskyError> {
        Self::follow(self, did_to_follow).await
    }

    async fn unfollow(&self, did_to_unfollow: &str) -> Result<(), BlueskyError> {
        Self::unfollow(self, did_to_unfollow).await
    }

    async fn block(&self, did_to_block: &str) -> Result<String, BlueskyError> {
        Self::block(self, did_to_block).await
    }

    async fn delete_post(&self, post_uri: &str) -> Result<(), BlueskyError> {
        Self::delete_post(self, post_uri).await
    }

    async fn publish_whitewind_entry(
        &self,
        title: &str,
        content_md: &str,
    ) -> Result<WhitewindEntry, BlueskyError> {
        Self::publish_whitewind_entry(self, title, content_md).await
    }

    async fn upload_blob(&self, bytes: &[u8], content_type: &str) -> Result<Value, BlueskyError> {
        Self::upload_blob(self, bytes, content_type).await
    }

    async fn resolve_post_cid(&self, uri: &str) -> Result<Cid, BlueskyError> {
        Self::resolve_post_cid(self, uri).await
    }

    async fn get_timeline(&self, limit: u32) -> Result<Vec<TimelinePost>, BlueskyError> {
        Self::get_timeline(self, limit).await
    }

    async fn get_post_thread(&self, uri: &str, depth: u32) -> Result<PostThread, BlueskyError> {
        Self::get_post_thread(self, uri, depth).await
    }

    async fn get_profile(&self, actor: &str) -> Result<ProfileView, BlueskyError> {
        Self::get_profile(self, actor).await
    }

    async fn get_profile_relationship(
        &self,
        actor: &str,
    ) -> Result<ProfileRelationship, BlueskyError> {
        Self::get_profile_relationship(self, actor).await
    }

    async fn get_author_feed(
        &self,
        actor: &str,
        limit: u32,
        filter: &str,
    ) -> Result<Vec<TimelinePost>, BlueskyError> {
        Self::get_author_feed(self, actor, limit, filter).await
    }

    async fn search_posts(
        &self,
        query: &str,
        author: Option<&str>,
        sort: &str,
        limit: u32,
    ) -> Result<Vec<PostView>, BlueskyError> {
        Self::search_posts(self, query, author, sort, limit).await
    }

    async fn thread_includes_did(
        &self,
        parent_uri: &str,
        me_did: &str,
    ) -> Result<bool, BlueskyError> {
        Self::thread_includes_did(self, parent_uri, me_did).await
    }

    async fn list_notifications(&self, limit: u32) -> Result<Vec<Notification>, BlueskyError> {
        Self::list_notifications(self, limit).await
    }

    async fn list_convos(&self, limit: u32) -> Result<Vec<DmConvo>, BlueskyError> {
        Self::list_convos(self, limit).await
    }

    async fn get_convo_for_members(&self, member_dids: &[String]) -> Result<DmConvo, BlueskyError> {
        Self::get_convo_for_members(self, member_dids).await
    }

    async fn get_dm_messages(
        &self,
        convo_id: &str,
        limit: u32,
    ) -> Result<Vec<DmMessage>, BlueskyError> {
        Self::get_dm_messages(self, convo_id, limit).await
    }

    async fn send_dm(&self, convo_id: &str, text: &str) -> Result<DmMessage, BlueskyError> {
        Self::send_dm(self, convo_id, text).await
    }

    async fn update_dm_read(
        &self,
        convo_id: &str,
        message_id: Option<&str>,
    ) -> Result<(), BlueskyError> {
        Self::update_dm_read(self, convo_id, message_id).await
    }
}
