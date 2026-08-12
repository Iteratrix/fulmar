//! Notifications: list/count/seen, plus activity subscriptions
//! (watch/unwatch).

use serde_json::{Value, json};

use super::Ctx;
use super::util::{page_params, paginate, split_page};
use crate::api::Route;
use crate::cli::{NotifsCmd, PageArgs};
use crate::output::render_notification;

pub async fn notifs(
    ctx: &Ctx,
    reasons: &[String],
    unread_only: bool,
    page: &PageArgs,
    cmd: Option<NotifsCmd>,
) -> anyhow::Result<()> {
    match cmd {
        None => list(ctx, reasons, unread_only, page).await,
        Some(NotifsCmd::Count) => count(ctx).await,
        Some(NotifsCmd::Seen { at }) => seen(ctx, at.as_deref()).await,
    }
}

async fn list(
    ctx: &Ctx,
    reasons: &[String],
    unread_only: bool,
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
            let items = if unread_only {
                items
                    .into_iter()
                    .filter(|n| n.get("isRead").and_then(Value::as_bool) != Some(true))
                    .collect()
            } else {
                items
            };
            Ok((items, cursor))
        }
    })
    .await
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
