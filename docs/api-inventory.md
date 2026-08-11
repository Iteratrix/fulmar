# Bluesky / AT Protocol API Inventory — August 2026

Design input for fulmar: the complete XRPC method surface, who serves each
method, and what auth it needs.

**Provenance.** Method list extracted from the lexicon JSON files in
`bluesky-social/atproto` @ `68aaad8` (2026-08-11) — 396 lexicon files, of
which **317 define XRPC methods** (query/procedure/subscription). Cross-checked
against the official HTTP reference (docs.bsky.app/docs/api now redirects to
**endpoints.bsky.app**, which publishes three OpenAPI specs covering only 235
endpoints — the docs lag the lexicons; the lexicon repo is authoritative).
"Public" claims for `app.bsky.*` reads were verified live against
`public.api.bsky.app` on 2026-08-11.

Column key: **Auth** — `no` (works anonymous), `opt` (works anonymous, richer
with auth), `yes` (user token), `admin` (PDS admin basic-auth token),
`svc` (inter-service auth / role). **Cur** — ✓ if the method paginates with a
`cursor`. **Serves** — which service actually implements it.

---

## 1. Services and routing (read this first)

| Service | Host | Serves |
|---|---|---|
| PDS | user's actual host, e.g. `*.host.bsky.network` | `com.atproto.server/repo/identity/sync.*`; proxies everything else |
| Entryway | `bsky.social` | fronts Bluesky-hosted PDSes: account creation, sessions, admin. Forwards most PDS calls — **but returns 501 for chat routes since ~2026-05** |
| AppView | `api.bsky.app` (authed via PDS proxy) / `public.api.bsky.app` (anon, cached) | `app.bsky.*` |
| Chat service | `api.bsky.chat` | `chat.bsky.*` |
| Video service | `video.bsky.app` | `app.bsky.video.*` |
| Relay | `bsky.network` | firehose + host enumeration parts of `com.atproto.sync.*` |
| Mod service (Ozone) | `mod.bsky.app` | `com.atproto.moderation.createReport`, `com.atproto.label.*`, `tools.ozone.*` |
| Feed generators | per-feed hosts | `app.bsky.feed.getFeedSkeleton`, `describeFeedGenerator`, `sendInteractions` |
| PLC directory | `plc.directory` | DID resolution/audit log — plain HTTP, **not XRPC** (`GET /{did}`, `/{did}/log/audit`) |

**Service proxying.** An authed client normally sends *everything* to its own
PDS. For methods the PDS doesn't implement, it sets the
`atproto-proxy` header and the PDS forwards with a short-lived signed service
JWT (`iss` = user DID, `aud` = target service DID, `lxm` = method, exp < 60s):

- AppView: `atproto-proxy: did:web:api.bsky.app#bsky_appview` (the default —
  header optional for `app.bsky.*`)
- Chat: `atproto-proxy: did:web:api.bsky.chat#bsky_chat` (**required** for all
  `chat.bsky.*`)
- Video: `atproto-proxy: did:web:video.bsky.app#bsky_video`
- Bluesky moderation: `atproto-proxy: did:plc:ar7c4by46qjdydhdevvrndac#atproto_labeler`
- A specific labeler/ozone instance: `<labeler-did>#atproto_labeler`
- A feed generator: `<feedgen-did>#bsky_fg`

Alternatively a client can mint its own service token with
`com.atproto.server.getServiceAuth` (specifying `aud` and `lxm`) and hit the
target service directly — this is how the official app talks to
`video.bsky.app`, and it's the fallback for chat when the entryway 501s
(see reference/muse-bluesky `CHAT_PROXY`).

**Entryway gotchas.** `bsky.social` fronts many PDS hosts. Sessions created
there work, but (a) chat routes 501 at the entryway — resolve the account's
real PDS from its DID document and send chat (and ideally everything) there;
(b) `createSession` is rate-limited at the entryway — docs say 30/5min and
300/day per account, but the ecosystem survey observed **~100/day enforced**.
Never design a login-per-invocation client.

