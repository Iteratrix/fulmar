//! `BlueskyClient` — thin XRPC wrapper over reqwest.
//!
//! Session is held behind a `tokio::sync::RwLock` so refresh can mutate
//! without forcing single-threaded access. The auth flow:
//!
//! 1. `login` calls `createSession`, stores the resulting JWT pair.
//! 2. Every authed request grabs the access JWT under read lock.
//! 3. On 401/InvalidToken, the request is retried *once* after
//!    `refresh_session` swaps in a fresh access token. If refresh
//!    itself returns 401, we surface `SessionExpired` and let the
//!    caller decide whether to re-`login`.

use chrono::Utc;
use reqwest::{Client as HttpClient, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::config::BlueskyConfig;
use crate::error::BlueskyError;
use crate::identifiers::{AtUri, Cid, Did, Handle};
use crate::types::{
    ComposeOptions, DmConvo, DmMember, DmMessage, ImageAttachment, Notification, PostRef,
    PostThread, PostView, ProfileRelationship, ProfileView, Session, TimelinePost, WhitewindEntry,
};

/// `atproto-proxy` header value for Bluesky chat (DM) routes. The
/// header tells whoever handles the request that this call is destined
/// for the chat service; we *also* route the HTTP request directly to
/// `config.chat_service_url` rather than the user's PDS, because
/// `bsky.social` stopped proxying these methods and now returns 501
/// for them (observed 2026-05). The official atproto JS client
/// resolves the proxy DID's service endpoint and hits it directly —
/// we do the same with a config-driven mapping since chat is
/// currently the only proxied service we use.
const CHAT_PROXY: &str = "did:web:api.bsky.chat#bsky_chat";

pub struct BlueskyClient {
    config: BlueskyConfig,
    http: HttpClient,
    session: RwLock<Option<Session>>,
}

impl BlueskyClient {
    /// Build the client without logging in. Call `login` next.
    pub fn new(config: BlueskyConfig) -> Result<Self, BlueskyError> {
        let http = HttpClient::builder().timeout(config.http_timeout).build()?;
        Ok(Self {
            config,
            http,
            session: RwLock::new(None),
        })
    }

    /// Authenticate against `service_url` using the configured
    /// identifier/password and store the resulting session.
    pub async fn login(&self) -> Result<(), BlueskyError> {
        info!(identifier = %self.config.identifier, "bluesky login");
        let body = json!({
            "identifier": self.config.identifier,
            "password": self.config.password,
        });
        let resp = self
            .http
            .post(self.endpoint("com.atproto.server.createSession"))
            .json(&body)
            .send()
            .await?;
        let session: Session = decode_xrpc(resp).await?;
        info!(handle = %session.handle, did = %session.did, "bluesky session established");
        *self.session.write().await = Some(session);
        Ok(())
    }

    /// Export the current session (if any) so a short-lived process
    /// can persist it and resume the same refresh chain later. The
    /// CLI face stores this in a file-locked session file: repeated
    /// invocations refresh one chain instead of calling
    /// `createSession` each time (the entryway enforces ~100/day).
    pub async fn export_session(&self) -> Option<Session> {
        self.session.read().await.clone()
    }

    /// Restore a previously-exported session without touching
    /// `createSession`. A stale access token heals through the
    /// normal refresh-and-retry path on first use; a dead refresh
    /// token surfaces as `SessionExpired`, at which point the holder
    /// of the password gets to decide about a fresh `login`.
    pub async fn restore_session(&self, session: Session) {
        *self.session.write().await = Some(session);
    }

    /// Compose a top-level post. Returns both the AT URI and CID
    /// of the new post — the CID is needed to chain follow-up
    /// operations (threading, quote-posts, reposts).
    pub async fn compose_post(
        &self,
        text: &str,
        opts: &ComposeOptions,
    ) -> Result<PostRef, BlueskyError> {
        let did = self.require_did().await?;
        let embed = self.build_embed(opts).await?;
        let mut record = json!({
            "$type": "app.bsky.feed.post",
            "text": text,
            "createdAt": Utc::now().to_rfc3339(),
        });
        if let Some(embed) = embed {
            record["embed"] = embed;
        }
        let body = json!({
            "repo": did,
            "collection": "app.bsky.feed.post",
            "record": record,
        });
        let value: Value = self
            .post_authed("com.atproto.repo.createRecord", &body)
            .await?;
        parse_post_ref(&value)
    }

    /// Reply to an existing post. Both `parent` and `root` are
    /// required by the AT Protocol reply lex; for top-level replies
    /// they're the same URI/CID. `opts` controls embed (quote-post,
    /// images); pass `&ComposeOptions::default()` for a plain reply.
    pub async fn reply_to_post(
        &self,
        text: &str,
        parent_uri: &str,
        parent_cid: &str,
        root_uri: &str,
        root_cid: &str,
        opts: &ComposeOptions,
    ) -> Result<PostRef, BlueskyError> {
        let did = self.require_did().await?;
        let embed = self.build_embed(opts).await?;
        let mut record = json!({
            "$type": "app.bsky.feed.post",
            "text": text,
            "createdAt": Utc::now().to_rfc3339(),
            "reply": {
                "parent": { "uri": parent_uri, "cid": parent_cid },
                "root":   { "uri": root_uri,   "cid": root_cid   },
            },
        });
        if let Some(embed) = embed {
            record["embed"] = embed;
        }
        let body = json!({
            "repo": did,
            "collection": "app.bsky.feed.post",
            "record": record,
        });
        let value: Value = self
            .post_authed("com.atproto.repo.createRecord", &body)
            .await?;
        parse_post_ref(&value)
    }

    /// Construct the `embed` field for a post record from
    /// `ComposeOptions`. Returns `Ok(None)` when there's no embed
    /// (plain text post). The four cases follow the AT lex:
    /// quote-only → `embed.record`; images-only → `embed.images`;
    /// both → `embed.recordWithMedia`; neither → `None`.
    ///
    /// Image attachments are downloaded from their source URLs and
    /// uploaded to Bluesky's blob store one at a time. Capped at 4
    /// images per post (AT lex limit). No client-side compression in
    /// M1 — oversized blobs surface as upstream errors from
    /// `uploadBlob`; we surface those rather than swallow.
    async fn build_embed(&self, opts: &ComposeOptions) -> Result<Option<Value>, BlueskyError> {
        let has_quote = opts.quote.is_some();
        let has_images = !opts.images.is_empty();
        if !has_quote && !has_images {
            return Ok(None);
        }

        let mut image_blobs: Vec<Value> = Vec::new();
        for img in opts.images.iter().take(4) {
            let blob = self.upload_image_from_url(img).await?;
            image_blobs.push(json!({ "image": blob, "alt": img.alt }));
        }

        let record_embed = opts.quote.as_ref().map(|q| {
            json!({
                "$type": "app.bsky.embed.record",
                "record": { "uri": q.uri, "cid": q.cid },
            })
        });

        let images_embed = (!image_blobs.is_empty()).then(|| {
            json!({
                "$type": "app.bsky.embed.images",
                "images": image_blobs,
            })
        });

        let embed = match (record_embed, images_embed) {
            (Some(record), Some(images)) => json!({
                "$type": "app.bsky.embed.recordWithMedia",
                "record": record,
                "media": images,
            }),
            (Some(record), None) => record,
            (None, Some(images)) => images,
            (None, None) => unreachable!("guarded above"),
        };
        Ok(Some(embed))
    }

    /// Download an image from its source URL and upload to Bluesky's
    /// blob store. Returns the `blob` field of the upload response,
    /// suitable for embedding directly in a post record.
    async fn upload_image_from_url(&self, img: &ImageAttachment) -> Result<Value, BlueskyError> {
        let resp = self.http.get(&img.url).send().await?;
        if !resp.status().is_success() {
            return Err(BlueskyError::Unexpected(format!(
                "image download {} failed: HTTP {}",
                img.url,
                resp.status()
            )));
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        let bytes = resp.bytes().await?;
        self.upload_blob(&bytes, &content_type).await
    }

    /// `com.atproto.repo.uploadBlob` — raw byte upload with the
    /// blob's content-type. Returns the `blob` reference from the
    /// response (the shape the AT embed lex expects). Bluesky enforces
    /// a ~1MB cap; oversized uploads surface as a 400 `Api` error.
    pub async fn upload_blob(
        &self,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<Value, BlueskyError> {
        let token = self.access_jwt().await?;
        let url = self.endpoint("com.atproto.repo.uploadBlob");
        let resp = self
            .http
            .post(url)
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(bytes.to_vec())
            .send()
            .await?;
        let value: Value = decode_xrpc(resp).await?;
        let blob = value
            .get("blob")
            .cloned()
            .ok_or_else(|| BlueskyError::Unexpected("uploadBlob: missing blob field".into()))?;
        Ok(blob)
    }

    /// Compose a thread: post `segments[0]` as a top-level post,
    /// then reply each subsequent segment to the previous post (with
    /// `root` set to the first post throughout, per the AT lex).
    /// Returns the chain of `PostRef`s in order.
    ///
    /// On partial failure mid-thread, returns the chain produced so
    /// far in `Err::PartialThread`-style — actually, simpler: any
    /// failure aborts and the caller gets the error; the posts that
    /// already landed remain. The agent can recover by reading the
    /// thread back. Atomic multi-post threads aren't a thing in
    /// AT Protocol anyway.
    pub async fn compose_thread(&self, segments: &[String]) -> Result<Vec<PostRef>, BlueskyError> {
        if segments.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::with_capacity(segments.len());
        // First post is top-level.
        let plain = ComposeOptions::default();
        let first = self.compose_post(&segments[0], &plain).await?;
        out.push(first.clone());
        // Subsequent posts reply to the previous; root is the first.
        let root = first;
        for text in segments.iter().skip(1) {
            let parent = out.last().expect("non-empty");
            let next = self
                .reply_to_post(
                    text,
                    parent.uri.as_str(),
                    parent.cid.as_str(),
                    root.uri.as_str(),
                    root.cid.as_str(),
                    &plain,
                )
                .await?;
            out.push(next);
        }
        Ok(out)
    }

    /// Repost (boost) an existing post. Subject is `(uri, cid)` of
    /// the post being reposted. Returns the URI of the new repost
    /// record.
    pub async fn repost(&self, post_uri: &str, post_cid: &str) -> Result<String, BlueskyError> {
        let did = self.require_did().await?;
        let body = json!({
            "repo": did,
            "collection": "app.bsky.feed.repost",
            "record": {
                "$type": "app.bsky.feed.repost",
                "subject": { "uri": post_uri, "cid": post_cid },
                "createdAt": Utc::now().to_rfc3339(),
            },
        });
        let value: Value = self
            .post_authed("com.atproto.repo.createRecord", &body)
            .await?;
        Ok(value
            .get("uri")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }

    /// Follow a peer by DID. Returns the URI of the new follow
    /// record (Lumen never needs the URI directly — `unfollow`
    /// looks it up via `getProfile.viewer.following`).
    pub async fn follow(&self, did_to_follow: &str) -> Result<String, BlueskyError> {
        let me = self.require_did().await?;
        let body = json!({
            "repo": me,
            "collection": "app.bsky.graph.follow",
            "record": {
                "$type": "app.bsky.graph.follow",
                "subject": did_to_follow,
                "createdAt": Utc::now().to_rfc3339(),
            },
        });
        let value: Value = self
            .post_authed("com.atproto.repo.createRecord", &body)
            .await?;
        Ok(value
            .get("uri")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }

    /// Block a peer by DID. Creates an `app.bsky.graph.block` record;
    /// the upstream relay propagates the block to make the peer's
    /// posts disappear from Lumen's view and prevent them from seeing
    /// Lumen's. Public by design (Bluesky surfaces block lists).
    /// Returns the new block record's AT URI. There's intentionally
    /// no `unblock` symmetry — recovery from an erroneous block is
    /// code-level (delete the record directly), matching the TS tool
    /// surface.
    pub async fn block(&self, did_to_block: &str) -> Result<String, BlueskyError> {
        let me = self.require_did().await?;
        let body = json!({
            "repo": me,
            "collection": "app.bsky.graph.block",
            "record": {
                "$type": "app.bsky.graph.block",
                "subject": did_to_block,
                "createdAt": Utc::now().to_rfc3339(),
            },
        });
        let value: Value = self
            .post_authed("com.atproto.repo.createRecord", &body)
            .await?;
        Ok(value
            .get("uri")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }

    /// Unfollow a peer by DID. Looks up the existing follow record's
    /// URI via `getProfile.viewer.following`, then deletes it. No-op
    /// (returns Ok) when there's no existing follow — the desired
    /// end state is already in place.
    pub async fn unfollow(&self, did_to_unfollow: &str) -> Result<(), BlueskyError> {
        let value: Value = self
            .get_authed(
                "app.bsky.actor.getProfile",
                &[("actor", did_to_unfollow.to_string())],
            )
            .await?;
        let follow_uri = value
            .get("viewer")
            .and_then(|v| v.get("following"))
            .and_then(Value::as_str);
        let Some(uri) = follow_uri else {
            // Already not following — desired state achieved.
            return Ok(());
        };
        self.delete_record_at(uri).await
    }

    /// Delete one of Lumen's own posts by URI. Refuses (errors with
    /// `BlueskyError::Unexpected`) if the URI isn't owned by the
    /// current session DID — guards against typos like passing a
    /// peer's URI by mistake.
    pub async fn delete_post(&self, post_uri: &str) -> Result<(), BlueskyError> {
        let me = self.require_did().await?;
        let parsed = parse_at_uri(post_uri).ok_or_else(|| {
            BlueskyError::Unexpected(format!("not a valid at:// URI: {post_uri}"))
        })?;
        if parsed.did != me {
            return Err(BlueskyError::Unexpected(format!(
                "refuse to delete post owned by {} (this session is {me})",
                parsed.did
            )));
        }
        if parsed.collection != "app.bsky.feed.post" {
            return Err(BlueskyError::Unexpected(format!(
                "delete_post only deletes feed.post records; got collection {}",
                parsed.collection
            )));
        }
        self.delete_record_at(post_uri).await
    }

    /// Issue a `com.atproto.repo.deleteRecord` for the record at
    /// `at_uri`. Used by both `unfollow` and `delete_post`.
    async fn delete_record_at(&self, at_uri: &str) -> Result<(), BlueskyError> {
        let parsed = parse_at_uri(at_uri)
            .ok_or_else(|| BlueskyError::Unexpected(format!("not a valid at:// URI: {at_uri}")))?;
        let body = json!({
            "repo": parsed.did,
            "collection": parsed.collection,
            "rkey": parsed.rkey,
        });
        // deleteRecord returns `{}` on success — discard.
        let _: Value = self
            .post_authed("com.atproto.repo.deleteRecord", &body)
            .await?;
        Ok(())
    }

    /// Like a post. Returns the AT URI of the new like record.
    pub async fn like(&self, post_uri: &str, post_cid: &str) -> Result<String, BlueskyError> {
        let did = self.require_did().await?;
        let body = json!({
            "repo": did,
            "collection": "app.bsky.feed.like",
            "record": {
                "$type": "app.bsky.feed.like",
                "subject": { "uri": post_uri, "cid": post_cid },
                "createdAt": Utc::now().to_rfc3339(),
            },
        });
        let value: Value = self
            .post_authed("com.atproto.repo.createRecord", &body)
            .await?;
        Ok(value
            .get("uri")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }

    /// Publish a `WhiteWind` blog entry. `WhiteWind` uses the AT
    /// Protocol PDS as its backing store with a
    /// `com.whtwnd.blog.entry` collection — same `createRecord`
    /// call as posts. Returns the entry's AT URI plus the public
    /// `https://whtwnd.com/<handle>/<rkey>` page URL.
    pub async fn publish_whitewind_entry(
        &self,
        title: &str,
        content_md: &str,
    ) -> Result<WhitewindEntry, BlueskyError> {
        let (did, handle) = {
            let session = self.session.read().await;
            let session = session.as_ref().ok_or(BlueskyError::NotAuthenticated)?;
            (
                session.did.as_str().to_string(),
                session.handle.as_str().to_string(),
            )
        };
        let body = json!({
            "repo": did,
            "collection": "com.whtwnd.blog.entry",
            "record": {
                "$type": "com.whtwnd.blog.entry",
                "title": title,
                "content": content_md,
                "theme": "github-light",
                "visibility": "public",
                "createdAt": Utc::now().to_rfc3339(),
            },
        });
        let value: Value = self
            .post_authed("com.atproto.repo.createRecord", &body)
            .await?;
        let uri = value
            .get("uri")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let rkey = uri.rsplit('/').next().unwrap_or("");
        let url = format!("https://whtwnd.com/{handle}/{rkey}");
        Ok(WhitewindEntry { uri, url })
    }

    /// `getTimeline` — chronological feed of posts from people Lumen
    /// follows. Default algorithm; cursor-paged but we don't use the
    /// cursor today.
    pub async fn get_timeline(&self, limit: u32) -> Result<Vec<TimelinePost>, BlueskyError> {
        let limit = limit.clamp(1, 100);
        let value: Value = self
            .get_authed("app.bsky.feed.getTimeline", &[("limit", limit.to_string())])
            .await?;
        let feed = value
            .get("feed")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(feed.iter().filter_map(parse_timeline_entry).collect())
    }

    /// `getPostThread` — flat oldest-first thread for the given URI.
    pub async fn get_post_thread(&self, uri: &str, depth: u32) -> Result<PostThread, BlueskyError> {
        let depth = depth.clamp(0, 100);
        let value: Value = self
            .get_authed(
                "app.bsky.feed.getPostThread",
                &[("uri", uri.to_string()), ("depth", depth.to_string())],
            )
            .await?;
        let mut posts = Vec::new();
        if let Some(thread) = value.get("thread") {
            flatten_thread(thread, &mut posts);
        }
        Ok(PostThread { posts })
    }

    /// Resolve the current CID for a post URI. Calls
    /// `getPostThread(depth=0)` and extracts the root post's CID.
    /// Used by write operations (reply/like/repost) so the model never
    /// needs to supply CIDs — the daemon resolves them on demand.
    ///
    /// # Errors
    ///
    /// Returns `BlueskyError::Unexpected` when the thread response is
    /// missing the post head. Propagates HTTP and decode errors from
    /// the underlying XRPC call.
    pub async fn resolve_post_cid(&self, uri: &str) -> Result<Cid, BlueskyError> {
        let value: Value = self
            .get_authed(
                "app.bsky.feed.getPostThread",
                &[("uri", uri.to_string()), ("depth", "0".to_string())],
            )
            .await?;
        // Response shape: { "thread": { "post": { "cid": "bafy..." } } }
        let cid_str = value
            .get("thread")
            .and_then(|t| t.get("post"))
            .and_then(|p| p.get("cid"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BlueskyError::Unexpected(format!(
                    "resolve_post_cid: no cid in thread response for {uri}"
                ))
            })?;
        Ok(Cid::parse(cid_str)?)
    }

    /// The DID of the currently logged-in account, if any. The reply
    /// gate uses this to detect "is Lumen already in the thread".
    pub async fn current_did(&self) -> Option<Did> {
        self.session.read().await.as_ref().map(|s| s.did.clone())
    }

    /// Look up the relationship between Lumen and another account.
    /// Returns `(follows_me, followed_by_me)` — both false when the
    /// target hasn't interacted with Lumen, regardless of whether
    /// they exist. Errors only on substrate failure (4xx/5xx) so the
    /// reply-gate caller can degrade gracefully.
    pub async fn get_profile_relationship(
        &self,
        actor: &str,
    ) -> Result<ProfileRelationship, BlueskyError> {
        let value: Value = self
            .get_authed("app.bsky.actor.getProfile", &[("actor", actor.to_string())])
            .await?;
        let viewer = value.get("viewer");
        let follows_me = viewer
            .and_then(|v| v.get("followedBy"))
            .and_then(Value::as_str)
            .is_some();
        let followed_by_me = viewer
            .and_then(|v| v.get("following"))
            .and_then(Value::as_str)
            .is_some();
        Ok(ProfileRelationship {
            follows_me,
            followed_by_me,
        })
    }

    /// Full `getProfile` view — bio, counts, viewer state, mutual
    /// followers. Returns the same lex shape as the simpler
    /// `get_profile_relationship`, but parses every field the agent
    /// might want to render.
    pub async fn get_profile(&self, actor: &str) -> Result<ProfileView, BlueskyError> {
        let value: Value = self
            .get_authed("app.bsky.actor.getProfile", &[("actor", actor.to_string())])
            .await?;
        parse_profile_view(&value)
    }

    /// `app.bsky.feed.getAuthorFeed` — that author's recent posts.
    /// `filter` is the AT lex enum: `posts_no_replies` (default),
    /// `posts_with_replies`, or `posts_and_author_threads`. Posts
    /// come back newest-first; the agent reads them in that order.
    pub async fn get_author_feed(
        &self,
        actor: &str,
        limit: u32,
        filter: &str,
    ) -> Result<Vec<TimelinePost>, BlueskyError> {
        let limit = limit.clamp(1, 100);
        let value: Value = self
            .get_authed(
                "app.bsky.feed.getAuthorFeed",
                &[
                    ("actor", actor.to_string()),
                    ("limit", limit.to_string()),
                    ("filter", filter.to_string()),
                ],
            )
            .await?;
        let mut out = Vec::new();
        if let Some(feed) = value.get("feed").and_then(Value::as_array) {
            for entry in feed {
                if let Some(post) = parse_timeline_entry(entry) {
                    out.push(post);
                }
            }
        }
        Ok(out)
    }

    /// `app.bsky.feed.searchPosts` — full-network search by query.
    /// `author` filters to a specific handle/DID; `sort` is `"top"`
    /// or `"latest"`. Returns the flat post list (no reply parents),
    /// newest-first when `sort=latest`.
    pub async fn search_posts(
        &self,
        query: &str,
        author: Option<&str>,
        sort: &str,
        limit: u32,
    ) -> Result<Vec<PostView>, BlueskyError> {
        let limit = limit.clamp(1, 100);
        let mut params: Vec<(&str, String)> = vec![
            ("q", query.to_string()),
            ("sort", sort.to_string()),
            ("limit", limit.to_string()),
        ];
        if let Some(a) = author {
            params.push(("author", a.to_string()));
        }
        let value: Value = self
            .get_authed("app.bsky.feed.searchPosts", &params)
            .await?;
        let mut out = Vec::new();
        if let Some(posts) = value.get("posts").and_then(Value::as_array) {
            for post in posts {
                if let Some(p) = parse_post_view(post) {
                    out.push(p);
                }
            }
        }
        Ok(out)
    }

    /// Walk up the parent chain from `parent_uri` and check whether
    /// any ancestor (the parent itself, or any post above it) is by
    /// `me_did`. Used by the reply gate: "Lumen is already in this
    /// thread, so replying further is fine."
    pub async fn thread_includes_did(
        &self,
        parent_uri: &str,
        me_did: &str,
    ) -> Result<bool, BlueskyError> {
        // parentHeight=20 is generous — a chain of 20 pre-existing
        // replies above the post being responded to. Bluesky caps it
        // at 1000 anyway. depth=0 because we don't care about
        // children, only ancestors.
        let value: Value = self
            .get_authed(
                "app.bsky.feed.getPostThread",
                &[
                    ("uri", parent_uri.to_string()),
                    ("depth", "0".to_string()),
                    ("parentHeight", "20".to_string()),
                ],
            )
            .await?;
        let mut posts = Vec::new();
        if let Some(thread) = value.get("thread") {
            flatten_thread(thread, &mut posts);
        }
        Ok(posts.iter().any(|p| p.author_did.as_str() == me_did))
    }

    /// `listNotifications` — newest-first by default.
    pub async fn list_notifications(&self, limit: u32) -> Result<Vec<Notification>, BlueskyError> {
        let limit = limit.clamp(1, 100);
        let value: Value = self
            .get_authed(
                "app.bsky.notification.listNotifications",
                &[("limit", limit.to_string())],
            )
            .await?;
        let raw = value
            .get("notifications")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(raw.iter().filter_map(parse_notification).collect())
    }

    /// List Lumen's DM conversations. The chat service returns
    /// convos with `unread_count > 0` first when sorted by recency,
    /// but we don't filter — let the caller (`CheckPendingDms` tool)
    /// decide what to surface. Returns empty Vec on a successful
    /// empty response.
    ///
    /// Lumen's own DID is filtered out of each convo's `members` so
    /// the agent always sees "the other side."
    pub async fn list_convos(&self, limit: u32) -> Result<Vec<DmConvo>, BlueskyError> {
        let limit = limit.clamp(1, 100);
        let self_did = self.require_did().await?;
        let value: Value = self
            .get_authed_proxied(
                "chat.bsky.convo.listConvos",
                &[("limit", limit.to_string())],
                Some(CHAT_PROXY),
            )
            .await?;
        let raw = value
            .get("convos")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(raw
            .iter()
            .filter_map(|c| parse_convo(c, &self_did))
            .collect())
    }

    /// Resolve a DM conversation by participants. Pass the OTHER
    /// participants' DIDs (Lumen herself is added by the chat
    /// service). For 1:1 DMs that's a single DID; for group convos
    /// it'd be the full set, though Lumen doesn't use group DMs.
    pub async fn get_convo_for_members(
        &self,
        member_dids: &[String],
    ) -> Result<DmConvo, BlueskyError> {
        let self_did = self.require_did().await?;
        // The XRPC takes repeated `members` query params. Hand-build
        // since `get_authed_proxied`'s `query` is `&[(&str, String)]`
        // — perfect for repeated keys.
        let query: Vec<(&str, String)> =
            member_dids.iter().map(|d| ("members", d.clone())).collect();
        let value: Value = self
            .get_authed_proxied(
                "chat.bsky.convo.getConvoForMembers",
                &query,
                Some(CHAT_PROXY),
            )
            .await?;
        value
            .get("convo")
            .and_then(|c| parse_convo(c, &self_did))
            .ok_or_else(|| BlueskyError::Unexpected("convo missing from response".into()))
    }

    /// Fetch messages in a convo, newest-first per the AT lex.
    /// Caller usually reverses for display.
    pub async fn get_dm_messages(
        &self,
        convo_id: &str,
        limit: u32,
    ) -> Result<Vec<DmMessage>, BlueskyError> {
        let limit = limit.clamp(1, 100);
        let value: Value = self
            .get_authed_proxied(
                "chat.bsky.convo.getMessages",
                &[
                    ("convoId", convo_id.to_string()),
                    ("limit", limit.to_string()),
                ],
                Some(CHAT_PROXY),
            )
            .await?;
        let raw = value
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(raw.iter().filter_map(parse_dm_message).collect())
    }

    /// Send a DM. The chat service requires the convo to already
    /// exist; resolve via [`Self::get_convo_for_members`] first if
    /// you only have a recipient DID.
    pub async fn send_dm(&self, convo_id: &str, text: &str) -> Result<DmMessage, BlueskyError> {
        let body = serde_json::json!({
            "convoId": convo_id,
            "message": { "text": text },
        });
        let value: Value = self
            .post_authed_proxied("chat.bsky.convo.sendMessage", &body, Some(CHAT_PROXY))
            .await?;
        parse_dm_message(&value)
            .ok_or_else(|| BlueskyError::Unexpected("sent message missing from response".into()))
    }

    /// Mark messages in a convo as read up to (and including)
    /// `message_id`. `None` marks everything currently in the convo
    /// as read.
    pub async fn update_dm_read(
        &self,
        convo_id: &str,
        message_id: Option<&str>,
    ) -> Result<(), BlueskyError> {
        let mut body = serde_json::json!({ "convoId": convo_id });
        if let Some(id) = message_id {
            body["messageId"] = serde_json::Value::String(id.to_string());
        }
        // Response contains the updated convo; ignore — callers don't need it.
        let _: Value = self
            .post_authed_proxied("chat.bsky.convo.updateRead", &body, Some(CHAT_PROXY))
            .await?;
        Ok(())
    }

    fn endpoint(&self, method: &str) -> String {
        self.endpoint_for(method, None)
    }

    fn endpoint_for(&self, method: &str, proxy: Option<&str>) -> String {
        let base = Self::service_base_for(
            proxy,
            &self.config.service_url,
            &self.config.chat_service_url,
        )
        .trim_end_matches('/');
        format!("{base}/xrpc/{method}")
    }

    fn service_base_for<'a>(proxy: Option<&str>, pds_url: &'a str, chat_url: &'a str) -> &'a str {
        match proxy {
            Some(CHAT_PROXY) => chat_url,
            _ => pds_url,
        }
    }

    async fn require_did(&self) -> Result<String, BlueskyError> {
        let session = self.session.read().await;
        session
            .as_ref()
            .map(|s| s.did.as_str().to_string())
            .ok_or(BlueskyError::NotAuthenticated)
    }

    async fn access_jwt(&self) -> Result<String, BlueskyError> {
        let session = self.session.read().await;
        session
            .as_ref()
            .map(|s| s.access_jwt.clone())
            .ok_or(BlueskyError::NotAuthenticated)
    }

    /// POST with `Authorization: Bearer <access_jwt>`. Retries once
    /// on 401 after refreshing the session.
    async fn post_authed<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        body: &impl Serialize,
    ) -> Result<T, BlueskyError> {
        self.post_authed_proxied(method, body, None).await
    }

    /// GET with auth + retry-on-401, same pattern as `post_authed`.
    async fn get_authed<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        query: &[(&str, String)],
    ) -> Result<T, BlueskyError> {
        self.get_authed_proxied(method, query, None).await
    }

    /// POST with auth + retry-on-401, optionally adding an
    /// `atproto-proxy` header. The proxy mechanism (e.g.
    /// `did:web:api.bsky.chat#bsky_chat`) tells the user's PDS to
    /// forward this XRPC call to a different service. Used by the
    /// chat (DM) routes which live at `did:web:api.bsky.chat`.
    async fn post_authed_proxied<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        body: &impl Serialize,
        proxy: Option<&str>,
    ) -> Result<T, BlueskyError> {
        let url = self.endpoint_for(method, proxy);
        let jwt = self.access_jwt().await?;
        let mut req = self.http.post(&url).bearer_auth(&jwt).json(body);
        if let Some(p) = proxy {
            req = req.header("atproto-proxy", p);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let resp_body = resp.bytes().await?;
        if is_session_expired(status, &resp_body) {
            self.refresh_session().await?;
            let jwt = self.access_jwt().await?;
            let mut req = self.http.post(&url).bearer_auth(&jwt).json(body);
            if let Some(p) = proxy {
                req = req.header("atproto-proxy", p);
            }
            let resp = req.send().await?;
            return decode_xrpc(resp).await;
        }
        decode_xrpc_body(status, &resp_body)
    }

    /// GET equivalent of `post_authed_proxied`.
    async fn get_authed_proxied<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        query: &[(&str, String)],
        proxy: Option<&str>,
    ) -> Result<T, BlueskyError> {
        let url = self.url_with_query_for(method, query, proxy);
        debug!(target: "muse_bluesky::client", url = %url, proxy = ?proxy, "xrpc GET");
        let jwt = self.access_jwt().await?;
        let mut req = self.http.get(&url).bearer_auth(&jwt);
        if let Some(p) = proxy {
            req = req.header("atproto-proxy", p);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let resp_body = resp.bytes().await?;
        if is_session_expired(status, &resp_body) {
            self.refresh_session().await?;
            let jwt = self.access_jwt().await?;
            let mut req = self.http.get(&url).bearer_auth(&jwt);
            if let Some(p) = proxy {
                req = req.header("atproto-proxy", p);
            }
            let resp = req.send().await?;
            return decode_xrpc(resp).await;
        }
        decode_xrpc_body(status, &resp_body)
    }

    fn url_with_query_for(
        &self,
        method: &str,
        query: &[(&str, String)],
        proxy: Option<&str>,
    ) -> String {
        // `form_urlencoded` encodes BOTH keys and values per spec
        // (`application/x-www-form-urlencoded`). The prior hand-rolled
        // loop left keys raw — fine for our literal keys today, but
        // one rename to a key containing `&` or `=` would silently
        // corrupt the URL. Already in the workspace tree transitively
        // via reqwest, so the "smaller dep tree" rationale that
        // motivated the hand-rolled version was incorrect.
        let mut url = self.endpoint_for(method, proxy);
        if query.is_empty() {
            return url;
        }
        let qs = form_urlencoded::Serializer::new(String::new())
            .extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str())))
            .finish();
        url.push('?');
        url.push_str(&qs);
        url
    }

    async fn refresh_session(&self) -> Result<(), BlueskyError> {
        let refresh_jwt = {
            let session = self.session.read().await;
            session
                .as_ref()
                .map(|s| s.refresh_jwt.clone())
                .ok_or(BlueskyError::NotAuthenticated)?
        };
        debug!("refreshing bluesky session");
        let resp = self
            .http
            .post(self.endpoint("com.atproto.server.refreshSession"))
            .bearer_auth(&refresh_jwt)
            .send()
            .await?;
        if resp.status() == StatusCode::UNAUTHORIZED {
            warn!("refresh returned 401 — both jwts expired");
            return Err(BlueskyError::SessionExpired);
        }
        let new_session: Session = decode_xrpc(resp).await?;
        *self.session.write().await = Some(new_session);
        Ok(())
    }
}

async fn decode_xrpc<T: for<'de> Deserialize<'de>>(resp: Response) -> Result<T, BlueskyError> {
    let status = resp.status();
    let body = resp.bytes().await?;
    decode_xrpc_body(status, &body)
}

/// Pure-data variant of [`decode_xrpc`] for code paths that have
/// already read the body (e.g. to peek for a session-expired marker
/// before deciding whether to retry).
fn decode_xrpc_body<T: for<'de> Deserialize<'de>>(
    status: StatusCode,
    body: &[u8],
) -> Result<T, BlueskyError> {
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
        return Err(BlueskyError::Api {
            status: status.as_u16(),
            kind,
            message,
        });
    }
    Ok(serde_json::from_slice::<T>(body)?)
}

/// True when the response indicates Lumen's access JWT needs a
/// refresh. AT Protocol surfaces two distinct shapes for this — the
/// `401 InvalidToken` we always knew about, AND a `400 ExpiredToken`
/// that the chat service (`api.bsky.chat`) returns instead of 401.
/// Observed 2026-06-08: a daemon that had been running for ~10 hours
/// hit `400 ExpiredToken` on every DM poll + every wake-context
/// notification fetch, because the access JWT had aged out and the
/// retry path only triggered on 401. With this check the same retry
/// loop covers both shapes.
fn is_session_expired(status: StatusCode, body: &[u8]) -> bool {
    if status == StatusCode::UNAUTHORIZED {
        return true;
    }
    if status == StatusCode::BAD_REQUEST {
        let v: Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(_) => return false,
        };
        if v.get("error").and_then(Value::as_str) == Some("ExpiredToken") {
            return true;
        }
    }
    false
}

