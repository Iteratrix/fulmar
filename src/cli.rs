//! The fulmar command tree.
//!
//! Grammar rules (docs/cli-design.md): flat verbs for common actions,
//! noun groups only for real families (`dm`, `group`, `session`, …).
//! Every list command takes `--limit/--cursor/--all`; every read
//! command honors `--json` (NDJSON for lists). Help text teaches:
//! arguments, an example, and the gotcha if there is one.
//!
//! Doc comments here are clap help text shown verbatim in a terminal,
//! so rustdoc backtick conventions don't apply.
#![allow(clippy::doc_markdown)]

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "fulmar",
    version,
    about = "A complete Bluesky CLI: posts, DMs, groups, cursors everywhere, lock-safe sessions",
    long_about = "A complete Bluesky / AT Protocol CLI.\n\n\
        Designed for scripting and agents: every read command takes --json (NDJSON \
        for lists — pipe into jq), errors go to stderr, cursors are first-class, and \
        authentication is a seeded session file that concurrent invocations share \
        safely (advisory file locking; refresh tokens rotate and fulmar never races \
        the chain).\n\n\
        Exit codes: 0 success · 1 runtime error · 2 usage error · \
        3 session dead (re-run `fulmar login`) · 4 not found.\n\n\
        Start with `fulmar login`, then `fulmar timeline --json | jq .`.\n\n\
        The raw-XRPC escape hatch `fulmar api <nsid>` reaches every AT Protocol \
        method, including ones fulmar has no verb for yet."
)]
pub struct Cli {
    /// Emit JSON (NDJSON for lists) instead of human-readable text.
    #[arg(long, global = true)]
    pub json: bool,

    /// Session file path (default: $FULMAR_SESSION, then
    /// ~/.local/state/fulmar/session.json).
    #[arg(long, global = true, value_name = "PATH")]
    pub session: Option<PathBuf>,

    /// Verbose logging to stderr (repeat for more).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Command,
}

/// Cursor/limit pagination, shared by every list command.
#[derive(Debug, Args, Clone)]
pub struct PageArgs {
    /// Max items per page (server-side cap usually 100).
    #[arg(long, value_name = "N")]
    pub limit: Option<u32>,

    /// Resume from a cursor printed by a previous invocation.
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,

    /// Auto-paginate to exhaustion. In --json mode no trailing cursor
    /// line is printed (there is nothing left to resume).
    #[arg(long, conflicts_with = "cursor")]
    pub all: bool,
}

/// Compose options shared by post/reply/quote.
#[derive(Debug, Args, Clone)]
pub struct ComposeArgs {
    /// Attach an image (path or URL); repeat for up to 4. Pair each
    /// with --alt in the same order.
    #[arg(long, value_name = "PATH")]
    pub image: Vec<String>,

    /// Alt text for the Nth --image. Strongly encouraged.
    #[arg(long, value_name = "TEXT")]
    pub alt: Vec<String>,

    /// Attach a video file (uploads via the video service and waits
    /// for processing).
    #[arg(long, value_name = "PATH", conflicts_with = "image")]
    pub video: Option<PathBuf>,

    /// Alt text for --video.
    #[arg(long, value_name = "TEXT")]
    pub video_alt: Option<String>,

    /// BCP-47 language tag(s) for the post text (repeatable).
    #[arg(long, value_name = "LANG")]
    pub lang: Vec<String>,

    /// Restrict who may reply: nobody, followers, following,
    /// mentioned, or list:<at-uri>. Repeatable (except nobody).
    #[arg(long, value_name = "WHO")]
    pub reply_gate: Vec<String>,

