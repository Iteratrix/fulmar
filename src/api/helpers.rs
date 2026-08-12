//! Convenience resolution built on the raw client: actors to DIDs,
//! post references (bsky.app URLs, handle-authority AT URIs) to
//! canonical `at://did/...` form, and CID/reply-root lookup so
//! callers never have to supply a CID by hand.

use serde_json::Value;

use super::{ApiError, Client, Route};
use crate::identifiers::{AtUri, Cid, Did};

/// URI + CID pair for a specific record version — what reply, quote,
/// like, and repost need to reference their subject.
#[derive(Debug, Clone)]
pub struct RecordRef {
    pub uri: AtUri,
    pub cid: Cid,
}

/// Reply anchoring: the immediate parent and the thread root.
#[derive(Debug, Clone)]
pub struct ReplyRefs {
    pub root: RecordRef,
    pub parent: RecordRef,
}

impl Client {
    /// Resolve an actor argument — handle (leading `@` tolerated) or
    /// DID — to a DID.
    ///
    /// # Errors
    ///
    /// [`ApiError::Api`] when the handle doesn't resolve.
    pub async fn resolve_actor(&self, actor: &str) -> Result<Did, ApiError> {
        let actor = actor.trim().trim_start_matches('@');
        if actor.starts_with("did:") {
            return Ok(Did::parse(actor)?);
        }
        let value = self
            .get(
                &Route::Pds,
                "com.atproto.identity.resolveHandle",
                &[("handle", actor.to_string())],
            )
            .await?;
        let did = value
            .get("did")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::Unexpected("resolveHandle response missing did".into()))?;
        Ok(Did::parse(did)?)
    }

    /// Normalize a post argument to a canonical `at://did/...` URI.
    /// Accepts `at://` URIs (with DID or handle authority) and
    /// `https://bsky.app/profile/<actor>/post/<rkey>` URLs.
    ///
    /// # Errors
    ///
    /// [`ApiError::Unexpected`] for unrecognizable input;
    /// [`ApiError::Api`] when a handle fails to resolve.
    pub async fn resolve_post_uri(&self, input: &str) -> Result<AtUri, ApiError> {
        let input = input.trim();
        if let Some(rest) = input.strip_prefix("at://") {
            let mut parts = rest.splitn(3, '/');
            let authority = parts.next().unwrap_or_default();
            let collection = parts.next();
            let rkey = parts.next();
            if authority.starts_with("did:") {
                return Ok(AtUri::parse(input)?);
            }
            let did = self.resolve_actor(authority).await?;
            return match (collection, rkey) {
                (Some(collection), Some(rkey)) => Ok(AtUri::from_trusted(format!(
                    "at://{did}/{collection}/{rkey}"
                ))),
                _ => Err(ApiError::Unexpected(format!(
                    "AT URI missing collection/rkey: {input}"
                ))),
            };
        }
        if let Some(rest) = input
            .strip_prefix("https://bsky.app/profile/")
            .or_else(|| input.strip_prefix("https://staging.bsky.app/profile/"))
        {
            let mut parts = rest.split('/');
            let (Some(actor), Some("post"), Some(rkey)) =
                (parts.next(), parts.next(), parts.next())
            else {
                return Err(ApiError::Unexpected(format!(
                    "unrecognized bsky.app URL shape: {input}"
                )));
            };
            let rkey = rkey.split(['?', '#']).next().unwrap_or(rkey);
            let did = self.resolve_actor(actor).await?;
            return Ok(AtUri::from_trusted(format!(
                "at://{did}/app.bsky.feed.post/{rkey}"
            )));
        }
        Err(ApiError::Unexpected(format!(
            "expected an at:// URI or a bsky.app post URL, got: {input}"
        )))
    }

    /// Fetch a post's hydrated view (`getPostThread` depth 0 — just
    /// the head).
    ///
    /// # Errors
    ///
    /// [`ApiError::Api`] with `NotFound` when the post doesn't exist.
    pub async fn get_post_view(&self, uri: &AtUri) -> Result<Value, ApiError> {
        let value = self
            .get(
                &Route::Pds,
                "app.bsky.feed.getPostThread",
                &[
                    ("uri", uri.as_str().to_string()),
                    ("depth", "0".to_string()),
                    ("parentHeight", "0".to_string()),
                ],
            )
            .await?;
        value
            .get("thread")
            .and_then(|t| t.get("post"))
            .cloned()
            .ok_or_else(|| {
                ApiError::Unexpected("getPostThread response missing thread.post".into())
            })
    }

    /// Resolve a post's current URI + CID so the caller doesn't have
    /// to supply the CID.
    ///
    /// # Errors
    ///
    /// As [`Client::get_post_view`].
    pub async fn record_ref(&self, uri: &AtUri) -> Result<RecordRef, ApiError> {
        let post = self.get_post_view(uri).await?;
        record_ref_from_view(&post)
    }

    /// Resolve reply anchoring for replying to `parent_uri`: the
    /// parent's ref, and the thread root (from the parent's own reply
    /// record if it is itself a reply; the parent otherwise).
    ///
    /// # Errors
    ///
    /// As [`Client::get_post_view`].
    pub async fn reply_refs(&self, parent_uri: &AtUri) -> Result<ReplyRefs, ApiError> {
        let post = self.get_post_view(parent_uri).await?;
        let parent = record_ref_from_view(&post)?;
        let root = post
            .get("record")
            .and_then(|r| r.get("reply"))
            .and_then(|r| r.get("root"))
            .and_then(parse_ref)
            .unwrap_or_else(|| parent.clone());
        Ok(ReplyRefs { root, parent })
    }

    /// Resolve a DM target — an existing convo id, or an actor
    /// (handle/DID) whose 1:1 convo is fetched-or-created via
    /// `getConvoForMembers`.
    ///
    /// # Errors
    ///
    /// [`ApiError::Api`] when the actor doesn't resolve or DMs are
    /// not available for them.
    pub async fn resolve_convo(&self, target: &str) -> Result<String, ApiError> {
        let looks_like_actor =
            target.starts_with('@') || target.starts_with("did:") || target.contains('.');
        if !looks_like_actor {
            return Ok(target.to_string());
        }
        let did = self.resolve_actor(target).await?;
        let value = self
            .get(
                &Route::Chat,
                "chat.bsky.convo.getConvoForMembers",
                &[("members", did.as_str().to_string())],
            )
            .await?;
        value
            .get("convo")
            .and_then(|c| c.get("id"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .ok_or_else(|| {
                ApiError::Unexpected("getConvoForMembers response missing convo.id".into())
            })
    }
}

fn record_ref_from_view(post: &Value) -> Result<RecordRef, ApiError> {
    parse_ref(post).ok_or_else(|| ApiError::Unexpected("post view missing uri/cid".into()))
}

fn parse_ref(value: &Value) -> Option<RecordRef> {
    let uri = AtUri::parse(value.get("uri")?.as_str()?).ok()?;
    let cid = Cid::parse(value.get("cid")?.as_str()?).ok()?;
    Some(RecordRef { uri, cid })
}