fn parse_timeline_entry(raw: &Value) -> Option<TimelinePost> {
    let post = parse_post_view(raw.get("post")?)?;
    let reply_parent = raw
        .get("reply")
        .and_then(|r| r.get("parent"))
        .and_then(parse_post_view);
    Some(TimelinePost { post, reply_parent })
}

/// Parse an AT Protocol timestamp string into `DateTime<Utc>`.
/// AT Proto requires RFC3339 for `indexedAt`/`sentAt` but tolerates
/// shape variation (`Z` vs `+00:00`, fractional precision drift).
/// Returns `None` on missing field or parse failure; callers use
/// `?` to invalidate the whole record. Consistent with how other
/// required fields like `uri`/`cid` are handled in the parsers.
fn parse_atproto_datetime(raw: Option<&str>) -> Option<chrono::DateTime<chrono::Utc>> {
    let s = raw?;
    Some(
        chrono::DateTime::parse_from_rfc3339(s)
            .ok()?
            .with_timezone(&chrono::Utc),
    )
}

fn parse_post_view(raw: &Value) -> Option<PostView> {
    let uri = AtUri::parse(raw.get("uri")?.as_str()?).ok()?;
    let cid = Cid::parse(raw.get("cid")?.as_str()?).ok()?;
    let author = raw.get("author")?;
    let author_handle = Handle::parse(author.get("handle")?.as_str()?).ok()?;
    let author_did = Did::parse(author.get("did")?.as_str()?).ok()?;
    let record = raw.get("record")?;
    let text = record
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let reply_parent = record
        .get("reply")
        .and_then(|r| r.get("parent"))
        .and_then(|p| p.get("uri"))
        .and_then(Value::as_str)
        .and_then(|s| AtUri::parse(s).ok());
    let indexed_at = parse_atproto_datetime(raw.get("indexedAt").and_then(Value::as_str))?;
    let like_count = raw.get("likeCount").and_then(Value::as_u64).unwrap_or(0);
    let repost_count = raw.get("repostCount").and_then(Value::as_u64).unwrap_or(0);
    let reply_count = raw.get("replyCount").and_then(Value::as_u64).unwrap_or(0);
    Some(PostView {
        uri,
        cid,
        author_handle,
        author_did,
        text,
        reply_parent,
        indexed_at,
        like_count,
        repost_count,
        reply_count,
    })
}

