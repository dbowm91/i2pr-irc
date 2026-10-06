# Plan 023 — M005-D Bouncer Networks and Local IRC Administration — Closure

Closed 2026-10-08. Outcome: **closed, no open findings**.

## Outcome

A client that has never heard of this bouncer can connect, discover which Networks exist,
select one, and administrate Networks, channels, and presence policy — over one local
connection, with no clearnet authority anywhere in the path. Two dialects reach the same
typed controller: `soju.im/bouncer-networks` for interop, and the local `BouncerServ`
service for administration. Neither decides anything on its own.

## What was landed

### Work package A — bouncer-networks adapter

`crates/runtime/src/bouncer_networks.rs`, new.

Parsing and rendering only: no I/O, no state, no store, no supervisor, no socket. Three
properties are stated in the module rather than left to call sites:

- **Attribute maps are never a durable contract.** Everything the draft names is decoded
  into types this codebase owns, so a draft revision that renames an attribute is a
  parsing change and not a schema migration.
- **`host` is a typed `I2pEndpoint` or it is nothing.** No URL parsing, no port extraction,
  no TLS material, no path to a resolver. `irc.example.org:6697` and
  `https://irc.example.org` are refused by `I2pEndpoint::parse` on shape alone.
- **Every unsupported attribute is named.** `port`, `tls`, and `pass` are recognised and
  refused as "not supported"; `state` and `error` are refused as read-only; anything else
  is "unknown attribute". Nothing is dropped.

The disposition table is single-sourced in `classify_attribute`/`writable`, so `ADDNETWORK`
and `CHANGENETWORK` cannot disagree about the same name.

### Work package B — CAP advertisement and registration-time BIND

`crates/runtime/src/{downstream,session,capability}.rs`.

`soju.im/bouncer-networks` and `soju.im/bouncer-networks-notify` are advertised together,
because both halves are live. Advertising only the initial batch would leave a client
unable to distinguish an idle bouncer from a broken one.

`BOUNCER BIND <netid>` is the one control verb accepted during registration. The bindable
set is read once from the controller's snapshot at accept time and handed to the reader, so
a `FAIL BOUNCER BIND` reaches the client *before* registration completes. A decode failure
is reported as itself — reporting a malformed netid as "not valid during registration"
would send a client looking for a timing problem that does not exist.

`BIND` after registration is refused by name and the session stays unbound for the rest of
its life.

`PRIVMSG BouncerServ :…` reaches the same surface through ordinary IRC, so an existing
client needs no new verb. A `PRIVMSG` to anybody else stays an ordinary forwarded message.

### Work package C — LISTNETWORKS, ADDNETWORK, CHANGENETWORK, DELNETWORK

`crates/runtime/src/{control_session,controller}.rs`.

Every mutation is a typed request through the controller's bounded queue. `ControlRequest`
gained `CreateNext`, `Record`, `ChannelPolicy`, and `PresencePolicy`.

Netids are allocated in the controller and nowhere else. `ChangeNetwork` reads the complete
durable record through the controller so a partial update carries every field it did not
name — including the credential — through untouched.

`DELNETWORK` is routed through the existing quiesce path: the owner is stopped and awaited
before the row is removed, so no owner outlives its Network.

### Work package D — I2P-safe attribute validation profile

`crates/runtime/src/bouncer_networks.rs`.

| Attribute | Disposition |
| --- | --- |
| `name` | writable; single IRC token, no space or separator |
| `host` | writable; typed I2P destination only |
| `nickname` | writable; client-nick grammar |
| `username` | writable; bounded, printable, no separators |
| `realname` | writable; bounded |
| `state` | read-only |
| `error` | read-only |
| `port` | recognised, refused — no port in this product generation |
| `tls` | recognised, refused — no TLS material to configure |
| `pass` | recognised, refused — no upstream `PASS` |
| anything else | unknown, refused by name |

A hostname-form `.i2p` name is accepted as what it is: a name inside I2P, the same
`I2pEndpoint` type the store validates. It is not a DNS name this bouncer resolves.