    /// Disallow quote posts of this post (writes a postgate).
    #[arg(long)]
    pub no_quotes: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Log in and create the session file (run once, by a human).
    ///
    /// The ONLY command that touches a password. Reads it from
    /// $FULMAR_PASSWORD or prompts on a TTY. Use an app password
    /// (Settings → App Passwords) with DM access if you need chat.
    /// After this, every command refreshes the stored session; no
    /// password is ever needed again until the refresh chain dies.
    #[command(after_long_help = "Examples:\n  \
        fulmar login alice.bsky.social\n  \
        FULMAR_PASSWORD=xxxx-xxxx-xxxx-xxxx fulmar login alice.bsky.social\n  \
        fulmar --session /srv/agent/session.json login alice.bsky.social")]
    Login {
        /// Handle or DID to log in as.
        identifier: String,
        /// PDS / entryway URL to authenticate against.
        #[arg(long, default_value = "https://bsky.social")]
        service: String,
    },

    /// Show the authenticated account (from the session file).
    Whoami {
        /// Round-trip getSession against the PDS to verify the
        /// session is actually alive.
        #[arg(long)]
        verify: bool,
    },

    /// Inspect or manage the session file.
    Session {
        #[command(subcommand)]
        cmd: SessionCmd,
    },

    /// Resolve a handle or DID: DID, handle, PDS endpoint, DID doc.
    #[command(after_long_help = "Examples:\n  \
        fulmar resolve alice.bsky.social\n  \
        fulmar resolve did:plc:ewvi7nxzyoun6zhxrhs64oiz --json | jq .didDoc")]
    Resolve {
        /// Handle or DID.
        actor: String,
    },