**Rate limits** (docs.bsky.app advanced guide, fetched 2026-08-11):
global 3000 req / 5 min per IP at the PDS; repo writes are point-scored
(CREATE 3, UPDATE 2, DELETE 1) with 5,000 points/hour and 35,000/day per
account (≈1,666 creates/hour); `updateHandle` 10/5min, 50/day;
`createAccount` 100/5min/IP; `createSession` 30/5min, 300/day (see caveat
above); blob upload max 50 MB. AppView limits are undocumented-but-generous.

**Unauthenticated reads.** Most `app.bsky.*` reads work with no token against
`https://public.api.bsky.app/xrpc/...` (marked `no`/`opt` below; verified by
live probe). Notable exceptions found by probing: `searchPosts`/`searchPostsV2`
are **edge-blocked (HTML 403) for anonymous datacenter traffic** — route search
through the authed PDS proxy; `getKnownFollowers` is 401 anonymous;
`com.atproto.identity.resolveIdentity`, `repo.describeRepo` and
`repo.listRecords` are 501 on the public AppView (but `repo.getRecord` *is*
served there).

---

## 2. com.atproto.* — protocol layer

### com.atproto.server (25) — sessions, accounts, app passwords. Serves: PDS/entryway

| Method | Purpose | Auth | Cur |
|---|---|---|---|
| activateAccount | activate a deactivated account (migration finalization) | yes | |
| checkAccountStatus | account status during import/recovery/migration | yes | |
| confirmEmail | confirm email with emailed token | yes | |
| createAccount | create an account | no | |
| createAppPassword | create an app password | yes | |
| createInviteCode | create one invite code | yes | |
| createInviteCodes | create invite codes in bulk | admin | |
| createSession | log in with identifier + password → JWT pair (rate-limited; see §1) | no | |
| deactivateAccount | stop serving repo (migration) | yes | |
| deleteAccount | delete account with emailed token + password | yes | |
| deleteSession | log out (send **refreshJwt**, not accessJwt) | yes | |
| describeServer | server capabilities, invite requirement, links | no | |
| getAccountInviteCodes | list own invite codes | yes | |
| getServiceAuth | mint a service JWT for another service (`aud`, `lxm`) — key for direct chat/video calls | yes | |
| getSession | current session info (handle, did, email) | yes | |
| listAppPasswords | list app passwords | yes | |
| refreshSession | rotate the JWT pair (send **refreshJwt**; old refresh token is invalidated — hence file locking) | yes | |
| requestAccountDelete | email a deletion token | yes | |
| requestEmailConfirmation | email a confirmation token | yes | |
| requestEmailUpdate | request token to change email | yes | |
| requestPasswordReset | start password reset via email | no | |
| reserveSigningKey | reserve a repo signing key (migration) | no | |
| resetPassword | complete password reset with token | no | |
| revokeAppPassword | revoke an app password by name | yes | |
| updateEmail | change account email | yes | |

### com.atproto.repo (10) — record CRUD. Serves: PDS (writes always; `getRecord` also on public AppView)

| Method | Purpose | Auth | Cur |
|---|---|---|---|
| applyWrites | batch create/update/delete in one commit | yes | |
| createRecord | create a record (post, like, follow, …) | yes | |
| deleteRecord | delete a record | yes | |
| describeRepo | account + repo info incl. list of collections | no | |
| getRecord | fetch one record by repo/collection/rkey (also served by public AppView) | no | |
| importRepo | import a CAR file (migration) | yes | |
| listMissingBlobs | blobs referenced but not uploaded (migration) | yes | ✓ |
| listRecords | list records in a collection, paginated | no | ✓ |
| putRecord | create-or-update a record | yes | |
| uploadBlob | upload a blob (images etc.); must be referenced by a record within minutes or it's GC'd | yes | |

### com.atproto.identity (9) — DIDs and handles. Serves: PDS (ops), any directory-aware service (resolution)

