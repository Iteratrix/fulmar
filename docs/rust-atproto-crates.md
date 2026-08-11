# Rust atproto crate survey (2026-08-11)

Research pass over crates.io, docs.rs, GitHub, and crate sources
(downloaded and grepped, not just READMEs). Focus: what changed since
May 2026, and what fulmar should depend on vs hand-roll. Companion to
docs/ecosystem-survey.md (competitive CLIs) and docs/cli-design.md
(requirements).

## TL;DR

| Layer | Verdict |
|---|---|
| Session store + flock + refresh discipline | **Hand-roll.** No crate on the ecosystem does file locking — not atrium, not jacquard. This is fulmar's core differentiator. |
| XRPC transport + chat routing | **Hand-roll** (reference client already has it correct). |
| Typed lexicon bindings | **Mostly skip.** A CLI that emits NDJSON can pass `serde_json::Value` through; type only what it manipulates. If typed coverage gets painful, `jacquard-api` (`default-features = false, features = ["bluesky"]`) is the best source — it's the only crate with chat.bsky.group, bookmark, and draft. |
| Facet extraction | **Hand-roll a port of the official TS detection** (regexes reproduced below) + `psl` for domain validation + property tests. bsky-sdk's `detection.rs` (MIT) is a good crib. |

## 1. atrium — plateaued

Repo: <https://github.com/atrium-rs/atrium> (moved from sugyan/atrium
to an org). 420 stars, 28 open issues, **last push 2026-03-26**.

| crate | version | released |
|---|---|---|
| atrium-api | 0.25.8 | 2026-03-26 |
| atrium-xrpc | 0.12.4 | 2025-12-27 |
| atrium-oauth | 0.1.7 | 2026-03-26 |
| bsky-sdk | 0.1.24 | 2026-03-26 |
| bsky-cli | 0.1.37 | 2026-03-26 |

Cadence tells the story: the 0.25.5→0.25.8 releases (Aug 2025 → Mar
2026) are dependency bumps and an MSRV bump. The last substantive
feature work was late 2025. Not dead, but coasting — and its lexicon
snapshot is frozen accordingly.

Answers to the specific questions:

- **(a) File locking: none.** `bsky-sdk`'s `FileStore`
  (`bsky-sdk/src/agent/config/file.rs`) is a plain
  `std::fs::read_to_string` / `std::fs::write` of pretty JSON. Not
  even atomic tmp+rename, let alone flock. Unchanged since the
  original survey.
- **(b) Service proxy: header only, no 501-aware routing.**
  `atrium_api::agent` has `Configure::configure_proxy_header(did,
  service_type)` and `Agent::api_with_proxy(...)` (issue #304,
  closed). It sets `atproto-proxy` on requests but does nothing about
  endpoint selection — if your session points at `bsky.social`, chat
  routes still 501. You'd still resolve the PDS yourself.
- **(c) RichText/detect_facets: good, closest to official TS.**
  `bsky-sdk/src/rich_text/detection.rs` ports the TS regexes,
  operates natively in UTF-8 byte offsets (`m.start()`/`m.end()` from
  the `regex` crate — correct by construction, no UTF-16 conversion
  bug surface), validates bare domains via the `psl` crate (Public
  Suffix List; the TS uses the npm `tlds` list — near-identical in
  practice), and replicates the strip-one-trailing-punctuation and
  closing-paren rules exactly. Missing: **cashtags** (added to TS
  detection recently — see §4). Depends on all of atrium-api.
- **(d) Weight:** atrium-api src is 2.1 MB / 347 generated files;
  features `namespace-appbsky`/`namespace-chatbsky` gate the
  namespaces (default = both). Compile time is noticeable but
  tolerable; bsky-sdk adds `psl` (large static table). Middling.
- **(e) 2026 lexicons: partial.** Has `app.bsky.bookmark`
  (create/delete/getBookmarks) and notification activity
  subscriptions (`put_activity_subscription`). **Missing:
  `chat.bsky.group` entirely, `app.bsky.draft` entirely**, and the
  newer convo methods (`get_unread_counts`, `list_convo_requests`,
  `lock_convo`/`unlock_convo` are absent from
  `atrium-api/src/chat/bsky/convo/`). The March 2026 freeze predates
  them. Using atrium today means raw-XRPC-ing exactly the new
  surfaces fulmar's design leans on.

