# Richer IRCv3 member-state mediation

Plan 026 / M005-G. This document records what the bouncer now does with member state
beyond nick and channel: account, realname, away state, and a complete membership prefix
run.

The upstream negotiation here is a *single* decision made once per generation. The set of
attached clients is not fixed at all, and any of them can attach or leave at any time. That
asymmetry is the whole content of this plan: everything below exists to turn one upstream
answer into N client answers that are each correct.

## The accepted set, and what is deliberately not

Five capabilities were accepted. Four were examined and **deferred with a stated reason**,
because a capability this build does not fully mediate must not be requested upstream and
must not be advertised downstream.

| Capability | Accepted | Advertised when | Asserts |
|---|---|---|---|
| `extended-join` | yes | upstream negotiated it | a JOIN carries an account and realname only for a session that negotiated the form |
| `account-notify` | yes | upstream negotiated it | an `ACCOUNT` change reaches only sessions that asked for it |
| `away-notify` | yes | upstream negotiated it | an `AWAY` change reaches only sessions that asked for it |
| `multi-prefix` | yes | upstream negotiated it | membership is widened only for a session that negotiated it, and only when the run was observed whole |
| `setname` | yes | upstream negotiated it | a `SETNAME` change reaches only sessions that asked, and the command travels only from sessions that asked |

`DOWNSTREAM_DEFERRED_MEMBER` records the four that were examined and withheld:

| Deferred | Why |
|---|---|
| `account-tag` | requires stamping a server tag onto messages the upstream did not stamp. Plan 025 established that no live frame is ever fabricated — `server-time` synthesis is confined to history replay — and a second synthesis surface contradicts that rule rather than extending it. |
| `chghost` | its specification falls back to a synthetic `QUIT`/`JOIN`/`MODE` sequence for clients that did not negotiate it. A bouncer whose whole projection discipline is that it never claims a membership a client did not see established must not synthesise membership events. Withholding it also makes a continuously attached legacy client's user/host stale while a freshly projected one is current — a real divergence, which is the honest reason to defer rather than half-solve. |
| `invite-notify` | `INVITE` already reaches sessions through ordinary fanout. The capability only carries meaning if invites are withheld per session, and M005 establishes no such requirement. Advertising it would promise routing behaviour that does not exist. |
| `extended-monitor` | changes the *format* of MONITOR replies, not which monitors work. M005-C reads `MONITOR` from ISUPPORT and its reclaim behaviour is satisfied by standard MONITOR. |

Every accepted capability is in `UPSTREAM_FOUNDATIONAL`, which is now ten names. A server
offering a deferred name gets no request for it: the runtime has no handling for the
message forms that name would enable, and requesting it would make the server send frames
the bouncer cannot interpret.

## The advertisement is conditional on upstream, for all five

`DownstreamCapabilities::advertise` adds each member capability only when upstream
*acknowledged* it. This is the same rule `echo-message` already used, and it has the same
reason: the bouncer mediates what the server supplied. Advertising `extended-join` to a
server that never offered it would promise every client a richer JOIN than any client could
ever receive.

Not *offered* is not enough — it is *acknowledged*. A server can offer a capability in
`CAP LS` and refuse it in the `CAP REQ` acknowledgement, and only the acknowledgement is a
statement the server has made about the connection in hand.

## Three mechanisms, and the difference between them

`member::mediate` returns one of three things per session:

| | When | Example |
|---|---|---|
| `Pass` | the session can read the frame | an ordinary `PRIVMSG` |
| `Rewritten` | the frame is well formed and unreadable for this session | an extended JOIN to a session without `extended-join` |
| `Withhold` | the message form exists only under a capability this session lacks | `ACCOUNT`, `AWAY`, `SETNAME` |

`ACCOUNT`, `AWAY` and `SETNAME` are *withheld* rather than rewritten because they are whole
message forms that exist only because a capability was negotiated — in both directions. A
server emits them because the bouncer asked for the capability, and the bouncer relays them
to a session that asked for it. A session that asked for none of them has no way to know
what any of them mean.

`extended-join` is *rewritten* rather than withheld because it is not a separate message. It
is the same JOIN with two extra parameters, and withholding it would hide a membership event
entirely. A client that never negotiated the form would read the trailing account and
realname as unrelated parameters and mis-parse the whole line, so it is reduced to the plain
form instead.

The rewrite goes through `Message::truncate_params`, which also clears the trailing marker.
That marker describes the parameter that was *last* — the realname — so leaving it set would
re-render the surviving channel as a trailing argument (`JOIN :#room`). That parses
identically, since `:` is only a delimiter, but no server writes a channel JOIN that way and
a rewriter that drops a parameter should not also restyle the one it kept.

## Mediation runs before the tag surface, not after

The per-session order in the fanout loop is: **mediate, then choose a tag surface**.

