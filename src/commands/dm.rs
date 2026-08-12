//! Direct messages — the complete cycle: convos, history, send,
//! updateRead, the getLog polling primitive, requests, reactions.
//! Every call routes to the chat service (see `crate::api` module
//! docs for the 501 story).

use serde_json::{Value, json};

use super::Ctx;
use super::util::{page_params, paginate, split_page, text_or_stdin};
use crate::api::Route;
use crate::cli::DmCmd;
use crate::output::{render_convo, render_message, render_raw};

pub async fn dm(ctx: &Ctx, cmd: DmCmd) -> anyhow::Result<()> {
    match cmd {
        DmCmd::Convos {
            unread_only,
            status,
            page,
        } => convos(ctx, unread_only, status.as_deref(), &page).await,
        DmCmd::History { who, reverse, page } => history(ctx, &who, reverse, &page).await,
        DmCmd::Send { who, text } => send(ctx, &who, &text).await,
        DmCmd::Read { who, message, all } => {
            read(ctx, who.as_deref(), message.as_deref(), all).await
        }
        DmCmd::Log { page } => log(ctx, &page).await,
        DmCmd::Unread => unread(ctx).await,
        DmCmd::Requests { page } => requests(ctx, &page).await,
        DmCmd::Accept { convo } => accept(ctx, &convo).await,
        DmCmd::Leave { who } => leave(ctx, &who).await,
        DmCmd::Mute { who } => mute_convo(ctx, &who, true).await,
        DmCmd::Unmute { who } => mute_convo(ctx, &who, false).await,
        DmCmd::React {
            who,
            message,
            emoji,
            remove,
        } => react(ctx, &who, &message, &emoji, remove).await,
    }
}

async fn convos(
    ctx: &Ctx,
    unread_only: bool,
    status: Option<&str>,
    page: &crate::cli::PageArgs,
) -> anyhow::Result<()> {
    let client = ctx.client()?;
    paginate(&ctx.out, page, render_convo, |cursor, limit| {
        let client = &client;
        async move {
            let mut params = page_params(cursor, limit);
            if unread_only {
                params.push(("readState", "unread".to_string()));
            }
            if let Some(status) = status {
                params.push(("status", status.to_string()));
            }
            let value = client
                .get(&Route::Chat, "chat.bsky.convo.listConvos", &params)
                .await?;
            Ok(split_page(&value, "convos"))
        }
    })
    .await
}

async fn history(
    ctx: &Ctx,
    who: &str,
    reverse: bool,
    page: &crate::cli::PageArgs,
) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let convo_id = client.resolve_convo(who).await?;
    if reverse {
        // Collect (respecting --limit/--all), then flip to oldest
        // first — the wire order is newest-first. The resume cursor
        // stays the LAST line even though items were reordered.
        let mut collected = Vec::new();
        let mut cursor = page.cursor.clone();
        let resume = loop {
            let mut params = page_params(cursor.clone(), page.limit);
            params.push(("convoId", convo_id.clone()));
            let value = client
                .get(&Route::Chat, "chat.bsky.convo.getMessages", &params)
                .await?;
            let (items, next) = split_page(&value, "messages");
            let done = next.is_none() || items.is_empty() || !page.all;
            collected.extend(items);
            if done {
                break if page.all { None } else { next };
            }
            cursor = next;
        };
        for item in collected.iter().rev() {
            ctx.out.item(item, render_message);
        }
        ctx.out.cursor(resume.as_deref());
        return Ok(());
    }
    paginate(&ctx.out, page, render_message, |cursor, limit| {
        let client = &client;
        let convo_id = convo_id.clone();
        async move {
            let mut params = page_params(cursor, limit);
            params.push(("convoId", convo_id));
            let value = client
                .get(&Route::Chat, "chat.bsky.convo.getMessages", &params)
                .await?;
            Ok(split_page(&value, "messages"))
        }
    })
    .await
}

async fn send(ctx: &Ctx, who: &str, text: &str) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let text = text_or_stdin(text)?;
    let convo_id = client.resolve_convo(who).await?;
    let value = client
        .post(
            &Route::Chat,
            "chat.bsky.convo.sendMessage",
            &json!({ "convoId": convo_id, "message": { "text": text } }),
        )
        .await?;
    ctx.out.object(&value, |v| {
        let id = v.get("id").and_then(Value::as_str).unwrap_or("?");
        format!("sent ({id})")
    });
    Ok(())
}

