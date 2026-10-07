# Plan 025 — M005-F Downstream IRCv3 Protocol Polish — Closure

Closed 2026-10-10. Outcome: **closed, no open findings**.

## Outcome

Five capabilities were promoted from "implemented but withheld" to advertised and
honoured, and the boundary between them was made explicit: each is negotiated **per
session**, and a capability one client holds must not change what another client on the
same Network receives.

| Capability | Advertised when | Asserts |
|---|---|---|
| `server-time` | always | a `time` tag reaches only a session that negotiated it |
| `standard-replies` | always | a refusal is `FAIL` only for a session that negotiated it, a numeric otherwise |
| `draft/no-implicit-names` | always | the membership block is omitted for the session that declined it and delivered to every other |
| `cap-notify` | always | a change in what this bouncer serves reaches only the sessions that asked |
| `echo-message` | upstream negotiated it | the upstream echo is the confirmation event, recorded once, as outgoing |

The substantive work was not the advertisement changes. It was that the live reader and
the owner were answering `CAP` from two different sources, and that a `time` tag and a
`FAIL` line were previously unconditional.

## What was landed

### One advertisement, not two

Before this plan the live `SessionReader` answered `CAP LS` and `CAP REQ` from the static
`downstream_supported()` list, while `DownstreamCapabilities::advertisement(upstream)`
computed the real one — and the real one was called from **tests only**.
`echo-message` was therefore advertised nowhere, despite the code that could serve it
existing since M003.

The advertisement now lives in one cell shared by the owner and the reader:

```text
SessionHandle.advertised: Arc<Mutex<Vec<String>>>
```

The owner seeds it at attach and at adoption — adoption matters because a transferred
reader was created during admission, when this Network's upstream negotiation was not yet
known — and replaces it when the upstream negotiation changes. `CAP LS`, `CAP REQ` and the
`005` welcome now answer from one source, and `NetworkSnapshot.advertisement` publishes
the same value because "why did my client not get `echo-message`" is a diagnostic
question.

### `server-time` as a separate permission from `message-tags`

`message-tags` says a frame may begin with a tag prefix the client can parse;
`server-time` says that prefix may carry *this* tag. They are different permissions, and
the fanout had exactly two states — all tags or none. `TagSurface` makes it three:

| | `message-tags` | `server-time` | receives |
|---|---|---|---|
| `None` | no | — | no tags |
| `WithoutTime` | yes | no | every tag **except** `time` |
| `All` | yes | yes | every tag |

The middle row is the load-bearing one. Stripping all tags would remove tags the client
*did* ask for; sending all tags would hand it one it never negotiated, in a frame whose
first bytes it may not be able to parse.

The same negotiation gates history replay (`execute_for`), so a `CHATHISTORY` reply for a
session without `server-time` carries no tags at all. **No live frame is ever stamped**:
an upstream `time` is forwarded when present and the tag is absent when not, because a
time on a live frame would be a claim about upstream delivery the bouncer never received.

### `standard-replies` negotiated per session

`render_refusal_for` sends `FAIL <COMMAND> <CODE> <subcommand> <params> :<reason>` to a
session that negotiated the capability and a real RFC 1459 numeric to one that did not,
with the same reason in both. The numeric is coarser because there is no command field to
correlate against — the honest cost of not having negotiated the capability.

Every reason is a fixed constant. A refusal that echoed the request would turn every
bound that is not met into a way to put arbitrary client text into a frame the Operator
sees.

### `draft/no-implicit-names` per session

`project_channel` omits `353`/`366` when *that* session negotiated the capability. One
client declining says nothing about what another client on the same connection wants.

### `cap-notify`

Three decisions define it:

1. **The report is the advertisement delta, not the server's announcement.** A server may
   newly offer a capability the bouncer does not implement; telling a client about it would
   advertise something no client could ever get. Diffing what this bouncer serves is both
   correct and idempotent.
2. **Both directions are emitted.** `echo-message` disappears on `CAP DEL` and appears once
   the bouncer holds it.
3. **Only sessions that negotiated it are addressed.**

