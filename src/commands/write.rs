//! Write commands: posting (with facets, images, video, quotes,
//! gates), engagement records, and graph mutations.

use std::collections::HashMap;

use anyhow::Context as _;
use serde_json::{Value, json};

use super::Ctx;
use super::util::text_or_stdin;
use crate::api::{ApiError, Client, Route};
use crate::cli::ComposeArgs;
use crate::facets::{self, FacetFeature};
use crate::identifiers::AtUri;

/// Bluesky post text limits (lexicon-verified 2026-08-11).
const MAX_GRAPHEMES: usize = 300;
const MAX_BYTES: usize = 3000;
/// Image blob limit per the current lexicon (2 MB, not the older
/// ~1 MB figure); we re-encode toward this with headroom.
const MAX_IMAGE_BYTES: usize = 2_000_000;

pub async fn post(
    ctx: &Ctx,
    text: &str,
    quote: Option<&str>,
    compose: &ComposeArgs,
) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let text = text_or_stdin(text)?;
    let record = build_post_record(&client, &text, quote, compose, None).await?;
    let created = create_record(&client, "app.bsky.feed.post", &record, None).await?;
    apply_gates(&client, &created, compose).await?;
    emit_created(ctx, &created);
    Ok(())
}

pub async fn reply(
    ctx: &Ctx,
    parent: &str,
    text: &str,
    compose: &ComposeArgs,
) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let text = text_or_stdin(text)?;
    let parent_uri = client.resolve_post_uri(parent).await?;
    let refs = client.reply_refs(&parent_uri).await?;
    let reply = json!({
        "root": { "uri": refs.root.uri.as_str(), "cid": refs.root.cid.as_str() },
        "parent": { "uri": refs.parent.uri.as_str(), "cid": refs.parent.cid.as_str() },
    });
    let record = build_post_record(&client, &text, None, compose, Some(reply)).await?;
    let created = create_record(&client, "app.bsky.feed.post", &record, None).await?;
    emit_created(ctx, &created);
    Ok(())
}

pub async fn thread(ctx: &Ctx, texts: &[String]) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let compose = ComposeArgs {
        image: vec![],
        alt: vec![],
        video: None,
        video_alt: None,
        lang: vec![],
        reply_gate: vec![],
        no_quotes: false,
    };
    let mut root: Option<Value> = None;
    let mut parent: Option<Value> = None;
    for text in texts {
        let text = text_or_stdin(text)?;
        let reply = match (&root, &parent) {
            (Some(root), Some(parent)) => Some(json!({
                "root": { "uri": root["uri"], "cid": root["cid"] },
                "parent": { "uri": parent["uri"], "cid": parent["cid"] },
            })),
            _ => None,
        };
        let record = build_post_record(&client, &text, None, &compose, reply).await?;
        let created = create_record(&client, "app.bsky.feed.post", &record, None).await?;
        emit_created(ctx, &created);
        if root.is_none() {
            root = Some(created.clone());
        }
        parent = Some(created);
    }
    Ok(())
}

async fn build_post_record(
    client: &Client,
    text: &str,
    quote: Option<&str>,
    compose: &ComposeArgs,
    reply: Option<Value>,
) -> anyhow::Result<Value> {
    let graphemes = facets::grapheme_len(text);
    anyhow::ensure!(
        graphemes <= MAX_GRAPHEMES,
        "post is {graphemes} graphemes; the limit is {MAX_GRAPHEMES}"
    );
    anyhow::ensure!(
        text.len() <= MAX_BYTES,
        "post is {} bytes; the limit is {MAX_BYTES}",
        text.len()
    );

    let mut record = json!({
        "$type": "app.bsky.feed.post",
        "text": text,
        "createdAt": chrono::Utc::now().to_rfc3339(),
    });

    let wire_facets = detect_and_resolve_facets(client, text).await;
    if !wire_facets.is_empty() {
        record["facets"] = Value::Array(wire_facets);
    }
    if !compose.lang.is_empty() {
        record["langs"] = json!(compose.lang);
    }
    if let Some(reply) = reply {
        record["reply"] = reply;
    }
    if let Some(embed) = build_embed(client, quote, compose).await? {
        record["embed"] = embed;
    }
    Ok(record)
}

