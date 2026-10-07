# Network and session ownership

## The same rule one layer down: one router scope per Network

Ownership does not stop at the IRC layer. Below the runtime, `SamProvider` holds exactly one
long-lived SAM session per durable `NetworkId`, and that session survives any number of IRC
connection generations. The two layers therefore agree on which Network a resource belongs
to: the owner is the only writer of IRC state, and the provider scope is the only holder of
that Network's router identity.

An IRC reconnect never releases the scope, because the identity belongs to the Network rather
than to a generation. Deleting the Network, or shutting down, releases it exactly once and only
after the owner has been joined. This is what stops two Networks from ever sharing a router
identity, and therefore what stops an observer at the bridge from correlating them.

## One live owner per Network

Each upstream Network has exactly one live owner. `NetworkOwner` holds that Network's `NetworkState` as a plain local value — never behind a shared lock — and is the only thing that may mutate it.

```text
RuntimeController                process-level: which Networks exist and are live
   |
   |-- DurableNetworks (the only durable Network mutator)
   |-- bounded control queue (CONTROL_REQUEST_CAPACITY)
   |
   |-- LiveOwner { SupervisorHandle, watch stop, owner snapshot, JoinHandle }
   |      |
   |      `-- NetworkOwner (network 1)
   |             NetworkState (local to this owner)
   |             upstream read/write for one ConnectionGeneration
   |             SessionTask(a), SessionTask(b), ...
   |
   `-- LiveOwner { ... }
          `-- NetworkOwner (network 2)
                 completely independent
