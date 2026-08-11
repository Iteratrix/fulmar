# Bluesky CLI ecosystem survey (2026-08-11)

Distilled from a research pass run for the Muse project; links verified
against repos/docs at survey time. This is the competitive landscape
fulmar is entering — and the gap it exists to close.

## The field

| Tool | DMs (list/read/send/updateRead) | Reply/quote/embeds | Cursors | JSON out | Daemon-safe auth | Status |
|---|---|---|---|---|---|---|
| [bluesky-social/goat](https://github.com/bluesky-social/goat) | none | text-only post | n/a | yes | cached token, cleartext | active (Bluesky org); "curl for atproto", not a client |
| [mattn/bsky](https://github.com/mattn/bsky) | list/read/send, **no updateRead** | yes (+images, video, facets) | **none** (fixed -n) | `--json` NDJSON | refresh-per-invocation, **no locking** → rotation races | active, v0.0.81 (06/2026) |
| [bsky-cli (atrium)](https://crates.io/crates/bsky-cli) | list/send, **no read** | **no reply/quote** | none (hardcoded) | JSON default | bsky-sdk FileStore, no locking | active; 625-line demo |
| hrbrmstr/bsky, App::bsky, bluesky_cli (Dart), bsky_tui | — | — | — | — | — | dead / niche / disqualified |

Nobody has: complete DM cycle, updateRead, cursors, lock-safe
concurrent auth. That's the whole opening.

## Facts that shape the design

- **createSession is ~100/day** at the entryway despite docs saying
  300/day ([rate limits](https://docs.bsky.app/docs/advanced-guides/rate-limits),
  [docs inconsistency #183](https://github.com/bluesky-social/bsky-docs/issues/183)).
  Session persistence isn't an optimization; it's viability.
- **Refresh tokens rotate on use.** Concurrent invocations sharing a
  chain invalidate each other → flock the session store.
- **Chat routes 501 on bsky.social** since ~2026-05 even with the
  `Atproto-Proxy: did:web:api.bsky.chat#bsky_chat` header — resolve
  the PDS/chat service from the DID doc and hit it directly
  ([sendMessage docs](https://docs.bsky.app/docs/api/chat-bsky-convo-send-message)).
  The reference client in this repo does this correctly.
- Multiple concurrent sessions per account are fine (each keeps its
  own refresh chain) — a CLI session coexists with any daemon's.
- mattn/bsky ships an MCP mode (`bsky mcp`) — convergent evidence
  that agent-driven use is where this category is heading. Prior art
  for agents driving Bluesky via CLI is thin
  ([one Claude skill, no DMs](https://claudeskills.club/skills/bluesky-by-fpl9000));
  fulmar would be early, not derivative.
- `goat` is worth having installed alongside (brew: `goat`) as a
  protocol-level debugger — at:// record fetch, PLC history, repo
  export. Complement, not competitor.