/// Detect facets, then resolve mention handles to DIDs. Unresolvable
/// mentions are dropped (official-client behavior), never an error.
async fn detect_and_resolve_facets(client: &Client, text: &str) -> Vec<Value> {
    let detected = facets::detect_facets(text);
    let mut resolutions: HashMap<String, Option<String>> = HashMap::new();
    for facet in &detected {
        let FacetFeature::Mention { handle } = &facet.feature else {
            continue;
        };
        if resolutions.contains_key(handle) {
            continue;
        }
        let did = client
            .resolve_actor(handle)
            .await
            .ok()
            .map(|did| did.as_str().to_string());
        resolutions.insert(handle.clone(), did);
    }
    facets::to_wire(&detected, |handle| {
        resolutions.get(handle).cloned().flatten()
    })
}

async fn build_embed(
    client: &Client,
    quote: Option<&str>,
    compose: &ComposeArgs,
) -> anyhow::Result<Option<Value>> {
    let media = build_media_embed(client, compose).await?;
    let quote_ref = match quote {
        Some(post) => {
            let uri = client.resolve_post_uri(post).await?;
            let record = client.record_ref(&uri).await?;
            Some(json!({ "uri": record.uri.as_str(), "cid": record.cid.as_str() }))
        }
        None => None,
    };
    Ok(match (quote_ref, media) {
        (Some(record), Some(media)) => Some(json!({
            "$type": "app.bsky.embed.recordWithMedia",
            "record": { "$type": "app.bsky.embed.record", "record": record },
            "media": media,
        })),
        (Some(record), None) => Some(json!({
            "$type": "app.bsky.embed.record",
            "record": record,
        })),
        (None, Some(media)) => Some(media),
        (None, None) => None,
    })
}

async fn build_media_embed(
    client: &Client,
    compose: &ComposeArgs,
) -> anyhow::Result<Option<Value>> {
    if let Some(video_path) = &compose.video {
        let embed = upload_video_embed(client, video_path, compose.video_alt.as_deref()).await?;
        return Ok(Some(embed));
    }
    if compose.image.is_empty() {
        return Ok(None);
    }
    anyhow::ensure!(
        compose.image.len() <= 4,
        "at most 4 images per post (got {})",
        compose.image.len()
    );
    let mut images = Vec::new();
    for (i, source) in compose.image.iter().enumerate() {
        let alt = compose.alt.get(i).cloned().unwrap_or_default();
        let (bytes, content_type) = load_image_source(source).await?;
        let prepared = prepare_image(bytes, &content_type)?;
        let blob = client
            .upload_blob(prepared.bytes, &prepared.content_type)
            .await?;
        let mut image = json!({ "alt": alt, "image": blob });
        if let Some((width, height)) = prepared.dimensions {
            image["aspectRatio"] = json!({ "width": width, "height": height });
        }
        images.push(image);
    }
    Ok(Some(
        json!({ "$type": "app.bsky.embed.images", "images": images }),
    ))
}

async fn load_image_source(source: &str) -> anyhow::Result<(Vec<u8>, String)> {
    if source.starts_with("http://") || source.starts_with("https://") {
        let resp = reqwest::get(source)
            .await
            .with_context(|| format!("fetching image {source}"))?
            .error_for_status()
            .with_context(|| format!("fetching image {source}"))?;
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("image/jpeg")
            .to_string();
        let bytes = resp.bytes().await?.to_vec();
        return Ok((bytes, content_type));
    }
    let bytes = std::fs::read(source).with_context(|| format!("reading image file {source}"))?;
    let content_type = guess_image_type(source, &bytes);
    Ok((bytes, content_type))
}

