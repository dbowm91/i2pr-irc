# Network and session ownership

## One live owner per Network

Each upstream Network has exactly one live owner. `NetworkOwner` holds that Network's `NetworkState` as a plain local value — never behind a shared lock — and is the only thing that may mutate it.

```text
NetworkCatalog                  process-level: which Networks exist
   |
   |-- SupervisorHandle -> NetworkOwner (network 1)
   |      NetworkState (local to this owner)
   |      upstream read/write for one ConnectionGeneration
   |      SessionTask(a), SessionTask(b), ...
   |
   `-- SupervisorHandle -> NetworkOwner (network 2)
          completely independent
```

There is deliberately no process-wide `Arc<Mutex<NetworkState>>`. A shared mutable state object would make one Network's slow or failing work stall every other, and would make "one live owner per Network" false at the type level.

## Sessions are tasks, not branches of one loop

With N sessions attached, a single `select!` cannot hold N read halves. Each session therefore owns a task with its own decoder, CAP/registration state, control queue, normal queue, and writer. The owner holds only bounded routing metadata: a `SessionHandle` with two `mpsc::Sender`s.

That split is what keeps "many clients" from widening the owner's own state. The owner's per-session footprint is two channel senders, not a stream or a parser.

## Session identity versus client lineage

`SessionId` names one live attachment. `ClientId` names the durable lineage that owns playback and read state. A client that reconnects receives a fresh `SessionId`, which is why a late result scoped to a previous attachment cannot reach its replacement.

Matching `ClientId` is never sufficient grounds for delivery. An event naming a session the owner does not hold is stale and is dropped, rather than being applied to whatever session now occupies that slot.

## Generation stamping

A session submits bytes and an intent class. The **owner** stamps the live `ConnectionGeneration`. A session therefore cannot attribute a frame to a generation it does not own.

The generation writer drops any intent stamped by an earlier generation rather than writing it. A disconnect after an outbound command leaves delivery ambiguous, so replaying user chat across a reconnect would risk duplicating a message whose delivery is unknown.

## Bounded fanout, local loss

One upstream event is normalized and applied once, then fanned out to every attached session.

- Control traffic uses a control queue, so a saturated normal queue cannot delay a keepalive answer.
- A session whose queue is full is refused. The owner **does not wait for it**: that one client loses that one frame, the loss is counted in `fanout_dropped`, and every other session and the upstream are unaffected.
- A slow reader applies backpressure at its own bounded queue rather than growing memory.

A full queue is *not* grounds for ending the attachment. The queue is the bouncer's own, not the client's misbehaviour, so dropping one frame is the bounded response; detaching a client over an internal queue would turn a momentary hiccup into a disconnect. Detachment stays reserved for what the client actually did: `QUIT`, EOF, a protocol violation, or a writer failure.

The refusal is counted rather than silent. A saturation that is invisible is indistinguishable from a healthy Network in the snapshot, which would make the only available diagnostic useless exactly when it is needed.

Teardown is deterministic: the owner takes ownership of every session task and shuts each one down before the generation ends, so no client task outlives the generation that created it.

## The snapshot is sampled every turn, not at teardown

`attached_sessions`, `upstream_normal_queue_depth`, `upstream_control_queue_depth`, and `response_routes` describe the live generation, so the owner republishes them after every turn of its loop rather than only when the generation ends.

Publishing only at teardown would report a healthy Network as having no sessions, an empty queue, and no open routes for its entire working life — an untruthful diagnostic that is worst precisely when an operator is watching. The write is conditional, so a busy but unchanged generation does not wake every subscriber once per line. Because these are sampled gauges, a reader that needs a settled value should compare against a later turn rather than assume atomicity.

## Persistence-first DesiredState

A client `JOIN`/`PART` is an intent, not a forwarded command:

1. The owner commits the durable intent to storage.
2. Only on commit does it write the upstream `JOIN`/`PART`.
3. On failure it tells the client and writes nothing upstream.

The invariant is therefore "no upstream JOIN without a prior durable commit", not "a JOIN eventually arrives". `a_failed_join_commit_writes_no_upstream_join` proves the strong form: with the store unavailable, the upstream stream contains no `JOIN` at all while the Network stays `Online`.

Writing a JOIN proves nothing about membership. Each attempt is recorded as outstanding, and only an authoritative self JOIN from the server closes it as confirmed.

A storage failure fails one client operation. It never ends the upstream Network.

## What a projection may claim

A client must never be shown something untrue. The post-registration projection includes only what is known:

- Observed membership, never desired intent or an unconfirmed attempt.
- A topic only when the server sent one.
- A mode snapshot only when it is known complete.
- A NAMES list only when membership is known complete.

Desired but not yet joined channels are deliberately absent, so a projection can never claim a channel the bouncer is not actually in.