| Method | Purpose | Auth | Cur |
|---|---|---|---|
| getRecommendedDidCredentials | DID-doc credentials for migrating in | yes | |
| refreshIdentity | ask server to re-resolve a DID/handle | no | |
| resolveDid | DID → DID document | no | |
| resolveHandle | handle → DID (works on public AppView) | no | |
| resolveIdentity | DID or handle → full verified identity (newer; **501 on public AppView**, use PDS) | no | |
| requestPlcOperationSignature | email a code for signing a PLC op | yes | |
| signPlcOperation | sign a PLC update op | yes | |
| submitPlcOperation | validate + submit PLC op to the registry | yes | |
| updateHandle | change own handle (rate-limited 10/5min) | yes | |

### com.atproto.sync (16) — repo sync / firehose. Serves: PDS (repo data), Relay (host enumeration). All public

| Method | Purpose | Auth | Cur |
|---|---|---|---|
| getBlob | fetch a blob by CID | no | |
| getBlocks | fetch repo data blocks by CID | no | |
| getCheckout | **DEPRECATED** → getRepo | no | |
| getHead | **DEPRECATED** → getLatestCommit | no | |
| getHostStatus | relay's view of an upstream host | no | |
| getLatestCommit | current commit CID + rev of a repo | no | |
| getRecord | merkle proof blocks for one record | no | |
| getRepo | full repo export as CAR (or diff since rev) — the backup verb | no | |
| getRepoStatus | hosting status of a repo (active/deactivated/taken down) | no | |
| listBlobs | blob CIDs for an account | no | ✓ |
| listHosts | upstream hosts a relay consumes (relay) | no | ✓ |
| listRepos | all repos on this host | no | ✓ |
| listReposByCollection | DIDs having records in a collection (relay/index services) | no | ✓ |
| notifyOfUpdate | **DEPRECATED** → requestCrawl | no | |
| requestCrawl | ask a relay to crawl a PDS | no | |
| subscribeRepos | **the firehose** (websocket subscription, seq cursor) | no | ✓ |

### com.atproto.moderation (1)

| Method | Purpose | Auth | Cur |
|---|---|---|---|
| createReport | report an account or record — served by mod services via PDS proxy (`#atproto_labeler`) | yes | |

### com.atproto.label (2) — Serves: labeler services (e.g. mod.bsky.app)

| Method | Purpose | Auth | Cur |
|---|---|---|---|
| queryLabels | find labels for AT-URI patterns | opt | ✓ |
| subscribeLabels | label event stream (websocket) | no | ✓ |

### com.atproto.lexicon (1)

| Method | Purpose | Auth | Cur |
|---|---|---|---|
| resolveLexicon | resolve an NSID to its published schema record | no | |

### com.atproto.admin (15) — PDS/entryway administration, admin basic-auth. Not CLI-relevant unless self-hosting

`deleteAccount`, `disableAccountInvites`, `disableInviteCodes`,
`enableAccountInvites`, `getAccountInfo`, `getAccountInfos`,
`getInviteCodes` (cursor), `getSubjectStatus`, `searchAccounts` (cursor),
`sendEmail`, `updateAccountEmail`, `updateAccountHandle`,
`updateAccountPassword`, `updateAccountSigningKey`, `updateSubjectStatus`.
All `admin` auth, served by PDS/entryway.

### com.atproto.temp (7) — explicitly temporary; don't build on these

`addReservedHandle` (admin), `checkHandleAvailability` (signup UX),
`checkSignupQueue`, `dereferenceScope` (OAuth scope refs),
`fetchLabels` (**DEPRECATED** → queryLabels/subscribeLabels),
`requestPhoneVerification`, `revokeAccountCredentials` (admin).

---

## 3. app.bsky.* — the Bluesky application. Serves: AppView (`api.bsky.app`), unauth reads on `public.api.bsky.app` unless noted

### app.bsky.actor (7)

| Method | Purpose | Auth | Cur |
|---|---|---|---|
| getPreferences | private account preferences blob | yes | |
| getProfile | one profile (viewer state added when authed) | opt | |
| getProfiles | up to 25 profiles | opt | |
| getSuggestions | suggested accounts to follow | opt | ✓ |
| putPreferences | write the preferences blob (read-modify-write with getPreferences!) | yes | |
| searchActors | full profile search | opt | ✓ |
| searchActorsTypeahead | prefix search for autocompletion | opt | |

