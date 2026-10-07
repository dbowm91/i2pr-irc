# Downstream IRCv3 protocol polish

Plan 025 / M005-F. This document records the five capabilities Plan 025 promoted and,
more importantly, the *boundary between them*: each is negotiated per session, and a
capability one client holds must not change what another client on the same Network
receives.

That boundary is the whole content of this plan. Everything here was already partly
true; the work was making the parts agree, and the failure mode of getting it wrong is a
client that receives a tag or a frame form it never asked for and has no way to parse.

## What was promoted, and what each promotion asserts

| Capability | Advertised when | Asserts |
|---|---|---|
| `server-time` | always | a `time` tag reaches only a session that negotiated it |
| `standard-replies` | always | a refusal is `FAIL` only for a session that negotiated it, and a numeric otherwise |
| `draft/no-implicit-names` | always | the membership block is omitted for the session that declined it and delivered to every other |
| `cap-notify` | always | a change in what this bouncer serves reaches only the sessions that asked |
| `echo-message` | **upstream negotiated it** | the upstream echo is the confirmation event, recorded once, as outgoing |

Each promotion is conditional on a specific piece of machinery existing. Promoting a
capability whose machinery does not exist is the failure this codebase's docs call a
`CAP LS` the client cannot rely on, and it is why `DOWNSTREAM_DEFERRED_SERVER_TIME` and
`DOWNSTREAM_DEFERRED_ECHO` are now empty arrays rather than deleted: the statement that
they were deliberately withheld is what made promoting them a decision.

## `server-time` is a separate permission from `message-tags`

This is the one that needed real machinery rather than an advertisement change.

`message-tags` says *a frame may begin with a tag prefix this client can parse*.
`server-time` says *and that prefix may carry this particular tag*. They are different
permissions, and before this plan the fanout had exactly two states — all tags or none.

`TagSurface` makes it three:

| | `message-tags` | `server-time` | receives |
|---|---|---|---|
| `None` | no | — | no tags |
| `WithoutTime` | yes | no | every tag **except** `time` |
| `All` | yes | yes | every tag |

The middle row is the point. Collapsing it into either neighbour is wrong in a way a
client cannot detect: stripping all tags would remove tags it *did* ask for, and sending
all tags would hand it one it never negotiated.

A `time` tag is never **synthesized** for a live frame. The upstream's value is forwarded
when it sent one and the tag is simply absent when it did not, because a time on a live
frame would be a claim about upstream delivery the bouncer never received. Synthesis
happens only in history replay, where there is a documented convention for a message the
upstream never stamped — and even there it is gated on the same per-session negotiation.

## `standard-replies` is negotiated per session too

Sending a `FAIL` line to a client that never negotiated the capability is as wrong as
sending an unrequested tag: the frame is unsolicited, and a client that has never heard
of the capability may not parse it.

`render_refusal_for` picks the form:

- negotiated → `FAIL <COMMAND> <CODE> <subcommand> <params> :<reason>`
- not negotiated → a real RFC 1459 numeric, with the same reason

Both forms carry the same reason. The numeric is coarser because there is no command
field to correlate against, which is the honest cost of not having negotiated the
capability.

Two details worth recording:

- **The field echoed after the code is the subcommand, not the command.** `FAIL CHATHISTORY
  INVALID_PARAMS BEFORE #room :…`. An earlier version echoed `CHATHISTORY` in both
  positions, which told the client its own command back twice and never said which of the
  six subcommands failed.
- **The reason is a fixed constant** in every case. A refusal that echoed the request
  would turn every bound that is not met into a way to put arbitrary client text into a
  frame the Operator sees.

## `draft/no-implicit-names` is per session, not per bouncer

A client that negotiated it asked to fetch membership itself — it wants to decide when to
pay for the bytes, which for a large channel can be the whole burst it receives.
Projection therefore omits `353`/`366` for that session only.

One client declining says nothing about what another client on the same connection wants,
so this is `handle.capabilities().negotiated_no_implicit_names()` read inside
`project_channel`, not a bouncer-wide setting.

## `cap-notify` reports a change in what *this bouncer* serves

`cap-notify` exists because the downstream set is conditional on what upstream negotiated
and the upstream may change that mid-generation.

Three decisions define it:

1. **The report is the advertisement delta, not the server's announcement.** A server may
   newly offer a capability the bouncer does not implement; telling a local client about
   it would advertise something no client could ever get. Diffing what this bouncer serves
   is both correct and idempotent — re-announcing the same thing changes nothing and so
   says nothing.
2. **Both directions are emitted.** `echo-message` disappears when a `CAP DEL` withdraws
   it upstream, and appears once the bouncer holds it.
3. **Only sessions that negotiated it are addressed.** A client that never asked is not
   sent `CAP NEW`, and a client that does not understand `cap-notify` is entitled to treat
   the line as an unknown command.

For `echo-message` to *appear* mid-generation the bouncer has to ask for it: a
`CAP NEW` records an offer, and only an `ACK` enables it. The owner therefore issues a
bounded `CAP REQ` for announced names it serves and has not enabled, capped by that one
announcement's names and skipping anything already enabled, so a server repeating `NEW`
cannot make it grow.

