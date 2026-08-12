//! Lists: reads via `app.bsky.graph.*`, membership via `listitem`
//! records, moderation via mute/block.

use anyhow::Context as _;
use serde_json::{Value, json};

use super::Ctx;
use super::util::{page_params, paginate, split_page};
use crate::api::{ApiError, Client, Route};
use crate::cli::{ListCmd, PageArgs};
use crate::output::{render_post, render_raw};

pub async fn lists(ctx: &Ctx, actor: &str, page: &PageArgs) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let did = client.resolve_actor(actor).await?;
    paginate(&ctx.out, page, render_list, |cursor, limit| {
        let client = &client;
        let did = did.clone();
        async move {
            let mut params = page_params(cursor, limit);
            params.push(("actor", did.as_str().to_string()));
            let value = client
                .get(&Route::Pds, "app.bsky.graph.getLists", &params)
                .await?;
            Ok(split_page(&value, "lists"))
        }
    })
    .await
}

fn render_list(value: &Value) -> String {
    let name = value.get("name").and_then(Value::as_str).unwrap_or("?");
    let purpose = value
        .get("purpose")
        .and_then(Value::as_str)
        .map_or("", |p| p.rsplit('#').next().unwrap_or(p));
    let uri = value.get("uri").and_then(Value::as_str).unwrap_or("");
    let count = value
        .get("listItemCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    format!("{name} ({purpose}, {count} members) · {uri}")
}

pub async fn list(ctx: &Ctx, cmd: ListCmd) -> anyhow::Result<()> {
    let client = ctx.client()?;
    match cmd {
        ListCmd::Show { list, page } => {
            paginate(&ctx.out, &page, render_raw, |cursor, limit| {
                let client = &client;
                let list = list.clone();
                async move {
                    let mut params = page_params(cursor, limit);
                    params.push(("list", list));
                    let value = client
                        .get(&Route::Pds, "app.bsky.graph.getList", &params)
                        .await?;
                    Ok(split_page(&value, "items"))
                }
            })
            .await
        }
        ListCmd::Feed { list, page } => {
            paginate(&ctx.out, &page, render_post, |cursor, limit| {
                let client = &client;
                let list = list.clone();
                async move {
                    let mut params = page_params(cursor, limit);
                    params.push(("list", list));
                    let value = client
                        .get(&Route::Pds, "app.bsky.feed.getListFeed", &params)
                        .await?;
                    Ok(split_page(&value, "feed"))
                }
            })
            .await
        }
        ListCmd::Create {
            name,
            purpose,
            description,
        } => create(ctx, &client, &name, &purpose, description.as_deref()).await,
        ListCmd::Add { list, actor } => add(ctx, &client, &list, &actor).await,
        ListCmd::Remove { list, actor } => remove(ctx, &client, &list, &actor).await,
        ListCmd::Delete { list } => delete(ctx, &client, &list).await,
        ListCmd::Mute { list } => {
            client
                .post(
                    &Route::Pds,
                    "app.bsky.graph.muteActorList",
                    &json!({ "list": list }),
                )
                .await?;
            ctx.out.confirm("list muted");
            Ok(())
        }
        ListCmd::Unmute { list } => {
            client
                .post(
                    &Route::Pds,
                    "app.bsky.graph.unmuteActorList",
                    &json!({ "list": list }),
                )
                .await?;
            ctx.out.confirm("list unmuted");
            Ok(())
        }
        ListCmd::Block { list } => {
            let me = client.did().await;
            let record = json!({
                "$type": "app.bsky.graph.listblock",
                "subject": list,
                "createdAt": chrono::Utc::now().to_rfc3339(),
            });
            client
                .post(
                    &Route::Pds,
                    "com.atproto.repo.createRecord",
                    &json!({
                        "repo": me.as_str(),
                        "collection": "app.bsky.graph.listblock",
                        "record": record,
                    }),
                )
                .await?;
            ctx.out.confirm("list blocked");
            Ok(())
        }
        ListCmd::Unblock { list } => unblock(ctx, &client, &list).await,
        ListCmd::Membership { actor, page } => {
            let did = client.resolve_actor(&actor).await?;
            paginate(&ctx.out, &page, render_raw, |cursor, limit| {
                let client = &client;
                let did = did.clone();
                async move {
                    let mut params = page_params(cursor, limit);
                    params.push(("actor", did.as_str().to_string()));
                    let value = client
                        .get(
                            &Route::Pds,
                            "app.bsky.graph.getListsWithMembership",
                            &params,
                        )
                        .await?;
                    Ok(split_page(&value, "listsWithMembership"))
                }
            })
            .await
        }
    }
}

async fn create(
    ctx: &Ctx,
    client: &Client,
    name: &str,
    purpose: &str,
    description: Option<&str>,
) -> anyhow::Result<()> {
    let purpose = match purpose {
        "curate" => "app.bsky.graph.defs#curatelist",
        "mod" => "app.bsky.graph.defs#modlist",
        other => anyhow::bail!("purpose must be curate or mod, got {other:?}"),
    };
    let mut record = json!({
        "$type": "app.bsky.graph.list",
        "name": name,
        "purpose": purpose,
        "createdAt": chrono::Utc::now().to_rfc3339(),
    });
    if let Some(description) = description {
        record["description"] = json!(description);
    }
    let did = client.did().await;
    let value = client
        .post(
            &Route::Pds,
            "com.atproto.repo.createRecord",
            &json!({
                "repo": did.as_str(),
                "collection": "app.bsky.graph.list",
                "record": record,
            }),
        )
        .await?;
    ctx.out.object(&value, |v| {
        v.get("uri")
            .and_then(Value::as_str)
            .unwrap_or("created")
            .to_string()
    });
    Ok(())
}

async fn add(ctx: &Ctx, client: &Client, list: &str, actor: &str) -> anyhow::Result<()> {
    let did = client.resolve_actor(actor).await?;
    let record = json!({
        "$type": "app.bsky.graph.listitem",
        "subject": did.as_str(),
        "list": list,
        "createdAt": chrono::Utc::now().to_rfc3339(),
    });
    let me = client.did().await;
    let value = client
        .post(
            &Route::Pds,
            "com.atproto.repo.createRecord",
            &json!({
                "repo": me.as_str(),
                "collection": "app.bsky.graph.listitem",
                "record": record,
            }),
        )
        .await?;
    ctx.out.object(&value, |_| format!("added {did}"));
    Ok(())
}

async fn remove(ctx: &Ctx, client: &Client, list: &str, actor: &str) -> anyhow::Result<()> {
    let did = client.resolve_actor(actor).await?;
    let rkey = find_listitem_rkey(client, list, &did)
        .await?
        .context("that actor is not on the list")?;
    let me = client.did().await;
    client
        .post(
            &Route::Pds,
            "com.atproto.repo.deleteRecord",
            &json!({
                "repo": me.as_str(),
                "collection": "app.bsky.graph.listitem",
                "rkey": rkey,
            }),
        )
        .await?;
    ctx.out.confirm("removed");
    Ok(())
}

async fn delete(ctx: &Ctx, client: &Client, list: &str) -> anyhow::Result<()> {
    let me = client.did().await;
    let rkey = list
        .rsplit('/')
        .next()
        .context("malformed list URI")?
        .to_string();
    client
        .post(
            &Route::Pds,
            "com.atproto.repo.deleteRecord",
            &json!({
                "repo": me.as_str(),
                "collection": "app.bsky.graph.list",
                "rkey": rkey,
            }),
        )
        .await?;
    ctx.out.confirm("list deleted");
    Ok(())
}

async fn unblock(ctx: &Ctx, client: &Client, list: &str) -> anyhow::Result<()> {
    let value = client
        .get(
            &Route::Pds,
            "app.bsky.graph.getList",
            &[("list", list.to_string()), ("limit", "1".to_string())],
        )
        .await?;
    let Some(record_uri) = value
        .get("list")
        .and_then(|l| l.get("viewer"))
        .and_then(|v| v.get("blocked"))
        .and_then(Value::as_str)
    else {
        anyhow::bail!("that list is not blocked");
    };
    let me = client.did().await;
    let rkey = record_uri
        .rsplit('/')
        .next()
        .context("malformed listblock URI")?;
    client
        .post(
            &Route::Pds,
            "com.atproto.repo.deleteRecord",
            &json!({
                "repo": me.as_str(),
                "collection": "app.bsky.graph.listblock",
                "rkey": rkey,
            }),
        )
        .await?;
    ctx.out.confirm("list unblocked");
    Ok(())
}

/// Find my listitem record pointing `did` at `list` by scanning my
/// listitem collection (the `AppView` has no direct lookup).
async fn find_listitem_rkey(
    client: &Client,
    list: &str,
    did: &crate::identifiers::Did,
) -> Result<Option<String>, ApiError> {
    let me = client.did().await;
    let mut cursor: Option<String> = None;
    loop {
        let mut params = vec![
            ("repo", me.as_str().to_string()),
            ("collection", "app.bsky.graph.listitem".to_string()),
            ("limit", "100".to_string()),
        ];
        if let Some(cursor) = cursor {
            params.push(("cursor", cursor));
        }
        let value = client
            .get(&Route::Pds, "com.atproto.repo.listRecords", &params)
            .await?;
        let (items, next) = super::util::split_page(&value, "records");
        for item in &items {
            let record = item.get("value");
            let matches = record.is_some_and(|r| {
                r.get("list").and_then(Value::as_str) == Some(list)
                    && r.get("subject").and_then(Value::as_str) == Some(did.as_str())
            });
            if matches {
                let rkey = item
                    .get("uri")
                    .and_then(Value::as_str)
                    .and_then(|u| u.rsplit('/').next())
                    .map(ToString::to_string);
                return Ok(rkey);
            }
        }
        if next.is_none() || items.is_empty() {
            return Ok(None);
        }
        cursor = next;
    }
}