## 2. jacquard — the real news

- crates.io: <https://crates.io/crates/jacquard>, 0.12.1
  (2026-06-26), first release 2025-10-04, ~120k downloads.
- Repo: <https://tangled.org/@nonbinary.computer/jacquard> (Tangled,
  an atproto-native forge). 130 stars, active — last commit ~July
  2026. Author: Orual (nonbinary.computer). License **MPL-2.0**
  (fine for a binary that links it).
- Cadence: 23 releases in 10 months; 0.10→0.11→0.12 in March–June
  2026 with **breaking changes each minor** — the README warns you to
  read changelogs. Used in production by Tranquil PDS, Weaver, etc.

Crate family: `jacquard` (client), `jacquard-common` (types, XRPC
traits), `jacquard-api` (generated bindings), `jacquard-oauth`,
`jacquard-identity`, `jacquard-lexicon`/`jacquard-lexgen` (codegen),
`jacquard-repo` (MST/CAR), `jacquard-axum`, `jacquard-derive`.
Edition 2024 (MSRV ≥ 1.85). Design: "borrow-or-share" string-generic
types (SmolStr default), validated spec-compliant syntax types.

What it gets right for fulmar's requirements:

- **PDS resolution built in**: `CredentialSession::login` resolves
  the DID document and stores the real PDS endpoint
  (`pds_endpoint()`), so authed calls never hit the `bsky.social`
  entryway — which means chat routes with the proxy header **work**.
- **Per-call service proxy**: `CallOptions.atproto_proxy` sets the
  `atproto-proxy` header per request
  (`src/client/credential_session.rs:877`).
- **Auto-refresh**: the send path detects expiry, calls
  `refreshSession`, persists via the store, and retries once —
  same shape as the reference client.
- **Lexicon coverage nobody else has**: `chat_bsky::group` (all 16
  methods incl. join links), `app_bsky::bookmark`, `app_bsky::draft`,
  full modern `chat_bsky::convo` (get_unread_counts,
  list_convo_requests, lock/unlock, reactions), plus `com_whtwnd`
  (WhiteWind!), `sh_tangled`, `site_standard`, and ~200 community
  namespaces — all feature-gated (`minimal` / `bluesky` / `other` /
  `lexicon_community` / `ufos`).
- Rich text: `RichText::parse` with mention/link/tag detection,
  markdown-link support, text sanitization, embed-candidate
  extraction, and async handle→DID resolution. (Fidelity caveats in
  §4 — it diverges from the TS reference.)

What it does NOT get right:

- **No file locking, no atomic writes.** `FileTokenStore`
  (`jacquard-common/src/session.rs:293`) is read-string /
  modify / `std::fs::write`, and its own docs say "NOT secure, only
  suitable for development." Every `set_value` is a full-file
  read-modify-write race. The session store trait IS pluggable, so a
  flock-based store could be supplied — but the double-checked
  re-read-under-exclusive-lock dance from cli-design.md has to wrap
  the *refresh* logic, not just storage, and jacquard's refresh path
  doesn't know about it.
- **Weight**: jacquard-api src is **46 MB / 2,432 generated files**
  with *all* namespaces; the bluesky-only subset
  (app_bsky + chat_bsky + com_atproto) is still 5.3 MB — ~2.5× the
  generated code of atrium-api. Critically, **jacquard's default
  features enable `api_full`** (every community lexicon). Anyone
  depending on it must set `default-features = false` or eat a very
  long compile.
- API churn: three breaking minors in four months. A CLI pinning it
  will need periodic migration work.

## 3. Other crates (new or grown since May 2026)

- **shrike** 0.2.0 (2026-06-23), <https://github.com/jcalabro/shrike>
  — "AT Protocol library, designed to be correct, fast, easy."
  Impressively broad for its age: syntax types, DAG-CBOR, MST, repo,
  CAR, lexicon validation, XRPC client *with retry and auth*, OAuth
  (PKCE+DPoP), identity, firehose/Jetstream, generated api for
  com.atproto / app.bsky / chat / tools.ozone. Young (0.2.0, small
  user base); one to watch, not to build on yet. Session persistence
  is in-memory `AuthInfo` — no store, no locking.