fn guess_image_type(path: &str, bytes: &[u8]) -> String {
    let by_magic = match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0xFF, 0xD8, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => Some("image/webp"),
        _ => None,
    };
    if let Some(t) = by_magic {
        return t.to_string();
    }
    let lower = path.to_lowercase();
    let by_ext = [
        (".png", "image/png"),
        (".jpg", "image/jpeg"),
        (".jpeg", "image/jpeg"),
        (".gif", "image/gif"),
        (".webp", "image/webp"),
    ]
    .iter()
    .find_map(|(ext, t)| lower.ends_with(ext).then_some(*t));
    by_ext.unwrap_or("image/jpeg").to_string()
}

struct PreparedImage {
    bytes: Vec<u8>,
    content_type: String,
    dimensions: Option<(u32, u32)>,
}

/// Decode for dimensions; re-encode (JPEG, shrinking) only when the
/// original is over the blob limit. Undecodable-but-small images pass
/// through untouched rather than failing the post.
fn prepare_image(bytes: Vec<u8>, content_type: &str) -> anyhow::Result<PreparedImage> {
    let decoded = image::load_from_memory(&bytes);
    match decoded {
        Err(_) if bytes.len() <= MAX_IMAGE_BYTES => Ok(PreparedImage {
            bytes,
            content_type: content_type.to_string(),
            dimensions: None,
        }),
        Err(e) => Err(e).context("image is over the 2MB limit and could not be re-encoded"),
        Ok(img) => {
            let dims = (img.width(), img.height());
            if bytes.len() <= MAX_IMAGE_BYTES {
                return Ok(PreparedImage {
                    bytes,
                    content_type: content_type.to_string(),
                    dimensions: Some(dims),
                });
            }
            let mut img = img;
            for _ in 0..6 {
                let mut buf = std::io::Cursor::new(Vec::new());
                img.write_to(&mut buf, image::ImageFormat::Jpeg)
                    .context("re-encoding image")?;
                let encoded = buf.into_inner();
                if encoded.len() <= MAX_IMAGE_BYTES {
                    return Ok(PreparedImage {
                        dimensions: Some((img.width(), img.height())),
                        bytes: encoded,
                        content_type: "image/jpeg".to_string(),
                    });
                }
                img = img.resize(
                    img.width() * 3 / 4,
                    img.height() * 3 / 4,
                    image::imageops::FilterType::Lanczos3,
                );
            }
            anyhow::bail!("could not shrink image under the 2MB limit")
        }
    }
}

async fn upload_video_embed(
    client: &Client,
    path: &std::path::Path,
    alt: Option<&str>,
) -> anyhow::Result<Value> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let name = path.file_name().map_or_else(
        || "video.mp4".to_string(),
        |n| n.to_string_lossy().to_string(),
    );
    let mut job = client.upload_video(bytes, &name).await?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    let blob = loop {
        if let Some(blob) = job.get("blob").filter(|b| !b.is_null()) {
            break blob.clone();
        }
        let state = job.get("state").and_then(Value::as_str).unwrap_or("");
        anyhow::ensure!(
            state != "JOB_STATE_FAILED",
            "video processing failed: {}",
            job.get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        );
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "video processing timed out after 300s (state: {state})"
        );
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let job_id = job
            .get("jobId")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::Unexpected("video job missing jobId".into()))?;
        job = client.video_job_status(job_id).await?;
    };
    let mut embed = json!({ "$type": "app.bsky.embed.video", "video": blob });
    if let Some(alt) = alt {
        embed["alt"] = json!(alt);
    }
    Ok(embed)
}