    /// Create a post. Facets (links, @mentions, #tags) are detected
    /// automatically with byte-correct offsets.
    ///
    /// Pass `-` to read text from stdin. Limits: 300 graphemes /
    /// 3000 bytes.
    #[command(after_long_help = "Examples:\n  \
        fulmar post \"hello from fulmar\"\n  \
        echo \"multi\\nline\" | fulmar post -\n  \
        fulmar post \"look:\" --image photo.jpg --alt \"a fulmar gliding\"\n  \
        fulmar post \"replies limited\" --reply-gate followers\n  \
        fulmar post \"quoting\" --quote at://did:plc:xxx/app.bsky.feed.post/yyy")]
    Post {
        /// Post text, or `-` for stdin.
        text: String,
        /// Quote another post (at:// URI or bsky.app URL).
        #[arg(long, value_name = "POST")]
        quote: Option<String>,
        #[command(flatten)]
        compose: ComposeArgs,
    },

    /// Reply to a post. The parent CID and thread root are resolved
    /// for you — just give the post.
    #[command(after_long_help = "Examples:\n  \
        fulmar reply at://did:plc:xxx/app.bsky.feed.post/yyy \"good point\"\n  \
        fulmar reply https://bsky.app/profile/alice.bsky.social/post/yyy \"nice\"")]
    Reply {
        /// Post to reply to (at:// URI or bsky.app URL).
        post: String,
        /// Reply text, or `-` for stdin.
        text: String,
        #[command(flatten)]
        compose: ComposeArgs,
    },

    /// Quote-post (sugar for `post --quote`).
    Quote {
        /// Post to quote (at:// URI or bsky.app URL).
        post: String,
        /// Post text, or `-` for stdin.
        text: String,
        #[command(flatten)]
        compose: ComposeArgs,
    },

    /// Post a thread: each argument becomes one post, chained as
    /// self-replies. Prints each post's URI in order.
    Thread {
        /// Post texts, in order (2+).
        #[arg(num_args = 2..)]
        texts: Vec<String>,
    },

    /// Delete one of your posts.
    Delete {
        /// Post to delete (at:// URI or bsky.app URL).
        post: String,
    },

    /// Like a post.
    Like {
        /// Post (at:// URI or bsky.app URL).
        post: String,
    },
    /// Remove your like from a post.
    Unlike {
        /// Post (at:// URI or bsky.app URL).
        post: String,
    },
    /// Repost a post.
    Repost {
        /// Post (at:// URI or bsky.app URL).
        post: String,
    },
    /// Remove your repost of a post.
    Unrepost {
        /// Post (at:// URI or bsky.app URL).
        post: String,
    },

    /// Follow an account.
    Follow {
        /// Handle or DID.
        actor: String,
    },
    /// Unfollow an account.
    Unfollow {
        /// Handle or DID.
        actor: String,
    },
    /// Block an account.
    Block {
        /// Handle or DID.
        actor: String,
    },
    /// Unblock an account.
    Unblock {
        /// Handle or DID.
        actor: String,
    },
    /// Mute an account (private; not visible to them).
    Mute {
        /// Handle or DID.
        actor: String,
    },
    /// Unmute an account.
    Unmute {
        /// Handle or DID.
        actor: String,
    },

    /// Your home timeline.
    #[command(
        visible_alias = "tl",
        after_long_help = "Examples:\n  \
        fulmar timeline --limit 50 --json | jq -r '.post.record.text // empty'\n  \
        fulmar timeline --json | jq -r '.cursor // empty' | tail -1   # resume point"
    )]
    Timeline {
        #[command(flatten)]
        page: PageArgs,
    },

    /// View a post and its thread (flattened, oldest first).
    #[command(visible_alias = "show")]
    View {
        /// Post (at:// URI or bsky.app URL).
        post: String,
        /// Reply depth to fetch below the post.
        #[arg(long, default_value_t = 6)]
        depth: u32,
        /// Parent height to fetch above the post.
        #[arg(long, default_value_t = 20)]
        parents: u32,
    },

    /// An account's posts (author feed).
    Posts {
        /// Handle or DID.
        actor: String,
        /// Filter: posts_with_replies, posts_no_replies,
        /// posts_with_media, posts_and_author_threads,
        /// posts_with_video.
        #[arg(long, default_value = "posts_with_replies")]
        filter: String,
        #[command(flatten)]
        page: PageArgs,
    },

    /// An account's profile, including your follow relationship.
    Profile {
        /// Handle or DID.
        actor: String,
    },

    /// Who follows an account.
    Followers {
        /// Handle or DID.
        actor: String,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Who an account follows.
    Following {
        /// Handle or DID.
        actor: String,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Followers of an account that you also follow (social proof).
    KnownFollowers {
        /// Handle or DID.
        actor: String,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Follow/block relationship between you (or --from) and actors.
    Relationship {
        /// Handles or DIDs to check (1+).
        #[arg(num_args = 1..)]
        actors: Vec<String>,
        /// Check from this actor's perspective instead of yours.
        #[arg(long, value_name = "ACTOR")]
        from: Option<String>,
    },
    /// Accounts you block.
    Blocks {
        #[command(flatten)]
        page: PageArgs,
    },
    /// Accounts you mute.
    Mutes {
        #[command(flatten)]
        page: PageArgs,
    },

    /// Search posts (or users with --users).
    ///
    /// Search is always authenticated — anonymous search is blocked
    /// at Bluesky's edge for non-residential IPs.
    #[command(after_long_help = "Examples:\n  \
        fulmar search \"seabird migration\" --limit 25\n  \
        fulmar search \"rust atproto\" --author alice.bsky.social --sort latest\n  \
        fulmar search --users \"ornithology\"")]
    Search {
        /// Query string (Lucene-ish syntax supported server-side).
        query: String,
        /// Search user profiles instead of posts.
        #[arg(long)]
        users: bool,
        /// Restrict to an author (handle or DID).
        #[arg(long, value_name = "ACTOR", conflicts_with = "users")]
        author: Option<String>,
        /// Sort: top or latest.
        #[arg(long, default_value = "latest", conflicts_with = "users")]
        sort: String,
        /// Only posts after this timestamp (RFC3339 or YYYY-MM-DD).
        #[arg(long, value_name = "TIME", conflicts_with = "users")]
        since: Option<String>,
        /// Only posts before this timestamp (RFC3339 or YYYY-MM-DD).
        #[arg(long, value_name = "TIME", conflicts_with = "users")]
        until: Option<String>,
        #[command(flatten)]
        page: PageArgs,
    },

    /// A custom feed's timeline (feed generator).
    Feed {
        /// Feed at:// URI or bsky.app feed URL.
        feed: String,
        #[command(flatten)]
        page: PageArgs,
    },

    /// Who liked a post.
    Likes {
        /// Post (at:// URI or bsky.app URL).
        post: String,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Who reposted a post.
    Reposts {
        /// Post (at:// URI or bsky.app URL).
        post: String,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Posts quoting a post.
    Quotes {
        /// Post (at:// URI or bsky.app URL).
        post: String,
        #[command(flatten)]
        page: PageArgs,
    },

    /// Notifications (default: list them).
    #[command(
        visible_alias = "notifications",
        after_long_help = "Examples:\n  \
        fulmar notifs --unread-only --json | jq -r .reason\n  \
        fulmar notifs --reason mention,reply --limit 50\n  \
        fulmar notifs count\n  \
        fulmar notifs seen        # mark everything read"
    )]
    Notifs {
        /// Filter by reason: mention, reply, quote, like, repost,
        /// follow (comma-separated).
        #[arg(long, value_name = "REASONS", value_delimiter = ',')]
        reason: Vec<String>,
        /// Only unread notifications.
        #[arg(long)]
        unread_only: bool,
        #[command(flatten)]
        page: PageArgs,
        #[command(subcommand)]
        cmd: Option<NotifsCmd>,
    },

    /// Subscribe to an account's posts (bell notifications).
    Watch {
        /// Handle or DID.
        actor: String,
        /// Also get notified for their replies.
        #[arg(long)]
        replies: bool,
    },
    /// Unsubscribe from an account's posts.
    Unwatch {
        /// Handle or DID.
        actor: String,
    },

    /// Direct messages — the full cycle: list, read, send, mark read.
    Dm {
        #[command(subcommand)]
        cmd: DmCmd,
    },

    /// Group chats: create, manage members, join links, requests.
    ///
    /// Messaging inside a group uses the same `dm` verbs — a group is
    /// a conversation; `fulmar dm send <convo-id> ...` works.
    Group {
        #[command(subcommand)]
        cmd: GroupCmd,
    },

    /// Lists created by an account.
    Lists {
        /// Handle or DID.
        actor: String,
        #[command(flatten)]
        page: PageArgs,
    },

    /// Manage a list: members, feed, moderation.
    List {
        #[command(subcommand)]
        cmd: ListCmd,
    },

    /// Bookmark a post (private — not a public record).
    Bookmark {
        /// Post (at:// URI or bsky.app URL).
        post: String,
    },
    /// Remove a bookmark.
    Unbookmark {
        /// Post (at:// URI or bsky.app URL).
        post: String,
    },
    /// Your bookmarks.
    Bookmarks {
        #[command(flatten)]
        page: PageArgs,
    },

    /// Your post drafts (private stash).
    Drafts {
        #[command(flatten)]
        page: PageArgs,
    },
    /// Save or delete a draft.
    Draft {
        #[command(subcommand)]
        cmd: DraftCmd,
    },

    /// Read or write account preferences (raw JSON).
    ///
    /// `set` replaces the ENTIRE preferences blob — always `get`,
    /// modify with jq, then `set`. Clobbering it destroys saved
    /// feeds and moderation settings.
    #[command(after_long_help = "Example (safe read-modify-write):\n  \
        fulmar prefs get > prefs.json\n  \
        jq '...edit...' prefs.json > new.json\n  \
        fulmar prefs set new.json")]
    Prefs {
        #[command(subcommand)]
        cmd: PrefsCmd,
    },

    /// Report an account or post to moderation.
    Report {
        /// Subject: handle, DID, or post URI/URL.
        subject: String,
        /// Reason: spam, violation, misleading, sexual, rude, other.
        #[arg(long)]
        reason: String,
        /// Additional details for the moderators.
        #[arg(long, value_name = "TEXT")]
        details: Option<String>,
    },

    /// Export your whole repo as a CAR file (account backup).
    Backup {
        /// Output file (default: <handle>.car).
        file: Option<PathBuf>,
    },

    /// Starter packs created by an account.
    Starterpacks {
        /// Handle or DID.
        actor: String,
        #[command(flatten)]
        page: PageArgs,
    },

    /// Publish a WhiteWind blog entry (markdown).
    Blog {
        #[command(subcommand)]
        cmd: BlogCmd,
    },

    /// Raw record access — read or write any collection in any repo.
    ///
    /// The escape hatch for lexicons fulmar has no verb for:
    /// threadgates, site.standard.* blogs, verifications, anything
    /// new. `list` works unauthenticated on public repos.
    #[command(after_long_help = "Examples:\n  \
        fulmar record get at://did:plc:xxx/app.bsky.feed.post/yyy\n  \
        fulmar record list alice.bsky.social app.bsky.feed.like --all\n  \
        cat entry.json | fulmar record create com.whtwnd.blog.entry")]
    Record {
        #[command(subcommand)]
        cmd: RecordCmd,
    },

    /// Call any XRPC method directly (the totality guarantee).
    ///
    /// Queries are GET with -f key=value params; procedures are POST
    /// with a JSON body from stdin (or -f pairs assembled into a flat
    /// object). Routed to your PDS by default; --proxy sends the
    /// atproto-proxy header (chat routes go to the chat service
    /// automatically).
    #[command(after_long_help = "Examples:\n  \
        fulmar api app.bsky.actor.getSuggestions -f limit=5\n  \
        fulmar api chat.bsky.convo.getLog --proxy chat -f cursor=xyz\n  \
        echo '{\"convoId\":\"abc\",\"message\":{\"text\":\"hi\"}}' | \
fulmar api chat.bsky.convo.sendMessage --proxy chat --post")]
    Api {
        /// Method NSID, e.g. app.bsky.feed.getTimeline.
        nsid: String,
        /// Query/body field as key=value (repeatable).
        #[arg(short = 'f', long = "field", value_name = "K=V")]
        fields: Vec<String>,
        /// Force POST (procedure). Body: stdin if piped, else -f
        /// pairs as a flat JSON object.
        #[arg(long)]
        post: bool,
        /// Service proxy: `chat`, `video`, or a raw
        /// `did:...#service_id` value.
        #[arg(long, value_name = "SERVICE")]
        proxy: Option<String>,
    },

    /// Generate shell completions.
    Completions {
        /// Shell: bash, zsh, fish, elvish, powershell.
        shell: clap_complete::Shell,
    },
}