```

There is deliberately no process-wide `Arc<Mutex<NetworkState>>`. A shared mutable state object would make one Network's slow or failing work stall every other, and would make "one live owner per Network" false at the type level.

### Why the owner is held with its task, not just by a handle

`NetworkOwner` is spawned as a task, so a bare `SupervisorHandle` proves nothing about whether the owner has finished. A controller holding only handles could replace a Network and briefly have two live owners for one `NetworkId` — exactly the state this document exists to forbid.

`LiveOwner` therefore holds the bounded handle, a dedicated stop signal, a read-only view of the owner's gauges, **and** the `JoinHandle`, in one value. Every mutation that replaces or ends a Network awaits the join before starting or returning, so "the Network is gone" and "the Network's task finished" are the same event rather than two that can disagree.

The stop signal is separate from the bounded command queue on purpose. Shutdown that queues behind work the owner has not reached is a deadlock: the queue is full precisely because the owner is busy, so it will not drain until it stops, and it will not stop until the queue drains.

`NetworkOwner::with_snapshot_channel` exists so the controller can publish a Network's own gauges without owning the owner. The controller holds a `watch::Receiver` — it can read them and can never drive them.

### Durable mutation happens before activation

`RuntimeController` is the only component that mutates durable Networks, through the narrow `DurableNetworks` trait. `StoreHandle` is never handed to a client task, a session, or an owner by the controller.

Create, change, and delete each commit durable state first and activate second:

- **Create** validates, commits, then starts. If activation fails the record stays durable and is reported as not live — the Operator's intent survives a failed start, and the reverse order would leave a live Network the Operator can neither see nor delete.
- **Change** stops the old owner and awaits its task *before* committing, so a replacement cannot race the owner it replaces.
- **Delete** stops the owner and awaits it *before* forgetting the row. The reverse order would leave a durable row whose owner is gone and whose sessions have silently ended.

A mutation whose commit state is unknown is never retried and never assumed. Durable state is re-read, and whatever is actually on disk is what gets started. Asking storage again is cheap; writing a candidate over a row another actor may have changed is not recoverable.

## Sessions are tasks, not branches of one loop

With N sessions attached, a single `select!` cannot hold N read halves. Each session therefore owns a task with its own decoder, CAP/registration state, control queue, normal queue, and writer. The owner holds only bounded routing metadata: a `SessionHandle` with two `mpsc::Sender`s.

That split is what keeps "many clients" from widening the owner's own state. The owner's per-session footprint is two channel senders, not a stream or a parser.

## Session identity versus client lineage

`SessionId` names one live attachment. `ClientId` names the durable lineage that owns playback and read state. A client that reconnects receives a fresh `SessionId`, which is why a late result scoped to a previous attachment cannot reach its replacement.

Matching `ClientId` is never sufficient grounds for delivery. An event naming a session the owner does not hold is stale and is dropped, rather than being applied to whatever session now occupies that slot.

## Generation stamping

A session submits bytes and an intent class. The **owner** stamps the live `ConnectionGeneration`. A session therefore cannot attribute a frame to a generation it does not own.

The generation writer drops any intent stamped by an earlier generation rather than writing it. A disconnect after an outbound command leaves delivery ambiguous, so replaying user chat across a reconnect would risk duplicating a message whose delivery is unknown.

## Bounded fanout, detachment rather than a skipped frame

One upstream event is normalized and applied once, then fanned out to every attached session.

- Control traffic uses a control queue, so a saturated normal queue cannot delay a keepalive answer.
- A session whose queue refuses the frame is **detached**. The owner does not wait for it, and every other session and the upstream are unaffected.
- A slow reader applies backpressure at its own bounded queue rather than growing memory.

A downstream IRC stream is ordered. Once a live frame is skipped the bouncer can no longer claim that client is in step with upstream, and there is no way to tell it which frames it missed. Keeping the attachment would mean serving a stream that is silently missing state — so a refused frame ends that one `SessionId`, with its routes dropped and a `downstream-overload` disposition.

There is deliberately no "drop chat but keep the `MODE`" refinement. Importance is not knowable from the command alone: a `MODE`, `NICK`, `JOIN`, `PART`, `KICK` or a `BATCH` boundary can all leave a client holding state that later frames depend on. Ordered delivery *is* the contract, and breaking it ends that attachment.

Detaching is cheap for everyone else. The refused frame is counted in `fanout_dropped`, the session in `fanout_detached`, and both counters are separate because one client can refuse several frames before it is detached. A replacement attachment gets a full projection rather than a resumption, since the bouncer cannot know what the previous attachment already saw.

Because the owner applies every line in one read without yielding, a burst would otherwise starve session writer tasks and exhaust their bounded queues without ever being read — desynchronizing healthy clients for pressure they did not cause. `UPSTREAM_LINES_PER_TURN` yields cooperatively mid-chunk, well under the per-session queue, so an attachment that is keeping up can always drain within one window. This is a scheduler handoff, not a spin: an idle owner still sleeps.

Durable history keeps a deliberately different policy. A refused ingestion item is dropped and counted, because best-effort history is not part of the live stream's ordering guarantee and silently losing a stored message cannot desynchronize anything.

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

## A refused upstream enqueue is a local failure; deferred DesiredState is not

Because the database commit happens first, a refused enqueue splits the outcome in two, and conflating them was the defect Corrective 013 corrected.

An **ordinary forwarded command** — chat, `MODE`, `WHOIS` — is not durable. A refusal means it definitely was not accepted for delivery, so it is reported to the originating `SessionId` on the control queue (the queue that just demonstrated it is under pressure) and counted in `upstream_rejected`. It is never retried: a later generation could not know whether writing it again would duplicate it. Nothing is buffered, and no response route survives, because route allocation and send happen in the same owner turn.

**Desired membership** is the opposite case. The Operator's intent is already committed, so the bouncer owes a live state that matches what it stored. It converges through `DesiredReconcile`: a generation-local set holding only a channel name and the direction of intent, capped at `MAX_DESIRED_RECONCILE`, which matches the observed-membership ceiling because the set can only hold channels the Operator already asked the bouncer to track.

The latest committed intent wins for a channel, since the database already committed that one — replaying a superseded `JOIN` would contradict the `PART` that replaced it.

Reconciliation never carries chat. `JOIN` and `PART` are idempotent, so replaying them is safe; replaying user traffic across a connection generation is exactly what the generation stamp exists to prevent. It drains at the top of every turn and on its own `DESIRED_RECONCILE_INTERVAL` timer, because convergence cannot depend on traffic arriving: a committed `JOIN` may be the last thing that ever happens on a Network. If the bounded set cannot hold another entry, the durable intent is still correct in storage, so the generation is deliberately restarted rather than dropping the Operator's intent.

## What a projection may claim

A client must never be shown something untrue. The post-registration projection includes only what is known:

- Observed membership, never desired intent or an unconfirmed attempt.
- A topic only when the server sent one.
- A mode snapshot only when it is known complete.
- A NAMES list only when membership is known complete.

Desired but not yet joined channels are deliberately absent, so a projection can never claim a channel the bouncer is not actually in.

A client that negotiated `draft/read-marker` additionally receives each channel's current marker, emitted from the `JOIN` so it arrives even when membership is incomplete. An unknown marker renders as the draft's own `*` sentinel rather than a fabricated instant, and a channel whose retained marker can no longer be read back is reported as unknown. A client that did not negotiate the draft is never sent a `MARKREAD` at all — an unnegotiated command is a protocol violation for a strict client.
## Detached channels: hiding is not leaving

A desired channel carries two independent durable facts: that the bouncer wants it, and whether attached sessions may see it. `DesiredChannelRecord` stores both — `target`, a strictly increasing `position`, and `detached` — so the decision is durable, ordered, and explicit rather than derived from anything the process happens to be doing right now.

The two directions are not symmetric in what they touch.

**Detaching** persists the flag first, and only a commit that certainly landed changes what clients are shown. It then emits a synthetic `:bouncer PART <channel>` to **every** attached session and stops ordinary fanout and projection for that channel. It writes no upstream `PART` at all: membership, history ingestion, and durable buffering are untouched, which is the whole point of the distinction. The synthetic frame is prefixed with the bouncer's own reserved name rather than the Operator's nickname or any participant upstream, because nothing happened to anybody in the room and a frame attributed to a real person would be a false statement about an event that did not occur.

**Reattaching** persists the cleared flag first, then either projects immediately or joins first:

- With observed membership present, the channel gets `:bouncer JOIN` followed by the same bounded topic/mode/NAMES projection a newly registered client would receive. Showing a channel as joined without the topic, modes, and names that make the claim coherent is not a truthful projection.
- With membership absent, a `JOIN` is written and the reveal is deferred to the authoritative self `JOIN`. Projecting at request time would tell a client it had joined a channel the bouncer had not; projecting never would leave it told that it had joined a channel it never saw.

A reconnect restores the policy from storage rather than from the record the owner was born with. `NetworkState` is built per generation from `durable_desired_policy()`, because a detach or join performed mid-generation is durable intent just like any other, and taking the birth record would silently drop it.

### Visibility has exactly one accessor

`NetworkState::visible_channels()` is the single source of downstream-visible membership. Projection, read markers, and legacy backlog all read it, so a new downstream-facing path cannot accidentally bypass the policy by reaching for `joined_channels()` instead. History buffers are keyed by casemapped identity and *do* include detached channels, so replay paths filter explicitly: a client must not be handed messages from a channel it has just been told the bouncer does not show.

### Withholding and redacting are different

A line *about* a detached channel is withheld. A line that merely *mentions* one alongside visible channels is redacted rather than dropped, because suppressing it whole would desynchronize the visible channels it also concerns. A `QUIT` listing `:alice!a@h QUIT :#secret,#open` is delivered as `:alice!a@h QUIT :#open`; a `QUIT` listing only detached channels is withheld, since nothing is left to say. A frame naming both detached and visible channels at once is withheld whole — over-suppressing is the safe direction, because a client must never be shown a frame whose meaning depends on a channel it may not see.

Redaction drops the frame's tags. A tag is optional in every direction, and a server-chosen tag value may itself name a channel.

### Detached state is Network policy, not per-session UI

One client's detach changes what every attached session sees. A client that stayed silent would otherwise keep a channel it can no longer see, with no explanation for it disappearing. Per-client read and playback cursors stay private throughout: a client that was disconnected for the entire detached interval can still query retained history normally when it returns, because the history was never withheld — only the live view was.

### Failure semantics

A store refusal applies no live transition: nothing is presented, because presenting a detach that did not happen would show every client a channel that is still going to be there on reconnect. A commit whose outcome is `CommitState::Unknown` is resolved by re-reading the durable flag and letting *that* decide, with a notice to the requesting client either way. Guessing would make what a client sees depend on which of two outcomes this process happened to observe first — and the other outcome is what a reconnect sees.

`ChannelPolicy` is a trait so that branch is reachable from a test. A bare `StoreHandle` cannot be made to answer ambiguously without corrupting a real database; production passes `StoreChannelPolicy`, which is one hop from the owner to the bounded worker.