/// Write threadgate/postgate records for a just-created post. Their
/// rkey must equal the post's rkey.
async fn apply_gates(
    client: &Client,
    created: &Value,
    compose: &ComposeArgs,
) -> anyhow::Result<()> {
    let uri = created["uri"].as_str().unwrap_or_default();
    let Some(rkey) = uri.rsplit('/').next() else {
        return Ok(());
    };
    if !compose.reply_gate.is_empty() {
        let mut allow = Vec::new();
        for gate in &compose.reply_gate {
            match gate.as_str() {
                "nobody" => {}
                "mentioned" => {
                    allow.push(json!({ "$type": "app.bsky.feed.threadgate#mentionRule" }));
                }
                "following" => {
                    allow.push(json!({ "$type": "app.bsky.feed.threadgate#followingRule" }));
                }
                "followers" => {
                    allow.push(json!({ "$type": "app.bsky.feed.threadgate#followerRule" }));
                }
                other => {
                    let Some(list) = other.strip_prefix("list:") else {
                        anyhow::bail!(
                            "unknown --reply-gate value {other:?} (expected nobody, mentioned, \
                             following, followers, or list:<at-uri>)"
                        );
                    };
                    allow.push(
                        json!({ "$type": "app.bsky.feed.threadgate#listRule", "list": list }),
                    );
                }
            }
        }
        let record = json!({
            "$type": "app.bsky.feed.threadgate",
            "post": uri,
            "allow": allow,
            "createdAt": chrono::Utc::now().to_rfc3339(),
        });
        create_record(client, "app.bsky.feed.threadgate", &record, Some(rkey)).await?;
    }
    if compose.no_quotes {
        let record = json!({
            "$type": "app.bsky.feed.postgate",
            "post": uri,
            "embeddingRules": [{ "$type": "app.bsky.feed.postgate#disableRule" }],
            "createdAt": chrono::Utc::now().to_rfc3339(),
        });
        create_record(client, "app.bsky.feed.postgate", &record, Some(rkey)).await?;
    }
    Ok(())
}

/// `createRecord` in the caller's repo; returns `{uri, cid}`.
async fn create_record(
    client: &Client,
    collection: &str,
    record: &Value,
    rkey: Option<&str>,
) -> anyhow::Result<Value> {
    let did = client.did().await;
    let mut body = json!({
        "repo": did.as_str(),
        "collection": collection,
        "record": record,
    });
    if let Some(rkey) = rkey {
        body["rkey"] = json!(rkey);
    }
    Ok(client
        .post(&Route::Pds, "com.atproto.repo.createRecord", &body)
        .await?)
}

fn emit_created(ctx: &Ctx, created: &Value) {
    ctx.out.object(created, |v| {
        v.get("uri")
            .and_then(Value::as_str)
            .unwrap_or("(created)")
            .to_string()
    });
}

pub async fn delete_post(ctx: &Ctx, post: &str) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let uri = client.resolve_post_uri(post).await?;
    delete_record_at(&client, &uri).await?;
    ctx.out.confirm("deleted");
    Ok(())
}

pub async fn like(ctx: &Ctx, post: &str) -> anyhow::Result<()> {
    subject_record(ctx, post, "app.bsky.feed.like", "liked").await
}

pub async fn repost(ctx: &Ctx, post: &str) -> anyhow::Result<()> {
    subject_record(ctx, post, "app.bsky.feed.repost", "reposted").await
}

/// Like and repost share a record shape: `subject: {uri, cid}`.
async fn subject_record(ctx: &Ctx, post: &str, collection: &str, verb: &str) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let uri = client.resolve_post_uri(post).await?;
    let subject = client.record_ref(&uri).await?;
    let record = json!({
        "$type": collection,
        "subject": { "uri": subject.uri.as_str(), "cid": subject.cid.as_str() },
        "createdAt": chrono::Utc::now().to_rfc3339(),
    });
    let created = create_record(&client, collection, &record, None).await?;
    ctx.out.object(&created, |_| verb.to_string());
    Ok(())
}

pub async fn unlike(ctx: &Ctx, post: &str) -> anyhow::Result<()> {
    undo_viewer_record(ctx, post, "like", "unliked").await
}

pub async fn unrepost(ctx: &Ctx, post: &str) -> anyhow::Result<()> {
    undo_viewer_record(ctx, post, "repost", "unreposted").await
}