This ordering is load-bearing. `TagForms` is built from a `Message`, so a mediated rewrite
that happened *after* the tag forms were chosen would hand a session bytes derived from one
frame carrying the tag set of another. A session that cannot read the richer form must not
receive it with tags attached either — the tags are optional in every direction, and the
alternative is reintroducing a detached channel name inside a server-chosen tag value.

`TagForms::build` also has a case worth naming: for a frame with no tags it re-encodes the
message rather than returning the raw bytes. The raw bytes are the *original* line, and a
mediated frame is not it. Returning `raw` there would undo the mediation precisely when the
mediation happened on an untagged frame — which is most frames, because most IRC traffic
carries no tags.

## `multi-prefix` is not just a NAMES width

`multi-prefix` widens membership in three different places, and handling only NAMES would
leave two of them wrong:

- **NAMES** (`353`) in the projection, and
- **WHO** (`352`) and **WHOIS channels** (`319`) in *routed* replies.

The routed two are the subtle ones. A reply goes to exactly one client, so the decision is
made per session in the route's rebuild closure, reading that session's own capabilities.
Two clients asking the same question on one Network get different answers, which is the
point: the question is "what may this client read", not "what did the server say".

### A WHO flags field has no fixed prefix position

RFC 2812 fixes only the leading `H`/`*` online marker and the optional `G`/`g` operator
marker. The membership run follows wherever the server puts it. So `reduce_who_flags` finds
the run by scanning for advertised membership symbols rather than by counting from the start,
and writes the highest one back at the position the run occupied. Every other character
keeps its place and its meaning.

Six characters are never treated as membership symbols, whatever the server's `PREFIX` map
contains: `H`, `*`, `G`, `g`, `?` and `!`. Those are status and transport markers with fixed
positions, and a server that advertised `H` as a mode would otherwise have this rewrite
delete an online-status flag that has nothing to do with channel membership.

## Completeness is a separate claim from the run

`MemberEntry` holds the symbols that were *observed* and, separately, whether that run is
the member's *complete* set. Keeping them apart is what makes the degradation safe:

- A NAMES entry observed **with** `multi-prefix` negotiated establishes a complete run.
- A NAMES entry observed **without** it does not, even if the server happened to send two
  symbols. The symbols are retained — they really were sent — but no completeness is claimed.
- A `MODE` delta adds one symbol to the run and never completes it. Completing an
  incomplete set would be a claim the observation does not support.
- A later NAMES entry *can* complete a run an earlier one left incomplete.

`MemberEntry::display` therefore requires **both** `multi-prefix` negotiated **and**
`symbols_complete` before showing a complete run. Either alone is not enough, and the reason
is specific: a client that negotiated `multi-prefix` reads an absent symbol as an absent
*mode*. Showing it a partial run would tell it the member holds no other modes — a claim
about membership the bouncer cannot make about a run it never saw whole.

Anything else yields the single highest symbol. That symbol was genuinely observed, and it
is the whole of what a view without `multi-prefix` can represent, so omitting it would
understate what the bouncer knows and widening it would overstate what the server said.

## `AccountState` has three states, because two facts are not one

| | means |
|---|---|
| `Unknown` | nothing has been observed |
| `Known(Some(..))` | observed login |
| `Known(None)` | **observed** logout |

The distinction is load-bearing. A member who joined before the bouncer negotiated
`extended-join` has `Unknown`. A member whose extended JOIN carried `*`, or whose
`ACCOUNT *` arrived, has an *observed* logout. Collapsing them would let the bouncer present
a member as logged out because it never looked.

This is why `own_join_line` does not default a missing account to `*`. `*` is the spec's
statement that the server told us "this member is not logged in". Rendering it from an
absence would assert something the bouncer does not know, so an unobserved profile yields
the plain JOIN — the shape every client can read — instead.

An extended JOIN is emitted only when the session negotiated `extended-join` **and** both
fields were actually observed.

## Bounded metadata, with the reason for each bound

| Field | Bound | Why this bound |
|---|---|---|
| account | `MAX_ACCOUNT_BYTES = 64` | a service-assigned identifier, not prose |
| realname | `MAX_REALNAME_BYTES = 128`, and the server's `NAMELEN` when published | free text in a field the bouncer cannot otherwise bound |
| away message | `MAX_AWAY_MESSAGE_BYTES = 200` | bounds what *other* members put in the field |

The away bound is deliberately a separate constant from the Operator's own
`MAX_AWAY_TEXT_BYTES` in `presence.rs`. That one bounds the words this bouncer writes
upstream on the Operator's behalf; this one bounds text that arrived from elsewhere and is on
its way to a client. Different subjects, different risks.

An over-long realname is *dropped*, which leaves the last known value in place. Retaining
the previous value is a stale answer; replacing it with a truncated one would be a false
one, and the projection omits a realname it does not have rather than inventing one.

