//! Output conventions (see docs/cli-design.md).
//!
//! `--json` mode: NDJSON — one server-shaped object per line on
//! stdout; when a list has more pages, the final line is
//! `{"cursor":"..."}` (items never carry a top-level `cursor` key, so
//! `jq -c 'select(has("cursor") | not)'` filters items and
//! `jq -r '.cursor // empty'` extracts the resume point).
//!
//! Human mode: compact renderings; the resume cursor goes to stderr
//! so stdout stays pipeable content either way.

use serde_json::Value;

/// Sink for command output, carrying the `--json` choice.
#[derive(Debug, Clone, Copy)]
pub enum Mode {
    Json,
    Human,
}

#[derive(Debug, Clone, Copy)]
pub struct Output {
    pub mode: Mode,
}

impl Output {
    #[must_use]
    pub fn new(json: bool) -> Self {
        let mode = if json { Mode::Json } else { Mode::Human };
        Self { mode }
    }

    /// Emit one item: raw NDJSON line in JSON mode, `render`'s text in
    /// human mode.
    pub fn item(&self, value: &Value, render: impl FnOnce(&Value) -> String) {
        match self.mode {
            Mode::Json => println!("{value}"),
            Mode::Human => println!("{}", render(value)),
        }
    }

    /// Emit a whole-object result (non-list commands).
    pub fn object(&self, value: &Value, render: impl FnOnce(&Value) -> String) {
        self.item(value, render);
    }

    /// Emit the trailing resume cursor, if the server said there are
    /// more pages.
    pub fn cursor(&self, cursor: Option<&str>) {
        let Some(cursor) = cursor else { return };
        match self.mode {
            Mode::Json => println!("{}", serde_json::json!({ "cursor": cursor })),
            Mode::Human => eprintln!("more available: --cursor {cursor}"),
        }
    }

    /// Emit a bare confirmation line (write commands). Quiet in JSON
    /// mode unless a value is given.
    pub fn confirm(&self, text: &str) {
        match self.mode {
            Mode::Json => {}
            Mode::Human => println!("{text}"),
        }
    }
}

/// `handle · relative-ish timestamp` header + indented text + counts
/// + URI. Defensive about missing fields: renders what's there.
#[must_use]
pub fn render_post(value: &Value) -> String {
    let post = value.get("post").unwrap_or(value);
    let author = post
        .get("author")
        .and_then(|a| a.get("handle"))
        .and_then(Value::as_str)
        .unwrap_or("?");
    let time = post.get("indexedAt").and_then(Value::as_str).unwrap_or("");
    let text = post
        .get("record")
        .and_then(|r| r.get("text"))
        .or_else(|| post.get("value").and_then(|r| r.get("text")))
        .and_then(Value::as_str)
        .unwrap_or("");
    let uri = post.get("uri").and_then(Value::as_str).unwrap_or("");
    let likes = count(post, "likeCount");
    let reposts = count(post, "repostCount");
    let replies = count(post, "replyCount");
    let mut header = format!("@{author} · {time}");
    if value.get("reason").and_then(|r| r.get("$type")).is_some() {
        let by = value
            .get("reason")
            .and_then(|r| r.get("by"))
            .and_then(|b| b.get("handle"))
            .and_then(Value::as_str)
            .unwrap_or("?");
        header = format!("↻ reposted by @{by} — {header}");
    }
    let body = indent(text);
    format!("{header}\n{body}\n  ♡{likes} ↻{reposts} 💬{replies} · {uri}\n")
}

/// One-line profile summary for follower/search lists.
#[must_use]
pub fn render_actor(value: &Value) -> String {
    let handle = value.get("handle").and_then(Value::as_str).unwrap_or("?");
    let name = value
        .get("displayName")
        .and_then(Value::as_str)
        .unwrap_or("");
    let did = value.get("did").and_then(Value::as_str).unwrap_or("");
    if name.is_empty() {
        format!("@{handle} · {did}")
    } else {
        format!("@{handle} ({name}) · {did}")
    }
}

