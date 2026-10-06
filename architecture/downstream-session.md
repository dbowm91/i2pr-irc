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

Registration requires NICK and USER before sending 001; the NICK must match the configured current upstream network nick under negotiated casemapping. The runtime answers client PING locally, handles CAP LS/REQ/END with an empty advertised capability set, and routes a bounded command allowlist upstream. Unsupported commands receive 421. Client-supplied prefixes are rejected, and client tag budgets are checked before re-encoding.

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

A client that entered CAP negotiation therefore cannot observe any part of the registration burst before it sends `CAP END`, and a client that never uses CAP is unaffected. Repeated or late CAP commands are deterministic: negotiation only ever starts before registration, and it ends at most once. Downstream CAP is mediated locally, so it cannot alter the upstream generation's negotiated capability set, and the advertised downstream capability set remains empty.

## Projection truthfulness

The projection is bounded and derived from state the runtime actually observed:

- `005` forwards the retained ISUPPORT token set, which the generation learned while detached.
- `332` is emitted only for a retained topic; a topic larger than the local ceiling is omitted rather than truncated, and the per-line projection is clipped at the wire limit.
- `324` is synthesized from retained channel mode state and only when that state is complete. Parameterized modes keep their arguments, so a retained `+kl key 42` is projected as `+kl key 42`.
- `353`/`366` are emitted only when a member list was observed and is complete.

Incomplete knowledge is expressed by omission, never by a false value. A mode letter the server has not declared in `CHANMODES`, a required argument that the server did not send, a membership change for an unknown member, or a ceiling breach marks the affected channel incomplete; an authoritative `324` restores completeness. A client that attaches mid-registration receives `001` once its own registration completes. Channel projection iterates observed membership only: a configured or written-but-unconfirmed join is never projected, and a join the server rejected never appears at all.

## Bounds

Every externally controlled quantity is bounded: 2048 members per channel, 8192 members in total, 128 channels, 128 ISUPPORT tokens, 128 mode letters per channel, 16 arguments per mode, 100-byte mode arguments, 400-byte topics, 8 prefix pairs, 8 channel-type symbols, 64 mode letters per `CHANMODES` group. Client queues hold 8 control and 64 normal frames, and the client writer task is aborted and joined on detach, so a canceled session cannot outlive itself. A projection larger than the bounded client queue fails the client explicitly with an overload disposition rather than unbounded buffering; the retained state itself stays with the generation for the next client.

The runtime does not advertise message-tags, batch, SASL, or other downstream capabilities because it does not implement those semantics for downstream clients.
