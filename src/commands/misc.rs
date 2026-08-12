//! Preferences, moderation reports, backup, `WhiteWind` blog, the
//! raw record verbs, and the `fulmar api` escape hatch.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::CommandFactory;
use serde_json::{Value, json};

use super::Ctx;
use super::util::{json_input, page_params, paginate, split_page, text_input};
use crate::api::{ApiError, Client, Route};
use crate::cli::{BlogCmd, Cli, PrefsCmd, RecordCmd};
use crate::output::render_raw;

/// Bluesky's moderation service (labeler) DID, for report routing.
const BSKY_LABELER: &str = "did:plc:ar7c4by46qjdydhdevvrndac#atproto_labeler";

pub async fn prefs(ctx: &Ctx, cmd: PrefsCmd) -> anyhow::Result<()> {
    let client = ctx.client()?;
    match cmd {
        PrefsCmd::Get => {
            let value = client
                .get(&Route::Pds, "app.bsky.actor.getPreferences", &[])
                .await?;
            ctx.out.object(&value, render_raw);
            Ok(())
        }
        PrefsCmd::Set { file } => {
            let input = json_input(file.as_ref())?;
            let body = if input.get("preferences").is_some() {
                input
            } else if input.is_array() {
                json!({ "preferences": input })
            } else {
                anyhow::bail!(
                    "expected the getPreferences shape ({{\"preferences\": [...]}}) or a bare array"
                );
            };
            client
                .post(&Route::Pds, "app.bsky.actor.putPreferences", &body)
                .await?;
            ctx.out.confirm("preferences replaced");
            Ok(())
        }
    }
}

pub async fn report(
    ctx: &Ctx,
    subject: &str,
    reason: &str,
    details: Option<&str>,
) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let reason_type = match reason {
        "spam" => "com.atproto.moderation.defs#reasonSpam",
        "violation" => "com.atproto.moderation.defs#reasonViolation",
        "misleading" => "com.atproto.moderation.defs#reasonMisleading",
        "sexual" => "com.atproto.moderation.defs#reasonSexual",
        "rude" => "com.atproto.moderation.defs#reasonRude",
        "other" => "com.atproto.moderation.defs#reasonOther",
        other => anyhow::bail!(
            "unknown reason {other:?} (expected spam, violation, misleading, sexual, rude, other)"
        ),
    };
    let subject_value = if subject.starts_with("at://") || subject.starts_with("https://bsky.app/")
    {
        let uri = client.resolve_post_uri(subject).await?;
        let record = client.record_ref(&uri).await?;
        json!({
            "$type": "com.atproto.repo.strongRef",
            "uri": record.uri.as_str(),
            "cid": record.cid.as_str(),
        })
    } else {
        let did = client.resolve_actor(subject).await?;
        json!({ "$type": "com.atproto.admin.defs#repoRef", "did": did.as_str() })
    };
    let mut body = json!({ "reasonType": reason_type, "subject": subject_value });
    if let Some(details) = details {
        body["reason"] = json!(details);
    }
    let value = client
        .post(
            &Route::Proxied(BSKY_LABELER.to_string()),
            "com.atproto.moderation.createReport",
            &body,
        )
        .await?;
    ctx.out.object(&value, |v| {
        let id = v.get("id").and_then(Value::as_u64).unwrap_or(0);
        format!("report filed (#{id})")
    });
    Ok(())
}

pub async fn backup(ctx: &Ctx, file: Option<PathBuf>) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let did = client.did().await;
    let handle = client.handle().await;
    let path = file.unwrap_or_else(|| PathBuf::from(format!("{handle}.car")));
    let bytes = client
        .get_bytes(
            &Route::Pds,
            "com.atproto.sync.getRepo",
            &[("did", did.as_str().to_string())],
        )
        .await?;
    let size = bytes.len();
    std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
    ctx.out.object(
        &json!({ "file": path, "bytes": size, "did": did.as_str() }),
        |_| format!("{} ({size} bytes)", path.display()),
    );
    Ok(())
}

pub async fn blog(ctx: &Ctx, cmd: BlogCmd) -> anyhow::Result<()> {
    let BlogCmd::Publish {
        file,
        title,
        visibility,
    } = cmd;
    match visibility.as_str() {
        "public" | "url" | "author" => {}
        other => anyhow::bail!("visibility must be public, url, or author (got {other:?})"),
    }
    let client = ctx.client()?;
    let content = text_input(file.as_ref())?;
    let record = json!({
        "$type": "com.whtwnd.blog.entry",
        "content": content,
        "title": title,
        "createdAt": chrono::Utc::now().to_rfc3339(),
        "visibility": visibility,
    });
    let did = client.did().await;
    let value = client
        .post(
            &Route::Pds,
            "com.atproto.repo.createRecord",
            &json!({
                "repo": did.as_str(),
                "collection": "com.whtwnd.blog.entry",
                "record": record,
            }),
        )
        .await?;
    let handle = client.handle().await;
    let rkey = value
        .get("uri")
        .and_then(Value::as_str)
        .and_then(|u| u.rsplit('/').next())
        .unwrap_or_default();
    // WhiteWind routes by rkey, never by title slug — this is the
    // only shareable link.
    let url = format!("https://whtwnd.com/{handle}/{rkey}");
    let mut value = value;
    value["url"] = json!(url);
    ctx.out.object(&value, |_| url.clone());
    Ok(())
}