/// Full profile block.
#[must_use]
pub fn render_profile(value: &Value) -> String {
    let handle = value.get("handle").and_then(Value::as_str).unwrap_or("?");
    let did = value.get("did").and_then(Value::as_str).unwrap_or("?");
    let name = value
        .get("displayName")
        .and_then(Value::as_str)
        .unwrap_or("");
    let description = value
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("");
    let followers = count(value, "followersCount");
    let follows = count(value, "followsCount");
    let posts = count(value, "postsCount");
    let viewer = value.get("viewer");
    let followed_by_me = viewer
        .and_then(|v| v.get("following"))
        .is_some_and(|f| !f.is_null());
    let follows_me = viewer
        .and_then(|v| v.get("followedBy"))
        .is_some_and(|f| !f.is_null());
    let relationship = match (followed_by_me, follows_me) {
        (true, true) => "mutuals",
        (true, false) => "you follow them",
        (false, true) => "follows you",
        (false, false) => "no follow relationship",
    };
    let name = if name.is_empty() {
        String::new()
    } else {
        format!(" ({name})")
    };
    let description = if description.is_empty() {
        String::new()
    } else {
        format!("{}\n", indent(description))
    };
    format!(
        "@{handle}{name}\n{did}\n{followers} followers · {follows} following · {posts} posts · {relationship}\n{description}"
    )
}

/// One-line notification rendering.
#[must_use]
pub fn render_notification(value: &Value) -> String {
    let reason = value.get("reason").and_then(Value::as_str).unwrap_or("?");
    let author = value
        .get("author")
        .and_then(|a| a.get("handle"))
        .and_then(Value::as_str)
        .unwrap_or("?");
    let time = value.get("indexedAt").and_then(Value::as_str).unwrap_or("");
    let read = if value.get("isRead").and_then(Value::as_bool) == Some(true) {
        ' '
    } else {
        '*'
    };
    let text = value
        .get("record")
        .and_then(|r| r.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let uri = value.get("uri").and_then(Value::as_str).unwrap_or("");
    let text = truncate(text, 100);
    format!("{read} {reason:<9} @{author} · {time} · {uri}\n    {text}")
}

/// DM conversation summary line.
#[must_use]
pub fn render_convo(value: &Value) -> String {
    let id = value.get("id").and_then(Value::as_str).unwrap_or("?");
    let unread = count(value, "unreadCount");
    let members: Vec<String> = value
        .get("members")
        .and_then(Value::as_array)
        .map(|ms| {
            ms.iter()
                .filter_map(|m| m.get("handle").and_then(Value::as_str))
                .map(|h| format!("@{h}"))
                .collect()
        })
        .unwrap_or_default();
    let last = value
        .get("lastMessage")
        .and_then(|m| m.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let marker = if unread > 0 {
        format!(" [{unread} unread]")
    } else {
        String::new()
    };
    format!(
        "{id}{marker} · {}\n    {}",
        members.join(", "),
        truncate(last, 100)
    )
}

/// DM message line.
#[must_use]
pub fn render_message(value: &Value) -> String {
    let sender = value
        .get("sender")
        .and_then(|s| s.get("did"))
        .and_then(Value::as_str)
        .unwrap_or("?");
    let time = value.get("sentAt").and_then(Value::as_str).unwrap_or("");
    let text = value.get("text").and_then(Value::as_str).unwrap_or("");
    let id = value.get("id").and_then(Value::as_str).unwrap_or("");
    format!("[{time}] {sender} ({id})\n{}", indent(text))
}

/// Fallback: pretty JSON even in human mode (prefs, records, api).
#[must_use]
pub fn render_raw(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn count(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|l| format!("  {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate(text: &str, max_chars: usize) -> String {
    let flat = text.replace('\n', " ");
    if flat.chars().count() <= max_chars {
        return flat;
    }
    let mut out: String = flat.chars().take(max_chars).collect();
    out.push('…');
    out
}
