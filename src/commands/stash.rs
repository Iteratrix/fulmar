//! Private stash storage: bookmarks and drafts. These are NOT repo
//! records — the `record` escape hatch can't reach them, which is
//! exactly why they get typed verbs.

use serde_json::{Value, json};

use super::Ctx;
use super::util::{json_input, page_params, paginate, split_page};
use crate::api::Route;
use crate::cli::{DraftCmd, PageArgs};
use crate::output::{render_post, render_raw};

pub async fn bookmark(ctx: &Ctx, post: &str) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let uri = client.resolve_post_uri(post).await?;
    let record = client.record_ref(&uri).await?;
    client
        .post(
            &Route::Pds,
            "app.bsky.bookmark.createBookmark",
            &json!({ "uri": record.uri.as_str(), "cid": record.cid.as_str() }),
        )
        .await?;
    ctx.out.confirm("bookmarked");
    Ok(())
}

pub async fn unbookmark(ctx: &Ctx, post: &str) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let uri = client.resolve_post_uri(post).await?;
    client
        .post(
            &Route::Pds,
            "app.bsky.bookmark.deleteBookmark",
            &json!({ "uri": uri.as_str() }),
        )
        .await?;
    ctx.out.confirm("bookmark removed");
    Ok(())
}

pub async fn bookmarks(ctx: &Ctx, page: &PageArgs) -> anyhow::Result<()> {
    let client = ctx.client()?;
    paginate(&ctx.out, page, render_bookmark, |cursor, limit| {
        let client = &client;
        async move {
            let value = client
                .get(
                    &Route::Pds,
                    "app.bsky.bookmark.getBookmarks",
                    &page_params(cursor, limit),
                )
                .await?;
            Ok(split_page(&value, "bookmarks"))
        }
    })
    .await
}

fn render_bookmark(value: &Value) -> String {
    value
        .get("item")
        .map_or_else(|| render_raw(value), render_post)
}

pub async fn drafts(ctx: &Ctx, page: &PageArgs) -> anyhow::Result<()> {
    let client = ctx.client()?;
    paginate(&ctx.out, page, render_raw, |cursor, limit| {
        let client = &client;
        async move {
            let value = client
                .get(
                    &Route::Pds,
                    "app.bsky.draft.getDrafts",
                    &page_params(cursor, limit),
                )
                .await?;
            Ok(split_page(&value, "drafts"))
        }
    })
    .await
}

pub async fn draft(ctx: &Ctx, cmd: DraftCmd) -> anyhow::Result<()> {
    let client = ctx.client()?;
    match cmd {
        DraftCmd::Save { file } => {
            let draft = json_input(file.as_ref())?;
            let value = client
                .post(&Route::Pds, "app.bsky.draft.createDraft", &draft)
                .await?;
            ctx.out.object(&value, |v| {
                let id = v.get("id").and_then(Value::as_str).unwrap_or("saved");
                format!("draft saved ({id})")
            });
            Ok(())
        }
        DraftCmd::Rm { id } => {
            client
                .post(
                    &Route::Pds,
                    "app.bsky.draft.deleteDraft",
                    &json!({ "id": id }),
                )
                .await?;
            ctx.out.confirm("draft deleted");
            Ok(())
        }
    }
}
