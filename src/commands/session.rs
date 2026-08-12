//! `login`, `whoami`, `session *`, `resolve`.

use anyhow::Context as _;
use serde_json::json;

use super::Ctx;
use crate::api::{Client, Route, identity};
use crate::cli::SessionCmd;
use crate::output::render_raw;

/// The only password-touching path. `$FULMAR_PASSWORD` wins (scripted
/// seeding); otherwise prompt on a TTY; otherwise fail with a message
/// that explains both options — never hang waiting for input in a
/// pipe.
pub async fn login(ctx: &Ctx, identifier: &str, service: &str) -> anyhow::Result<()> {
    let password = match std::env::var("FULMAR_PASSWORD") {
        Ok(password) if !password.is_empty() => password,
        _ => {
            if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                anyhow::bail!(
                    "no password available: set $FULMAR_PASSWORD or run interactively \
                     (an app password with DM access is recommended — Settings → App Passwords)"
                );
            }
            rpassword::prompt_password(format!("App password for {identifier}: "))
                .context("reading password")?
        }
    };

    let client = Client::login(
        ctx.store.clone(),
        &ctx.options,
        service,
        identifier.trim().trim_start_matches('@'),
        &password,
    )
    .await?;

    let handle = client.handle().await;
    let did = client.did().await;
    let pds = client.pds_url().await;
    ctx.out.object(
        &json!({ "handle": handle.as_str(), "did": did.as_str(), "pdsUrl": pds, "sessionFile": ctx.store.path() }),
        |_| {
            format!(
                "logged in as @{handle} ({did})\npds: {pds}\nsession: {}",
                ctx.store.path().display()
            )
        },
    );
    Ok(())
}

pub async fn whoami(ctx: &Ctx, verify: bool) -> anyhow::Result<()> {
    let session = ctx.store.load()?;
    if verify {
        let client = ctx.client()?;
        let live = client
            .get(&Route::Pds, "com.atproto.server.getSession", &[])
            .await?;
        ctx.out.object(&live, |v| {
            let handle = v
                .get("handle")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?");
            let did = v
                .get("did")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?");
            format!("@{handle} ({did}) — session verified live")
        });
        return Ok(());
    }
    ctx.out.object(
        &json!({ "handle": session.handle.as_str(), "did": session.did.as_str() }),
        |_| format!("@{} ({})", session.handle, session.did),
    );
    Ok(())
}

pub async fn session(ctx: &Ctx, cmd: SessionCmd) -> anyhow::Result<()> {
    match cmd {
        SessionCmd::Show { secrets } => {
            let session = ctx.store.load()?;
            let mut value = json!({
                "sessionFile": ctx.store.path(),
                "handle": session.handle.as_str(),
                "did": session.did.as_str(),
                "pdsUrl": session.pds_url,
                "updatedAt": session.updated_at,
            });
            if secrets {
                value["accessJwt"] = json!(session.access_jwt);
                value["refreshJwt"] = json!(session.refresh_jwt);
            }
            ctx.out.object(&value, |_| {
                let age = chrono::Utc::now() - session.updated_at;
                let secrets = if secrets {
                    format!(
                        "\naccess: {}\nrefresh: {}",
                        session.access_jwt, session.refresh_jwt
                    )
                } else {
                    String::new()
                };
                format!(
                    "session: {}\naccount: @{} ({})\npds: {}\nlast refreshed: {} ({} min ago){secrets}",
                    ctx.store.path().display(),
                    session.handle,
                    session.did,
                    session.pds_url,
                    session.updated_at.to_rfc3339(),
                    age.num_minutes(),
                )
            });
            Ok(())
        }
        SessionCmd::Refresh => {
            let client = ctx.client()?;
            client.force_refresh().await?;
            ctx.out.confirm("session refreshed");
            Ok(())
        }
        SessionCmd::Delete => {
            ctx.store.delete()?;
            ctx.out.confirm("session deleted");
            Ok(())
        }
    }
}

pub async fn resolve(ctx: &Ctx, actor: &str) -> anyhow::Result<()> {
    let client = ctx.client()?;
    let did = client.resolve_actor(actor).await?;
    let http = crate::api::http_client(ctx.options.http_timeout)?;
    let doc = identity::fetch_did_doc(&http, &ctx.options.plc_url, &did).await?;
    let pds = identity::pds_endpoint(&doc);
    let value = json!({
        "did": did.as_str(),
        "pdsUrl": pds,
        "didDoc": doc,
    });
    ctx.out.object(&value, render_raw);
    Ok(())
}