async fn read(
    ctx: &Ctx,
    who: Option<&str>,
    message: Option<&str>,
    all: bool,
) -> anyhow::Result<()> {
    let client = ctx.client()?;
    if all {
        let value = client
            .post(&Route::Chat, "chat.bsky.convo.updateAllRead", &json!({}))
            .await?;
        ctx.out.object(&value, |v| {
            let n = v.get("updatedCount").and_then(Value::as_u64).unwrap_or(0);
            format!("{n} conversations marked read")
        });
        return Ok(());
    }
    let Some(who) = who else {
        anyhow::bail!("give a conversation (handle/DID/convo id) or --all");
    };
    let convo_id = client.resolve_convo(who).await?;
    let mut body = json!({ "convoId": convo_id });
    if let Some(message) = message {
        body["messageId"] = json!(message);
    }
    client
        .post(&Route::Chat, "chat.bsky.convo.updateRead", &body)
        .await?;
    ctx.out.confirm("marked read");
    Ok(())
}

async fn log(ctx: &Ctx, page: &crate::cli::PageArgs) -> anyhow::Result<()> {
    let client = ctx.client()?;
    paginate(&ctx.out, page, render_raw, |cursor, _limit| {
        let client = &client;
        async move {
            let mut params = Vec::new();
            if let Some(cursor) = cursor {
                params.push(("cursor", cursor));
            }
            let value = client
                .get(&Route::Chat, "chat.bsky.convo.getLog", &params)
                .await?;
            Ok(split_page(&value, "logs"))
        }
    })
    .await
}

async fn unread(ctx: &Ctx) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let value = client
        .get(&Route::Chat, "chat.bsky.convo.getUnreadCounts", &[])
        .await?;
    ctx.out.object(&value, render_raw);
    Ok(())
}

async fn requests(ctx: &Ctx, page: &crate::cli::PageArgs) -> anyhow::Result<()> {
    let client = ctx.client()?;
    paginate(&ctx.out, page, render_convo, |cursor, limit| {
        let client = &client;
        async move {
            let params = page_params(cursor, limit);
            let value = client
                .get(&Route::Chat, "chat.bsky.convo.listConvoRequests", &params)
                .await?;
            Ok(split_page(&value, "convos"))
        }
    })
    .await
}

async fn accept(ctx: &Ctx, convo: &str) -> anyhow::Result<()> {
    let client = ctx.client()?;
    client
        .post(
            &Route::Chat,
            "chat.bsky.convo.acceptConvo",
            &json!({ "convoId": convo }),
        )
        .await?;
    ctx.out.confirm("accepted");
    Ok(())
}

async fn leave(ctx: &Ctx, who: &str) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let convo_id = client.resolve_convo(who).await?;
    client
        .post(
            &Route::Chat,
            "chat.bsky.convo.leaveConvo",
            &json!({ "convoId": convo_id }),
        )
        .await?;
    ctx.out.confirm("left conversation");
    Ok(())
}

async fn mute_convo(ctx: &Ctx, who: &str, mute: bool) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let convo_id = client.resolve_convo(who).await?;
    let nsid = if mute {
        "chat.bsky.convo.muteConvo"
    } else {
        "chat.bsky.convo.unmuteConvo"
    };
    client
        .post(&Route::Chat, nsid, &json!({ "convoId": convo_id }))
        .await?;
    ctx.out.confirm(if mute { "muted" } else { "unmuted" });
    Ok(())
}

async fn react(
    ctx: &Ctx,
    who: &str,
    message: &str,
    emoji: &str,
    remove: bool,
) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let convo_id = client.resolve_convo(who).await?;
    let nsid = if remove {
        "chat.bsky.convo.removeReaction"
    } else {
        "chat.bsky.convo.addReaction"
    };
    client
        .post(
            &Route::Chat,
            nsid,
            &json!({ "convoId": convo_id, "messageId": message, "value": emoji }),
        )
        .await?;
    ctx.out.confirm(if remove {
        "reaction removed"
    } else {
        "reacted"
    });
    Ok(())
}
