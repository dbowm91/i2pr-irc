# Network supervisor

M002's runtime is composed by `NetworkSupervisor::serve`. It owns one provider attempt, one accepted local client stream, one upstream connection generation, both stream readers/writers, and the backoff loop. No task is detached. The injected `I2pStreamProvider` is the only upstream authority; `LocalAcceptor` supplies the downstream stream separately.

Each connection attempt receives a monotonically increasing `ConnectionGeneration`. The generation is attached to runtime ownership and ping tokens; a failed generation is dropped before another provider attempt. Registration and stream writes have ceilings. `LineDecoder` bounds retained input and rejects malformed/oversized messages.

The online owner uses separate bounded control and normal output queues (8 and 64 frames). The normal queue carries typed `OutboundIntent` values tagged with their generation; the writer discards any intent whose generation does not match its owner. The writer always selects control before queued normal traffic. A full queue returns an explicit overload failure and ends that session/generation; commands are never silently dropped or retained for replay. Upstream and downstream writer tasks belong to the generation's `JoinSet`, which aborts children on owner exit and joins them on clean stop.

`subscribe_snapshot` exposes phase, generation, active nick/channels, reconnect attempt and delay, last error class, queue depths, and attached-client state. It does not expose raw errors, message payloads, endpoints, or credentials.

The initial implementation supports one downstream client at a time. A failed upstream generation drops that local connection; a later generation accepts a new local client. Upstream user messages are never queued across the failure boundary. Desired channels are sent again only after a fresh welcome.

Configuration validation bounds identities, channels, and SASL credential sizes before the runtime starts. Snapshots and diagnostics must not include credential values or payloads.