Records (via com.atproto.repo): `app.bsky.actor.profile`, `app.bsky.actor.status`.

### app.bsky.feed (19)

| Method | Purpose | Auth | Cur | Serves |
|---|---|---|---|---|
| describeFeedGenerator | feedgen's own metadata/policies | no | | feed generator |
| getActorFeeds | feeds published by an actor | opt | ✓ | AppView |
| getActorLikes | posts liked by actor — **self only** | yes | ✓ | AppView |
| getAuthorFeed | an actor's posts/reposts (filter param: posts_with_replies, posts_no_replies, posts_with_media, posts_and_author_threads, posts_with_video) | opt | ✓ | AppView |
| getFeed | hydrated custom-feed timeline | opt | ✓ | AppView (calls feedgen) |
| getFeedGenerator | info about one feedgen | opt | | AppView |
| getFeedGenerators | info about many feedgens | opt | | AppView |
| getFeedSkeleton | raw skeleton from the feedgen itself | opt | ✓ | feed generator |
| getLikes | who liked a post | opt | ✓ | AppView |
| getListFeed | feed of posts from members of a list | opt | ✓ | AppView |
| getPostThread | thread view around a post (depth/parentHeight params) | opt | | AppView |
| getPosts | hydrate up to 25 posts by AT-URI | opt | | AppView |
| getQuotes | posts quoting a given post | opt | ✓ | AppView |
| getRepostedBy | who reposted a post | opt | ✓ | AppView |
| getSuggestedFeeds | suggested feedgens | opt | ✓ | AppView |
| getTimeline | the home timeline | yes | ✓ | AppView |
| searchPosts | post search (**anon requests edge-blocked with HTML 403 — call authed via PDS**) | yes* | ✓ | AppView/search |
| searchPostsV2 | newer search: query + structured filters (not yet in docs site) | yes* | ✓ | AppView/search |
| sendInteractions | feedback (seen/like/…) to a feed generator | yes | | feedgen via AppView |

Records: `post`, `like`, `repost`, `generator`, `threadgate` (reply controls,
rkey = post rkey), `postgate` (quote/embedding controls, rkey = post rkey).

### app.bsky.graph (24)

| Method | Purpose | Auth | Cur |
|---|---|---|---|
| getActorStarterPacks | starter packs created by actor | opt | ✓ |
| getBlocks | accounts *you* block | yes | ✓ |
| getFollowers | who follows an actor | opt | ✓ |
| getFollows | who an actor follows | opt | ✓ |
| getKnownFollowers | followers of X that *you* also follow (verified: 401 anon) | yes | ✓ |
| getList | a list + its items | opt | ✓ |
| getListBlocks | mod lists you block | yes | ✓ |
| getListMutes | mod lists you mute | yes | ✓ |
| getLists | lists created by an actor | opt | ✓ |
| getListsWithMembership | your lists + whether a given actor is in each (list-management verb) | yes | ✓ |
| getMutes | accounts you mute | yes | ✓ |
| getRelationships | follow/block relationship between one actor and others | no | |
| getStarterPack | one starter pack view | opt | |
| getStarterPacks | many starter pack views | opt | |
| getStarterPacksWithMembership | your starter packs + membership of a given actor | yes | ✓ |
| getSuggestedFollowsByActor | "similar accounts" after following someone (verified: works anon) | opt | |
| muteActor | mute an account (private, not a repo record; supports scoped mutes) | yes | |
| muteActorList | mute everyone on a list | yes | |
| muteThread | mute notifications from a thread | yes | |
| searchStarterPacks | starter pack search | opt | ✓ |
| searchStarterPacksV2 | newer starter pack search | opt | ✓ |
| unmuteActor | unmute an account | yes | |
| unmuteActorList | unmute a list | yes | |
| unmuteThread | unmute a thread | yes | |

Records: `follow`, `block`, `list`, `listitem`, `listblock`, `starterpack`,
`verification`.

### app.bsky.notification (10)