#[derive(Debug, Subcommand)]
pub enum SessionCmd {
    /// Show session file location, account, PDS, and age. JWTs are
    /// redacted unless --secrets.
    Show {
        /// Include the JWTs (careful where you paste this).
        #[arg(long)]
        secrets: bool,
    },
    /// Force a token refresh now (health check; also useful right
    /// after seeding a session file onto a new machine).
    Refresh,
    /// Delete the session file.
    Delete,
}

#[derive(Debug, Subcommand)]
pub enum NotifsCmd {
    /// Unread notification count (cheap poll target).
    Count,
    /// Mark notifications as seen (up to now, or --at).
    Seen {
        /// RFC3339 timestamp to mark seen up to (default: now).
        #[arg(long, value_name = "TIME")]
        at: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum DmCmd {
    /// List conversations (1:1 and group).
    Convos {
        /// Only convos with unread messages.
        #[arg(long)]
        unread_only: bool,
        /// Filter by status: request or accepted.
        #[arg(long, value_name = "STATUS")]
        status: Option<String>,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Message history with an actor or convo (newest first;
    /// --reverse for oldest first).
    #[command(after_long_help = "Examples:\n  \
        fulmar dm history alice.bsky.social --limit 20 --reverse\n  \
        fulmar dm history 3kxxconvoid --json | jq -r .text")]
    History {
        /// Handle, DID, or convo id.
        who: String,
        /// Oldest first (chronological reading order).
        #[arg(long)]
        reverse: bool,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Send a message. Creates the 1:1 convo if none exists.
    #[command(after_long_help = "Examples:\n  \
        fulmar dm send alice.bsky.social \"hey!\"\n  \
        echo \"multi-line\" | fulmar dm send alice.bsky.social -")]
    Send {
        /// Handle, DID, or convo id.
        who: String,
        /// Message text, or `-` for stdin.
        text: String,
    },
    /// Mark a conversation read (updateRead — do this after reading,
    /// or unread counts lie forever).
    Read {
        /// Handle, DID, or convo id. Omit with --all to mark every
        /// conversation read.
        who: Option<String>,
        /// Mark read only up to this message id.
        #[arg(long, value_name = "MSG_ID")]
        message: Option<String>,
        /// Mark ALL conversations read.
        #[arg(long, conflicts_with_all = ["who", "message"])]
        all: bool,
    },
    /// Event log across ALL conversations since a cursor — the
    /// efficient polling primitive (one call instead of
    /// convos+messages).
    #[command(after_long_help = "Poll pattern:\n  \
        fulmar dm log --json               # first call; note trailing cursor\n  \
        fulmar dm log --cursor <c> --json  # later: only what's new")]
    Log {
        #[command(flatten)]
        page: PageArgs,
    },
    /// Unread counts (cheap poll target; pairs with `notifs count`).
    Unread,
    /// Incoming chat requests awaiting acceptance.
    Requests {
        #[command(flatten)]
        page: PageArgs,
    },
    /// Accept a chat request.
    Accept {
        /// Convo id (from `dm requests`).
        convo: String,
    },
    /// Leave a conversation.
    Leave {
        /// Handle, DID, or convo id.
        who: String,
    },
    /// Mute / unmute a conversation's notifications.
    Mute {
        /// Handle, DID, or convo id.
        who: String,
    },
    /// Unmute a conversation.
    Unmute {
        /// Handle, DID, or convo id.
        who: String,
    },
    /// Add or remove an emoji reaction to a message.
    React {
        /// Handle, DID, or convo id.
        who: String,
        /// Message id (from `dm history --json`).
        message: String,
        /// The emoji.
        emoji: String,
        /// Remove the reaction instead of adding it.
        #[arg(long)]
        remove: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum GroupCmd {
    /// Create a group chat.
    Create {
        /// Group name.
        #[arg(long)]
        name: String,
        /// Group description.
        #[arg(long)]
        description: Option<String>,
        /// Initial members (handles or DIDs).
        #[arg(long, value_name = "ACTOR")]
        member: Vec<String>,
    },
    /// Edit a group's name/description.
    Edit {
        /// Group convo id.
        convo: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
    },
    /// Add members.
    Add {
        /// Group convo id.
        convo: String,
        /// Handles or DIDs (1+).
        #[arg(num_args = 1..)]
        actors: Vec<String>,
    },
    /// Remove members.
    Remove {
        /// Group convo id.
        convo: String,
        /// Handles or DIDs (1+).
        #[arg(num_args = 1..)]
        actors: Vec<String>,
    },
    /// Lock a group (owner only): members can't be added.
    Lock {
        /// Group convo id.
        convo: String,
    },
    /// Unlock a group.
    Unlock {
        /// Group convo id.
        convo: String,
    },
    /// Create or manage a join link.
    Link {
        /// Group convo id.
        convo: String,
        /// Disable the link.
        #[arg(long, conflicts_with = "enable")]
        disable: bool,
        /// Re-enable the link.
        #[arg(long)]
        enable: bool,
    },
    /// Preview a group by join link (public, no membership needed).
    Preview {
        /// Join link URL or code.
        link: String,
    },
    /// Request to join a group by link.
    Join {
        /// Join link URL or code.
        link: String,
    },
    /// Withdraw your pending join request.
    Withdraw {
        /// Group convo id.
        convo: String,
    },
    /// Pending join requests for a group you manage.
    Requests {
        /// Group convo id.
        convo: String,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Approve a join request.
    Approve {
        /// Group convo id.
        convo: String,
        /// Handle or DID.
        actor: String,
    },
    /// Reject a join request.
    Reject {
        /// Group convo id.
        convo: String,
        /// Handle or DID.
        actor: String,
    },
    /// Groups you share with an actor.
    Mutual {
        /// Handle or DID.
        actor: String,
        #[command(flatten)]
        page: PageArgs,
    },
}

#[derive(Debug, Subcommand)]
pub enum ListCmd {
    /// A list's metadata and members.
    Show {
        /// List at:// URI.
        list: String,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Posts from a list's members.
    Feed {
        /// List at:// URI.
        list: String,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Create a list.
    Create {
        /// List name.
        name: String,
        /// Purpose: curate (default) or mod.
        #[arg(long, default_value = "curate")]
        purpose: String,
        #[arg(long)]
        description: Option<String>,
    },
    /// Add an actor to a list.
    Add {
        /// List at:// URI.
        list: String,
        /// Handle or DID.
        actor: String,
    },
    /// Remove an actor from a list.
    Remove {
        /// List at:// URI.
        list: String,
        /// Handle or DID.
        actor: String,
    },
    /// Delete a list.
    Delete {
        /// List at:// URI.
        list: String,
    },
    /// Mute everyone on a list.
    Mute {
        /// List at:// URI.
        list: String,
    },
    /// Unmute a list.
    Unmute {
        /// List at:// URI.
        list: String,
    },
    /// Block everyone on a list.
    Block {
        /// List at:// URI.
        list: String,
    },
    /// Unblock a list.
    Unblock {
        /// List at:// URI.
        list: String,
    },
    /// Your lists, with whether an actor is in each (no O(n) scan).
    Membership {
        /// Handle or DID.
        actor: String,
        #[command(flatten)]
        page: PageArgs,
    },
}

#[derive(Debug, Subcommand)]
pub enum DraftCmd {
    /// Save a draft from a JSON file or stdin.
    Save {
        /// JSON file (draft record shape); stdin if omitted.
        file: Option<PathBuf>,
    },
    /// Delete a draft by id.
    Rm {
        /// Draft id (from `fulmar drafts --json`).
        id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum PrefsCmd {
    /// Print the raw preferences JSON.
    Get,
    /// Replace the preferences blob from a JSON file or stdin.
    Set {
        /// JSON file; stdin if omitted.
        file: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
pub enum BlogCmd {
    /// Publish a WhiteWind entry from a markdown file or stdin.
    /// Prints the whtwnd.com URL.
    Publish {
        /// Markdown file; stdin if omitted.
        file: Option<PathBuf>,
        /// Entry title.
        #[arg(long)]
        title: String,
        /// Visibility: public (default), url, or author.
        #[arg(long, default_value = "public")]
        visibility: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum RecordCmd {
    /// Fetch one record by at:// URI (raw JSON).
    Get {
        /// Record at:// URI.
        uri: String,
    },
    /// List records in a collection (raw NDJSON).
    List {
        /// Repo: handle or DID.
        actor: String,
        /// Collection NSID, e.g. app.bsky.feed.post.
        collection: String,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Create a record from JSON (file or stdin); prints its at:// URI.
    Create {
        /// Collection NSID.
        collection: String,
        /// JSON file; stdin if omitted.
        file: Option<PathBuf>,
        /// Record key (server-generated if omitted).
        #[arg(long)]
        rkey: Option<String>,
    },
    /// Create-or-update a record at a specific at:// URI.
    Put {
        /// Record at:// URI (must be in your repo).
        uri: String,
        /// JSON file; stdin if omitted.
        file: Option<PathBuf>,
    },
    /// Delete a record by at:// URI (must be in your repo).
    Delete {
        /// Record at:// URI.
        uri: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