Note that relay is not retention. A negotiated session is still owed the frame the server
sent, and dropping it would hide an event that really happened. The bound is on what the
bouncer *holds*, not on what it forwards.

## Invalidation

| Event | Effect on metadata |
|---|---|
| `NICK` | the entry is renamed; account, realname, away and symbols follow it |
| `QUIT` | the entry is removed; the metadata goes with it |
| `PART` / `KICK` | the entry leaves that channel only |
| `ACCOUNT` / `SETNAME` / `AWAY` | applied to every channel the member is in |
| reconnect | the whole `NetworkState` is new; nothing carries across |
| ceiling breach | the channel's `members_complete` is already false and stays false |

`multi_prefix` is generation-local, and so is `namelen`. A reconnect that failed to negotiate
`multi-prefix` genuinely has *less* membership information than the generation before it, and
must not inherit its claim of completeness. `NetworkState::set_upstream_multi_prefix` is
called from every place the upstream negotiation is recorded: the registration `CAP` ACK, a
mid-generation `CAP` ACK, and a `CAP` withdrawal.

## What a reattaching client is and is not shown

A reattaching client is shown the same **membership** a live client had: the same members at
the same prefix width, reconstructed from what was observed while it was away.

Per-member **account and realname** are *not* shown, and that boundary is deliberate. No
projection frame carries them — `353` has no field for an account or a realname, and
inventing a bouncer-only frame to hold them would be an extension no client understands. So
they are available as they happen, and a client that wants the current value asks the server,
which is exactly what `extended-join` plus `WHOIS` is for.

The bouncer's own account and realname *are* projected, because the projection carries the
bouncer's own channel JOIN and that JOIN has somewhere to put them.

## `NAMELEN` is published once, and only where it is owed

`setname` obliges the server to publish a realname ceiling. The bouncer relays the upstream
`NAMELEN` token verbatim, so it owes its own only when upstream published none — two answers
to one question in a single `005` is worse than one.

When it does owe one, it is owed only to a session that negotiated `setname`: a client that
never asked for the semantics has no use for the ceiling, and unsolicited `005` tokens are
noise.

## The `SETNAME` command, in both directions

The server-to-client form is gated on the receiving session, like `ACCOUNT` and `AWAY`.

The client-to-server form is gated on the *sending* session: a `SETNAME` from a client that
never negotiated it is refused with `421`, explicitly, rather than silently ignored.

The specification's alternative is to handle it silently, on the reasoning that a client
which sent the command cannot be confused about it. But this bouncer's upstream negotiation
is conditional — a server that never offered `setname` means the session was never offered
it either, and its `SETNAME` could not be honoured anywhere upstream. Silence would leave the
client waiting forever for a confirmation that cannot arrive. The explicit refusal is
consistent with how this bouncer refuses an unsupported `CHATHISTORY` or `SEARCH`, and it is
not grounds for ending the session.

## Registration is where the negotiation has to happen

The projection is sent exactly once, at attach. So the surface it is rendered at is the
surface the session negotiated *at registration time*, and a capability negotiated after that
changes only what arrives afterwards.

This is why admission reads the selected Network's **live** advertisement rather than a
build-wide default, through `ControlRequest::Advertisement`. The published `ControlSnapshot`
is a copy taken at the last controller revision, and an owner negotiates with its upstream
moments after it is inserted — so a copy can report an advertisement that is still empty
while the owner has long since published a real one. A client registering in that window
would have every upstream-conditional capability refused at exactly the moment it could
still have used it, and would then receive an extended projection for itself without ever
being able to ask for one.

## File map

| File | Role |
|---|---|
| `crates/runtime/src/member.rs` | the mediation module. Pure functions over `(state, message, capabilities)`. No authority. |
| `crates/runtime/src/state.rs` | `MemberEntry`, `AccountState`, `AwayState`, `MemberObservation`, `PrefixMap::split_prefix_run` / `merge_symbols` / `highest`, and the `AWAY` / `ACCOUNT` / `SETNAME` arms |
| `crates/runtime/src/capability.rs` | `MEMBER_CAPABILITIES`, `DOWNSTREAM_DEFERRED_MEMBER`, the ten-name upstream request set, the conditional advertisement |
| `crates/runtime/src/session.rs` | per-session negotiation flags, the `SETNAME` command gate, the advertisement passed into registration |
| `crates/runtime/src/owner.rs` | `TagForms`, the per-session mediate-then-tag loop, the routed-reply reduction |
| `crates/runtime/src/admission.rs` | reads the Network's live advertisement before registration begins |
| `crates/runtime/src/controller.rs` | `ControlRequest::Advertisement` and `ControlNetwork::advertisement` |
| `crates/wire/src/lib.rs` | `Message::truncate_params`, which owns the trailing-marker bookkeeping |