For `echo-message` to appear mid-generation the bouncer must ask: `CAP NEW` records an
offer, only `ACK` enables it. The owner issues a bounded `CAP REQ` for announced names it
serves and has not enabled, capped by that announcement's names and skipping anything
already enabled.

`note_change` finds the subcommand **by position**, because the nick is an optional first
parameter.

### `echo-message` confirmation

`HistoryJournal::direction_of` records `Outbound` when the prefix nick folds-equal to this
Network's own nick **and** the frame carries a `time`. The `time` requirement is what
makes it conservative: without one there is no evidence the frame is an echo rather than a
late conversation line, and guessing would put someone else's words in the Operator's own
outbound history.

The judgement needs no content matching — a message from our own nick arriving *from the
server* can only be the server echoing what it accepted.

### A `CAP` line is never fanned out

An upstream `CAP` line is the bouncer's negotiation with the server. It is consumed and
never relayed to a local client.

## Defects found during implementation

Six, four of them found by the qualification suite rather than by reading the code:

1. **Upstream `CAP` lines were fanned out to clients verbatim.** With `cap-notify`
   implemented, an upstream `CAP * NEW :echo-message` reached every client under the
   upstream's own prefix, which a local client reads as the server addressing it — and
   which discloses the upstream connection's shape to every attached Operator. Found by
   the test that negotiated `cap-notify` and checked what the *other* client saw.
2. **`note_change` read the subcommand from the wrong index**, so `CAP * NEW :x` parsed as
   having no subcommand and every announcement a server sends with its own nick attached
   was silently ignored. With `cap-notify` implemented, nothing was ever reported.
3. **The live reader answered `CAP` from a static list**, so `echo-message` was advertised
   nowhere. Found because the first M005-F test asked a live client for `CAP LS`.
4. **`FAIL` echoed the command name in the subcommand field**, telling the client its own
   command back twice and never saying which of the six subcommands failed.
5. **`server-time` was promoted before the per-session filter existed**, which would have
   delivered a `time` tag to any client with `message-tags`. The promotion and the filter
   landed together; had they not, the defect would have been invisible, because a client
   receiving an extra tag is indistinguishable from a client receiving what it wanted.
6. **A self-deadlock in `watch`.** `snapshot.borrow()` was held across `attach_session`,
   which itself calls `send_modify`. `watch` shares one lock between reads and writes and
   `std::sync::RwLock` is not reentrant for read-then-write on one thread, so two
   `corrective_019` tests **hung for over 60 seconds rather than failing**. The
   advertisement is now threaded into `handle_command` as a parameter instead of read
   through the snapshot.

Defect 6 is recorded in full because of how it presented: a hang is not a failure, so it
does not appear in a test report, and the workspace run had to be interrupted rather than
read. Any future code that reads a `watch` value and then writes to the same channel in
the same scope is the same bug.

## Recorded limits

1. **`echo-message` requires the upstream to have negotiated it**, both to advertise and to
   confirm. Without an upstream echo there is nothing to advertise.
2. **A `CAP NEW` from the server does not by itself change what a client may negotiate.**
   The bouncer must ask and receive an `ACK`. A `NEW` for a capability it does not serve is
   not reported at all, because reporting it would advertise something unobtainable.
3. **A mid-generation `CAP REQ` is bounded** by the names on the single announcement that
   triggered it and skips anything already enabled, so a server repeating `NEW` cannot make
   it grow.
4. **`TAG_SURFACE` is decided at fanout time from the session's negotiated set**, not from
   what the client last `CAP REQ`'d. A client that negotiates `server-time` after a frame is
   already in flight receives that frame untagged, which is the only ordering the stream
   allows.
5. **No `time` tag is ever fabricated on a live frame.** Synthesis is confined to history
   replay, where there is a documented convention for a message the upstream never stamped.
6. **No `msgid` synthesis.** A `msgid` on a replayed message is emitted only when one was
   genuinely preserved upstream, unchanged from the pre-M005 plan.