### Work package E — bounded notify snapshots and deltas

`crates/runtime/src/control_session.rs`.

A bounded initial batch, then deltas derived from consecutive `ControlSnapshot`s. There is
no event log: a session that fell behind is reconciled against current state rather than
replayed, which is what keeps a slow client's cost at one snapshot and one bounded queue
however long it stalls. `MAX_BOUNCER_BATCH` bounds a single reply.

### Work package F — BouncerServ

`crates/runtime/src/bouncerserv.rs`, new.

A fixed, non-environment-derived service identity reached by ordinary `PRIVMSG`. The parser
has no variant that can express command execution, file access, HTTP, plugin loading, router
administration, or raw IRC quotation. Naming those absences is weaker than the type, so the
type is the guarantee: there is nothing to reach.

`SASL SET` accepts a password because that is the only way to set one. The value enters a
`Secret` that redacts its `Debug` and zeroes on drop, and leaves only as a `StoredSecret`.
`ServCommand`'s `Debug` is hand-written for the same reason.

### Work package G — interoperability matrix

`crates/runtime/tests/m005d_bouncer_networks.rs`, new: 28 tests covering discovery,
listing redaction, pre-registration BIND, late-BIND refusal, unknown-netid refusal,
non-canonical netids, netid stability across restart, every attribute disposition, the
catalog ceiling, notification deltas, a stalled reader, bound *and* unbound administration,
line injection, the absent host-reaching commands, and credential non-disclosure.

### Work package H — boundary and anonymity qualification

`scripts/check-network-boundary.py` passes with the new modules in scope, and
`no_host_or_environment_value_can_reach_this_control_plane` asserts the adapter, service,
and surface sources interpolate no host or environment value.

### Work package I — docs

`architecture/bouncer-networks.md` is new. `architecture/control-session.md`,
`architecture/overview.md`, and `architecture/testing.md` are updated.

## Protocol isolation

The plan's isolation requirement was that the draft's attribute vocabulary is versioned and
profile-scoped rather than becoming durable schema. That holds structurally: the adapter
decodes to owned types, and `NetworkRecord` gained no field. A draft revision that renames
`nickname` is a change to `classify_attribute` and nothing else.

`standard-replies` is **not** advertised. This build emits the draft's required
`FAIL BOUNCER <subcommand> :<reason>` form, which is what the draft itself mandates, but it
does not implement that capability's full semantics. Advertising a capability in order to
borrow its failure format would be exactly the lie the plan's own notification rule warns
about. Plan 025 promotes the advertisement once the semantics are real.

## Design decisions and deviations

1. **Administration is not a privilege of being unbound.** A session bound to a Network is
   still the local Operator's connection, so the owner answers its administrative requests
   by submitting them to the same controller that owns every live owner. The owner holds a
   bounded sender and gains no authority it did not already route there.
2. **Netids are allocated by the controller.** A caller-chosen identity makes `ADDNETWORK` a
   race: two clients pick the same free id and the loser is refused for an unrelated reason.
3. **`BouncerServ` needs no capability.** It is ordinary IRC, and every session that reaches
   this bouncer is a local Operator admitted with a trusted `ClientId`. It is not advertised
   in `005` and is not joined anywhere.
4. **`MAX_BOUNCER_LINE_BYTES` is the wire's own constant.** See the limitation below.
5. **Presence and nick policy changed here goes through the controller**, not through a raw
   store write, so it is the same serialized queue as every other mutation and it is
   partial by construction.
6. **The anonymity test is structural, not environmental.** `unsafe` is denied at build, so
   the suite reads the construction sites as source. Weaker than a live sentinel, recorded
   as such.

## Limits recorded

1. **A canonical raw `Destination` cannot be configured through this surface.** It is 516
   characters and an IRC line is at most 512, so no `BOUNCER ADDNETWORK host=…` line can
   carry one. `MAX_BOUNCER_LINE_BYTES` is therefore `i2pr_irc_wire::MAX_LINE_BYTES` rather
   than a larger number describing a line the decoder would have refused. The `.b32.i2p`
   and `.i2p` forms both fit and are what a draft client actually sends.
   `the_bouncer_line_ceiling_is_the_wire_ceiling_and_not_a_separate_claim` pins this.
