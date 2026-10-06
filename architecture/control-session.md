# M005 process control and downstream admission

Status: planned architecture authority for M005

Decision authority: plans/adrs/ADR-0003-process-runtime-control-and-pre-bind-downstream-admission.md

Research authority: plans/research/006-m005-mature-bouncer-and-control-session-research.md

## Ownership

The process-level control path and one Network data path are separate.

~~~text
LocalAcceptor
     |
     | authenticated/trusted ClientId + ByteStream
     v
DownstreamAdmission ----------------------+
     |                                    |
     | unbound control                    | typed RuntimeControl requests
     v                                    v
Control-only session <-------------- RuntimeController
                                          |
                                          | owns lifecycle only
                                          v
                                     NetworkCatalog
                                          |
                       +------------------+------------------+
                       |                                     |
                       v                                     v
                 NetworkOwner A                        NetworkOwner B
                       |                                     |
                 bound SessionTask(s)                   bound SessionTask(s)
                       |                                     |
                       v                                     v
                 I2pStreamProvider                     I2pStreamProvider
~~~

RuntimeController never becomes an upstream IRC state owner. NetworkOwner remains the only mutable owner of one Network.

## Admission lifetime

Admission allocates SessionId before Network selection and creates the bounded downstream writer immediately.

It retains the read half and LineDecoder while processing local pre-registration commands.

The only terminal admission outcomes are:

- local failure/detach;
- registered unbound control-only session;
- PreparedSession transferred exactly once to one selected NetworkOwner.

PreparedSession carries enough state that no IRC byte, CAP decision or registration fact is lost at handoff.

## Control requests

Bouncer administration crosses a bounded typed RuntimeControlHandle. StoreHandle and provider handles are never exposed to a client task.

Expected request families include:

- list/status Networks;
- select/bind Network;
- create/change/delete Network;
- reconcile Network;
- channel policy changes;
- operator policy changes.

Wire adapters such as soju.im/bouncer-networks and BouncerServ render these typed results independently.

## Durable/live convergence

The store remains durable authority. The controller owns the sequencing required to make live owners converge.

Unknown commit state is always resolved by re-reading durable state.

Deletion quiesces a live owner before durable removal and restarts it only if re-read proves the row survived.

Connection-sensitive change quiesces the old owner, persists the candidate, then starts whichever record re-read proves durable.

## Notifications

Process status and privileged control views are distinct.

CatalogStatus stays redacted and safe for diagnostics.

A local control view may contain configuration needed by authenticated operator tooling but must not be emitted through generic logs.

A bounded watch snapshot with monotonic revision feeds network-notify semantics; clients derive deltas from bounded current state rather than accumulating an unbounded event log.

## Compatibility

Legacy callers that already know NetworkId may continue using direct pre-bound attachment.

M005 bouncer-network support adds unbound admission; it does not remove the simple path.

## What Plan 020 landed

This section records the ownership that actually shipped in M005-A, as distinct from the
design this document anticipated.

### RuntimeController

`RuntimeController<P>` is one task that owns every Network owner, the durable Network
records, and the single bounded queue through which all of it is mutated.

```text
RuntimeControlHandle (cloneable, bounded, redacted)
   |  typed ControlRequest over CONTROL_REQUEST_CAPACITY
   v
RuntimeController task
   |-- DurableNetworks  (the only durable Network mutator)
   |-- BTreeMap<NetworkId, LiveOwner { handle, stop, snapshot, join }>
   `-- watch<ControlSnapshot>  (bounded, revisioned)
```

A caller may hold `RuntimeControlHandle`. It is a bounded request sender, a stop signal,
and a read-only status subscription. It is not a `NetworkCatalog`, not a `StoreHandle`,
and not a `SupervisorHandle`, so no client task and no downstream session can reach a
supervisor directly or reach storage at all.

Every mutation is a named `ControlRequest` variant. There is deliberately no "run this
closure against the runtime" form: a closure would hand the controller's invariants to
its caller, while an enum keeps every capability that exists written down where it can
be read and bounded.

`DurableNetworks` is a trait rather than a bare `StoreHandle` so that the
`CommitState::Unknown` path is testable. A real SQLite commit failure cannot be provoked
from a test, but a durable layer that answers with that state can be, and that branch is
the single most consequential one in the controller.

### Bounded, revisioned, redacted status

`ControlSnapshot` carries a monotonic `revision` plus one bounded entry per Network, read
from the owner's own gauges. Comparing only field values cannot distinguish "nothing
changed" from "everything changed back", so a reader that must prove it saw a specific
change compares revisions.

A Network with no live owner reports `live: false` and no gauges at all, rather than a
zero that would read as "healthy and empty".

The snapshot contains only counts, durable names, and fixed classifications. It never
carries an endpoint, payload, nickname, credential, or path, so publishing it cannot leak
what `AGENTS.md` forbids leaking into IRC-visible or operator-visible fields.

A mutation republishes *before* it touches storage. An owner that has been stopped is
stopped, and a snapshot that still claims a live owner would leave an Operator waiting for
something that is never coming.

### Shutdown is on a side channel

Stop is a dedicated `watch` signal, not a queued request. A saturated control queue can
therefore never make shutdown unreachable — shutdown is the one operation that must always
be available, and it is exactly the operation a busy runtime is least able to serve.

### DownstreamAdmission

Admission takes ownership of a client socket at accept time, before anything is known
about which Network it will use. It splits the stream, starts the writer, allocates the
ephemeral `SessionId`, and drives registration.

Registration happens on the client's own socket and is bounded by an explicit ceiling. A
client that stalls is told why on the socket it opened and then closed; the ceiling is
applied inside the registration step so the write half survives it, because cancelling
outside that step would drop the only half of the socket that can say anything.

When registration completes, admission either hands the client to the selected Network or
serves it locally as a control-only session. A selection that names a Network that does not
exist is refused; there is no fallback Network.

### PreparedSession transfer

`PreparedSession` is consumed by `bind` and carries the reader itself — the socket half,
the decoder with its undecoded bytes, the writer task, the negotiated capabilities, and
the `SessionId` allocation. The owner adopts it with `SessionTask::resume`, which adds one
task and nothing else: registration is not re-run, the decoder is not rebuilt, the writer
is not respawned, and the session identity does not change.

Client lines that arrived in the same read that completed registration are preserved by
the decoder's parked batch, so a command sent immediately after `USER` is answered rather
than lost.

The transfer is one-shot. `PreparedSession` is neither `Clone` nor passed by reference, so
a conversation cannot be offered to a second owner.

The owner re-validates the claimed nickname against its own registered one. The binding
that selected a Network may predate a configuration change, and an owner must never
project a session under an identity it does not hold. A mismatch is answered with `433`
on the client's own socket, not a silent close.

The registration projection is requested through the ordinary intent path rather than
performed during adoption. There is exactly one implementation of the projection, so an
adopted client and an attached one cannot diverge, and neither can be projected twice.

### The unbound control-only session

A client with no Network selected keeps a live socket and a working protocol. It is told
plainly that it has no Network, and every command that needs one is refused by name with
a reason. It receives no channel list, no upstream projection, and no history, and it has
no upstream authority of any kind.

### Compatibility

The direct pre-bound attachment path is unchanged and still supported. M005-A adds an
admission path alongside it; both are covered by tests asserting they project the same
welcome burst, so the two cannot drift apart unnoticed.
