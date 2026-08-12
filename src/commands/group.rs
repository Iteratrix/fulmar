//! Group chats (`chat.bsky.group.*`) — brand new lexicon surface; no
//! other CLI has these. Messaging inside a group reuses the `dm`
//! verbs (a group is a conversation).

use serde_json::{Value, json};

use super::Ctx;
use super::util::{page_params, paginate, split_page};
use crate::api::{Client, Route};
use crate::cli::GroupCmd;
use crate::output::render_raw;

#[allow(clippy::too_many_lines)] // dispatch table; splitting it would obscure the grammar
pub async fn group(ctx: &Ctx, cmd: GroupCmd) -> anyhow::Result<()> {
    let client = ctx.client()?;
    match cmd {
        GroupCmd::Create {
            name,
            description,
            member,
        } => create(ctx, &client, &name, description.as_deref(), &member).await,
        GroupCmd::Edit {
            convo,
            name,
            description,
        } => {
            edit(
                ctx,
                &client,
                &convo,
                name.as_deref(),
                description.as_deref(),
            )
            .await
        }
        GroupCmd::Add { convo, actors } => {
            members_op(ctx, &client, "chat.bsky.group.addMembers", &convo, &actors).await
        }
        GroupCmd::Remove { convo, actors } => {
            members_op(
                ctx,
                &client,
                "chat.bsky.group.removeMembers",
                &convo,
                &actors,
            )
            .await
        }
        GroupCmd::Lock { convo } => {
            client
                .post(
                    &Route::Chat,
                    "chat.bsky.convo.lockConvo",
                    &json!({ "convoId": convo }),
                )
                .await?;
            ctx.out.confirm("locked");
            Ok(())
        }
        GroupCmd::Unlock { convo } => {
            client
                .post(
                    &Route::Chat,
                    "chat.bsky.convo.unlockConvo",
                    &json!({ "convoId": convo }),
                )
                .await?;
            ctx.out.confirm("unlocked");
            Ok(())
        }
        GroupCmd::Link {
            convo,
            disable,
            enable,
        } => link(ctx, &client, &convo, disable, enable).await,
        GroupCmd::Preview { link } => preview(ctx, &client, &link).await,
        GroupCmd::Join { link } => join(ctx, &client, &link).await,
        GroupCmd::Withdraw { convo } => {
            client
                .post(
                    &Route::Chat,
                    "chat.bsky.group.withdrawJoinRequest",
                    &json!({ "convoId": convo }),
                )
                .await?;
            ctx.out.confirm("join request withdrawn");
            Ok(())
        }
        GroupCmd::Requests { convo, page } => {
            paginate(&ctx.out, &page, render_raw, |cursor, limit| {
                let client = &client;
                let convo = convo.clone();
                async move {
                    let mut params = page_params(cursor, limit);
                    params.push(("convoId", convo));
                    let value = client
                        .get(&Route::Chat, "chat.bsky.group.listJoinRequests", &params)
                        .await?;
                    Ok(split_page(&value, "requests"))
                }
            })
            .await
        }
        GroupCmd::Approve { convo, actor } => {
            request_op(
                ctx,
                &client,
                "chat.bsky.group.approveJoinRequest",
                &convo,
                &actor,
            )
            .await
        }
        GroupCmd::Reject { convo, actor } => {
            request_op(
                ctx,
                &client,
                "chat.bsky.group.rejectJoinRequest",
                &convo,
                &actor,
            )
            .await
        }
        GroupCmd::Mutual { actor, page } => {
            let did = client.resolve_actor(&actor).await?;
            paginate(&ctx.out, &page, render_raw, |cursor, limit| {
                let client = &client;
                let did = did.clone();
                async move {
                    let mut params = page_params(cursor, limit);
                    params.push(("actor", did.as_str().to_string()));
                    let value = client
                        .get(&Route::Chat, "chat.bsky.group.listMutualGroups", &params)
                        .await?;
                    Ok(split_page(&value, "convos"))
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
    description: Option<&str>,
    member: &[String],
) -> anyhow::Result<()> {
    let mut members = Vec::new();
    for actor in member {
        members.push(client.resolve_actor(actor).await?.as_str().to_string());
    }
    let mut body = json!({ "name": name, "members": members });
    if let Some(description) = description {
        body["description"] = json!(description);
    }
    let value = client
        .post(&Route::Chat, "chat.bsky.group.createGroup", &body)
        .await?;
    ctx.out.object(&value, |v| {
        let id = v
            .get("convo")
            .and_then(|c| c.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("?");
        format!("group created: {id}")
    });
    Ok(())
}

async fn edit(
    ctx: &Ctx,
    client: &Client,
    convo: &str,
    name: Option<&str>,
    description: Option<&str>,
) -> anyhow::Result<()> {
    let mut body = json!({ "convoId": convo });
    if let Some(name) = name {
        body["name"] = json!(name);
    }
    if let Some(description) = description {
        body["description"] = json!(description);
    }
    client
        .post(&Route::Chat, "chat.bsky.group.editGroup", &body)
        .await?;
    ctx.out.confirm("group updated");
    Ok(())
}

async fn link(
    ctx: &Ctx,
    client: &Client,
    convo: &str,
    disable: bool,
    enable: bool,
) -> anyhow::Result<()> {
    let nsid = if disable {
        "chat.bsky.group.disableJoinLink"
    } else if enable {
        "chat.bsky.group.enableJoinLink"
    } else {
        "chat.bsky.group.createJoinLink"
    };
    let value = client
        .post(&Route::Chat, nsid, &json!({ "convoId": convo }))
        .await?;
    ctx.out.object(&value, render_raw);
    Ok(())
}

async fn preview(ctx: &Ctx, client: &Client, link: &str) -> anyhow::Result<()> {
    let code = link.rsplit('/').next().unwrap_or(link);
    let value = client
        .get(
            &Route::Chat,
            "chat.bsky.group.getJoinLinkPreviews",
            &[("code", code.to_string())],
        )
        .await?;
    ctx.out.object(&value, render_raw);
    Ok(())
}

async fn join(ctx: &Ctx, client: &Client, link: &str) -> anyhow::Result<()> {
    let code = link.rsplit('/').next().unwrap_or(link);
    let value = client
        .post(
            &Route::Chat,
            "chat.bsky.group.requestJoin",
            &json!({ "code": code }),
        )
        .await?;
    ctx.out.object(&value, |_| "join requested".to_string());
    Ok(())
}

async fn members_op(
    ctx: &Ctx,
    client: &Client,
    nsid: &str,
    convo: &str,
    actors: &[String],
) -> anyhow::Result<()> {
    let mut members = Vec::new();
    for actor in actors {
        members.push(client.resolve_actor(actor).await?.as_str().to_string());
    }
    client
        .post(
            &Route::Chat,
            nsid,
            &json!({ "convoId": convo, "members": members }),
        )
        .await?;
    ctx.out.confirm("done");
    Ok(())
}

async fn request_op(
    ctx: &Ctx,
    client: &Client,
    nsid: &str,
    convo: &str,
    actor: &str,
) -> anyhow::Result<()> {
    let did = client.resolve_actor(actor).await?;
    client
        .post(
            &Route::Chat,
            nsid,
            &json!({ "convoId": convo, "member": did.as_str() }),
        )
        .await?;
    ctx.out.confirm("done");
    Ok(())
}
