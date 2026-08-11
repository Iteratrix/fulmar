# Chassis crates — verified against crates.io, 2026-08-11

Versions and release dates pulled from the crates.io API on 2026-08-11.
Verdicts: **use** / avoid / watch.

## 1. CLI parsing

| Crate | Version | Last release | Verdict | Rationale |
|---|---|---|---|---|
| clap | 4.6.6 | 2026-08-06 | **use** | 4.6 line (since 2026-03) is current; derive API unchanged. v5 exists only as `unstable-v5` feature flags — no release on the horizon. |
| clap_complete | 4.6.9 | 2026-08-06 | **use** | Shell completions; versioned in lockstep with clap. |
| clap-markdown | 0.1.5 | 2025-05-02 | **use** | Slow-moving but works; generates markdown from the command tree. No better alternative appeared. |
| clap_mangen | 0.3.2 | 2026-08-06 | watch | Only if man pages are wanted; maintained alongside clap. |

## 2. File locking

| Crate | Version | Last release | Verdict | Rationale |
|---|---|---|---|---|
| std `File::lock` | Rust ≥1.89 | 2025-08 (stable) | **use** | Advisory flock is in std now (`lock`/`lock_shared`/`try_lock`/`unlock`). Zero deps; enough for the session file. |
| fd-lock | 4.0.4 | 2025-03-10 | **use** (alt) | RAII read/write guards over rustix flock; pick it if guard ergonomics beat std's manual unlock. |
| fs4 | 1.1.0 | 2026-04-28 | watch | fs2 fork, hit 1.0 in 2026-04, async support — fine, but redundant next to std. |
| fs2 | 0.4.3 | 2018-01-06 | avoid | Dead since 2018. |
| rustix flock | 1.1.4 | 2026-02-22 | avoid | No reason to go raw; std/fd-lock cover it. |

Note: no advisory-lock API offers atomic shared→exclusive upgrade (that's a
deadlock recipe on flock anyway). Take the exclusive lock up front whenever a
refresh might happen — which for the session file is essentially always.

## 3. Atomic file writes

| Crate | Version | Last release | Verdict | Rationale |
|---|---|---|---|---|
| tempfile | 3.27.0 | 2026-03-11 | **use** | `NamedTempFile::persist()` (same-dir rename) is the standard pattern. |
| atomicwrites | 0.4.4 | 2024-09-19 | avoid | Quiet; tempfile covers the same ground with more eyes on it. |

## 4. HTTP

| Crate | Version | Last release | Verdict | Rationale |
|---|---|---|---|---|
| reqwest | 0.13.4 | 2026-05-25 | **use** | **0.13 (2025-12-30) is a breaking release**: rustls is now the default TLS backend (aws-lc provider, platform-verifier certs), and `query`/`form` are opt-in features — enable them or `.query()`/`.form()` vanish. |
| reqwest-middleware | 0.5.2 | 2026-05-19 | watch | Tracks reqwest ^0.13.1; use only if retry middleware earns its weight vs. a hand-rolled retry in the client. |
| ureq | 3.4.0 | 2026-08-08 | avoid (here) | Confirmed sync-only by design; the DM/pagination flows and the reference client are tokio async. |

## 5. Async streams / pagination

| Crate | Version | Last release | Verdict | Rationale |
|---|---|---|---|---|
| futures (`stream::unfold`) | 0.3.34 | 2026-08-11 | **use** | Still the answer. std `AsyncIterator` remains unstable as of Aug 2026 — no movement worth waiting for. |
| async-stream | 0.3.6 | 2024-10-01 | watch | Works, stable, but macro magic buys little over `unfold` for a cursor loop. |
| tokio-stream | 0.1.19 | 2026-07-22 | watch | Only if tokio util adapters are already needed. |

## 6. Errors

| Crate | Version | Last release | Verdict | Rationale |
|---|---|---|---|---|
| thiserror | 2.0.20 | 2026-08-08 | **use** | v2 is the settled current line; very active. |
| anyhow | 1.0.104 | 2026-07-18 | **use** | Top-level in `main`/command dispatch; thiserror in the client library layer. |
| miette | 7.6.0 | 2025-04-27 | avoid (here) | Alive but slowing; fancy diagnostics are wasted on an agent parsing stderr. |