- **bsky-bot-sdk** 1.0.0 (2026-07-23) — event-driven bot framework
  built on **atrium-api 0.25.8 + bsky-sdk** (so atrium remains the
  incumbent foundation for downstream crates). Not CLI-relevant.
- **atproto-extras** 0.14.5 (2026-04-02) — standalone facet parsing
  (`parse_mentions`/`parse_urls`/`parse_tags`) with UTF-8 offsets and
  optional handle→DID resolution; part of a small independent
  `atproto-record`/`atproto-lexicon` family. Inspected: byte-regex
  based, no TLD validation, no punctuation/paren rules, tiny
  adoption (~200–550 downloads). Not a match for TS behavior — skip.
- **rustproto** 0.2.1 (2026-07-29) — actor-resolution utilities,
  tiny. **hubble-sync** (sync 1.1), **repo-stream** (CAR processing),
  **atmoq** (firehose over MoQ) — infra-side, not client-relevant.
- **tangled-cli / tangled-api** 0.1.0 (2026-07-10) — lexicon-generated
  XRPC client for the Tangled forge; evidence the jacquard-lexgen
  toolchain is being reused by others.
- GitHub topic sweep + crates.io recent-updates confirm no other
  general-purpose Rust atproto client emerged.

## 4. Facet extraction — the reference behavior to match

Official TS source (verified from
`bluesky-social/atproto/packages/api/src/rich-text/{detection,util}.ts`,
main branch, 2026-08):

```
MENTION_REGEX  /(^|\s|\()(@)([a-zA-Z0-9.-]+)(\b)/g
URL_REGEX      /(^|\s|\()((https?:\/\/[\S]+)|((?<domain>[a-z][a-z0-9]*(\.[a-z0-9]+)+)[\S]*))/gim
TRAILING_PUNCTUATION_REGEX  /\p{P}+$/gu          (tags only)
TAG_REGEX      /(^|\s)[#\uFF03]((?!\ufe0f)[^\s\u00AD\u2060\u200A\u200B\u200C\u200D\u20e2]*
                [^\d\s\p{P}\u00AD\u2060\u200A\u200B\u200C\u200D\u20e2]+
                [^\s\u00AD\u2060\u200A\u200B\u200C\u200D\u20e2]*)?/gu   (escapes shown; wrapped)
CASHTAG_REGEX  /(^|\s|\()\$([A-Za-z][A-Za-z0-9]{0,4})(?=\s|$|[.,;:!?)"'\u2019])/gu
```

Behavioral rules that trip up reimplementations:

1. **Mentions**: the captured handle must pass `isValidDomain` —
   last dot-separated label must be a known TLD from the npm `tlds`
   list (or the handle ends in `.test`). Invalid → silently skipped,
   NOT an error. The DID is resolved afterwards.
2. **Bare-domain links** (no scheme): same TLD validation, then
   `https://` is prepended. Scheme-ful URLs skip validation entirely.
3. **Link punctuation**: strip exactly **one** trailing char of
   `[.,;:!?]`, then strip a trailing `)` only if the URI contains no
   `(` (protects Wikipedia-style URLs).
4. **Tags**: trailing `\p{P}+` stripped, zero-width/soft-hyphen chars
   excluded, length limit **64 graphemes** (not bytes, not chars).
5. **Cashtags** (newer, post-cutoff for most Rust ports): `$TSLA` →
   a `#tag` facet with tag `"$TSLA"`, uppercased, 1–5 letters.
6. All indices are **UTF-8 byte offsets** (`byteStart`/`byteEnd`);
   TS computes in UTF-16 and converts — a Rust `regex` port gets
   UTF-8 offsets natively, which kills the classic multibyte bug by
   construction, but property tests should still assert offsets land
   on char boundaries and slice back to the matched text.

How the Rust implementations compare:

