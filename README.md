# fulmar

A complete Bluesky / AT Protocol command-line client, built for
scripting and for AI agents working from headless shells — and for
any human who lives in a terminal.

*(fulmar: a gliding seabird. Working title; the binary may yet get
its true name.)*

## Why another Bluesky CLI

A survey of the field (docs/ecosystem-survey.md) found that no
existing CLI has all of:

- **The complete DM cycle** — list, read, send, *and mark read*
  (`chat.bsky.convo.updateRead`), plus the cursored cross-convo event
  log (`dm log`) that makes polling cheap. fulmar also speaks the
  brand-new group-chat lexicon.
- **Cursors on every list** — timeline, notifications, search,
  followers, everything. Page manually or `--all`.
- **Lock-safe session sharing.** AT Protocol refresh tokens rotate on
  use; two concurrent invocations sharing a session can invalidate
  each other's credentials. fulmar serializes refreshes with advisory
  file locking and double-checked re-reads, so `cron` jobs, parallel
  pipelines, and agents never sever their own auth. There's a
  two-process race test proving it.

## Install

Grab a release binary (macOS arm64/x86_64, Linux arm64/x86_64) from
GitHub Releases, or build from source:

```sh
cargo build --release   # needs Rust 1.89+
```

## Quickstart

```sh
fulmar login alice.bsky.social      # once, interactively (app password)
fulmar timeline
fulmar post "hello from fulmar"
fulmar dm send bob.bsky.social "hey!"
fulmar --help                       # the full tour
```

Use an app password (Settings → App Passwords) with DM access
enabled. Login happens **once**: it creates a session file and every
later invocation silently refreshes it.

## The session model (read this if you're wiring up an agent)

The session file (`~/.local/state/fulmar/session.json` by default) is
a *capability*: whoever holds it can act as the account, no password
needed. That's the point — an agent's environment holds the file, not
the password.

- Seed it: run `fulmar login` anywhere, then copy the file (or run
  `FULMAR_PASSWORD=... fulmar --session /path login handle` directly).
- Override the location with `--session <path>` or `$FULMAR_SESSION`.
- Health-check with `fulmar session refresh` / inspect with
  `fulmar session show`.
- When the refresh chain dies (revoked app password, seeded file gone
  stale), commands exit **3** with a message saying to re-run
  `fulmar login`. fulmar never prompts for a password and never
  retries into a rate limit — `createSession` is limited to ~100/day,
  which is exactly why sessions persist.
- Concurrency is safe. Refreshes take an exclusive `flock` on a
  sibling `.lock` file, re-read the session after acquiring it, and
  adopt another process's fresh tokens instead of double-spending a
  rotated refresh token.

## Output conventions

Every read command takes `--json`. Lists are **NDJSON** — one
server-shaped object per line — because lines pipe:

```sh
fulmar timeline --json | jq -r '.post.record.text // empty'
```

When more pages exist, the final line is `{"cursor":"..."}` (items
never have a top-level `cursor` key):

```sh
items()  { jq -c 'select(has("cursor") | not)'; }
cursor() { jq -r '.cursor // empty' | tail -1; }

page1=$(fulmar notifs --json)
next=$(echo "$page1" | cursor)
fulmar notifs --json --cursor "$next"   # resume
```

`--all` auto-paginates to exhaustion. In human mode the resume cursor
goes to stderr, so stdout stays clean content either way.

**Exit codes**: `0` success · `1` runtime error · `2` usage error ·
`3` session dead (re-run `fulmar login`) · `4` not found. Errors
always go to stderr.

## Command tour

| Family | Commands |
|---|---|
| Session | `login` `whoami` `session show/refresh/delete` `resolve` |
| Write | `post` `reply` `quote` `thread` `delete` `like/unlike` `repost/unrepost` `follow/unfollow` `block/unblock` `mute/unmute` |
| Read | `timeline` `me` `view` `posts` `profile` `followers` `following` `known-followers` `relationship` `blocks` `mutes` `search` `feed` `likes` `reposts` `quotes` `starterpacks` |
| Notifications | `notifs` (`--previews` hydrates what was liked/reposted) `notifs count` `notifs seen` `watch/unwatch` |
| DMs | `dm convos/history/send/read/log/unread/requests/accept/leave/mute/unmute/react` |
| Groups | `group create/edit/add/remove/lock/unlock/link/preview/join/withdraw/requests/approve/reject/mutual` |
| Lists | `lists` `list show/feed/create/add/remove/delete/mute/unmute/block/unblock/membership` |
| Private stash | `bookmark/unbookmark/bookmarks` `drafts` `draft save/rm` |
| Misc | `prefs get/set` `report` `backup` `blog publish` `completions` |
| Escape hatches | `record get/list/create/put/delete` · `api` |