## 7. XDG / dirs

| Crate | Version | Last release | Verdict | Rationale |
|---|---|---|---|---|
| etcetera | 0.11.0 | 2025-10-28 | **use** | Most actively maintained (uv/ruff ecosystem); explicit strategy choice (XDG vs native) suits a session-file path. |
| directories | 6.0.0 | 2025-01-12 | watch | Feature-complete and fine; just quieter. |
| dirs | 6.0.0 | 2025-01-12 | watch | Lower-level sibling of directories; same story. |

## 8. Terminal output

| Crate | Version | Last release | Verdict | Rationale |
|---|---|---|---|---|
| anstream + anstyle | 1.0.0 / 1.0.14 | 2026-02-11 / 2026-03-13 | **use** | anstream hit 1.0 (2026-02). What clap uses; auto-handles NO_COLOR, CLICOLOR, tty detection — exactly the convention story needed. |
| owo-colors | 4.3.0 | 2026-02-22 | watch | Good crate, but mixing two color stacks with clap's is pointless. |
| colored | 3.1.1 | 2026-01-16 | avoid | Global mutable state; weakest of the three. |
| tabled | 0.21.0 | 2026-05-31 | **use** | Human-mode tables; active. Keep it out of `--json` paths. |

## 9. Testing

| Crate | Version | Last release | Verdict | Rationale |
|---|---|---|---|---|
| wiremock | 0.6.5 | 2025-08-24 | **use** | Still the async HTTP mock standard; server-side, so reqwest 0.13 compat is a non-issue. |
| proptest | 1.11.0 | 2026-03-24 | **use** | Active; use for facet byte-offset properties. |
| quickcheck | 1.1.0 | 2026-02-10 | avoid | First release in 5 years (revival attempt) but proptest's shrinking/strategies are still far ahead. |
| assert_cmd + predicates | 2.2.2 / 3.1.4 | 2026-05-11 / 2026-02-11 | **use** | CLI integration tests; both active. |
| trycmd / snapbox | 1.2.1 / 1.2.2 | 2026-07-21 / 2026-05-26 | **use** | Both crossed 1.0 in early 2026. trycmd for help-text snapshots is the "help is documentation" enforcement tool. |
| insta | 1.48.0 | 2026-06-11 | watch | Overlaps with snapbox here; pick one snapshot stack (trycmd/snapbox fits CLIs better). |

## 10. JSON

| Crate | Version | Last release | Verdict | Rationale |
|---|---|---|---|---|
| serde_json | 1.0.151 | 2026-07-20 | **use** | Unchallenged. |
| sonic-rs | 0.5.8 | 2026-03-25 | avoid | SIMD/unsafe-heavy for perf a CLI will never notice; tiny adoption. |

## 11. Unicode

| Crate | Version | Last release | Verdict | Rationale |
|---|---|---|---|---|
| unicode-segmentation | 1.13.3 | 2026-06-01 | **use** | Grapheme counting for the 300-grapheme limit (lexicon verified: `maxGraphemes: 300`, `maxLength: 3000` bytes). |
| (std `encode_utf16`) | — | — | **use** | UTF-16 code-unit offsets need no crate; facets themselves are UTF-8 byte offsets from `str` indices. |

## 12. Image handling

| Crate | Version | Last release | Verdict | Rationale |
|---|---|---|---|---|
| image | 0.25.10 | 2026-03-10 | **use** | Mature encode+decode across formats; resize/re-encode before upload. |
| zune-image | 0.5.0 | 2026-01-24 | avoid | Fast decoder, but ~81k total downloads and thin encoder coverage — not mature enough to bet on. |

Blob limit note: the current `app.bsky.embed.images` lexicon (checked
2026-08-11) says `maxSize: 2,000,000` bytes per image, `accept: image/*`, max
4 images — the widely cited ~1MB (976.56KB) figure is stale. Resize target
should still leave headroom under 2MB.
