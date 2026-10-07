# Plan 026 — M005-G Richer IRCv3 Member-State Mediation — Closure

Closed 2026-10-10. Outcome: **closed, no open findings**.

## Outcome

Five richer member-state capabilities were accepted, mediated per session, and made
conditional on what the upstream actually negotiated. Four were examined and **deferred
with a stated reason**, because a capability this build does not fully mediate must
neither be requested upstream nor advertised downstream.

| Capability | Advertised when | Asserts |
|---|---|---|
| `extended-join` | upstream acknowledged it | a JOIN carries an account and realname only for a session that negotiated the form; every other session gets the plain JOIN |
| `account-notify` | upstream acknowledged it | an `ACCOUNT` change reaches only sessions that negotiated it |
| `away-notify` | upstream acknowledged it | an `AWAY` change reaches only sessions that negotiated it |
| `multi-prefix` | upstream acknowledged it | membership is widened only for a session that negotiated it, and only where the run was observed complete |
| `setname` | upstream acknowledged it | a `SETNAME` change reaches only sessions that negotiated it, and the command travels only from sessions that negotiated it |

The plan's acceptance criterion was a per-capability matrix across ten categories. All ten
are asserted in `crates/runtime/tests/m005g_member_state.rs`.

## The deferred set, and why each was withheld

`DOWNSTREAM_DEFERRED_MEMBER` records these as named constants rather than deleting them,
so "not offered" is a reviewable decision.

| Deferred | Reason |
|---|---|
| `account-tag` | requires stamping a server tag onto messages the upstream did not stamp. Plan 025 established that no live frame is ever fabricated and confined `server-time` synthesis to history replay; a second synthesis surface contradicts that rule rather than extending it. |
| `chghost` | its specification falls back to a synthetic `QUIT`/`JOIN`/`MODE` sequence for non-negotiating clients. A bouncer whose projection discipline is that it never claims a membership a client did not see established must not synthesise membership events. Withholding it also makes a continuously attached legacy client's user/host stale while a freshly projected one is current — a real divergence, which is the honest reason to defer rather than half-solve. |
| `invite-notify` | `INVITE` already reaches sessions through ordinary fanout. The capability only carries meaning if invites are withheld per session, and M005 establishes no such requirement. |
| `extended-monitor` | changes the *format* of MONITOR replies, not which monitors work. M005-C reads `MONITOR` from ISUPPORT and its reclaim behaviour is satisfied by standard MONITOR. |

A test asserts that a server offering all four gets no request for any of them, and that
none appears in the downstream `CAP LS`.

## What was landed

### One bounded observed member model

`MemberEntry` gained bounded observed metadata alongside the prefix run:

```text
MemberEntry {
    nick: String,
    symbols: Vec<char>,          // the run that was observed, rank order
    symbols_complete: bool,      // whether that run is the member's complete set
    account: AccountState,       // Unknown | Known(Option<String>)
    realname: Option<String>,
    away: Option<AwayState>,     // None = never observed; Here | Away(String)
}
```

`AccountState` has three states because two facts are not one. `Unknown` is "nothing
observed"; `Known(None)` is an **observed** logout, which is what `extended-join`'s `*` and
`account-notify`'s `ACCOUNT *` both mean. Collapsing them would let the bouncer present a
member as logged out because it never looked — and `own_join_line` therefore never defaults
a missing account to `*`.