| Method | Purpose | Auth | Cur |
|---|---|---|---|
| getPreferences | notification preferences | yes | |
| getUnreadCount | unread notification count (seenAt param) | yes | |
| listActivitySubscriptions | accounts you subscribed to ("bell" notifications) | yes | ✓ |
| listNotifications | enumerate notifications (`reasons` filter, `priority`, `seenAt`) | yes | ✓ |
| putActivitySubscription | subscribe/unsubscribe to an account's posts/replies | yes | |
| putPreferences | set notification prefs (v1; superseded by V2) | yes | |
| putPreferencesV2 | set notification prefs, per-reason channels (its `chatPreference` field is deprecated → chat.bsky.notification) | yes | |
| registerPush | register a push token with a push service | yes | |
| unregisterPush | remove a push token | yes | |
| updateSeen | mark notifications seen up to a timestamp — the read-state verb | yes | |

Record: `app.bsky.notification.declaration` (who may subscribe to your activity).

### app.bsky.video (3) — Serves: video service `video.bsky.app` (direct with service token from getServiceAuth, or PDS proxy `#bsky_video`)

| Method | Purpose | Auth | Cur |
|---|---|---|---|
| getJobStatus | poll a processing job | svc | |
| getUploadLimits | remaining daily video quota | svc | |
| uploadVideo | upload video → async job → blob on your PDS | svc | |

Upload flow: `getServiceAuth` (aud = video service, lxm = uploadVideo/getUploadLimits)
→ `uploadVideo` (returns jobId) → poll `getJobStatus` until `JOB_STATE_COMPLETED`
(yields blob ref) → embed blob in a post record as `app.bsky.embed.video`.

### app.bsky.labeler (1)

| Method | Purpose | Auth | Cur |
|---|---|---|---|
| getServices | hydrated views of labeler services by DID | opt | |

Record: `app.bsky.labeler.service`.

### app.bsky.bookmark (3) — private bookmarks ("stash" storage, NOT repo records)

| Method | Purpose | Auth | Cur |
|---|---|---|---|
| createBookmark | bookmark a post | yes | |
| deleteBookmark | remove a bookmark | yes | |
| getBookmarks | list bookmarks | yes | ✓ |

### app.bsky.draft (4) — private post drafts (stash storage)

| Method | Purpose | Auth | Cur |
|---|---|---|---|
| createDraft | save a draft | yes | |
| deleteDraft | delete a draft by id | yes | |
| getDrafts | list drafts | yes | ✓ |
| updateDraft | update a draft (silently ignores unknown id) | yes | |

### app.bsky.contact (8) — phone-contact matching (mobile onboarding; skip for a CLI)

`dismissMatch`, `getMatches` (cursor), `getSyncStatus`, `importContacts`,
`removeData`, `sendNotification` (svc), `startPhoneVerification`,
`verifyPhone`. All `yes` auth, AppView.

### app.bsky.ageassurance (3) — regulatory age verification (skip)

`begin` (yes), `getConfig` (opt), `getState` (yes). AppView.

### app.bsky.embed (1)

| Method | Purpose | Auth | Cur |
|---|---|---|---|
| getEmbedExternalView | resolve URLs → enhanced external-embed data (associatedRefs) for link cards | opt | |

### app.bsky.unspecced (30) — explicitly unstable; "WILL change without notice". Serves: AppView (+ internal skeleton backends)

