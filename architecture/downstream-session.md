# Downstream session

A `DownstreamSession` is a disposable view of a live upstream generation. It holds its own read half, line decoder, bounded control and normal queues, and registration state, and it borrows a point-in-time projection of the generation-owned state. Session completion is data the network owner handles; it is never upstream generation completion.

| event | session | upstream generation | upstream bytes |
| --- | --- | --- | --- |
| client accepted | attaches | unaffected | none |
| client registration complete | ready | unaffected | none |
| client `QUIT` | detached | continues | none |
| client EOF | detached | continues | none |
| client prefix, framing, or tag-budget violation | detached | continues | none |
| client queue overload | attached; that frame dropped and counted | continues | none |
| client writer failure | detached | continues | none |
| local accept failure | not attached | continues | none |
| upstream failure | terminated with the generation | discarded | none |
| explicit supervisor stop | terminated | stopped | one bounded `QUIT` |

Only the last two rows may coincide with upstream shutdown. Client detach, protocol violation, and overload never send upstream `QUIT`; the runtime has regression evidence for each.

Queue overload is deliberately *not* a detach. A saturated queue is the bouncer's own bound, not client misbehaviour, so the bounded response is to lose that one frame for that one client and count it. Detachment stays reserved for what the client actually did — `QUIT`, EOF, a protocol violation, or a writer failure.

Registration requires NICK and USER before sending 001; the NICK must match the configured current upstream network nick under negotiated casemapping. The runtime answers client PING locally, mediates CAP LS/REQ/END against a fixed reviewed downstream capability set, and routes a bounded command allowlist upstream. Unsupported commands receive 421. Client-supplied prefixes are rejected, and client tag budgets are checked before re-encoding.

## Registration and CAP negotiation

Registration state is four explicit facts, not an implicit `ready` flag: a valid NICK received, USER received, no outstanding CAP negotiation, and already projected.

| client sends | negotiating | registered | effect |
| --- | --- | --- | --- |
| `CAP LS 302` before NICK/USER | yes | no | local `CAP * LS :` reply |
| `CAP REQ :…` before NICK/USER | yes | no | local `CAP * NAK :…`; nothing goes upstream |
| `CAP END` before NICK/USER | no | no | waits for both remaining facts |
| NICK/USER with no CAP sent | no | on the second fact | single `001` projection |
| NICK/USER while negotiating | yes | no | no `001`, no `005`, no JOIN/topic/mode/NAMES |
| `CAP END` after NICK/USER | no | yes | one `001` projection using current state |
| `CAP LS`/`LIST` after registration | no | unchanged | locally answered; registration is never undone |
| `CAP ACK`/`NAK` from a client | unchanged | unchanged | `410 … Invalid CAP subcommand` |

A client that entered CAP negotiation therefore cannot observe any part of the registration burst before it sends `CAP END`, and a client that never uses CAP is unaffected. Repeated or late CAP commands are deterministic: negotiation only ever starts before registration, and it ends at most once. Downstream CAP is mediated locally, so it cannot alter the upstream generation's negotiated capability set. Advertisement is derived from semantics the bouncer itself serves and is independent of the attached client's brand or upstream CAP offer.

## Projection truthfulness

The projection is bounded and derived from state the runtime actually observed:

- `005` forwards the retained ISUPPORT token set, which the generation learned while detached.
- `332` is emitted only for a retained topic; a topic larger than the local ceiling is omitted rather than truncated, and the per-line projection is clipped at the wire limit.
- `324` is synthesized from retained channel mode state and only when that state is complete. Parameterized modes keep their arguments, so a retained `+kl key 42` is projected as `+kl key 42`.
- `353`/`366` are emitted only when a member list was observed and is complete.

Incomplete knowledge is expressed by omission, never by a false value. A mode letter the server has not declared in `CHANMODES`, a required argument that the server did not send, a membership change for an unknown member, or a ceiling breach marks the affected channel incomplete; an authoritative `324` restores completeness. A client that attaches mid-registration receives `001` once its own registration completes. Channel projection iterates observed membership only: a configured or written-but-unconfirmed join is never projected, and a join the server rejected never appears at all.

## Bounds

Every externally controlled quantity is bounded: 2048 members per channel, 8192 members in total, 128 channels, 128 ISUPPORT tokens, 128 mode letters per channel, 16 arguments per mode, 100-byte mode arguments, 400-byte topics, 8 prefix pairs, 8 channel-type symbols, 64 mode letters per `CHANMODES` group. Client queues hold 8 control and 64 normal frames, and the client writer task is aborted and joined on detach, so a canceled session cannot outlive itself. A projection larger than the bounded client queue fails the client explicitly with an overload disposition rather than unbounded buffering; the retained state itself stays with the generation for the next client.