2. **An over-long line ends the session rather than being refused.** An over-long
   *attribute* is a request the bouncer understood and refused; a line past the decoder's
   ceiling is never decoded, so the only honest answer is to close.
   `a_line_past_the_reader_ceiling_ends_the_session` pins this.
3. **`standard-replies` is deferred to Plan 025** by design, not by omission.
4. **`upstream PASS` remains unsupported**, so `pass` is refused rather than stored. That is
   recorded in the durable Network record and is unchanged by this plan.
5. **Channel policy requires a live owner.** `ControlRequest::ChannelPolicy` is routed to
   the owner because the owner owns the commit-then-present ordering. A Network with no live
   owner is refused with "not connected" rather than having its flag written behind a
   non-existent owner's back.
6. **The notification baseline is per surface.** A control session runs two surfaces over
   one controller — one answering commands from fresh state, one carrying the one revision
   the client was last told about. They cannot disagree: one defines what to say, the other
   records what was said.
7. **There is one local Operator.** No per-command authority, no second role, and no remote
   path to either surface. A multi-user deployment is out of scope and was not designed for.

## Defects found and fixed during implementation

Five, all found by the qualification suite rather than by reading the code:

1. **An ordinary `PRIVMSG` was swallowed.** Adding `PRIVMSG` to the control branch removed
   it from the forward arm, and the fall-through dropped the message instead of forwarding
   it upstream. A client's message would never have reached the channel it was addressed to.
   The forward is now explicit in the same arm.
2. **`create` let the caller choose the netid**, so two `ADDNETWORK`s both received netid 0
   and the second was refused for an unrelated reason. Fixed with `CreateNext` and a
   controller-owned allocation.
3. **A `BouncerServ` `PRIVMSG` was parsed at the wrong parameter offsets**, so the service
   never answered at all. The first three "the service cannot do X" tests had been passing
   vacuously; they only became meaningful once a positive test failed.
4. **A registration-time decode failure was reported as "not valid during registration"**,
   pointing a client at a timing problem instead of at its malformed netid.
5. **The controller's record cache went stale after an owner-side channel-policy write.**
   `channel status` answered about the world as it was before the command it had just
   acknowledged. The controller now re-reads after such a change.

Two latent test-harness defects were fixed as well, and are recorded because the same class
would hide a real defect later: assertions inside a loop matched text the previous
iteration had already buffered, and a client that had never read stalled the suite rather
than proving the stall was bounded.

## Verification

- `./scripts/check-network-boundary.py` — pass
- `cargo fmt --all -- --check` — pass
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — pass
- `cargo test -p i2pr-irc-store` — pass (11 unit + 46 qualification)
- `cargo test -p i2pr-irc-runtime --lib` — pass (213 tests, 10 new)
- `cargo test -p i2pr-irc-runtime --test m005d_bouncer_networks` — pass (28 tests)
- `cargo test --workspace --all-features` — pass
- `./scripts/verify.sh quick` — pass
- `./scripts/verify.sh full` — pass

## Findings

None open. Plan 023 introduced no architectural conflict, no new network boundary, and no
deferred obligation beyond the two recorded above.

## M005-E readiness

Plan 024 (M005-E, indexed history search and CHATHISTORY completion) is **unblocked and
dependency-ready**. Everything it needs is landed:

- the `soju.im/bouncer-networks` capability and a live `ControlSurface`, which is where the
  plan's `CHATHISTORY TARGETS` reporting attaches;
- `ControlRequest::Record`, so a search request can be answered from the controller's own
  copy of durable configuration rather than from a session's private view;
- the notification machinery, which is the mechanism a search completion announcement
  would reuse;
- bounded snapshot revisions, which is the reconciliation pattern the plan's completion
  tracking is described in terms of.

Plan 025 remains gated on Plan 024's closure, and Plans 026-028 remain gated on their
sequential predecessors.