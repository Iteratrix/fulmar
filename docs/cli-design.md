# fulmar CLI design (draft for review)

Status: draft, 2026-08-11. Cross-checked against docs/api-inventory.md
(lexicons @ 68aaad8); routing and auth facts cited from there.

## Conventions (apply to every command)

- **Output**: human-readable by default; `--json` on every read
  command emits NDJSON for lists (one object per line), a single
  object otherwise. Errors go to stderr, always; stdout stays clean
  for piping. Success is quiet — write commands print the created
  URI (or nothing) and exit 0.
- **Cursors**: every list command takes `--cursor <c>` and
  `--limit <n>`. In `--json` mode, when more pages exist the final
  stdout line is `{"cursor":"..."}` — items never have a top-level
  `cursor` key, so `jq -c 'select(has("cursor") | not)'` filters
  items and `jq -r '.cursor // empty' | tail -1` extracts the
  resume point. `--all` auto-paginates to exhaustion (internally an
  `impl Stream` driving cursor-in/cursor-out).
- **Exit codes**: `0` success, `1` runtime error (network, API),
  `2` usage error (clap), `3` session dead — re-run `fulmar login`
  (this code is load-bearing: agents branch on it), `4` not found
  (profile/post/convo doesn't exist).
- **Actor arguments** accept a handle (`alice.bsky.social`, leading
  `@` tolerated), a DID, or where sensible an `at://` URI.
- **Post arguments** accept `at://` URIs or `https://bsky.app/...`
  URLs; CIDs are always resolved internally, never demanded.
- **Help text teaches**: every subcommand's long help has at least
  one example and the gotcha if there is one.

## Session store

- Location: `$XDG_STATE_HOME/fulmar/session.json`
  (`~/.local/state/fulmar/session.json`), overridable by
  `$FULMAR_SESSION` then `--session <path>`.
- Contents: `did`, `handle`, `pds_url` (resolved from the DID
  document at login), `access_jwt`, `refresh_jwt`, `updated_at`.
- **Locking protocol** (the part nobody else gets right):
  - Read under shared flock; make the API call with the access JWT.
  - On ExpiredToken/401: take **exclusive** flock, **re-read the
    file** — if another process already refreshed (JWTs changed),
    adopt its tokens and retry without refreshing. Otherwise call
    `refreshSession`, write the new pair (atomic tmp+rename, still
    under the lock), release, retry.
  - Double-checked locking, because refresh tokens rotate on use
    and a stampede of refreshes kills the chain.
- `fulmar login` is the ONLY command that touches a password
  (prompt on tty, `$FULMAR_PASSWORD` for scripted seeding; app
  passwords recommended in help text). Everything else refreshes.
  A dead refresh chain exits 3 with a message naming the fix;
  never prompts.

## Command tree

### Session & identity
| command | notes |
|---|---|
| `fulmar login [IDENTIFIER]` | createSession; resolves DID doc, stores PDS URL. Human runs this once. |
| `fulmar whoami` | handle + DID from session file; `--verify` round-trips getSession. |
| `fulmar session show` | session file path, handle, DID, PDS, age. Never prints JWTs unless `--secrets`. |
| `fulmar session refresh` | force a refresh (seeding/health-check tool). |
| `fulmar session delete` | remove the file. |
| `fulmar resolve <handle-or-did>` | handle → DID → DID document / PDS endpoint. Debugging + custom-PDS visibility. |

### Writing
| command | notes |
|---|---|
| `fulmar post <text>` | facets auto-extracted (links, @mentions, #tags). `--image PATH --alt TEXT` (repeatable, ≤4), `--quote <post>`, `--lang`. `-` reads stdin. |
| `fulmar reply <post> <text>` | resolves parent CID and thread root internally. Same flags as post. |
| `fulmar quote <post> <text>` | sugar for `post --quote`. |
| `fulmar thread <text>...` | multiple args → chained self-reply thread; prints each URI. |
| `fulmar delete <post>` | own posts only (API enforces). |
| `fulmar like <post>` / `unlike <post>` | unlike finds and deletes the like record. |
| `fulmar repost <post>` / `unrepost <post>` | |
| `fulmar follow <actor>` / `unfollow <actor>` | |
| `fulmar block <actor>` / `unblock <actor>` | |
| `fulmar mute <actor>` / `unmute <actor>` | |

### Reading
| command | notes |
|---|---|
| `fulmar timeline` | cursor/limit/all per conventions. |
| `fulmar view <post>` | getPostThread flattened oldest-first; `--depth`, `--parents`. Name TBD — `show`? `thread` collides with the composer. |
| `fulmar posts <actor>` | getAuthorFeed; `--filter posts_with_replies\|posts_no_replies\|posts_with_media`. |
| `fulmar profile <actor>` | rich view incl. relationship (follows you / you follow). |
| `fulmar followers <actor>` / `following <actor>` | paginated. |
| `fulmar search <query>` | posts; `--author`, `--sort top\|latest`, `--since/--until`. Always authed via PDS — anonymous search is edge-blocked (HTML 403), see api-inventory §1. `search --users <query>` or `fulmar search-users`? TBD. |
| `fulmar feed <feed-uri-or-url>` | getFeed for custom feed generators. |
| `fulmar likes <post>` | who liked; `reposts <post>` and `quotes <post>` likewise (getLikes / getRepostedBy / getQuotes). |
| `fulmar known-followers <actor>` | followers of X that I also follow (getKnownFollowers) — social proof for the agent. |
| `fulmar relationship <actor>...` | bulk follow/block state between me and N actors (getRelationships). |
| `fulmar blocks` / `fulmar mutes` | my block/mute lists (getBlocks / getMutes), paginated. |

### Notifications
| command | notes |
|---|---|
| `fulmar notifs` | list; `--reason mention,reply,...`, `--unread-only`, cursor/limit. |
| `fulmar notifs count` | unread count (cheap poll target). |
| `fulmar notifs seen` | updateSeen — marks read up to now (`--at <rfc3339>` optional). |
| `fulmar watch <actor>` / `unwatch <actor>` | putActivitySubscription — get notified of an account's posts/replies ("bell"). No existing CLI has this; ideal for an agent tracking specific accounts. |

### DMs (complete cycle — the differentiator)
| command | notes |
|---|---|
| `fulmar dm convos` | list conversations; unread counts; cursor/limit. |
| `fulmar dm history <convo-or-actor>` | messages; accepts convo id OR handle/DID (getConvoForMembers). cursor/limit. |
| `fulmar dm send <convo-or-actor> <text>` | `-` reads stdin. |
| `fulmar dm read <convo-or-actor>` | updateRead; `--message <id>` for partial. `dm read --all` = updateAllRead. |
| `fulmar dm log [--cursor C]` | getLog — cursored event stream across ALL convos. THE agent polling primitive: one call replaces listConvos + per-convo getMessages. |
| `fulmar dm unread` | getUnreadCounts — cheap poll target, pairs with `notifs count`. |
| `fulmar dm requests` | listConvoRequests — incoming chat requests. Without surfacing these, new contacts sit in request purgatory. |
| `fulmar dm accept <convo>` | acceptConvo — move request → accepted. |
| `fulmar dm react <convo-or-actor> <message-id> <emoji>` | addReaction; `--remove` for removeReaction. Both idempotent. |

### Blog (nice-to-have)
| command | notes |
|---|---|
| `fulmar blog publish --title <t> [FILE]` | WhiteWind entry from file/stdin; prints whtwnd.com URL. |

### Escape hatch
| command | notes |
|---|---|
| `fulmar api <nsid> [-f key=val]... [--proxy chat\|video\|<did#id>] [--method get\|post]` | Generic authed XRPC call, `gh api`-style; JSON body from stdin for procedures. Raw JSON out. The totality guarantee: every lexicon method — including ones that don't exist yet — is reachable and correctly routed without a fulmar release. Typed commands are ergonomics on top of this, not the coverage boundary. |
| `fulmar record get <at-uri>` | com.atproto.repo.getRecord, raw JSON. |
| `fulmar record list <actor> <collection>` | listRecords, raw NDJSON. Covers whatever we didn't wrap. |
| `fulmar record create <collection> [FILE]` | createRecord with arbitrary JSON from file/stdin. `record put <at-uri>` / `record delete <at-uri>` likewise. This is how novel lexicons (site.standard.*, threadgates, …) stay reachable without a fulmar release. |
| `fulmar prefs get` / `fulmar prefs set [FILE]` | getPreferences / putPreferences, raw JSON for jq surgery. Help text warns: put replaces the WHOLE blob — always get, modify, put. |

### Group chats (`chat.bsky.group` — brand new, no CLI has these)
Messaging inside a group convo reuses the `dm` verbs (send/history/
read/log all take convo ids; group convos must render sanely there).
Group *management* gets its own family:

| command | notes |
|---|---|
| `fulmar group create --name <n> [--description]` | createGroup. |
| `fulmar group edit <convo>` | editGroup (`--name`, `--description`, `--avatar`). |
| `fulmar group add <convo> <actor>...` / `group remove <convo> <actor>...` | addMembers / removeMembers. |
| `fulmar group leave <convo>` | leaveConvo (shared with DMs; alias here for discoverability). |
| `fulmar group lock <convo>` / `unlock <convo>` | owner-only. |
| `fulmar group link <convo>` | createJoinLink; `--disable`, `--enable`, `--edit` variants. |
| `fulmar group preview <link>` | getJoinLinkPreviews — public info before joining. |
| `fulmar group join <link>` / `group withdraw <convo>` | requestJoin / withdrawJoinRequest. |
| `fulmar group requests <convo>` | listJoinRequests; `group approve <convo> <actor>` / `group reject <convo> <actor>`. |
| `fulmar group mutual <actor>` | listMutualGroups — groups shared with an actor. |

### Lists
| command | notes |
|---|---|
| `fulmar lists <actor>` | getLists — lists an actor created. |
| `fulmar list show <list-uri>` | getList — metadata + members, paginated. |
| `fulmar list feed <list-uri>` | getListFeed — posts from members. |
| `fulmar list create <name> [--purpose curate\|mod] [--description]` | list record. |
| `fulmar list add <list-uri> <actor>` / `list remove <list-uri> <actor>` | listitem records (remove finds the rkey). |
| `fulmar list delete <list-uri>` | deletes the list record. |
| `fulmar list mute <list-uri>` / `unmute` | muteActorList. |
| `fulmar list block <list-uri>` / `unblock` | listblock record. |
| `fulmar list membership <actor>` | getListsWithMembership — my lists, flagged by whether actor is in each. No O(n) scans. |

### Bookmarks & drafts (private stash — NOT repo records; unreachable via `record`)
| command | notes |
|---|---|
| `fulmar bookmark <post>` / `unbookmark <post>` | createBookmark / deleteBookmark. |
| `fulmar bookmarks` | getBookmarks, paginated. |
| `fulmar drafts` | getDrafts, paginated. |
| `fulmar draft save [FILE]` / `draft rm <id>` | createDraft/updateDraft (JSON body) / deleteDraft. Thin: drafts are app-shaped, but stash data should never be CLI-invisible. |

### Media, moderation, misc
| command | notes |
|---|---|
| `fulmar post --video PATH --alt TEXT` | full 3-step flow internally: getServiceAuth → uploadVideo → poll getJobStatus → embed. `--wait-timeout` for the poll. |
| `fulmar post --reply-gate <who>` | threadgate record (same rkey as post): `followers`, `following`, `mentioned`, `list:<uri>`, `none`. `--no-quotes` writes a postgate. |
| `fulmar report <at-uri-or-actor> --reason spam\|violation\|...` | createReport via labeler proxy. |
| `fulmar backup [FILE]` | sync.getRepo → CAR file (`--since <rev>` for incremental). |
| `fulmar starterpacks <actor>` | getActorStarterPacks; `fulmar starterpack <uri>` shows one. Creation stays post-1.0. |

### Excluded — wrong audience, not effort (see api-inventory §5, §7)

- `com.atproto.admin.*`, `tools.ozone.*`, `chat.bsky.moderation.*` —
  server/mod-service operator tooling.
- `app.bsky.contact.*`, `app.bsky.ageassurance.*`, `registerPush` —
  mobile-app plumbing, meaningless from a shell.
- Account migration & PLC key ops (`activateAccount`,
  `signPlcOperation`, …) — dangerous, niche, and `fulmar api` reaches
  them if a power user insists.
- Deprecated methods (inventory §7) and `app.bsky.unspecced.*` —
  unstable by contract.
- `subscribeRepos` (firehose) — different runtime shape (long-lived
  websocket, CBOR/CAR decoding). Post-1.0 candidate as
  `fulmar firehose` streaming NDJSON; agents poll `dm log` +
  `notifs` in the meantime.

## Open questions

1. `view` vs `show` vs splitting `thread` (read) / `thread post`
   (write) — naming collision to resolve.
2. Trailing `{"cursor":...}` line vs cursor on stderr vs
   `--cursor-file`. Draft picks the trailing line.
3. `notifs` vs `notifications` (alias both?).
4. Does `search --users` deserve its own subcommand?
5. `fulmar api` flag grammar: mirror `gh api` (`-f`/`-F`, stdin body)
   exactly, or simplify? Mirroring buys familiarity for agents
   trained on gh.