Client-relevant, verified working anonymously: `getConfig`,
`getTrends`, `getTrendingTopics` (legacy of getTrends), `getTaggedSuggestions`,
`getPopularFeedGenerators` (cursor), `getSuggestedFeeds`, `getSuggestedUsers`,
`getSuggestedStarterPacks`, `getSuggestedUsersForDiscover`/`ForExplore`/`ForSeeMore`,
`getOnboardingSuggestedStarterPacks`, `getSuggestedOnboardingUsers`,
`getPostThreadV2` / `getPostThreadOtherV2` (next-gen thread API — will replace
getPostThread; don't build on it yet), `getAgeAssuranceState`,
`initAgeAssurance` (yes).
Backend-only skeleton variants (hydrated by the AppView, not for clients):
`getSuggestionsSkeleton`, `getSuggestedFeedsSkeleton`,
`getSuggestedStarterPacksSkeleton`, `getOnboardingSuggestedStarterPacksSkeleton`,
`getOnboardingSuggestedUsersSkeleton`, `getSuggestedUsersSkeleton` (+ the
ForDiscover/ForExplore/ForSeeMore skeleton trio), `searchActorsSkeleton`,
`searchPostsSkeleton`, `searchStarterPacksSkeleton`, `getTrendsSkeleton`.

---

## 4. chat.bsky.* — DMs. Serves: chat service `api.bsky.chat`. ALL require auth

**Routing (the hard-won part):** every call needs
`atproto-proxy: did:web:api.bsky.chat#bsky_chat` sent to the **user's actual
PDS** — `bsky.social` entryway returns **501** for chat routes since ~2026-05
even with the header. Alternative: `getServiceAuth` + hit `api.bsky.chat`
directly. Chat requires a full session token (app passwords need the
"allow DMs" flag; OAuth needs the chat scope).

### chat.bsky.convo (22)

| Method | Purpose | Cur |
|---|---|---|
| acceptConvo | accept a chat request (moves request → accepted) | |
| addReaction | add emoji reaction to a message (idempotent) | |
| deleteMessageForSelf | delete a message from your own view | |
| getConvo | one conversation by id | |
| getConvoAvailability | can a 1-1 chat happen? returns existing convo if any, never creates | |
| getConvoForMembers | get-or-create the 1-1 convo for members — **the way to start a DM** | |
| getConvoMembers | paginated member list | ✓ |
| getLog | event log across convos since a cursor — the DM "firehose" for polling | ✓ |
| getMessages | messages in a convo, newest first | ✓ |
| getUnreadCounts | unread counts split by convo status | |
| leaveConvo | leave (direct: hides; group: removes membership) | |
| listConvoRequests | incoming chat/group-join requests | ✓ |
| listConvos | list conversations (`readState=unread`, `status=request\|accepted` filters) | ✓ |
| lockConvo | lock a group convo (owner) | |
| muteConvo | mute a convo's notifications | |
| removeReaction | remove own reaction (idempotent) | |
| sendMessage | send a message (facets supported; embed record allowed) | |
| sendMessageBatch | send up to 100 messages across convos in one call | |
| unlockConvo | unlock a group convo | |
| unmuteConvo | unmute | |
| updateAllRead | mark all convos read (with filters) | |
| updateRead | mark one convo read up to a message — **the read-state verb** | |

### chat.bsky.group (16) — group chats (new since the 2025 surveys)

`addMembers`, `approveJoinRequest`, `createGroup`, `createJoinLink`,
`disableJoinLink`, `editGroup`, `editJoinLink`, `enableJoinLink`,
`getJoinLinkPreviews` (public preview by link code), `listJoinRequests` (✓),
`listMutualGroups` (✓), `rejectJoinRequest`, `removeMembers`, `requestJoin`,
`updateJoinRequestsRead`, `withdrawJoinRequest`.
(The docs site still lists the old name `getGroupPublicInfo`; the lexicon
renamed it `getJoinLinkPreviews`.)

### chat.bsky.actor (3)

| Method | Purpose |
|---|---|
| deleteAccount | delete chat account data |
| exportAccountData | export chat data (JSONL) |
| getStatus | chat-disabled? group-adds restricted to follows? |

### chat.bsky.moderation (7) — Bluesky-staff role auth; not CLI-relevant

`getActorMetadata`, `getConvo`, `getConvoMembers` (✓), `getConvos`,
`getMessageContext`, `subscribeModEvents` (subscription), `updateActorAccess`.

### chat.bsky.notification (2)

| Method | Purpose |
|---|---|
| getPreferences | chat notification prefs (replaces the deprecated `chatPreference` in app.bsky.notification) |
| putPreferences | set chat notification prefs (partial update) |

---

## 5. tools.ozone.* — moderation-service admin tooling (67 methods). Ozone-instance auth (team member / admin roles). Out of scope for fulmar; inventoried for completeness

| Sub-namespace | n | One-line summary (method names) |
|---|---|---|
| moderation | 15 | core mod actions & subject views: `emitEvent`, `queryEvents`✓, `queryStatuses`✓, `getEvent`, `getRecord/getRecords`, `getRepo/getRepos`, `getSubjects`, `getAccountTimeline`, `getReporterStats`, `searchRepos`✓, `scheduleAction`, `listScheduledActions`✓, `cancelScheduledActions` |
| report | 14 | report-queue workflow (new 2026): `queryReports`✓, `getReport`, `getLatestReport`, `closeReports`, `createActivity`, `listActivities`✓, `queryActivities`✓, `assignModerator`, `unassignModerator`, `getAssignments`✓, `reassignQueue`, `getLiveStats`, `getHistoricalStats`✓, `refreshStats` |
| queue | 8 | mod-queue management (new 2026): `createQueue`, `updateQueue`, `deleteQueue`, `listQueues`✓, `assignModerator`, `unassignModerator`, `getAssignments`✓, `routeReports` |
| set | 6 | keyword/value sets: `upsertSet`, `deleteSet`, `addValues`, `deleteValues`, `getValues`✓, `querySets`✓ |
| safelink | 5 | URL safety rules: `addRule`, `updateRule`, `removeRule`, `queryRules`✓, `queryEvents`✓ |
| communication | 4 | email templates: `createTemplate`, `updateTemplate`, `deleteTemplate`, `listTemplates` |
| team | 4 | team members: `addMember`, `updateMember`, `deleteMember`, `listMembers`✓ |
| setting | 3 | instance settings: `upsertOption`, `removeOptions`, `listOptions`✓ |
| signature | 3 | threat signatures: `findCorrelation`, `findRelatedAccounts`✓, `searchAccounts`✓ |
| verification | 3 | blue-check issuance: `grantVerifications`, `revokeVerifications`, `listVerifications`✓ |
| hosting | 1 | `getAccountHistory`✓ |
| server | 1 | `getConfig` |

Also: `internal.bsky.actor.getProfiles` (1) — service-to-service only; ignore.

---

## 6. Methods CLI clients commonly miss (fulmar's edge)

**DM completeness** — the survey found only one CLI with working DMs and none
with read-state:
- `chat.bsky.convo.updateRead` / `updateAllRead` — without these the agent
  re-reads everything forever.
- `chat.bsky.convo.getConvoForMembers` — starting a DM from a handle (vs. only
  replying to existing convos). `getConvoAvailability` to check first.
- `chat.bsky.convo.getLog` — cursored event log; the right primitive for an
  agent polling for new messages (cheaper than listConvos+getMessages).
- `muteConvo`/`unmuteConvo`, `acceptConvo`, `listConvoRequests`,
  `addReaction`/`removeReaction`, `deleteMessageForSelf`, `sendMessageBatch`.
- The whole `chat.bsky.group.*` namespace — group chats exist now; no CLI has
  them at all.

**Graph intelligence** (all missing from every surveyed CLI):
- `getKnownFollowers` — "who that I follow follows X"; social-proof for an agent.
- `getSuggestedFollowsByActor`, `getRelationships` (bulk follow/block checks),
  `getListsWithMembership` / `getStarterPacksWithMembership` (list management
  without O(n) scans), `muteThread`/`unmuteThread`.

**Notifications done right:** `listNotifications` with `reasons` filter +
`seenAt`; `getUnreadCount`; `updateSeen` (the read-state half);
`putActivitySubscription` (subscribe to an account's posts — an agent watching
specific accounts wants this).

**Repo escape hatches** — make the whole protocol scriptable even where no
typed verb exists: `com.atproto.repo.getRecord` / `listRecords` (any
collection, any repo, no auth — this is how you read threadgates, postgates,
WhiteWind `com.whtwnd.blog.entry` records, `site.standard.*` blog records,
verifications…), `putRecord`/`createRecord` with arbitrary collection NSIDs
(how you *write* WhiteWind), `applyWrites` for atomic batches,
`sync.getRepo` for account backup, `sync.listReposByCollection` for
"who uses lexicon X".

**Preferences:** `getPreferences`/`putPreferences` is a read-modify-write on an
opaque union array — clobbering it destroys the user's saved feeds and mod
settings. A CLI exposing it raw (with jq-ability) beats every existing client.

**Media:** the three-step video flow (§ app.bsky.video) — no surveyed CLI has
video. Image upload via `uploadBlob` + `app.bsky.embed.images` with alt text.

**Search caveat:** `searchPosts` anonymous is edge-blocked (HTML 403) from
datacenter IPs — always search authed through the PDS. `searchPostsV2` /
`searchStarterPacksV2` exist in lexicons but not yet in the docs site.

**Identity:** `resolveHandle`/`resolveDid`/`resolveIdentity` + plc.directory
for the DID→PDS-host resolution the chat routing requires anyway. Expose it;
agents constantly need handle↔DID.

**Session hygiene:** `refreshSession` on the refresh JWT (rotates — flock the
file), `getSession` as a cheap validity probe, `deleteSession` on the refresh
JWT (not access), `getServiceAuth` for direct chat/video calls.

## 7. Deprecated / legacy — do not implement

- `com.atproto.sync.getCheckout` → `getRepo`; `getHead` → `getLatestCommit`;
  `notifyOfUpdate` → `requestCrawl`.
- `com.atproto.temp.fetchLabels` → `queryLabels`/`subscribeLabels` (the whole
  `temp` namespace is unstable by contract).
- `app.bsky.notification.putPreferences` (v1) → `putPreferencesV2`; and V2's
  `chatPreference` field → `chat.bsky.notification.getPreferences`/`putPreferences`.
- `app.bsky.unspecced.getTrendingTopics` → `getTrends`;
  `getTaggedSuggestions` and `getPopularFeedGenerators` are legacy discovery
  surfaces kept for old app versions.
- All `*Skeleton` unspecced endpoints: internal AppView↔backend plumbing.
- `app.bsky.unspecced.getPostThreadV2`/`OtherV2`: explicitly "WILL change
  without notice" — stick with `app.bsky.feed.getPostThread` until promoted.
- Docs-only ghost: `chat.bsky.group.getGroupPublicInfo` (renamed
  `getJoinLinkPreviews` in the lexicons).
- `com.atproto.admin.*`, `chat.bsky.moderation.*`, `tools.ozone.*`,
  `internal.bsky.*`, `app.bsky.contact.*`, `app.bsky.ageassurance.*`: not
  deprecated, but wrong audience for a user CLI.

## 8. Method counts (main-def queries + procedures + subscriptions, lexicons @ 68aaad8)

| Namespace | Methods |
|---|---|
| com.atproto.server | 25 |
| com.atproto.repo | 10 |
| com.atproto.identity | 9 |
| com.atproto.sync | 16 |
| com.atproto.moderation | 1 |
| com.atproto.label | 2 |
| com.atproto.lexicon | 1 |
| com.atproto.admin | 15 |
| com.atproto.temp | 7 |
| **com.atproto total** | **86** |
| app.bsky.actor | 7 |
| app.bsky.feed | 19 |
| app.bsky.graph | 24 |
| app.bsky.notification | 10 |
| app.bsky.video | 3 |
| app.bsky.labeler | 1 |
| app.bsky.bookmark | 3 |
| app.bsky.draft | 4 |
| app.bsky.contact | 8 |
| app.bsky.ageassurance | 3 |
| app.bsky.embed | 1 |
| app.bsky.unspecced | 30 |
| **app.bsky total** | **113** |
| chat.bsky.convo | 22 |
| chat.bsky.group | 16 |
| chat.bsky.actor | 3 |
| chat.bsky.moderation | 7 |
| chat.bsky.notification | 2 |
| **chat.bsky total** | **50** |
| tools.ozone (12 sub-namespaces) | 67 |
| internal.bsky.actor | 1 |
| **Grand total** | **317** |