/// The post view's `viewer` block carries the URI of *your* like or
/// repost record — no scan needed to undo.
async fn undo_viewer_record(
    ctx: &Ctx,
    post: &str,
    viewer_key: &str,
    verb: &str,
) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let uri = client.resolve_post_uri(post).await?;
    let view = client.get_post_view(&uri).await?;
    let Some(record_uri) = view
        .get("viewer")
        .and_then(|v| v.get(viewer_key))
        .and_then(Value::as_str)
    else {
        anyhow::bail!("you have no {viewer_key} on that post");
    };
    delete_record_at(&client, &AtUri::parse(record_uri)?).await?;
    ctx.out.confirm(verb);
    Ok(())
}

pub async fn follow(ctx: &Ctx, actor: &str) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let did = client.resolve_actor(actor).await?;
    let record = json!({
        "$type": "app.bsky.graph.follow",
        "subject": did.as_str(),
        "createdAt": chrono::Utc::now().to_rfc3339(),
    });
    let created = create_record(&client, "app.bsky.graph.follow", &record, None).await?;
    ctx.out.object(&created, |_| format!("followed {did}"));
    Ok(())
}

pub async fn block(ctx: &Ctx, actor: &str) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let did = client.resolve_actor(actor).await?;
    let record = json!({
        "$type": "app.bsky.graph.block",
        "subject": did.as_str(),
        "createdAt": chrono::Utc::now().to_rfc3339(),
    });
    let created = create_record(&client, "app.bsky.graph.block", &record, None).await?;
    ctx.out.object(&created, |_| format!("blocked {did}"));
    Ok(())
}

pub async fn unfollow(ctx: &Ctx, actor: &str) -> anyhow::Result<()> {
    undo_profile_record(ctx, actor, "following", "unfollowed").await
}

pub async fn unblock(ctx: &Ctx, actor: &str) -> anyhow::Result<()> {
    undo_profile_record(ctx, actor, "blocking", "unblocked").await
}

/// The profile view's `viewer` block carries the URI of your follow
/// or block record.
async fn undo_profile_record(
    ctx: &Ctx,
    actor: &str,
    viewer_key: &str,
    verb: &str,
) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let did = client.resolve_actor(actor).await?;
    let profile = client
        .get(
            &Route::Pds,
            "app.bsky.actor.getProfile",
            &[("actor", did.as_str().to_string())],
        )
        .await?;
    let Some(record_uri) = profile
        .get("viewer")
        .and_then(|v| v.get(viewer_key))
        .and_then(Value::as_str)
    else {
        anyhow::bail!("no {viewer_key} relationship with {actor}");
    };
    delete_record_at(&client, &AtUri::parse(record_uri)?).await?;
    ctx.out.confirm(verb);
    Ok(())
}

pub async fn mute(ctx: &Ctx, actor: &str) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let did = client.resolve_actor(actor).await?;
    client
        .post(
            &Route::Pds,
            "app.bsky.graph.muteActor",
            &json!({ "actor": did.as_str() }),
        )
        .await?;
    ctx.out.confirm(&format!("muted {did}"));
    Ok(())
}

pub async fn unmute(ctx: &Ctx, actor: &str) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let did = client.resolve_actor(actor).await?;
    client
        .post(
            &Route::Pds,
            "app.bsky.graph.unmuteActor",
            &json!({ "actor": did.as_str() }),
        )
        .await?;
    ctx.out.confirm(&format!("unmuted {did}"));
    Ok(())
}

/// Delete a record in the caller's repo by its at:// URI.
async fn delete_record_at(client: &Client, uri: &AtUri) -> anyhow::Result<()> {
    let rest = uri.as_str().strip_prefix("at://").unwrap_or(uri.as_str());
    let mut parts = rest.splitn(3, '/');
    let (Some(repo), Some(collection), Some(rkey)) = (parts.next(), parts.next(), parts.next())
    else {
        anyhow::bail!("malformed record URI: {uri}");
    };
    client
        .post(
            &Route::Pds,
            "com.atproto.repo.deleteRecord",
            &json!({ "repo": repo, "collection": collection, "rkey": rkey }),
        )
        .await?;
    Ok(())
}