| | bsky-sdk 0.1.24 | jacquard 0.12.1 | atproto-extras |
|---|---|---|---|
| Domain/TLD validation | `psl` crate (≈ tlds list) | **none** — syntactic `HANDLE_REGEX` only; any domain-shaped string links | none |
| Link trailing punct | exact TS rule (one char + paren guard) | **strips ALL trailing `\p{P}+`** — mangles `…/Foo_(bar)` URLs | none |
| Tag limit | 64 | 64 **chars**, not graphemes | none |
| Cashtags | no | no | no |
| Extras | — | markdown links, sanitization, embed extraction, async mention resolution | binary tool |
| Cost of adoption | drags atrium-api | drags jacquard-common | tiny but wrong |

Nobody matches TS exactly. The detection logic is ~250 lines of Rust;
port it from the TS above, use `psl` (or vendor the `tlds` list for
byte-exact parity) for domain validation, include cashtags, and
property-test byte offsets on multibyte/emoji/ZWJ text per the
CLAUDE.md quality bar.

## 5. Competitive CLI delta since May 2026

Nothing changes the survey verdict:

- **bsky-cli** 0.1.37 (2026-03-26) — version bump only, still the
  625-line atrium demo; no reply/quote, no updateRead, no cursors.
- **Skyscraper** (Cameron Banga, ~Apr 2026, Rust) — a *TUI* client
  (interactive full-screen, Homebrew-distributed). Different category
  from a scriptable CLI; no threat to the composability niche.
- **perch** 0.3.4 (2026-08-02) — TUI for Mastodon+Bluesky. Same.
- **elsewhere** 0.1.1 (2026-08-03) — POSSE cross-posting CLI for
  static-site writers; write-only, niche.
- No new scriptable Bluesky CLI with DMs on crates.io or GitHub. The
  gap identified in docs/ecosystem-survey.md is still open.

## 6. Recommendation

**Stay hand-rolled on the core; treat jacquard as the typed-bindings
quarry, not the foundation.**

1. **Session store, flock protocol, refresh discipline: hand-roll.**
   The double-checked-locking refresh (shared-read → on 401 take
   exclusive → re-read → adopt-or-refresh → atomic write) is the
   product. No crate implements it; both atrium's and jacquard's file
   stores are racy plain writes, and wiring flock *around* a
   dependency's internal auto-refresh is harder than owning the ~200
   lines. The reference client's `export_session`/`restore_session`
   pair was built for exactly this.
2. **XRPC transport + chat routing: hand-roll** (keep the reference
   reqwest client). It already resolves the PDS and handles the
   `did:web:api.bsky.chat#bsky_chat` proxy + 501 story correctly;
   atrium wouldn't fix routing for us and jacquard would only
   replicate what we have.
3. **Typed bindings: don't take a client-crate dependency for them.**
   fulmar's read commands emit NDJSON — `serde_json::Value`
   pass-through covers most of the surface, and `fulmar api` is
   untyped by design. Define small serde structs only where fulmar
   manipulates data (post/reply/facets/session/convo ids). If a typed
   surface grows tiresome, prefer `jacquard-api` with
   `default-features = false, features = ["bluesky"]` — the only
   crate with chat.bsky.group / bookmark / draft — accepting its
   breaking-minor churn; atrium-api is frozen pre-group-chat and
   would force raw XRPC for exactly fulmar's differentiators.
4. **Facets: hand-roll the TS port** (§4), crib structure from
   bsky-sdk's `detection.rs` (MIT), add `psl`, add cashtags, property
   tests on byte offsets. This also leaves us free to adopt
   jacquard's nice extras (markdown links) behind a flag without
   inheriting its TS divergences.
5. **Watchlist**: shrike (if it matures, it's the philosophically
   closest library to fulmar's needs), jacquard 0.13+ (if the session
   internals refactor lands a pluggable-refresh hook, revisit №1's
   calculus), and atrium (if the org wakes up and regenerates
   lexicons).

Sources: crates.io API (versions/dates above, retrieved 2026-08-11);
crate sources for jacquard 0.12.1, jacquard-common 0.12.1,
jacquard-api 0.12.1, atrium-api 0.25.8, shrike 0.2.0,
atproto-extras 0.14.5 (downloaded from static.crates.io and read);
github.com/atrium-rs/atrium (commits, issues #304, #264);
github.com/bluesky-social/atproto rich-text sources;
tangled.org/@nonbinary.computer/jacquard.
