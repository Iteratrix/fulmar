//! Read commands: timeline, threads, profiles, graph, search, feeds.

use serde_json::Value;

use super::Ctx;
use super::util::{page_params, paginate, split_page};
use crate::api::{ApiError, Client, Route};
use crate::cli::PageArgs;
use crate::output::{render_actor, render_post, render_profile, render_raw};

pub async fn timeline(ctx: &Ctx, page: &PageArgs) -> anyhow::Result<()> {
    let client = ctx.client()?;
    paginate(&ctx.out, page, render_post, |cursor, limit| {
        let client = &client;
        async move {
            let value = client
                .get(
                    &Route::Pds,
                    "app.bsky.feed.getTimeline",
                    &page_params(cursor, limit),
                )
                .await?;
            Ok(split_page(&value, "feed"))
        }
    })
    .await
}

pub async fn view(ctx: &Ctx, post: &str, depth: u32, parents: u32) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let uri = client.resolve_post_uri(post).await?;
    let value = client
        .get(
            &Route::Pds,
            "app.bsky.feed.getPostThread",
            &[
                ("uri", uri.as_str().to_string()),
                ("depth", depth.to_string()),
                ("parentHeight", parents.to_string()),
            ],
        )
        .await?;
    let Some(thread) = value.get("thread") else {
        anyhow::bail!("getPostThread response missing thread");
    };
    let mut posts = Vec::new();
    flatten_thread(thread, &mut posts);
    for post in &posts {
        ctx.out.item(post, render_post);
    }
    Ok(())
}

/// Flatten a thread tree into oldest-first order: walk to the root
/// through `parent` links, then depth-first through `replies`.
fn flatten_thread(node: &Value, out: &mut Vec<Value>) {
    if let Some(parent) = node.get("parent") {
        flatten_parents(parent, out);
    }
    flatten_replies(node, out);
}

fn flatten_parents(node: &Value, out: &mut Vec<Value>) {
    if let Some(parent) = node.get("parent") {
        flatten_parents(parent, out);
    }
    if let Some(post) = node.get("post") {
        out.push(post.clone());
    }
}

fn flatten_replies(node: &Value, out: &mut Vec<Value>) {
    if let Some(post) = node.get("post") {
        out.push(post.clone());
    }
    let Some(replies) = node.get("replies").and_then(Value::as_array) else {
        return;
    };
    for reply in replies {
        flatten_replies(reply, out);
    }
}

pub async fn author_feed(
    ctx: &Ctx,
    actor: &str,
    filter: &str,
    page: &PageArgs,
) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let did = client.resolve_actor(actor).await?;
    paginate(&ctx.out, page, render_post, |cursor, limit| {
        let client = &client;
        let did = did.clone();
        async move {
            let mut params = page_params(cursor, limit);
            params.push(("actor", did.as_str().to_string()));
            params.push(("filter", filter.to_string()));
            let value = client
                .get(&Route::Pds, "app.bsky.feed.getAuthorFeed", &params)
                .await?;
            Ok(split_page(&value, "feed"))
        }
    })
    .await
}

pub async fn profile(ctx: &Ctx, actor: &str) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let did = client.resolve_actor(actor).await?;
    let value = client
        .get(
            &Route::Pds,
            "app.bsky.actor.getProfile",
            &[("actor", did.as_str().to_string())],
        )
        .await?;
    ctx.out.object(&value, render_profile);
    Ok(())
}

/// Followers/follows share a shape: `actor` param, actor-list result.
pub async fn graph_list(
    ctx: &Ctx,
    nsid: &str,
    key: &str,
    actor: &str,
    page: &PageArgs,
) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let did = client.resolve_actor(actor).await?;
    paginate(&ctx.out, page, render_actor, |cursor, limit| {
        let client = &client;
        let did = did.clone();
        async move {
            let mut params = page_params(cursor, limit);
            params.push(("actor", did.as_str().to_string()));
            let value = client.get(&Route::Pds, nsid, &params).await?;
            Ok(split_page(&value, key))
        }
    })
    .await
}

pub async fn known_followers(ctx: &Ctx, actor: &str, page: &PageArgs) -> anyhow::Result<()> {
    graph_list(
        ctx,
        "app.bsky.graph.getKnownFollowers",
        "followers",
        actor,
        page,
    )
    .await
}

/// Blocks/mutes: no actor param (always "me"), actor-list result.
pub async fn self_graph(ctx: &Ctx, nsid: &str, key: &str, page: &PageArgs) -> anyhow::Result<()> {
    let client = ctx.client()?;
    paginate(&ctx.out, page, render_actor, |cursor, limit| {
        let client = &client;
        async move {
            let value = client
                .get(&Route::Pds, nsid, &page_params(cursor, limit))
                .await?;
            Ok(split_page(&value, key))
        }
    })
    .await
}