The current runtime advertises draft/chathistory, draft/read-marker, message-tags, batch and labeled-response. server-time and echo-message remain deliberately withheld until their full downstream semantics are implemented. Local SASL server authentication is not part of the current bound-session core.

## Admission owns the socket before any Network does

Since M005-A a client socket is not handed to a Network owner at accept time.
`DownstreamAdmission` takes it first: it splits the stream, starts the writer task,
allocates the ephemeral `SessionId`, and drives registration against a bounded ceiling.

That ordering is what makes three things possible that were not possible before:

- a client that registers with no Network selected has somewhere to be refused;
- a client refused before registration can be told *why*, because the component holding
  its write half is not the owner that just rejected it;
- the per-Network session ceiling stops being the only admission control, so a client
  flood against a full Network no longer spends owner work to be told the answer is no.

Registration is enforced against the selected Network's registered nickname while it runs,
so a client cannot claim an identity its Network did not register. The owner re-validates
the same claim when it adopts the session, because the binding that selected the Network
may predate a configuration change.

### The transfer

`PreparedSession` is one-shot and carries the reader itself: the socket half, the decoder
with its undecoded bytes, the writer task, the negotiated capabilities, and the `SessionId`
allocation. `SessionTask::resume` adds one task and nothing else.

Nothing is recreated: no second socket, no second decoder, no second writer, no second
registration projection, and no change of identity. Client lines that arrived in the same
read that completed registration survive, because the decoder's decoded-but-untranslated
batch is drained before the socket is read again.

Because the session is consumed by value and is not `Clone`, a conversation cannot be
offered to a second owner.

### The unbound control-only session

A client with no Network selected keeps a live socket and a working protocol. It is told
plainly that it has no Network, receives no channel list, and every command that needs one
is refused by name with a reason rather than dropped. It has no upstream authority.

### Refusals are written, not dropped

Ending a client's socket without saying why is indistinguishable from a fault. A refused
registration writes its reason on the socket the client opened and then drains the writer
before closing, so the explanation reaches the client rather than dying with the
connection.

## A detached channel is not a client UI state

Attached sessions may be shown fewer channels than the bouncer holds. `NetworkState::visible_channels()` is the single accessor that decides this, and projection, initial read markers, and legacy backlog all read it rather than reaching for observed membership directly. A future downstream-facing path that used `joined_channels()` would bypass the policy; there is one door and it is the right one.

When a channel becomes detached, every attached session receives a synthetic `:bouncer PART <channel>`. The prefix is the bouncer's reserved name, never the requesting client's nickname and never a participant upstream: the bouncer is still in the room, and a frame attributed to a real person would say otherwise. The transition is written to the session's control queue and is never a durable `HistoryEvent` — it records a local presentation decision, not something that happened on the network.

When a channel is reattached, each attached session receives `:bouncer JOIN <channel>` and then the same bounded topic/mode/NAMES block a newly registered client would receive. A session whose capabilities include the read-marker draft also receives that channel's marker, because the draft requires it to follow the `JOIN`.

Legacy backlog still runs only after the projection, and still only for a client that wants it. A client that negotiated `draft/chathistory` receives no automatic replay, so a reattach cannot duplicate history it is about to ask for. Reattaching does not create a second cursor system: the per-client playback cursor remains the delivery boundary, and it advances only after the session writer confirms the bytes reached the socket.

The compatibility shorthand is `PART <channel> :detach` and `PART <channel> :attach`, and it requires exactly two parameters. Anything else — a real `PART` with a part message, or a longer form — is an ordinary part, because guessing at near-misses would let a client that meant to leave a channel instead silently change the bouncer's whole presentation policy. `BouncerServ`, landed in Plan 023, is the primary explicit administration path; the shorthand exists so the policy is reachable before it.

## A session is active or passive, and only one of those is presence

A session carries a `SessionPresence` classification rather than being counted as a socket. It starts `Active` and becomes `Passive` when the session itself says so, which is what keeps a background history-sync or monitoring client from holding the Operator permanently present. Presence aggregation reads the classification, never a socket count; see [presence and preferred-nick policy](presence-and-nick.md).

The channel is `draft/pre-away`, advertised in `CAP LS` and honoured only when the client negotiated it. A client that asks for it may declare `PASSIVE` or `ACTIVE` during registration, before `CAP END` completes, and `SessionReader::registration_intent` drains the decoded buffer first so that pre-registration declaration is actually observed rather than discarded after registration. An unnegotiated declaration is refused rather than absorbed: a client must not be able to silence the Operator's presence with a command whose consequences it never asked for.

A downstream session is refused unless its claimed nick fold-matches the nick this Network currently holds. After an upstream collision that means a client registering as the preferred nick is refused and told why on its own socket, because projecting it under an identity the bouncer does not hold would show every client a nickname the network does not agree to.