Bounds, each with its reason in the code: `MAX_ACCOUNT_BYTES = 64` (an identifier, not
prose), `MAX_REALNAME_BYTES = 128` and the server's `NAMELEN` when published (free text in
a field otherwise unbounded), `MAX_AWAY_MESSAGE_BYTES = 200` (bounds what *other* members
put in the field, deliberately separate from the Operator's own `MAX_AWAY_TEXT_BYTES`).

An over-long value is **dropped**, which leaves the last known value in place. Retaining the
previous value is a stale answer; replacing it with a truncated one would be a false one.

### The run is not the same claim as its completeness

`PrefixMap::split_prefix_run` replaces the old highest-symbol-only reader, and every call
site uses it, so "which symbols did this entry actually claim" is decided in one place.

The run is re-sorted into rank order and de-duplicated on read. A server that sent
`+@Alice` would otherwise let a reordered prefix change which symbol a legacy client is
shown as its highest.

Completeness is a separate boolean because the two can disagree:

- a NAMES entry observed **with** `multi-prefix` negotiated establishes a complete set;
- one observed **without** it does not, even if the server sent two symbols — the symbols are
  retained because they really were sent, but no completeness is claimed;
- a `MODE` delta adds a symbol and never completes a run;
- a later NAMES entry can complete an earlier incomplete one.

`MemberEntry::display` requires **both** `multi-prefix` negotiated **and** `symbols_complete`
before widening. Either alone is not enough: a client that negotiated `multi-prefix` reads
an absent symbol as an absent *mode*, so a partial run would tell it the member holds no
other modes.

### `member.rs`: three mechanisms

A new pure module, no authority, deciding per `(state, message, capabilities)`:

| | when | example |
|---|---|---|
| `Pass` | the session can read the frame | an ordinary `PRIVMSG` |
| `Rewritten` | well formed but unreadable for this session | extended JOIN → plain JOIN |
| `Withhold` | the form exists only under a capability this session lacks | `ACCOUNT`, `AWAY`, `SETNAME` |

`extended-join` is rewritten rather than withheld because it is not a separate message: it
is the same JOIN with two extra parameters, and withholding it would hide a membership event
entirely.

### `multi-prefix` reaches three places, not one

NAMES in the projection, **and** WHO (`352`) and WHOIS channels (`319`) in *routed* replies.
Handling only NAMES would have left two of the three wrong.

The routed two are decided per session in the route's rebuild closure, reading that session's
own capabilities, so two clients asking the same question on one Network get different
answers.

`reduce_who_flags` scans for advertised membership symbols rather than counting from the
start, because RFC 2812 fixes only the leading `H`/`*` and optional `G`/`g` markers — the run
follows wherever the server puts it. Six characters are never treated as membership symbols
whatever the `PREFIX` map contains (`H`, `*`, `G`, `g`, `?`, `!`): those are status and
transport markers, and treating one as a prefix would delete a flag that has nothing to do
with channel membership.

### Mediation runs before the tag surface

The fanout loop is now **mediate, then choose a tag surface**. `TagForms` is built from a
`Message`, so a rewrite applied after the tag forms were chosen would hand a session bytes
derived from one frame carrying the tag set of another.

`TagForms` replaces three separately-computed byte forms with one struct that knows how to
render itself, so the "which form" decision and the "what bytes" decision cannot drift.

### `NAMELEN` published once, and only where owed

`setname` obliges the server to publish a realname ceiling, and the bouncer relays the
upstream token verbatim — so it owes its own only when upstream published none. Two answers
to one question in one `005` is worse than one. When it does owe one, it is owed only to a
session that negotiated `setname`.

### `Message::truncate_params`

The JOIN reduction drops parameters, and the trailing marker describes the parameter that
was *last* — the realname. Left set, the encoder would re-render the surviving channel as a
trailing argument (`JOIN :#room`): parseable, since `:` is only a delimiter, but no server
writes a channel JOIN that way. The operation is a `wire` method because the bookkeeping is
wire-owned.

### Registration reads the Network's live advertisement

The projection is sent exactly once, at attach, so the surface it is rendered at is the
surface negotiated **at registration time**. Admission now reads the selected Network's
advertisement through `ControlRequest::Advertisement`, which reads the *live* owner rather
than the published `ControlSnapshot`.

This matters because the published snapshot is a copy taken at the last controller revision,
and an owner negotiates with its upstream moments after it is inserted. A copy can report an
advertisement that is still empty while the owner has long since published a real one — and
a client registering in that window would have every upstream-conditional capability refused
at exactly the moment it could still have used it.

## Defects found during implementation

Nine. Six were found by the suite; three were latent defects in the test harness that had
been passing because the tests did not exercise the path.

1. **`MemberEntry::display` ignored `symbols_complete`** (found by the new state test). It
   widened any run for a `multi-prefix` session, so a member whose run was never observed
   whole was shown as holding exactly the modes the bouncer happened to see. A client that
   negotiated `multi-prefix` reads an absent symbol as an absent mode, so this asserted a
   membership fact the bouncer did not have.

2. **A mediated reduction was discarded whenever the frame carried no tags** (found by the
   suite). `TagForms::build` returned the raw bytes for an untagged message, and the raw
   bytes are the *original* line — so the reduction was undone precisely when it mattered
   most, because most IRC traffic carries no tags.

3. **A reduced extended JOIN was re-encoded as `JOIN :#room`** (found by the suite). The
   trailing marker was left set after truncating, so the surviving channel became a trailing
   argument.

4. **`reduce_who_flags` assumed the membership run started at a fixed offset** (found by the
   suite). It skipped the first two characters as status, which is wrong for the common
   `H@+` shape and would have turned `H@+` into `H@` unchanged for the wrong reason while
   corrupting `H~&@%+`.

5. **Registration answered `CAP` from a static list** (found by the suite; a **pre-existing**
   Plan 025 defect that M005-G made load-bearing). `ClientWiring::new` seeded the
   advertisement from `downstream_supported()`, so a client negotiating an
   upstream-conditional capability during registration was NAKed — with `echo-message` too,
   but no M005-F test requested one at registration. Fixed by reading the Network's live
   advertisement. Without this, a client could never obtain an extended projection for
   itself.

6. **The bouncer published its own `NAMELEN` beside the relayed upstream one** (found by the
   suite). Two answers to one question in a single `005`.

7. **An unsolicited upstream `353` fans out verbatim** (harness defect, recorded not fixed).
   A test that wrote a `353` after startup and then attached a client was measuring the
   fanout rather than the bouncer's projection, and passed or failed depending on whether the
   frame happened to have been applied yet.

8. **Upstream frames written in a test are only queued** (harness defect). The harness waited
   for liveness, which proves the owner exists but not that it has *applied* the frames
   written so far. The suite passed in isolation and failed under parallel load, which is the
   worst possible failure mode for a harness. Fixed with a probe client whose barrier proves
   ordering, and a `sync` helper used wherever an assertion depends on earlier frames.

9. **A reply's route label must precede the prefix** (harness defect). Placing it after the
   prefix produced an unparseable frame, whose invalid command tore down the whole
   generation and silently detached every session — a convincing failure with a misleading
   symptom.

Two limits were also corrected rather than assumed. A test asserting that a reattaching
client sees another member's realname was rewritten, because no projection frame carries
one; and a test asserting the over-long value never reaches the wire was rewritten, because
**relay is not retention** — a negotiated session is still owed the frame the server sent,
and the bound is on what the bouncer holds.

## Recorded limits

- **Per-member account and realname are live-only.** A reattaching client is shown the same
  *membership* a live client had, but not members' accounts or realnames: `353` has no field
  for either, and inventing a bouncer-only frame would be an extension no client
  understands. The bouncer's *own* profile is projected, because the projection carries the
  bouncer's own JOIN and that has somewhere to put them. A client wanting a current value
  asks the server, which is what `extended-join` plus `WHOIS` is for.
- **`AWAY` for the bouncer's own nick is relayed**, contrary to the specification's `SHOULD
  NOT`. That guidance assumes the server sends `RPL_NOWAWAY`/`RPL_UNAWAY`, and this bouncer
  produces neither for its own auto-away transitions — the server's echo is the Operator's
  only signal. Suppressing it would remove information rather than remove redundancy.
- **A capability negotiated after attach does not change the projection.** The projection is
  sent once, at attach, so the negotiated surface applies to it only if negotiated at
  registration. This is inherent to sending a snapshot at connect and matches ordinary
  server behaviour.
- **An incomplete prefix run falls back to one symbol.** A `multi-prefix` client whose
  member's run was never observed whole sees the single highest symbol rather than a partial
  run, because a partial run would be read as a complete one.

## Verification

- `./scripts/check-network-boundary.py` — pass
- `cargo fmt --all -- --check` — pass
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — pass
- `cargo test -p i2pr-irc-wire` — pass (39 + 7)
- `cargo test -p i2pr-irc-runtime --lib` — pass (226)
- `cargo test -p i2pr-irc-runtime --test m005g_member_state` — pass (14, all new)
- `cargo test -p i2pr-irc-runtime --test m005f_protocol_polish` — pass (11, from Plan 025)
- `cargo test -p i2pr-irc-runtime --test m005e_search_history` — pass (15, from Plan 024)
- `cargo test --workspace --all-features` — pass (31 suites)

## Protocol transcript matrix

Every row is asserted in `crates/runtime/tests/m005g_member_state.rs`.

| Category | `✓` negotiated | `✗` not negotiated | `!` refused |
|---|---|---|---|
| advertisement | all five named in `CAP LS` when upstream acknowledged them | none named when upstream supplied nothing | four deferred names never named |
| `extended-join` | `:Alice!u@h JOIN #room acct :Alice Example` | `:Alice!u@h JOIN #room` | — |
| own channel JOIN | `:bot JOIN #room bouncerbot :The Bouncer` | `:bot JOIN #room` when the profile was never observed | never `:bot JOIN #room * :…` |
| `account-notify` | `:Alice!u@h ACCOUNT aliceacct :Logged in` | withheld | — |
| `away-notify` | `:Alice!u@h AWAY :back shortly` | withheld | — |
| `setname` | `:Alice!u@h SETNAME :Alice Example` | withheld | — |
| `SETNAME` command | forwarded upstream | — | `421 Unsupported command` |
| `multi-prefix` NAMES | `@+Alice` | `@Alice` | — |
| `multi-prefix` WHO | `352 … Alice H@+` | `352 … Alice H@` | — |
| `multi-prefix` WHOIS | `319 … :@+#alpha +#beta` | `319 … :@#alpha +#beta` | — |
| `multi-prefix` incomplete | `@Alice` (highest only) | `@Alice` | never a partial run as a complete one |
| `NAMELEN` | published once when upstream published none | not published | never duplicated beside the relayed token |

## Findings

None open for this plan. Plan 026 introduced no architectural conflict and no new network
boundary: it changed what is stored about members and what is delivered per session, not
what crosses a process boundary.

The `ControlRequest::Advertisement` addition is the one structural change, and it is a read
of state the owner already publishes rather than a new authority — it grants nothing and
routes nothing.

Two pre-existing suites were run specifically because this work touched their paths.
`ircv3_routing` encodes the pre-M005-G decision to withhold `extended-join` and
`away-notify` upstream, and was updated to the new reviewed set while keeping its actual
property — that a capability this build does not mediate is never requested. `corrective_019`
covers the SASL and quit-fence paths that `admission.rs` and `controller.rs` changed.

### A pre-existing flake recorded, not fixed

`crates/runtime/tests/adverse.rs::a_startup_herd_at_the_ceiling_never_exceeds_the_connect_ceiling`
intermittently fails with *"the process did not return to its baseline"* (1 run in 8 on
several occasions, including on `63b10db` with this plan stashed). The difference is
`store_queue: 1` against a baseline of `0`; every ledger-owned gauge is already zero.

This is a teardown race in the test, not a runtime defect: it asserts the process returned
to an exact gauge snapshot including a queue depth that drains asynchronously and is not
owned by the ledger's shutdown path. It is unrelated to this plan's changes — the
connect-ceiling claim that test actually exists to make is not what fails.

It is recorded rather than fixed here because the test belongs to M004 resource accounting
and fixing it inside Plan 026 would blur the plan boundary. It needs to be fixed before
Plan 028's integrated qualification, which will run this suite and cannot distinguish a
flake from a regression.

## M005-H readiness

Plan 027 (M005-H, operator diagnostics, configuration snapshots and registration actions)
is **unblocked and dependency-ready**. Everything it needs is landed:

- **the advertisement is live and readable per Network**, so a diagnostics surface can report
  what a client would be offered and why, without recomputing it from two sources;
- **observed member state is bounded and generation-local**, so a configuration snapshot can
  report membership without retaining anything beyond the generation;
- **one upstream `CAP` authority exists**, so a capability change has exactly one place it is
  recorded and one place it is reported from;
- **no deferred capability is advertised**, so a registration action can be gated on the same
  reviewed set the rest of the runtime uses.

Plans 027-028 remain gated on their sequential predecessors.