`note_change` finds the subcommand **by position**. The nick is an optional first
parameter, and assuming `params[0]` is the subcommand reads a `CAP * NEW :x` as having no
subcommand at all — which silently ignores every announcement a server sends with its own
nick attached.

## `echo-message`: the echo is the confirmation

The rule is unchanged from the pre-M005 plan and is now *load-bearing*: a local
`PRIVMSG`/`NOTICE` enters history at the upstream echo, and at no earlier point. A local
socket write is not evidence of upstream delivery — the queue accepted the bytes, which
says nothing about whether the server received or accepted them.

`HistoryJournal::direction_of` decides `Outbound`:

```text
prefix nick folds-equal to this Network's own nick  AND  the frame carries a time
    => Outbound
otherwise
    => Inbound
```

The judgement needs no content matching. A message from our own nick arriving *from the
server* can only be the server echoing what it accepted: nobody else can speak as us.
Matching on the body would need a bounded set of pending message bodies — unbounded work,
and a server that echoes with a `batch` reference or reformats the text would defeat it
anyway.

The `server_time` half is what makes it conservative. Without a `time` there is no
ordering evidence that this is an echo rather than a late conversation line, and guessing
wrong would put someone else's words in the Operator's own outbound history. `None` when
the Network has not registered a nickname yet, which can mislabel one outgoing message but
can never label someone else's as ours.

## One advertisement, not two

Before this plan the live reader answered `CAP LS` and `CAP REQ` from a **static** list
while `DownstreamCapabilities::advertisement(upstream)` computed the real one — and the
real one was called only from tests. `echo-message` was therefore advertised nowhere,
despite the code that would have served it existing since M003.

The advertisement now lives in a cell shared by the owner and the reader:

```text
SessionHandle.advertised: Arc<Mutex<Vec<String>>>
        ^                    ^
        |                    +-- reader answers CAP LS / CAP REQ from it
        +-- owner replaces it when the upstream negotiation changes
```

They cannot disagree because there is only one. The owner seeds it at attach and at
adoption — adoption matters because a transferred reader was created during admission,
when this Network's upstream negotiation was not yet known — and replaces it on change.

`NetworkSnapshot.advertisement` publishes the same value, because "why did my client not
get `echo-message`" is a diagnostic question and deserves an answer rather than a guess.

## A `CAP` line is never fanned out

An upstream `CAP` line is the bouncer's negotiation with the *server*. It is consumed and
never relayed to a local client.

This was a real defect, found by the M005-F suite rather than by reading the code: with
`cap-notify` implemented, an upstream `CAP * NEW :echo-message` reached clients verbatim
under the upstream's own prefix, which a local client reads as the server addressing it —
and which discloses the upstream connection's shape to every attached Operator.

`state.apply_line` still runs first, so network and membership state are unaffected.

## Defects found during implementation

1. **Upstream `CAP` lines were fanned out to clients verbatim** (above). Found by the
   suite that negotiated `cap-notify` and checked what the *other* client saw.
2. **`note_change` read the subcommand from the wrong index.** `CAP * NEW :x` parsed as
   having no subcommand, so every announcement a server sends with its own nick attached
   was silently ignored — and with `cap-notify` implemented, nothing was ever reported.
3. **The live reader answered `CAP` from a static list**, so `echo-message` was advertised
   nowhere even though the code to serve it existed. Found because the first M005-F test
   asked a live client for `CAP LS` and it was missing.
4. **`FAIL` echoed the command name in the subcommand field**, telling the client its own
   command back twice.
5. **`server-time` was promoted before the per-session filter existed**, which would have
   delivered a `time` tag to any client with `message-tags`. The promotion and the filter
   landed together; had they not, the defect would have been invisible, because a client
   receiving an extra tag is indistinguishable from a client receiving what it wanted.
6. **A self-deadlock in `watch`.** `snapshot.borrow()` was held across `attach_session`,
   which itself calls `send_modify`. `watch` shares one lock between reads and writes, and
   `std::sync::RwLock` is not reentrant for read-then-write on one thread, so two
   `corrective_019` tests hung for over 60 seconds rather than failing. The advertisement
   is now threaded into `handle_command` as a parameter instead of read through the
   snapshot.

Two test-harness defects are recorded for the same reason as Plan 024's:

- assertions that read `client.since(before)` without a read window, where the frame was
  still in flight — the same "read from a mark, and give the frame a chance to arrive"
  rule;
- the harness offered capabilities upstream but never ACKed the bouncer's `CAP REQ`, so
  registration never completed.

## What is deliberately not here

- **No mid-generation `CAP REQ` for anything the bouncer does not serve.** Requesting a
  capability it cannot use would be a request for nothing.
- **No `away-notify` or `account-notify`.** The bouncer does not implement them, and an
  upstream announcement about one is not reported: it would advertise something no client
  could ever get.
- **No upstream `time` fabrication on live frames.** See above.
- **No `msgid` synthesis.** A `msgid` on a replayed message is emitted only when one was
  genuinely preserved upstream, unchanged from the pre-M005 plan.