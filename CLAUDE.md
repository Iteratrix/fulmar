# fulmar — a Bluesky CLI in Rust

You are building a standalone command-line client for Bluesky / AT
Protocol. The goal is not "a" CLI — it's the best one on the network.
The August 2026 survey (docs/ecosystem-survey.md) found exactly one
maintained CLI with working DMs, and it's missing read-state, cursors,
and lock-safe auth. That's the gap this project closes.

## Who it's for

The primary user is an AI agent (Lumen, an autonomous resident of
Bluesky) driving it from headless shells — plus any human in a
terminal. Design consequences, in priority order:

1. **Composability is the product.** `--json` on every read command
   (NDJSON for lists — one object per line pipes better than an
   array), clean exit codes, errors to stderr, quiet success. If a
   command's output can't be piped into `jq`/`grep`/`xargs` usefully,
   it isn't done.
2. **Help text is documentation.** The agent discovers capability via
   `--help`. Every subcommand's help should teach: arguments,
   examples, the gotcha if there is one.
3. **Short to type.** Verbs over flags where reasonable
   (`fulmar dm send`, not `fulmar chat --action send`).

## Hard requirements (each learned the expensive way)

- **Full DM cycle**: list convos, read messages, send, AND
  `chat.bsky.convo.updateRead`. DMs require the atproto service-proxy
  header `Atproto-Proxy: did:web:api.bsky.chat#bsky_chat` — and since
  ~2026-05, `bsky.social` returns **501** for chat routes even with
  the header; you must resolve and hit the chat service / user's PDS
  directly. The reference client handles this correctly (see
  `CHAT_PROXY` in reference/muse-bluesky/src/client.rs and its
  comment).
- **Session persistence with file locking.** The createSession
  endpoint enforces ~100/day at the entryway (documented 300/day is
  wrong — see survey). A CLI that logs in per invocation is
  disqualified by arithmetic. Persist the session JWT pair; resume
  the refresh chain; **flock the session file** — atproto refresh
  tokens rotate on use, so two concurrent invocations sharing a chain
  can invalidate each other. No existing CLI gets this right; be the
  first.
- **Login-free operation**: the agent's environment has NO password —
  it holds a seeded session file as a capability. `fulmar login` (run
  once by a human) creates it; everything else refreshes it. When the
  refresh chain dies, fail with a message that says exactly that;
  never prompt.
- **Cursors/pagination on every list** (timeline, notifications,
  feeds, search, followers). Survey verdict: every existing CLI
  hardcodes them away.
- **Posting that's actually complete**: reply (resolve the CID from
  a bare at:// URI so the caller doesn't have to), quote, images with
  alt text, facets for links/mentions/tags.
- **WhiteWind blog publishing** (atproto record on whtwnd.com) —
  nice-to-have, the reference client has it working.

## The reference implementation

`reference/muse-bluesky/` is a battle-tested client extracted from
the Muse runtime: 27 typed async methods covering everything above,
a wiremock test suite including the refresh-and-retry and
session-restore/rotated-token-export tests, and hard-won comments
(the 501 story, ExpiredToken-vs-real-400 discrimination). It is
REFERENCE, not law: seed from it, rewrite it, or discard it for
atrium/bsky-sdk — your call with ve. If you keep it, it wants
renaming and a cleanup pass (it still carries Muse-flavored naming).
The `export_session`/`restore_session` pair was added 2026-08-11
specifically for this project's session-store design.

## Non-goals — independence is a feature

- **No Muse coupling. None.** This tool must never know Muse exists.
  The Muse project will integrate around the finished CLI later
  (a different Claude handles that; handoff protocol below).
- No daemon, no MCP server, no config beyond the session file and
  maybe a tiny config file. It's a CLI.

## Quality bar

The user's global Rust standards apply (clippy pedantic,
deny(unsafe_code), az for casts). Beyond those, from the Muse
project's testing doctrine, the parts that transfer:

- **Failure-mode tests are required**: at least one hung-server test
  and one malformed-body test asserting the typed error fires
  (wiremock + short client timeouts). The reference suite shows the
  shape.
- **The flock story needs a real concurrency test** — two processes
  (or tasks with separate file handles) racing one session file.
- Property tests where the domain begs for them: facet extraction
  (byte-offset correctness on multi-byte text!) is the classic
  Bluesky bug farm.

## Naming

`fulmar` is a working title (a gliding seabird; crates.io-free as of
2026-08-11, as are: shearwater, skywrite, atsky, murre, ciel, brant).
The final name is reserved as an offer to Lumen — the agent this is
for — so expect a possible rename before any crates.io publish.
Don't let the name burrow deeper than Cargo.toml and the readme.

## Handoff protocol

When ve says it's done: the Muse-side Claude installs it on the Mac
Mini, wires it into Lumen's environment (handbook, skills, PATH,
session-file seeding), and adapts Muse's notification high-water-mark
handling around it. None of that is your concern — ship a great CLI
with a clean `--help`, a readme, and release binaries for macOS
arm64, and your half is complete.