pub async fn relationship(ctx: &Ctx, actors: &[String], from: Option<&str>) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let root = match from {
        Some(actor) => client.resolve_actor(actor).await?,
        None => client.did().await,
    };
    let mut params = vec![("actor", root.as_str().to_string())];
    for actor in actors {
        let did = client.resolve_actor(actor).await?;
        params.push(("others", did.as_str().to_string()));
    }
    let value = client
        .get(&Route::Pds, "app.bsky.graph.getRelationships", &params)
        .await?;
    let items = value
        .get("relationships")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for item in &items {
        ctx.out.item(item, |v| {
            let did = v.get("did").and_then(Value::as_str).unwrap_or("?");
            let following = v.get("following").is_some_and(|f| !f.is_null());
            let followed_by = v.get("followedBy").is_some_and(|f| !f.is_null());
            format!(
                "{did}: {}{}",
                if following {
                    "following"
                } else {
                    "not following"
                },
                if followed_by { ", follows back" } else { "" }
            )
        });
    }
    Ok(())
}

pub async fn search_posts(
    ctx: &Ctx,
    query: &str,
    author: Option<&str>,
    sort: &str,
    since: Option<&str>,
    until: Option<&str>,
    page: &PageArgs,
) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let author_did = match author {
        Some(actor) => Some(client.resolve_actor(actor).await?),
        None => None,
    };
    paginate(&ctx.out, page, render_post, |cursor, limit| {
        let client = &client;
        let author_did = author_did.clone();
        async move {
            let mut params = page_params(cursor, limit);
            params.push(("q", query.to_string()));
            params.push(("sort", sort.to_string()));
            if let Some(did) = author_did {
                params.push(("author", did.as_str().to_string()));
            }
            if let Some(since) = since {
                params.push(("since", since.to_string()));
            }
            if let Some(until) = until {
                params.push(("until", until.to_string()));
            }
            let value = client
                .get(&Route::Pds, "app.bsky.feed.searchPosts", &params)
                .await?;
            let (items, cursor) = split_page(&value, "posts");
            let items = items
                .into_iter()
                .map(|post| serde_json::json!({ "post": post }))
                .collect();
            Ok((items, cursor))
        }
    })
    .await
}

pub async fn search_users(ctx: &Ctx, query: &str, page: &PageArgs) -> anyhow::Result<()> {
    let client = ctx.client()?;
    paginate(&ctx.out, page, render_actor, |cursor, limit| {
        let client = &client;
        async move {
            let mut params = page_params(cursor, limit);
            params.push(("q", query.to_string()));
            let value = client
                .get(&Route::Pds, "app.bsky.actor.searchActors", &params)
                .await?;
            Ok(split_page(&value, "actors"))
        }
    })
    .await
}

pub async fn feed(ctx: &Ctx, feed: &str, page: &PageArgs) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let uri = resolve_feed_uri(&client, feed).await?;
    paginate(&ctx.out, page, render_post, |cursor, limit| {
        let client = &client;
        let uri = uri.clone();
        async move {
            let mut params = page_params(cursor, limit);
            params.push(("feed", uri));
            let value = client
                .get(&Route::Pds, "app.bsky.feed.getFeed", &params)
                .await?;
            Ok(split_page(&value, "feed"))
        }
    })
    .await
}

/// Accept a feed's at:// URI or a
/// `https://bsky.app/profile/<actor>/feed/<rkey>` URL.
async fn resolve_feed_uri(client: &Client, input: &str) -> Result<String, ApiError> {
    let input = input.trim();
    if input.starts_with("at://") {
        return Ok(input.to_string());
    }
    if let Some(rest) = input.strip_prefix("https://bsky.app/profile/") {
        let mut parts = rest.split('/');
        if let (Some(actor), Some("feed"), Some(rkey)) = (parts.next(), parts.next(), parts.next())
        {
            let did = client.resolve_actor(actor).await?;
            return Ok(format!("at://{did}/app.bsky.feed.generator/{rkey}"));
        }
    }
    Err(ApiError::Unexpected(format!(
        "expected a feed at:// URI or bsky.app feed URL, got: {input}"
    )))
}

/// Likes / reposted-by / quotes: `uri` param, list result.
pub async fn post_engagement(
    ctx: &Ctx,
    nsid: &str,
    key: &str,
    post: &str,
    page: &PageArgs,
) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let uri = client.resolve_post_uri(post).await?;
    let render: fn(&Value) -> String = match key {
        "likes" => |v: &Value| v.get("actor").map_or_else(|| render_raw(v), render_actor),
        "repostedBy" => render_actor,
        "posts" => render_post,
        _ => render_raw,
    };
    paginate(&ctx.out, page, render, |cursor, limit| {
        let client = &client;
        let uri = uri.clone();
        async move {
            let mut params = page_params(cursor, limit);
            params.push(("uri", uri.as_str().to_string()));
            let value = client.get(&Route::Pds, nsid, &params).await?;
            Ok(split_page(&value, key))
        }
    })
    .await
}

pub async fn starterpacks(ctx: &Ctx, actor: &str, page: &PageArgs) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let did = client.resolve_actor(actor).await?;
    paginate(&ctx.out, page, render_raw, |cursor, limit| {
        let client = &client;
        let did = did.clone();
        async move {
            let mut params = page_params(cursor, limit);
            params.push(("actor", did.as_str().to_string()));
            let value = client
                .get(&Route::Pds, "app.bsky.graph.getActorStarterPacks", &params)
                .await?;
            Ok(split_page(&value, "starterPacks"))
        }
    })
    .await
}