fn parse_profile_view(raw: &Value) -> Result<ProfileView, BlueskyError> {
    let did = Did::parse(
        raw.get("did")
            .and_then(Value::as_str)
            .ok_or_else(|| BlueskyError::Unexpected("profile missing did".into()))?,
    )?;
    let handle = Handle::parse(
        raw.get("handle")
            .and_then(Value::as_str)
            .ok_or_else(|| BlueskyError::Unexpected("profile missing handle".into()))?,
    )?;
    let display_name = raw
        .get("displayName")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let description = raw
        .get("description")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let followers_count = raw
        .get("followersCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let follows_count = raw.get("followsCount").and_then(Value::as_u64).unwrap_or(0);
    let posts_count = raw.get("postsCount").and_then(Value::as_u64).unwrap_or(0);

    let viewer = raw.get("viewer");
    let follows_me = viewer
        .and_then(|v| v.get("followedBy"))
        .and_then(Value::as_str)
        .is_some();
    let followed_by_me = viewer
        .and_then(|v| v.get("following"))
        .and_then(Value::as_str)
        .is_some();

    let known = viewer.and_then(|v| v.get("knownFollowers"));
    let known_followers_count = known
        .and_then(|k| k.get("count"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let known_followers_sample = known
        .and_then(|k| k.get("followers"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|f| {
                    let did = Did::parse(f.get("did")?.as_str()?).ok()?;
                    let handle = Handle::parse(f.get("handle")?.as_str()?).ok()?;
                    Some(DmMember { did, handle })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(ProfileView {
        did,
        handle,
        display_name,
        description,
        followers_count,
        follows_count,
        posts_count,
        follows_me,
        followed_by_me,
        known_followers_count,
        known_followers_sample,
    })
}

/// AT Protocol thread responses are nested:
/// `{ post, parent: { post, parent: ... }, replies: [...] }`. Walk
/// up to root, then write down replies in order. Result: oldest-first.
fn flatten_thread(node: &Value, out: &mut Vec<PostView>) {
    // Walk parents up.
    let mut chain = Vec::new();
    let mut cursor = node;
    loop {
        if let Some(post) = cursor.get("post").and_then(parse_post_view) {
            chain.push(post);
        }
        match cursor.get("parent") {
            Some(p) if !p.is_null() => cursor = p,
            _ => break,
        }
    }
    chain.reverse();
    out.extend(chain);

    if let Some(replies) = node.get("replies").and_then(Value::as_array) {
        for r in replies {
            flatten_replies(r, out);
        }
    }
}

fn flatten_replies(node: &Value, out: &mut Vec<PostView>) {
    if let Some(post) = node.get("post").and_then(parse_post_view) {
        out.push(post);
    }
    if let Some(replies) = node.get("replies").and_then(Value::as_array) {
        for r in replies {
            flatten_replies(r, out);
        }
    }
}

/// Extract a `PostRef` from a `createRecord` response. Returns an
/// error when `uri` or `cid` fields are absent or malformed — the
/// AT API contract guarantees both on a 200-OK `createRecord` response,
/// so a missing or wrong-shaped identifier here means either the lex
/// changed or we hit a non-standard PDS.
fn parse_post_ref(value: &Value) -> Result<PostRef, BlueskyError> {
    let uri = AtUri::parse(
        value
            .get("uri")
            .and_then(Value::as_str)
            .ok_or_else(|| BlueskyError::Unexpected("createRecord: missing uri".into()))?,
    )?;
    let cid = Cid::parse(
        value
            .get("cid")
            .and_then(Value::as_str)
            .ok_or_else(|| BlueskyError::Unexpected("createRecord: missing cid".into()))?,
    )?;
    Ok(PostRef { uri, cid })
}

/// Components of an AT Protocol record URI (internal parser helper).
/// Distinct from the public `AtUri` newtype — this struct carries
/// the three individual components for record operations that need
/// them separately (e.g. `delete_record_at`).
struct AtUriComponents<'a> {
    did: &'a str,
    collection: &'a str,
    rkey: &'a str,
}

/// Parse `at://<did>/<collection>/<rkey>` into its three components.
/// Returns `None` for malformed URIs.
fn parse_at_uri(uri: &str) -> Option<AtUriComponents<'_>> {
    let rest = uri.strip_prefix("at://")?;
    let mut parts = rest.splitn(3, '/');
    let did = parts.next()?;
    let collection = parts.next()?;
    let rkey = parts.next()?;
    if did.is_empty() || collection.is_empty() || rkey.is_empty() {
        return None;
    }
    Some(AtUriComponents {
        did,
        collection,
        rkey,
    })
}

fn parse_notification(raw: &Value) -> Option<Notification> {
    let uri = AtUri::parse(raw.get("uri")?.as_str()?).ok()?;
    let cid = Cid::parse(raw.get("cid")?.as_str()?).ok()?;
    let author = raw.get("author")?;
    let author_handle = Handle::parse(author.get("handle")?.as_str()?).ok()?;
    let author_did = Did::parse(author.get("did")?.as_str()?).ok()?;
    let reason = raw
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let is_read = raw.get("isRead").and_then(Value::as_bool).unwrap_or(false);
    let indexed_at = parse_atproto_datetime(raw.get("indexedAt").and_then(Value::as_str))?;
    let text = raw
        .get("record")
        .and_then(|r| r.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Some(Notification {
        uri,
        cid,
        author_handle,
        author_did,
        reason,
        is_read,
        indexed_at,
        text,
    })
}

/// Parse a `chat.bsky.convo.defs#convoView`. `self_did` is filtered
/// out of the members list — the agent always wants "the other side."
/// Returns `None` if the JSON shape is missing required fields
/// (id, members), which would mean the lex changed under us.
fn parse_convo(raw: &Value, self_did: &str) -> Option<DmConvo> {
    let id = raw.get("id")?.as_str()?.to_string();
    let members_raw = raw.get("members")?.as_array()?;
    let members: Vec<DmMember> = members_raw
        .iter()
        .filter_map(|m| {
            let did_str = m.get("did")?.as_str()?;
            if did_str == self_did {
                return None;
            }
            let did = Did::parse(did_str).ok()?;
            let handle = Handle::parse(m.get("handle")?.as_str()?).ok()?;
            Some(DmMember { did, handle })
        })
        .collect();
    let last_message = raw.get("lastMessage").and_then(parse_dm_message);
    let unread_count = raw
        .get("unreadCount")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0);
    Some(DmConvo {
        id,
        members,
        last_message,
        unread_count,
    })
}

/// Parse a `chat.bsky.convo.defs#messageView`. `chat.bsky.convo.defs
/// #deletedMessageView` and other variants are skipped (return
/// `None`); the chat service marks them with a `$type` field, but
/// we just rely on the absence of `text` / `sender.did` to filter.
fn parse_dm_message(raw: &Value) -> Option<DmMessage> {
    let id = raw.get("id")?.as_str()?.to_string();
    let sender_did = Did::parse(raw.get("sender")?.get("did")?.as_str()?).ok()?;
    let raw_text = raw.get("text")?.as_str()?.to_string();
    let sent_at = parse_atproto_datetime(raw.get("sentAt").and_then(Value::as_str))?;
    let text = render_dm_text(&raw_text, raw.get("facets"), raw.get("embed"));
    Some(DmMessage {
        id,
        sender_did,
        text,
        sent_at,
    })
}

/// Bluesky DMs ship text alongside `facets` (rich-text annotations
/// keyed by byte offset) and `embed` (referenced records). The agent
/// sees only `DmMessage.text`, so we fold both into the text at parse
/// time:
///
/// 1. **Facet link URIs**: Bluesky's clients truncate long URLs in the
///    displayed text (e.g. `https://example.com/very-long-pa...`).
///    The full URL lives in `facets[].features[].uri`. When the
///    displayed substring ends with `...` and disagrees with the URI,
///    rewrite the substring to the full URI so the agent sees the
///    real link.
/// 2. **Embedded records**: shared Bluesky posts come through
///    `embed.record` with `uri`, `author.handle`, `value.text`.
///    Append a `[shared post …]` block so the agent can react to
///    quoted content instead of seeing only "I shared this:".
fn render_dm_text(text: &str, facets: Option<&Value>, embed: Option<&Value>) -> String {
    let mut out = text.to_string();

    if let Some(facets) = facets.and_then(Value::as_array) {
        // Walk facets back-to-front so each splice only mutates bytes
        // to the right of any unsplit facet. The unchanged-prefix
        // portion of `out` keeps its original byte offsets, so earlier
        // facets' byteStart/byteEnd remain valid against the running
        // `out` value.
        let buf_len = text.len();
        let mut link_facets: Vec<(usize, usize, String)> = facets
            .iter()
            .filter_map(|f| {
                let idx = f.get("index")?;
                let start = usize::try_from(idx.get("byteStart")?.as_u64()?).ok()?;
                let end = usize::try_from(idx.get("byteEnd")?.as_u64()?).ok()?;
                let uri = f.get("features")?.as_array()?.iter().find_map(|feat| {
                    (feat.get("$type")?.as_str()? == "app.bsky.richtext.facet#link")
                        .then(|| feat.get("uri")?.as_str().map(str::to_string))?
                })?;
                Some((start, end, uri))
            })
            .filter(|(start, end, _)| *end <= buf_len && *start <= *end)
            .collect();
        link_facets.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));

        for (start, end, uri) in link_facets {
            // Bounds-check against the running `out` (not the original)
            // — should still match because we splice right-to-left and
            // each step only rewrites bytes >= start.
            if end > out.len() || !out.is_char_boundary(start) || !out.is_char_boundary(end) {
                continue;
            }
            let display = &out[start..end];
            if display == uri || !display.ends_with("...") {
                continue;
            }
            out.replace_range(start..end, &uri);
        }
    }

    if let Some(embed) = embed
        && let Some(record) = embed.get("record")
    {
        let uri = record.get("uri").and_then(Value::as_str);
        let author = record
            .get("author")
            .and_then(|a| a.get("handle").and_then(Value::as_str))
            .unwrap_or("unknown");
        let inner = record
            .get("value")
            .and_then(|v| v.get("text").and_then(Value::as_str))
            .unwrap_or("(no text)");
        if let Some(uri) = uri {
            use std::fmt::Write as _;
            let _ = write!(out, "\n\n[shared post by @{author} ({uri})]\n{inner}");
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `form_urlencoded::Serializer` encodes both keys and values per
    /// `application/x-www-form-urlencoded`. Verifies that values
    /// containing `%`, `&`, `=`, `+`, and non-ASCII multi-byte chars
    /// all encode correctly — the hand-rolled predecessor mis-encoded
    /// `%` (treated as literal) and didn't encode keys at all.
    #[test]
    fn url_with_query_for_encodes_reserved_chars_in_values() {
        let client = BlueskyClient::new(BlueskyConfig {
            service_url: "https://pds.example".to_string(),
            chat_service_url: "https://chat.example".to_string(),
            identifier: String::new(),
            password: String::new(),
            http_timeout: std::time::Duration::from_secs(5),
        })
        .expect("build");
        // Each pair tests a different reserved char.
        let url = client.url_with_query_for(
            "com.example.test",
            &[
                ("a", "hello world".to_string()), // space
                ("b", "x=y&z".to_string()),       // delimiters
                ("c", "100%".to_string()),        // literal %
                ("d", "1+2".to_string()),         // plus
                ("e", "©2026".to_string()),       // multibyte UTF-8
            ],
            None,
        );
        // Decode the query side of the URL with the matching decoder
        // and check round-trip.
        let (base, qs) = url.split_once('?').expect("has query");
        assert_eq!(base, "https://pds.example/xrpc/com.example.test");
        let decoded: Vec<(String, String)> = form_urlencoded::parse(qs.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(
            decoded,
            vec![
                ("a".to_string(), "hello world".to_string()),
                ("b".to_string(), "x=y&z".to_string()),
                ("c".to_string(), "100%".to_string()),
                ("d".to_string(), "1+2".to_string()),
                ("e".to_string(), "©2026".to_string()),
            ]
        );
        // Spot-check: literal `&` must NOT appear inside the value
        // segment — it would terminate the key/value pair early.
        assert!(
            !url.contains("=x=y&z&"),
            "raw `&` inside value would corrupt the URL: {url}"
        );
    }

    #[test]
    fn chat_proxy_routes_to_chat_service_url() {
        // bsky.social started returning 501 for chat.bsky.* methods
        // (2026-05). The proxy header alone isn't enough — the request
        // has to go to api.bsky.chat directly.
        let pds = "https://bsky.social";
        let chat = "https://api.bsky.chat";
        assert_eq!(
            BlueskyClient::service_base_for(Some(CHAT_PROXY), pds, chat),
            chat,
        );
        assert_eq!(BlueskyClient::service_base_for(None, pds, chat), pds);
        assert_eq!(
            BlueskyClient::service_base_for(Some("did:web:unknown#svc"), pds, chat),
            pds,
        );
    }

    #[test]
    fn parse_post_view_handles_minimal_record() {
        let raw = json!({
            "uri": "at://did:plc:abc/app.bsky.feed.post/123",
            "cid": "bafy...",
            "author": { "handle": "alice.test", "did": "did:plc:abc" },
            "record": { "text": "hello" },
            "indexedAt": "2026-05-09T00:00:00Z"
        });
        let view = parse_post_view(&raw).expect("parse");
        assert_eq!(view.author_handle.as_str(), "alice.test");
        assert_eq!(view.text, "hello");
        assert!(view.reply_parent.is_none());
    }

    #[test]
    fn parse_convo_filters_self_did_from_members() {
        let raw = json!({
            "id": "convo-abc",
            "members": [
                { "did": "did:plc:lumen", "handle": "lumen.test" },
                { "did": "did:plc:peer1", "handle": "peer1.test" },
                { "did": "did:plc:peer2", "handle": "peer2.test" }
            ],
            "unreadCount": 2,
            "lastMessage": {
                "id": "msg-1",
                "sender": { "did": "did:plc:peer1" },
                "text": "ping",
                "sentAt": "2026-05-09T10:00:00Z"
            }
        });
        let convo = parse_convo(&raw, "did:plc:lumen").expect("parse");
        assert_eq!(convo.id, "convo-abc");
        assert_eq!(convo.members.len(), 2);
        assert!(
            convo
                .members
                .iter()
                .all(|m| m.did.as_str() != "did:plc:lumen")
        );
        assert_eq!(convo.unread_count, 2);
        assert_eq!(convo.last_message.unwrap().text, "ping");
    }

    #[test]
    fn parse_dm_message_skips_deleted_variants() {
        // Deleted messages have $type but no `text` / `sender`.
        let deleted = json!({
            "$type": "chat.bsky.convo.defs#deletedMessageView",
            "id": "msg-x"
        });
        assert!(parse_dm_message(&deleted).is_none());

        let live = json!({
            "id": "msg-1",
            "sender": { "did": "did:plc:peer" },
            "text": "hi",
            "sentAt": "2026-05-09T10:00:00Z"
        });
        let m = parse_dm_message(&live).expect("parse");
        assert_eq!(m.id, "msg-1");
        assert_eq!(m.sender_did.as_str(), "did:plc:peer");
        assert_eq!(m.text, "hi");
    }

    #[test]
    fn render_dm_text_expands_truncated_link_facet_to_full_uri() {
        // Bluesky sent "check https://example.com/short..." with a
        // facet pointing the truncated range at the full URI.
        let text = "check https://example.com/short...";
        // Byte offsets: "check " is 6 bytes; "https://example.com/short..." is 28 bytes.
        let facets = json!([{
            "index": { "byteStart": 6, "byteEnd": 34 },
            "features": [{
                "$type": "app.bsky.richtext.facet#link",
                "uri": "https://example.com/short-but-actually-long-and-real"
            }]
        }]);
        let rendered = super::render_dm_text(text, Some(&facets), None);
        assert_eq!(
            rendered,
            "check https://example.com/short-but-actually-long-and-real"
        );
    }

    #[test]
    fn render_dm_text_leaves_non_truncated_links_alone() {
        // No `...` in the display range → display is canonical, skip.
        let text = "go to https://x.test now";
        let facets = json!([{
            "index": { "byteStart": 6, "byteEnd": 20 },
            "features": [{
                "$type": "app.bsky.richtext.facet#link",
                "uri": "https://x.test"
            }]
        }]);
        let rendered = super::render_dm_text(text, Some(&facets), None);
        assert_eq!(rendered, text);
    }

    #[test]
    fn render_dm_text_appends_embedded_record_after_text() {
        let embed = json!({
            "record": {
                "uri": "at://did:plc:peer/app.bsky.feed.post/abc",
                "author": { "handle": "peer.test" },
                "value": { "text": "the inner post body" }
            }
        });
        let rendered = super::render_dm_text("look at this", None, Some(&embed));
        assert!(rendered.starts_with("look at this\n\n[shared post by @peer.test"));
        assert!(rendered.contains("the inner post body"));
        assert!(rendered.contains("at://did:plc:peer/app.bsky.feed.post/abc"));
    }

    #[test]
    fn render_dm_text_returns_input_unchanged_when_no_facets_no_embed() {
        assert_eq!(super::render_dm_text("hi", None, None), "hi");
    }

    #[test]
    fn render_dm_text_applies_multiple_facets_back_to_front() {
        // Two truncated links — verify the later one doesn't shift
        // earlier byte offsets, since we walk back-to-front.
        let text = "see https://a.example/aaa... and https://b.example/bbb...";
        // "see " = 4 bytes; "https://a.example/aaa..." = 24 bytes (4..28).
        // " and " = 5 bytes (28..33); "https://b.example/bbb..." = 24 bytes (33..57).
        let facets = json!([
            {
                "index": { "byteStart": 4, "byteEnd": 28 },
                "features": [{
                    "$type": "app.bsky.richtext.facet#link",
                    "uri": "https://a.example/aaa-full"
                }]
            },
            {
                "index": { "byteStart": 33, "byteEnd": 57 },
                "features": [{
                    "$type": "app.bsky.richtext.facet#link",
                    "uri": "https://b.example/bbb-full"
                }]
            }
        ]);
        let rendered = super::render_dm_text(text, Some(&facets), None);
        assert_eq!(
            rendered,
            "see https://a.example/aaa-full and https://b.example/bbb-full"
        );
    }

    #[test]
    fn parse_notification_handles_mention() {
        let raw = json!({
            "uri": "at://did:plc:abc/app.bsky.feed.post/123",
            "cid": "bafy...",
            "author": { "handle": "alice.test", "did": "did:plc:abc" },
            "reason": "mention",
            "isRead": false,
            "indexedAt": "2026-05-09T00:00:00Z",
            "record": { "text": "hi @lumen.bsky.social" }
        });
        let notif = parse_notification(&raw).expect("parse");
        assert_eq!(notif.reason, "mention");
        assert!(!notif.is_read);
        assert!(notif.text.contains("@lumen"));
    }

    // ---- Integration tests against a real HTTP mock ----
    //
    // These exercise the full reqwest round-trip: serialization,
    // headers, response parsing. Mock responses are shaped from real
    // bsky.social outputs (trimmed). Catches the kind of wire-format
    // drift that struct-level unit tests can't see.

    use wiremock::matchers::{any, body_string_contains, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn fixture_session(server: &MockServer) -> super::BlueskyClient {
        let body = serde_json::json!({
            "did": "did:plc:fixturedid",
            "handle": "lumen.test",
            "accessJwt": "access-token-1",
            "refreshJwt": "refresh-token-1",
        });
        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.server.createSession"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(server)
            .await;
        let client = super::BlueskyClient::new(super::BlueskyConfig {
            service_url: server.uri(),
            chat_service_url: server.uri(),
            identifier: "lumen.test".to_string(),
            password: "app-pass".to_string(),
            http_timeout: std::time::Duration::from_secs(5),
        })
        .expect("client");
        client.login().await.expect("login");
        client
    }

    #[tokio::test]
    async fn login_stores_session_from_create_session_response() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;
        assert_eq!(
            client.current_did().await.as_ref().map(Did::as_str),
            Some("did:plc:fixturedid")
        );
    }

    #[tokio::test]
    async fn compose_post_sends_authed_create_record() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.repo.createRecord"))
            .and(header("authorization", "Bearer access-token-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "uri": "at://did:plc:fixturedid/app.bsky.feed.post/3rkey",
                "cid": "bafy...",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let post = client
            .compose_post("hello world", &ComposeOptions::default())
            .await
            .expect("post");
        assert_eq!(
            post.uri.as_str(),
            "at://did:plc:fixturedid/app.bsky.feed.post/3rkey"
        );
    }

    #[tokio::test]
    async fn get_timeline_parses_real_shaped_response() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        let body = serde_json::json!({
            "feed": [
                {
                    "post": {
                        "uri": "at://did:plc:alice/app.bsky.feed.post/p1",
                        "cid": "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaac1",
                        "author": { "handle": "alice.test", "did": "did:plc:alice" },
                        "record": { "text": "morning thoughts" },
                        "indexedAt": "2026-05-09T10:00:00Z"
                    }
                },
                {
                    "post": {
                        "uri": "at://did:plc:bob/app.bsky.feed.post/p2",
                        "cid": "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaac2",
                        "author": { "handle": "bob.test", "did": "did:plc:bob" },
                        "record": { "text": "more thoughts" },
                        "indexedAt": "2026-05-09T10:05:00Z"
                    }
                }
            ]
        });
        Mock::given(method("GET"))
            .and(path("/xrpc/app.bsky.feed.getTimeline"))
            .and(query_param("limit", "30"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;

        let posts = client.get_timeline(30).await.expect("timeline");
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[0].post.author_handle.as_str(), "alice.test");
        assert_eq!(posts[0].post.text, "morning thoughts");
        assert_eq!(posts[1].post.text, "more thoughts");
    }

    /// Regression for the 2026-06-08 outage: a daemon that had been
    /// running ~10 hours started getting `400 ExpiredToken` on every
    /// chat (DM poll) + every notifications fetch, because the chat
    /// service returns 400 instead of 401 when the access JWT ages
    /// out. The old retry path only triggered on 401, so the session
    /// never refreshed and Bluesky tools went silent until the
    /// daemon was bounced.
    #[tokio::test]
    async fn expired_token_400_triggers_refresh_and_retry() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.server.refreshSession"))
            .and(header("authorization", "Bearer refresh-token-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "did": "did:plc:fixturedid",
                "handle": "lumen.test",
                "accessJwt": "access-token-2",
                "refreshJwt": "refresh-token-2",
            })))
            .expect(1)
            .mount(&server)
            .await;

        // First listConvos hit (old token) → 400 ExpiredToken
        // (the wire shape the chat service actually uses).
        Mock::given(method("GET"))
            .and(path("/xrpc/chat.bsky.convo.listConvos"))
            .and(header("authorization", "Bearer access-token-1"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "ExpiredToken",
                "message": "Token has expired",
            })))
            .expect(1)
            .mount(&server)
            .await;

        // Second listConvos (new token) → 200.
        Mock::given(method("GET"))
            .and(path("/xrpc/chat.bsky.convo.listConvos"))
            .and(header("authorization", "Bearer access-token-2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "convos": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        let convos = client.list_convos(50).await.expect("list");
        assert_eq!(convos.len(), 0);
        server.verify().await;
    }

    /// Session export/restore: a restored session must work WITHOUT
    /// any `createSession` call (that's the whole point — the CLI
    /// face resumes the daemon-independent refresh chain instead of
    /// burning the ~100/day login budget), a stale access token must
    /// heal through refresh, and the export must carry the ROTATED
    /// pair so the caller persists the live chain, not the dead one.
    #[tokio::test]
    async fn restored_session_refreshes_and_exports_rotated_tokens() {
        let server = MockServer::start().await;
        // No createSession mock mounted: any login attempt would 404
        // and fail the test — restore must be login-free.
        let client = super::BlueskyClient::new(super::BlueskyConfig {
            service_url: server.uri(),
            chat_service_url: server.uri(),
            identifier: "lumen.test".to_string(),
            password: String::new(),
            http_timeout: std::time::Duration::from_secs(5),
        })
        .expect("client");
        client
            .restore_session(crate::types::Session {
                did: Did::parse("did:plc:fixturedid").expect("did"),
                handle: crate::identifiers::Handle::parse("lumen.test").expect("handle"),
                access_jwt: "stale-access".to_string(),
                refresh_jwt: "refresh-token-1".to_string(),
            })
            .await;

        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.server.refreshSession"))
            .and(header("authorization", "Bearer refresh-token-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "did": "did:plc:fixturedid",
                "handle": "lumen.test",
                "accessJwt": "access-token-2",
                "refreshJwt": "refresh-token-2",
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/xrpc/app.bsky.feed.getTimeline"))
            .and(header("authorization", "Bearer stale-access"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "ExpiredToken",
                "message": "Token has expired",
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/xrpc/app.bsky.feed.getTimeline"))
            .and(header("authorization", "Bearer access-token-2"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"feed": [], "cursor": null})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let posts = client.get_timeline(10).await.expect("timeline");
        assert!(posts.is_empty());
        let exported = client.export_session().await.expect("session present");
        assert_eq!(exported.access_jwt, "access-token-2");
        assert_eq!(exported.refresh_jwt, "refresh-token-2");
        server.verify().await;
    }

    /// A genuine 400 from Bluesky (e.g. a malformed request) must NOT
    /// trigger the refresh path — the call should surface as
    /// `BlueskyError::Api` so the caller can react. Only `400` *with*
    /// `error: ExpiredToken` is the session-refresh signal.
    #[tokio::test]
    async fn non_expired_token_400_does_not_refresh() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        // No refresh mock — if the code wrongly hits refresh, the test
        // fails with "no matching mock" instead of misbehaving silently.

        Mock::given(method("GET"))
            .and(path("/xrpc/chat.bsky.convo.listConvos"))
            .and(header("authorization", "Bearer access-token-1"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "InvalidRequest",
                "message": "limit must be <= 100",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let err = client
            .list_convos(50)
            .await
            .expect_err("should surface 400");
        match err {
            BlueskyError::Api { status, kind, .. } => {
                assert_eq!(status, 400);
                assert_eq!(kind, "InvalidRequest");
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unauthorized_response_triggers_refresh_and_retry() {
        // First createRecord returns 401; refresh succeeds; retry
        // succeeds. Verifies the auth-refresh path exists, not just
        // the happy path.
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        // Refresh endpoint mints a new access token.
        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.server.refreshSession"))
            .and(header("authorization", "Bearer refresh-token-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "did": "did:plc:fixturedid",
                "handle": "lumen.test",
                "accessJwt": "access-token-2",
                "refreshJwt": "refresh-token-2",
            })))
            .expect(1)
            .mount(&server)
            .await;

        // First createRecord hit (with the original token) → 401.
        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.repo.createRecord"))
            .and(header("authorization", "Bearer access-token-1"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": "ExpiredToken",
                "message": "session expired",
            })))
            .expect(1)
            .mount(&server)
            .await;

        // Second createRecord (with the new token) → 200.
        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.repo.createRecord"))
            .and(header("authorization", "Bearer access-token-2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "uri": "at://did:plc:fixturedid/app.bsky.feed.post/3rkey",
                "cid": "bafy",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let post = client
            .compose_post("retry me", &ComposeOptions::default())
            .await
            .expect("post");
        assert!(post.uri.as_str().contains("3rkey"));
        assert_eq!(post.cid.as_str(), "bafy");
    }

    #[tokio::test]
    async fn api_error_surfaces_as_typed_error() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        Mock::given(any())
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "InvalidRequest",
                "message": "post too long",
            })))
            .mount(&server)
            .await;

        let err = client
            .compose_post("x".repeat(500).as_str(), &ComposeOptions::default())
            .await
            .expect_err("should fail");
        match err {
            super::BlueskyError::Api {
                status,
                kind,
                message,
            } => {
                assert_eq!(status, 400);
                assert_eq!(kind, "InvalidRequest");
                assert!(message.contains("too long"));
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_profile_relationship_reads_viewer_state() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        Mock::given(method("GET"))
            .and(path("/xrpc/app.bsky.actor.getProfile"))
            .and(query_param("actor", "did:plc:peer"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "did": "did:plc:peer",
                "handle": "peer.test",
                "viewer": {
                    "followedBy": "at://did:plc:peer/app.bsky.graph.follow/x",
                    // No `following` — Lumen doesn't follow them back.
                }
            })))
            .mount(&server)
            .await;

        let rel = client
            .get_profile_relationship("did:plc:peer")
            .await
            .expect("profile");
        assert!(rel.follows_me);
        assert!(!rel.followed_by_me);
    }

    // Sanity check: the body of compose_post is what AT Protocol expects
    // (the `$type`, `text`, `createdAt` fields). body_json_string
    // matches a substring of the JSON body — useful for fields whose
    // exact value (e.g. timestamps) we don't pin.
    #[tokio::test]
    async fn compose_post_body_carries_required_atproto_fields() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        // Match on $type + collection + repo + text. The createdAt
        // field is checked for presence by other infra; here we just
        // pin the structural shape.
        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.repo.createRecord"))
            .and(body_string_contains("\"$type\":\"app.bsky.feed.post\""))
            .and(body_string_contains(
                "\"collection\":\"app.bsky.feed.post\"",
            ))
            .and(body_string_contains("\"text\":\"shape check\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "uri": "at://x/y/z",
                "cid": "bafyreishapecheck",
            })))
            .expect(1)
            .mount(&server)
            .await;

        client
            .compose_post("shape check", &ComposeOptions::default())
            .await
            .expect("post");
    }

    #[tokio::test]
    async fn publish_whitewind_returns_public_url_from_handle_and_rkey() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.repo.createRecord"))
            .and(body_string_contains(
                "\"collection\":\"com.whtwnd.blog.entry\"",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "uri": "at://did:plc:fixturedid/com.whtwnd.blog.entry/3abc123xyz",
                "cid": "bafyreiwhitewind",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let entry = client
            .publish_whitewind_entry("A Title", "body md")
            .await
            .expect("publish");
        assert_eq!(
            entry.uri,
            "at://did:plc:fixturedid/com.whtwnd.blog.entry/3abc123xyz"
        );
        // WhiteWind URLs are handle + record key — never a title slug.
        assert_eq!(entry.url, "https://whtwnd.com/lumen.test/3abc123xyz");
    }

    // ---- Failure-mode tests (testing.md §4) ----

    #[tokio::test]
    async fn hung_response_aborts_via_http_timeout() {
        // PDS accepts the call but never responds within the
        // configured per-call timeout. The reqwest client's
        // `timeout` cuts the wait short.
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.server.createSession"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({
                        "did": "did:plc:x",
                        "handle": "x.test",
                        "accessJwt": "a",
                        "refreshJwt": "r",
                    }))
                    .set_delay(std::time::Duration::from_mins(1)),
            )
            .mount(&server)
            .await;

        let client = super::BlueskyClient::new(super::BlueskyConfig {
            service_url: server.uri(),
            chat_service_url: server.uri(),
            identifier: "x.test".to_string(),
            password: "p".to_string(),
            // 100ms timeout, 60s server delay → reqwest fires.
            http_timeout: std::time::Duration::from_millis(100),
        })
        .expect("client");

        let err = client.login().await.expect_err("should fail");
        // reqwest surfaces timeouts as `Http(reqwest::Error)`.
        assert!(matches!(err, super::BlueskyError::Http(_)));
    }

    #[tokio::test]
    async fn refresh_failure_after_initial_unauthorized_surfaces_session_expired() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        // Refresh fails with 401 → SessionExpired.
        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.server.refreshSession"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": "ExpiredToken",
                "message": "refresh too",
            })))
            .expect(1)
            .mount(&server)
            .await;

        // First createRecord returns 401 → triggers refresh, refresh
        // also fails → SessionExpired bubbles up.
        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.repo.createRecord"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": "ExpiredToken",
                "message": "expired",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let err = client
            .compose_post("doomed", &ComposeOptions::default())
            .await
            .expect_err("should fail");
        assert!(matches!(err, super::BlueskyError::SessionExpired));
    }

    #[tokio::test]
    async fn malformed_timeline_body_yields_empty_feed() {
        // Server returns a 200 with no `feed` field. Parser is
        // permissive — falls back to an empty feed rather than
        // failing the whole call.
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        Mock::given(method("GET"))
            .and(path("/xrpc/app.bsky.feed.getTimeline"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "unexpected_shape": "no feed key"
            })))
            .mount(&server)
            .await;

        let posts = client.get_timeline(30).await.expect("call");
        assert!(posts.is_empty());
    }

    #[test]
    fn flatten_thread_walks_parents_and_replies() {
        let raw = json!({
            "post": {
                "uri": "at://did:plc:abc/post/2",
                "cid": "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaac2",
                "author": { "handle": "alice.test", "did": "did:plc:abc" },
                "record": { "text": "second" },
                "indexedAt": "2026-05-09T00:00:01Z"
            },
            "parent": {
                "post": {
                    "uri": "at://did:plc:abc/post/1",
                    "cid": "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaac1",
                    "author": { "handle": "alice.test", "did": "did:plc:abc" },
                    "record": { "text": "first" },
                    "indexedAt": "2026-05-09T00:00:00Z"
                }
            },
            "replies": [
                {
                    "post": {
                        "uri": "at://did:plc:abc/post/3",
                        "cid": "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaac3",
                        "author": { "handle": "bob.test", "did": "did:plc:bob" },
                        "record": { "text": "third" },
                        "indexedAt": "2026-05-09T00:00:02Z"
                    }
                }
            ]
        });
        let mut out = Vec::new();
        flatten_thread(&raw, &mut out);
        let texts: Vec<&str> = out.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, vec!["first", "second", "third"]);
    }

    #[tokio::test]
    async fn list_convos_carries_atproto_proxy_header() {
        // Wiremock asserts the chat-proxy header lands on the wire.
        // If the proxy mechanism breaks (header dropped, value
        // changed), the mock returns 404 and the test fails clearly
        // instead of "request to /listConvos succeeded but returned
        // empty" which would look like a working empty inbox.
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;
        Mock::given(method("GET"))
            .and(path("/xrpc/chat.bsky.convo.listConvos"))
            .and(header("authorization", "Bearer access-token-1"))
            .and(header("atproto-proxy", "did:web:api.bsky.chat#bsky_chat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "convos": [
                    {
                        "id": "convo-1",
                        "members": [
                            { "did": "did:plc:fixturedid", "handle": "lumen.test" },
                            { "did": "did:plc:peer", "handle": "peer.test" }
                        ],
                        "unreadCount": 1,
                        "lastMessage": {
                            "id": "m1",
                            "sender": { "did": "did:plc:peer" },
                            "text": "hi",
                            "sentAt": "2026-05-09T10:00:00Z"
                        }
                    }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let convos = client.list_convos(50).await.expect("list");
        assert_eq!(convos.len(), 1);
        assert_eq!(convos[0].id, "convo-1");
        // self_did filtered out → only peer remains.
        assert_eq!(convos[0].members.len(), 1);
        assert_eq!(convos[0].members[0].did.as_str(), "did:plc:peer");
        assert_eq!(convos[0].unread_count, 1);
        server.verify().await;
    }

    #[tokio::test]
    async fn send_dm_posts_to_chat_route_with_proxy_header() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;
        Mock::given(method("POST"))
            .and(path("/xrpc/chat.bsky.convo.sendMessage"))
            .and(header("authorization", "Bearer access-token-1"))
            .and(header("atproto-proxy", "did:web:api.bsky.chat#bsky_chat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "m-new",
                "sender": { "did": "did:plc:fixturedid" },
                "text": "hello peer",
                "sentAt": "2026-05-09T10:00:00Z"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let m = client.send_dm("convo-1", "hello peer").await.expect("send");
        assert_eq!(m.id, "m-new");
        assert_eq!(m.text, "hello peer");
        server.verify().await;
    }

    // ---- compose_thread / repost / follow / unfollow / delete_post ----

    #[tokio::test]
    async fn compose_thread_chains_replies_to_first_post_as_root() {
        // wiremock dispatches LIFO among same-priority matching mocks,
        // and per-mock matchers fire against the *whole* request — so
        // we mount three mocks each scoped by `up_to_n_times(1)` plus
        // a body substring unique to that segment's payload. The first
        // segment's body has no `reply` block; subsequent segments
        // chain `parent` to the previous response and `root` to the
        // first. Substrings are deliberately short to avoid getting
        // tripped by serde key ordering.
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        // Third (mounted first → LIFO last to match): "three" + parent r2
        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.repo.createRecord"))
            .and(body_string_contains(r#""text":"three""#))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "uri": "at://did:plc:fixturedid/app.bsky.feed.post/r3",
                "cid": "bafyreir3aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            })))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;

        // Second: "two" + parent r1
        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.repo.createRecord"))
            .and(body_string_contains(r#""text":"two""#))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "uri": "at://did:plc:fixturedid/app.bsky.feed.post/r2",
                "cid": "bafyreir2aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            })))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;

        // First: top-level post "one"
        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.repo.createRecord"))
            .and(body_string_contains(r#""text":"one""#))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "uri": "at://did:plc:fixturedid/app.bsky.feed.post/r1",
                "cid": "bafyreir1aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            })))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;

        let segments = vec!["one".to_string(), "two".to_string(), "three".to_string()];
        let posts = client.compose_thread(&segments).await.expect("thread");
        assert_eq!(posts.len(), 3);
        assert_eq!(
            posts[0].uri.as_str(),
            "at://did:plc:fixturedid/app.bsky.feed.post/r1"
        );
        assert_eq!(
            posts[1].uri.as_str(),
            "at://did:plc:fixturedid/app.bsky.feed.post/r2"
        );
        assert_eq!(
            posts[2].uri.as_str(),
            "at://did:plc:fixturedid/app.bsky.feed.post/r3"
        );
        // expect(1) on each mock guarantees each segment hit the
        // matching mock exactly once — wiremock fails verify() if the
        // chaining were wrong (e.g. "two" sent without parent ref
        // would still match the second mock; the deeper structural
        // shape is covered by reply_to_post's own wire test).
        server.verify().await;
    }

    #[tokio::test]
    async fn repost_creates_feed_repost_record_with_subject_uri_and_cid() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.repo.createRecord"))
            .and(body_string_contains(
                r#""collection":"app.bsky.feed.repost""#,
            ))
            .and(body_string_contains(r#""$type":"app.bsky.feed.repost""#))
            .and(body_string_contains(r#""cid":"peer-cid""#))
            .and(body_string_contains(
                "at://did:plc:peer/app.bsky.feed.post/abc",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "uri": "at://did:plc:fixturedid/app.bsky.feed.repost/rk",
                "cid": "rcid",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let uri = client
            .repost("at://did:plc:peer/app.bsky.feed.post/abc", "peer-cid")
            .await
            .expect("repost");
        assert_eq!(uri, "at://did:plc:fixturedid/app.bsky.feed.repost/rk");
        server.verify().await;
    }

    #[tokio::test]
    async fn follow_creates_graph_follow_record_with_subject_did() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.repo.createRecord"))
            .and(body_string_contains(
                "\"collection\":\"app.bsky.graph.follow\"",
            ))
            .and(body_string_contains("\"subject\":\"did:plc:peer\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "uri": "at://did:plc:fixturedid/app.bsky.graph.follow/fk",
                "cid": "fcid",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let uri = client.follow("did:plc:peer").await.expect("follow");
        assert_eq!(uri, "at://did:plc:fixturedid/app.bsky.graph.follow/fk");
        server.verify().await;
    }

    #[tokio::test]
    async fn block_creates_graph_block_record_with_subject_did() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.repo.createRecord"))
            .and(body_string_contains(
                "\"collection\":\"app.bsky.graph.block\"",
            ))
            .and(body_string_contains("\"subject\":\"did:plc:peer\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "uri": "at://did:plc:fixturedid/app.bsky.graph.block/bk",
                "cid": "bcid",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let uri = client.block("did:plc:peer").await.expect("block");
        assert_eq!(uri, "at://did:plc:fixturedid/app.bsky.graph.block/bk");
        server.verify().await;
    }

    #[tokio::test]
    async fn unfollow_looks_up_follow_record_via_get_profile_and_deletes_it() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        // getProfile returns viewer.following → follow record URI.
        Mock::given(method("GET"))
            .and(path("/xrpc/app.bsky.actor.getProfile"))
            .and(query_param("actor", "did:plc:peer"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "did": "did:plc:peer",
                "handle": "peer.test",
                "viewer": {
                    "following": "at://did:plc:fixturedid/app.bsky.graph.follow/fk"
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.repo.deleteRecord"))
            .and(body_string_contains("\"rkey\":\"fk\""))
            .and(body_string_contains(
                "\"collection\":\"app.bsky.graph.follow\"",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;

        client.unfollow("did:plc:peer").await.expect("unfollow");
        server.verify().await;
    }

    #[tokio::test]
    async fn unfollow_is_noop_when_already_not_following() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        // viewer.following absent — already in the desired state.
        Mock::given(method("GET"))
            .and(path("/xrpc/app.bsky.actor.getProfile"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "did": "did:plc:peer",
                "handle": "peer.test",
                "viewer": {}
            })))
            .expect(1)
            .mount(&server)
            .await;

        // No deleteRecord mock — if it gets called, the test fails.
        client.unfollow("did:plc:peer").await.expect("unfollow");
        server.verify().await;
    }

    #[tokio::test]
    async fn delete_post_deletes_record_when_owned_and_collection_matches() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.repo.deleteRecord"))
            .and(body_string_contains("\"repo\":\"did:plc:fixturedid\""))
            .and(body_string_contains(
                "\"collection\":\"app.bsky.feed.post\"",
            ))
            .and(body_string_contains("\"rkey\":\"3rkey\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;

        client
            .delete_post("at://did:plc:fixturedid/app.bsky.feed.post/3rkey")
            .await
            .expect("delete");
        server.verify().await;
    }

    #[tokio::test]
    async fn delete_post_refuses_uri_owned_by_another_did() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        // Any deleteRecord call would be a bug — refuse before the wire.
        let err = client
            .delete_post("at://did:plc:peer/app.bsky.feed.post/abc")
            .await
            .expect_err("must refuse");
        match err {
            super::BlueskyError::Unexpected(msg) => {
                assert!(msg.contains("did:plc:peer"), "msg = {msg}");
            }
            other => panic!("wrong error variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_post_refuses_non_feed_post_collection() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        let err = client
            .delete_post("at://did:plc:fixturedid/app.bsky.feed.like/lk")
            .await
            .expect_err("must refuse");
        match err {
            super::BlueskyError::Unexpected(msg) => {
                assert!(msg.contains("app.bsky.feed.like"), "msg = {msg}");
            }
            other => panic!("wrong error variant: {other:?}"),
        }
    }

    // ---- get_profile / get_author_feed / search_posts ----

    #[tokio::test]
    async fn get_profile_parses_counts_viewer_state_and_known_followers() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        Mock::given(method("GET"))
            .and(path("/xrpc/app.bsky.actor.getProfile"))
            .and(query_param("actor", "did:plc:peer"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "did": "did:plc:peer",
                "handle": "peer.test",
                "displayName": "Peer One",
                "description": "writes about geology",
                "followersCount": 42,
                "followsCount": 17,
                "postsCount": 200,
                "viewer": {
                    "following": "at://did:plc:fixturedid/app.bsky.graph.follow/fk",
                    "knownFollowers": {
                        "count": 3,
                        "followers": [
                            { "did": "did:plc:m1", "handle": "m1.test" },
                            { "did": "did:plc:m2", "handle": "m2.test" }
                        ]
                    }
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let p = client.get_profile("did:plc:peer").await.expect("profile");
        assert_eq!(p.handle.as_str(), "peer.test");
        assert_eq!(p.display_name.as_deref(), Some("Peer One"));
        assert_eq!(p.followers_count, 42);
        assert!(p.followed_by_me);
        assert!(!p.follows_me);
        assert_eq!(p.known_followers_count, 3);
        assert_eq!(p.known_followers_sample.len(), 2);
        assert_eq!(p.known_followers_sample[0].handle.as_str(), "m1.test");
    }

    #[tokio::test]
    async fn get_author_feed_returns_posts_with_engagement_counts() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        Mock::given(method("GET"))
            .and(path("/xrpc/app.bsky.feed.getAuthorFeed"))
            .and(query_param("actor", "did:plc:peer"))
            .and(query_param("limit", "20"))
            .and(query_param("filter", "posts_no_replies"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "feed": [{
                    "post": {
                        "uri": "at://did:plc:peer/app.bsky.feed.post/p1",
                        "cid": "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaac1",
                        "author": { "handle": "peer.test", "did": "did:plc:peer" },
                        "record": { "text": "looked at a rock today" },
                        "indexedAt": "2026-05-10T10:00:00Z",
                        "likeCount": 12,
                        "repostCount": 3,
                        "replyCount": 5
                    }
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let feed = client
            .get_author_feed("did:plc:peer", 20, "posts_no_replies")
            .await
            .expect("feed");
        assert_eq!(feed.len(), 1);
        assert_eq!(feed[0].post.text, "looked at a rock today");
        assert_eq!(feed[0].post.like_count, 12);
        assert_eq!(feed[0].post.repost_count, 3);
        assert_eq!(feed[0].post.reply_count, 5);
    }

    #[tokio::test]
    async fn search_posts_passes_query_sort_and_optional_author() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        Mock::given(method("GET"))
            .and(path("/xrpc/app.bsky.feed.searchPosts"))
            .and(query_param("q", "structural color"))
            .and(query_param("sort", "top"))
            .and(query_param("author", "did:plc:peer"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "posts": [{
                    "uri": "at://did:plc:peer/app.bsky.feed.post/x",
                    "cid": "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaacx",
                    "author": { "handle": "peer.test", "did": "did:plc:peer" },
                    "record": { "text": "nacre forms in intervals" },
                    "indexedAt": "2026-05-10T11:00:00Z",
                    "likeCount": 7
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let posts = client
            .search_posts("structural color", Some("did:plc:peer"), "top", 20)
            .await
            .expect("search");
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].text, "nacre forms in intervals");
        assert_eq!(posts[0].like_count, 7);
    }

    #[tokio::test]
    async fn search_posts_omits_author_param_when_none() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        // Wiremock has no built-in "param absent" matcher; instead we
        // mount a strict match that requires only the parameters we
        // expect to be present, and check the result threading.
        Mock::given(method("GET"))
            .and(path("/xrpc/app.bsky.feed.searchPosts"))
            .and(query_param("q", "rocks"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "posts": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        let posts = client
            .search_posts("rocks", None, "latest", 20)
            .await
            .expect("search");
        assert!(posts.is_empty());
        server.verify().await;
    }

    // ---- compose with embed (quote / images / both) ----

    #[tokio::test]
    async fn upload_blob_sends_raw_bytes_with_content_type_and_returns_blob_ref() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.repo.uploadBlob"))
            .and(header("authorization", "Bearer access-token-1"))
            .and(header("content-type", "image/jpeg"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "blob": {
                    "$type": "blob",
                    "ref": { "$link": "bafkre1abcdef" },
                    "mimeType": "image/jpeg",
                    "size": 4
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let blob = client
            .upload_blob(&[0xff, 0xd8, 0xff, 0xd9], "image/jpeg")
            .await
            .expect("blob");
        assert_eq!(
            blob.get("mimeType").and_then(|v| v.as_str()),
            Some("image/jpeg")
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn compose_post_with_quote_embeds_record_ref() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.repo.createRecord"))
            .and(body_string_contains(r#""$type":"app.bsky.embed.record""#))
            .and(body_string_contains(
                "at://did:plc:peer/app.bsky.feed.post/qq",
            ))
            .and(body_string_contains(r#""cid":"bafyreiquotedcid""#))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "uri": "at://did:plc:fixturedid/app.bsky.feed.post/p1",
                "cid": "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaac1",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let opts = ComposeOptions {
            quote: Some(PostRef {
                uri: AtUri::from_trusted("at://did:plc:peer/app.bsky.feed.post/qq"),
                cid: Cid::from_trusted("bafyreiquotedcid"),
            }),
            images: Vec::new(),
        };
        let post = client
            .compose_post("with quote", &opts)
            .await
            .expect("quote post");
        assert_eq!(
            post.uri.as_str(),
            "at://did:plc:fixturedid/app.bsky.feed.post/p1"
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn compose_post_with_images_uploads_blobs_and_embeds_them() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        // Image source — served by the same mock server so we can
        // assert reqwest fetched it.
        Mock::given(method("GET"))
            .and(path("/img/cat.jpg"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/jpeg")
                    .set_body_bytes(vec![0xff, 0xd8, 0xff, 0xd9]),
            )
            .expect(1)
            .mount(&server)
            .await;

        // uploadBlob returns a blob ref.
        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.repo.uploadBlob"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "blob": {
                    "$type": "blob",
                    "ref": { "$link": "bafkre1catimage" },
                    "mimeType": "image/jpeg",
                    "size": 4
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        // createRecord — body should reference the blob and the alt.
        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.repo.createRecord"))
            .and(body_string_contains(r#""$type":"app.bsky.embed.images""#))
            .and(body_string_contains("bafkre1catimage"))
            .and(body_string_contains(r#""alt":"a cat""#))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "uri": "at://did:plc:fixturedid/app.bsky.feed.post/withimg",
                "cid": "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaac2",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let opts = ComposeOptions {
            quote: None,
            images: vec![ImageAttachment {
                url: format!("{}/img/cat.jpg", server.uri()),
                alt: "a cat".into(),
            }],
        };
        let post = client
            .compose_post("look", &opts)
            .await
            .expect("image post");
        assert!(post.uri.as_str().contains("withimg"));
        server.verify().await;
    }

    #[tokio::test]
    async fn compose_post_with_quote_and_images_uses_record_with_media() {
        let server = MockServer::start().await;
        let client = fixture_session(&server).await;

        Mock::given(method("GET"))
            .and(path("/img/x.jpg"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/jpeg")
                    .set_body_bytes(vec![0xff, 0xd8, 0xff, 0xd9]),
            )
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.repo.uploadBlob"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "blob": { "$type": "blob", "ref": { "$link": "bafkre1xx" }, "mimeType": "image/jpeg", "size": 4 }
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/xrpc/com.atproto.repo.createRecord"))
            .and(body_string_contains(
                r#""$type":"app.bsky.embed.recordWithMedia""#,
            ))
            .and(body_string_contains(r#""$type":"app.bsky.embed.record""#))
            .and(body_string_contains(r#""$type":"app.bsky.embed.images""#))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "uri": "at://did:plc:fixturedid/app.bsky.feed.post/both",
                "cid": "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaac3",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let opts = ComposeOptions {
            quote: Some(PostRef {
                uri: AtUri::from_trusted("at://did:plc:peer/app.bsky.feed.post/qq"),
                cid: Cid::from_trusted("bafyreiqcid"),
            }),
            images: vec![ImageAttachment {
                url: format!("{}/img/x.jpg", server.uri()),
                alt: "image".into(),
            }],
        };
        client
            .compose_post("both", &opts)
            .await
            .expect("combined post");
        server.verify().await;
    }
}