Posting is complete: automatic facets (links, @mentions, #tags,
$cashtags — byte-correct offsets on any Unicode), up to 4 images with
alt text (auto-downscaled under the 2MB blob limit), video (uploaded
through the video service, processing awaited), link cards
(`--link URL` fetches OpenGraph title/description/thumbnail), quote
posts, reply gates (`--reply-gate followers`), and quote gates
(`--no-quotes`). `--dry-run` prints the fully built record — facets
resolved, limits validated — without posting. Replies resolve the
parent CID and thread root for you — paste any `at://` URI or
`bsky.app` URL.

## Polling patterns for agents

```sh
fulmar notifs count                  # cheap: unread notifications
fulmar dm unread                     # cheap: unread DM counts
fulmar dm log --json --cursor "$c"   # everything new across ALL convos
fulmar dm read alice.bsky.social     # mark read after processing
fulmar notifs seen                   # ditto for notifications
```

`dm log` is the efficient primitive: one call returns every chat
event since your cursor, replacing convos+per-convo-messages sweeps.
Keep the cursor; pass it next time.

## Escape hatches

Anything fulmar has no verb for is still reachable:

```sh
# Raw records in any repo (threadgates, novel lexicons, ...)
fulmar record list alice.bsky.social app.bsky.feed.like --all
cat entry.json | fulmar record create com.whtwnd.blog.entry

# Any XRPC method, correctly authed and routed
fulmar api app.bsky.actor.getSuggestions -f limit=5
fulmar api chat.bsky.convo.getLog --proxy chat -f cursor=xyz
```

## Environment

| Variable | Purpose |
|---|---|
| `FULMAR_SESSION` | Session file path (flag `--session` wins) |
| `FULMAR_PASSWORD` | Password for non-interactive `fulmar login` |
| `FULMAR_CHAT_URL` / `FULMAR_PLC_URL` / `FULMAR_VIDEO_URL` | Service base overrides (tests, self-hosting) |
| `FULMAR_TIMEOUT` | HTTP timeout in seconds (default 30) |
| `FULMAR_NATIVE_ROOTS` | `1` = verify TLS via the OS trust store instead of the bundled Mozilla roots. Only needed for a custom PDS behind a private CA; costs sandbox-friendliness on macOS (the OS verifier XPCs to `trustd`, which Seatbelt-style sandboxes deny). |

Custom PDSes need no configuration: `login` resolves your PDS
endpoint from your DID document and stores it in the session file.

## Development

```sh
cargo test          # unit + wiremock + property + integration tests
cargo clippy --all-targets -- -D warnings   # pedantic, deny(unsafe)
```

The test suite includes the failure modes that matter: hung server,
malformed body, `400 ExpiredToken` vs real 400 discrimination, the
adopt-don't-double-spend refresh race (in-process and as two real OS
processes), and property tests for facet byte offsets on multibyte
text.

### Live tests (opt-in, real network)

The mocked suite owns behavioral coverage; a small live suite checks
the wiring against real servers — real PDS, real `api.bsky.chat`
routing, real authed search. It needs a session file, not a password:

```sh
fulmar login you.bsky.social     # once
FULMAR_LIVE_SESSION=~/.local/state/fulmar/session.json cargo test --test live
```

Without the env var every live test skips silently. The default tier
is strictly read-only (it may rotate the session's tokens — that's
normal). One write test exists behind `FULMAR_LIVE_WRITE=1`: it
posts, likes, unlikes, and deletes its own post, leaving the account
as it found it — even so, point it ONLY at a dedicated test account.

**Pre-commit hook**: `git config core.hooksPath .githooks` enables
fmt/clippy/test before every commit, plus the live read tier whenever
`FULMAR_LIVE_SESSION` is exported in your shell.

**Live tests in CI**: create a dedicated test account, then add two
repository secrets — `FULMAR_TEST_IDENTIFIER` (the handle) and
`FULMAR_TEST_PASSWORD` (an app password with DM access). The `Live`
job logs in fresh each run (a session file can't be a static secret:
refresh tokens rotate on use, so a stored session would sever its own
chain on the second run) and runs the full suite including the write
tier. With the secrets absent, the job skips and stays green.

Releases are built by cargo-dist on tag push (`vX.Y.Z`).

## License

MIT OR Apache-2.0
