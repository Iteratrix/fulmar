//! Command dispatch. Each family lives in its own module and takes a
//! [`Ctx`]; the clap tree in [`crate::cli`] stays declarative.

mod dm;
mod group;
mod list_cmds;
mod misc;
mod notifs;
mod read;
mod session;
mod stash;
pub mod util;
mod write;

use crate::api::{Client, ClientOptions};
use crate::cli::{Cli, Command};
use crate::output::Output;
use crate::session::SessionStore;

/// Per-invocation context: session store handle, client knobs, and
/// the output sink. The HTTP client is built lazily — commands that
/// never touch the network (completions, `session show`) shouldn't
/// require a session file.
pub struct Ctx {
    pub store: SessionStore,
    pub options: ClientOptions,
    pub out: Output,
}

impl Ctx {
    /// Build the authed client from the session file.
    ///
    /// # Errors
    ///
    /// [`crate::api::ApiError::Session`] when the session file is
    /// missing (exit code 3) or unreadable.
    pub fn client(&self) -> Result<Client, crate::api::ApiError> {
        Client::from_store(self.store.clone(), &self.options)
    }
}

/// Run a parsed CLI invocation.
///
/// # Errors
///
/// Any command failure; `main` maps the chain to exit codes.
#[allow(clippy::too_many_lines)] // pure dispatch table; splitting it would obscure the grammar
pub async fn run(cli: Cli) -> anyhow::Result<()> {
    let Cli {
        json,
        session,
        verbose: _,
        command,
    } = cli;
    let ctx = Ctx {
        store: SessionStore::resolve(session)?,
        options: ClientOptions::from_env(),
        out: Output::new(json),
    };

    match command {
        Command::Login {
            identifier,
            service,
        } => session::login(&ctx, &identifier, &service).await,
        Command::Whoami { verify } => session::whoami(&ctx, verify).await,
        Command::Session { cmd } => session::session(&ctx, cmd).await,
        Command::Resolve { actor } => session::resolve(&ctx, &actor).await,

        Command::Post {
            text,
            quote,
            link,
            dry_run,
            compose,
        } => {
            write::post(
                &ctx,
                &text,
                quote.as_deref(),
                link.as_deref(),
                dry_run,
                &compose,
            )
            .await
        }
        Command::Reply {
            post,
            text,
            compose,
        } => write::reply(&ctx, &post, &text, &compose).await,
        Command::Quote {
            post,
            text,
            compose,
        } => write::post(&ctx, &text, Some(&post), None, false, &compose).await,
        Command::Thread { texts } => write::thread(&ctx, &texts).await,
        Command::Delete { post } => write::delete_post(&ctx, &post).await,
        Command::Like { post } => write::like(&ctx, &post).await,
        Command::Unlike { post } => write::unlike(&ctx, &post).await,
        Command::Repost { post } => write::repost(&ctx, &post).await,
        Command::Unrepost { post } => write::unrepost(&ctx, &post).await,
        Command::Follow { actor } => write::follow(&ctx, &actor).await,
        Command::Unfollow { actor } => write::unfollow(&ctx, &actor).await,
        Command::Block { actor } => write::block(&ctx, &actor).await,
        Command::Unblock { actor } => write::unblock(&ctx, &actor).await,
        Command::Mute { actor } => write::mute(&ctx, &actor).await,
        Command::Unmute { actor } => write::unmute(&ctx, &actor).await,

        Command::Timeline { page } => read::timeline(&ctx, &page).await,
        Command::Me { filter, page } => read::me(&ctx, &filter, &page).await,
        Command::View {
            post,
            depth,
            parents,
        } => read::view(&ctx, &post, depth, parents).await,
        Command::Posts {
            actor,
            filter,
            page,
        } => read::author_feed(&ctx, &actor, &filter, &page).await,
        Command::Profile { actor } => read::profile(&ctx, &actor).await,
        Command::Followers { actor, page } => {
            read::graph_list(
                &ctx,
                "app.bsky.graph.getFollowers",
                "followers",
                &actor,
                &page,
            )
            .await
        }
        Command::Following { actor, page } => {
            read::graph_list(&ctx, "app.bsky.graph.getFollows", "follows", &actor, &page).await
        }
        Command::KnownFollowers { actor, page } => read::known_followers(&ctx, &actor, &page).await,
        Command::Relationship { actors, from } => {
            read::relationship(&ctx, &actors, from.as_deref()).await
        }
        Command::Blocks { page } => {
            read::self_graph(&ctx, "app.bsky.graph.getBlocks", "blocks", &page).await
        }
        Command::Mutes { page } => {
            read::self_graph(&ctx, "app.bsky.graph.getMutes", "mutes", &page).await
        }
        Command::Search {
            query,
            users,
            author,
            sort,
            since,
            until,
            page,
        } => {
            if users {
                read::search_users(&ctx, &query, &page).await
            } else {
                read::search_posts(
                    &ctx,
                    &query,
                    author.as_deref(),
                    &sort,
                    since.as_deref(),
                    until.as_deref(),
                    &page,
                )
                .await
            }
        }
        Command::Feed { feed, page } => read::feed(&ctx, &feed, &page).await,
        Command::Likes { post, page } => {
            read::post_engagement(&ctx, "app.bsky.feed.getLikes", "likes", &post, &page).await
        }
        Command::Reposts { post, page } => {
            read::post_engagement(
                &ctx,
                "app.bsky.feed.getRepostedBy",
                "repostedBy",
                &post,
                &page,
            )
            .await
        }
        Command::Quotes { post, page } => {
            read::post_engagement(&ctx, "app.bsky.feed.getQuotes", "posts", &post, &page).await
        }
        Command::Starterpacks { actor, page } => read::starterpacks(&ctx, &actor, &page).await,

        Command::Notifs {
            reason,
            unread_only,
            previews,
            page,
            cmd,
        } => notifs::notifs(&ctx, &reason, unread_only, previews, &page, cmd).await,
        Command::Watch { actor, replies } => notifs::watch(&ctx, &actor, replies).await,
        Command::Unwatch { actor } => notifs::unwatch(&ctx, &actor).await,

        Command::Dm { cmd } => dm::dm(&ctx, cmd).await,
        Command::Group { cmd } => group::group(&ctx, cmd).await,

        Command::Lists { actor, page } => list_cmds::lists(&ctx, &actor, &page).await,
        Command::List { cmd } => list_cmds::list(&ctx, cmd).await,

        Command::Bookmark { post } => stash::bookmark(&ctx, &post).await,
        Command::Unbookmark { post } => stash::unbookmark(&ctx, &post).await,
        Command::Bookmarks { page } => stash::bookmarks(&ctx, &page).await,
        Command::Drafts { page } => stash::drafts(&ctx, &page).await,
        Command::Draft { cmd } => stash::draft(&ctx, cmd).await,

        Command::Prefs { cmd } => misc::prefs(&ctx, cmd).await,
        Command::Report {
            subject,
            reason,
            details,
        } => misc::report(&ctx, &subject, &reason, details.as_deref()).await,
        Command::Backup { file } => misc::backup(&ctx, file).await,
        Command::Blog { cmd } => misc::blog(&ctx, cmd).await,
        Command::Record { cmd } => misc::record(&ctx, cmd).await,
        Command::Api {
            nsid,
            fields,
            post,
            proxy,
        } => misc::api(&ctx, &nsid, &fields, post, proxy.as_deref()).await,
        Command::Completions { shell } => {
            misc::completions(shell);
            Ok(())
        }
    }
}