7. **`away-notify` and `account-notify` remain unserved**, and an upstream announcement
   about either is not reported — the bouncer does not implement them, so there is nothing a
   client could do with the news.

## Verification

- `./scripts/check-network-boundary.py` — pass
- `cargo fmt --all -- --check` — pass
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — pass
- `cargo test -p i2pr-irc-wire` — pass (39 + 7)
- `cargo test -p i2pr-irc-runtime --lib` — pass (223)
- `cargo test -p i2pr-irc-runtime --test m005f_protocol_polish` — pass (11, all new)
- `cargo test -p i2pr-irc-runtime --test m005e_search_history` — pass (15, from Plan 024)
- `cargo test -p i2pr-irc-runtime --test chathistory` — pass (19)
- `cargo test --workspace --all-features` — pass (30 suites)
- `./scripts/verify.sh quick` — pass
- `./scripts/verify.sh full` — pass

## Protocol transcript matrix

Every row is asserted in `crates/runtime/tests/m005f_protocol_polish.rs` unless noted.

| Request | Reply |
|---|---|
| `CAP LS` on a live client | names `server-time`, `standard-replies`, `cap-notify`, `draft/no-implicit-names`; omits `echo-message` when upstream did not negotiate it |
| `CAP REQ :echo-message` with upstream echo enabled | `CAP bot ACK :echo-message` |
| `CAP REQ :echo-message` with upstream echo disabled | `CAP bot NAK :Unsupported capabilities` |
| tagged upstream frame to a session with both capabilities | `@…;time=…` — every tag |
| the same frame to a session with `message-tags` only | every tag **except** `time` |
| the same frame to a session with neither | no tags |
| history replay to a session with `server-time` | stamped |
| history replay to a session without it | unstamped |
| malformed `CHATHISTORY` with `standard-replies` | `FAIL CHATHISTORY INVALID_PARAMS BEFORE #room :…` |
| malformed `CHATHISTORY` without it | `:bouncer 461 #room :Invalid timestamp`, no `FAIL` |
| malformed `SEARCH` with / without `standard-replies` | `FAIL SEARCH INVALID_SEARCH …` / `:bouncer 461 * :…`, both followed by a complete empty batch |
| projection to a session with `draft/no-implicit-names` | no `353` |
| projection to any other session on the same Network | `353` with the full member list |
| upstream `CAP * NEW :echo-message` | bouncer sends `CAP REQ :echo-message`; on `ACK`, `CAP * NEW :echo-message` to `cap-notify` sessions only |
| upstream `CAP * DEL :echo-message` | `CAP * DEL :echo-message` to `cap-notify` sessions only, and no follow-up request |
| upstream `CAP * NEW :away-notify account-notify` | nothing reported |
| local `PRIVMSG` before the echo | not in history, and not confirmed to the initiator |
| upstream echo of it | in history exactly once, `sender=bot`, and delivered to the initiator |
| upstream line carrying our nick but no `time` | retained as conversation, not as confirmation |

## Findings

None open. Plan 025 introduced no architectural conflict and no new network boundary: it
changed what is advertised and what is delivered per session, not what crosses a process
boundary.

The one thing this plan changed structurally — a shared advertisement cell rather than a
static list — was verified against the pre-existing `corrective_019` suite specifically
because that suite covers the SASL and quit-fence paths this work touched. It is the
suite that caught defect 6, and it is the suite that would catch a regression there.

## M005-G readiness

Plan 026 (M005-G, richer IRCv3 member-state mediation) is **unblocked and
dependency-ready**. Everything it needs is landed:

- **the tag surface is per session and correct**, so member-state frames carrying tags are
  mediated on the same three-state surface the rest of the fanout uses;
- **`no-implicit-names` gives member state an explicit client-side control**, so a client
  that does not want the full roster already has a way to decline it;
- **the advertisement is live rather than static**, so M005-G's capabilities enter the
  same single-source `CAP LS`/`REQ`/welcome path rather than a second one;
- **one upstream `CAP` authority exists**, so a member-state capability negotiated
  upstream and downstream has one place it can change.

Plans 027-028 remain gated on their sequential predecessors.