pub async fn record(ctx: &Ctx, cmd: RecordCmd) -> anyhow::Result<()> {
    let client = ctx.client()?;
    match cmd {
        RecordCmd::Get { uri } => {
            let (repo, collection, rkey) = split_record_uri(&client, &uri).await?;
            let value = client
                .get(
                    &Route::Pds,
                    "com.atproto.repo.getRecord",
                    &[("repo", repo), ("collection", collection), ("rkey", rkey)],
                )
                .await?;
            ctx.out.object(&value, render_raw);
            Ok(())
        }
        RecordCmd::List {
            actor,
            collection,
            page,
        } => {
            let did = client.resolve_actor(&actor).await?;
            paginate(&ctx.out, &page, render_raw, |cursor, limit| {
                let client = &client;
                let did = did.clone();
                let collection = collection.clone();
                async move {
                    let mut params = page_params(cursor, limit);
                    params.push(("repo", did.as_str().to_string()));
                    params.push(("collection", collection));
                    let value = client
                        .get(&Route::Pds, "com.atproto.repo.listRecords", &params)
                        .await?;
                    Ok(split_page(&value, "records"))
                }
            })
            .await
        }
        RecordCmd::Create {
            collection,
            file,
            rkey,
        } => {
            let record = json_input(file.as_ref())?;
            let did = client.did().await;
            let mut body = json!({
                "repo": did.as_str(),
                "collection": collection,
                "record": record,
            });
            if let Some(rkey) = rkey {
                body["rkey"] = json!(rkey);
            }
            let value = client
                .post(&Route::Pds, "com.atproto.repo.createRecord", &body)
                .await?;
            ctx.out.object(&value, |v| {
                v.get("uri")
                    .and_then(Value::as_str)
                    .unwrap_or("created")
                    .to_string()
            });
            Ok(())
        }
        RecordCmd::Put { uri, file } => {
            let record = json_input(file.as_ref())?;
            let (repo, collection, rkey) = split_record_uri(&client, &uri).await?;
            let value = client
                .post(
                    &Route::Pds,
                    "com.atproto.repo.putRecord",
                    &json!({
                        "repo": repo,
                        "collection": collection,
                        "rkey": rkey,
                        "record": record,
                    }),
                )
                .await?;
            ctx.out.object(&value, |v| {
                v.get("uri")
                    .and_then(Value::as_str)
                    .unwrap_or("put")
                    .to_string()
            });
            Ok(())
        }
        RecordCmd::Delete { uri } => {
            let (repo, collection, rkey) = split_record_uri(&client, &uri).await?;
            client
                .post(
                    &Route::Pds,
                    "com.atproto.repo.deleteRecord",
                    &json!({ "repo": repo, "collection": collection, "rkey": rkey }),
                )
                .await?;
            ctx.out.confirm("deleted");
            Ok(())
        }
    }
}

/// Split `at://authority/collection/rkey`, resolving a handle
/// authority to a DID.
async fn split_record_uri(
    client: &Client,
    uri: &str,
) -> Result<(String, String, String), ApiError> {
    let rest = uri
        .trim()
        .strip_prefix("at://")
        .ok_or_else(|| ApiError::Unexpected(format!("expected an at:// URI, got {uri}")))?;
    let mut parts = rest.splitn(3, '/');
    let (Some(authority), Some(collection), Some(rkey)) =
        (parts.next(), parts.next(), parts.next())
    else {
        return Err(ApiError::Unexpected(format!(
            "at:// URI must be at://repo/collection/rkey: {uri}"
        )));
    };
    let repo = if authority.starts_with("did:") {
        authority.to_string()
    } else {
        client.resolve_actor(authority).await?.as_str().to_string()
    };
    Ok((repo, collection.to_string(), rkey.to_string()))
}

pub async fn api(
    ctx: &Ctx,
    nsid: &str,
    fields: &[String],
    force_post: bool,
    proxy: Option<&str>,
) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let route = match proxy {
        None => {
            if nsid.starts_with("chat.bsky.") {
                Route::Chat
            } else {
                Route::Pds
            }
        }
        Some("chat") => Route::Chat,
        Some("video") => Route::Proxied("did:web:video.bsky.app#bsky_video".to_string()),
        Some(raw) => Route::Proxied(raw.to_string()),
    };

    let mut pairs = Vec::new();
    for field in fields {
        let Some((key, value)) = field.split_once('=') else {
            anyhow::bail!("-f expects key=value, got {field:?}");
        };
        pairs.push((key.to_string(), value.to_string()));
    }

    let stdin_piped = !std::io::IsTerminal::is_terminal(&std::io::stdin());
    let value = if force_post || (stdin_piped && pairs.is_empty()) {
        let body = if stdin_piped {
            let raw = std::io::read_to_string(std::io::stdin())?;
            if raw.trim().is_empty() {
                fields_to_object(&pairs)
            } else {
                serde_json::from_str(&raw).context("parsing JSON body from stdin")?
            }
        } else {
            fields_to_object(&pairs)
        };
        client.post(&route, nsid, &body).await?
    } else {
        let params: Vec<(&str, String)> =
            pairs.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        client.get(&route, nsid, &params).await?
    };
    ctx.out.object(&value, render_raw);
    Ok(())
}

fn fields_to_object(pairs: &[(String, String)]) -> Value {
    let mut object = serde_json::Map::new();
    for (key, value) in pairs {
        let parsed = serde_json::from_str::<Value>(value).unwrap_or_else(|_| json!(value));
        object.insert(key.clone(), parsed);
    }
    Value::Object(object)
}

pub fn completions(shell: clap_complete::Shell) {
    let mut command = Cli::command();
    clap_complete::generate(shell, &mut command, "fulmar", &mut std::io::stdout());
}
