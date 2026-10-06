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
