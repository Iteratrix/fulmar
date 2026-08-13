//! Notifications: list/count/seen, plus activity subscriptions
//! (watch/unwatch).

use std::collections::HashMap;

use serde_json::{Value, json};

use super::Ctx;
use super::util::{page_params, paginate, split_page};
use crate::api::{ApiError, Client, Route};
use crate::cli::{NotifsCmd, PageArgs};
use crate::output::render_notification;

pub async fn notifs(
    ctx: &Ctx,
    reasons: &[String],
    unread_only: bool,
    previews: bool,
    page: &PageArgs,
    cmd: Option<NotifsCmd>,
) -> anyhow::Result<()> {
    match cmd {
        None => list(ctx, reasons, unread_only, previews, page).await,
        Some(NotifsCmd::Count) => count(ctx).await,
        Some(NotifsCmd::Seen { at }) => seen(ctx, at.as_deref()).await,
    }
}

async fn list(
    ctx: &Ctx,
    reasons: &[String],
    unread_only: bool,
    previews: bool,
    page: &PageArgs,
) -> anyhow::Result<()> {
    let client = ctx.client()?;
    paginate(&ctx.out, page, render_notification, |cursor, limit| {
        let client = &client;
        async move {
            let mut params = page_params(cursor, limit);
            for reason in reasons {
                params.push(("reasons", reason.clone()));
            }
            let value = client
                .get(
                    &Route::Pds,
                    "app.bsky.notification.listNotifications",
                    &params,
                )
                .await?;
            let (items, cursor) = split_page(&value, "notifications");
            let mut items = if unread_only {
                items
                    .into_iter()
                    .filter(|n| n.get("isRead").and_then(Value::as_bool) != Some(true))
                    .collect()
            } else {
                items
            };
            if previews {
                hydrate_previews(client, &mut items).await?;
            }
            Ok((items, cursor))
        }
    })
    .await
}

/// Attach a synthetic `$preview` (dollar-prefixed: injected by
/// fulmar, never server data) carrying the subject post's text to
/// notifications that reference one via `reasonSubject` (like,
/// repost, reply, quote). One batched `getPosts` (25 URIs/call) per
/// page. Subjects that fail to hydrate (deleted posts) are simply
/// left bare.
async fn hydrate_previews(client: &Client, items: &mut [Value]) -> Result<(), ApiError> {
    let mut uris: Vec<String> = items
        .iter()
        .filter_map(|n| n.get("reasonSubject").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect();
    uris.sort();
    uris.dedup();
    if uris.is_empty() {
        return Ok(());
    }

    let mut posts: HashMap<String, Value> = HashMap::new();
    for chunk in uris.chunks(25) {
        let params: Vec<(&str, String)> = chunk.iter().map(|u| ("uris", u.clone())).collect();
        let value = client
            .get(&Route::Pds, "app.bsky.feed.getPosts", &params)
            .await?;
        let hydrated = value
            .get("posts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for post in hydrated {
            let Some(uri) = post.get("uri").and_then(Value::as_str) else {
                continue;
            };
            posts.insert(uri.to_string(), post);
        }
    }

    for item in items.iter_mut() {
        let Some(subject) = item.get("reasonSubject").and_then(Value::as_str) else {
            continue;
        };
        let Some(post) = posts.get(subject) else {
            continue;
        };
        let text = post
            .get("record")
            .and_then(|r| r.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("");
        item["$preview"] = json!({ "uri": subject, "text": text });
    }
    Ok(())
}

async fn count(ctx: &Ctx) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let value = client
        .get(&Route::Pds, "app.bsky.notification.getUnreadCount", &[])
        .await?;
    ctx.out.object(&value, |v| {
        v.get("count")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .to_string()
    });
    Ok(())
}

async fn seen(ctx: &Ctx, at: Option<&str>) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let seen_at = at.map_or_else(|| chrono::Utc::now().to_rfc3339(), ToString::to_string);
    client
        .post(
            &Route::Pds,
            "app.bsky.notification.updateSeen",
            &json!({ "seenAt": seen_at }),
        )
        .await?;
    ctx.out
        .confirm(&format!("notifications marked seen up to {seen_at}"));
    Ok(())
}

pub async fn watch(ctx: &Ctx, actor: &str, replies: bool) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let did = client.resolve_actor(actor).await?;
    client
        .post(
            &Route::Pds,
            "app.bsky.notification.putActivitySubscription",
            &json!({
                "subject": did.as_str(),
                "activitySubscription": { "post": true, "reply": replies },
            }),
        )
        .await?;
    ctx.out.confirm(&format!("watching {did}"));
    Ok(())
}

pub async fn unwatch(ctx: &Ctx, actor: &str) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let did = client.resolve_actor(actor).await?;
    client
        .post(
            &Route::Pds,
            "app.bsky.notification.putActivitySubscription",
            &json!({
                "subject": did.as_str(),
                "activitySubscription": { "post": false, "reply": false },
            }),
        )
        .await?;
    ctx.out.confirm(&format!("unwatched {did}"));
    Ok(())
}
