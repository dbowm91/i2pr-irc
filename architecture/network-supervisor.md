# Network supervisor

`NetworkSupervisor::serve` owns one upstream Network across connection generations. It owns provider attempts, the connection-generation counter, backoff, and the upstream generation actor. It does not own any local client: a `LocalAcceptor` supplies at most one disposable downstream view at a time, and the generation actor handles attachment as data.

```
NetworkSupervisor::serve                         (owns generations + backoff)
└── run_generation(upstream, generation, acceptor, stop)
    ├── upstream read half        owner loop      registration, liveness, observed state
    ├── upstream write half       owner loop      CAP/SASL + welcome (registration only)
    └── upstream writer task      JoinSet         control(8) + normal(64) bounded queues

    LocalAcceptor (zero-or-one at any instant)
    ├── accepted  -> DownstreamSession { read half, decoder, bounded queues }
    │                                  SessionWriter { exit signal, JoinHandle }
    └── detached  -> session + writer aborted and joined; generation continues
```

Registration happens with no client attached. The generation reaches `Online` on its own, so local-client absence can never prevent or delay upstream registration. Each attempt receives a monotonically increasing `ConnectionGeneration`, which tags outbound intents and liveness tokens; a failed generation is discarded before another provider attempt.

Observed state (`NetworkState`) lives inside the generation actor, so it is retained across every downstream attach and detach and reset only when a generation is discarded. Upstream and downstream stream halves, the decoder, and the session writer task are owned by the generation; nothing is detached. On exit the generation drops the session, aborts and joins the session writer task, then either joins the upstream writer after the final QUIT or aborts it immediately.

The owner loop uses separate bounded control and normal output queues (8 and 64 frames). The normal queue carries typed `OutboundIntent` values tagged with their generation; the writer discards any intent whose generation does not match its owner. The writer always selects control before queued normal traffic. A full upstream queue is an explicit failure that ends the generation; a full downstream queue ends only that client. Commands are never silently dropped or retained for replay.

`subscribe_snapshot` exposes phase, generation, current nick and joined channels, reconnect attempt and delay, last error class, upstream and downstream queue depths, downstream attachment state, cumulative attached-session and detach counters, the most recent detach disposition, and the upstream event count. It does not expose raw errors, message payloads, endpoints, or credentials. Both counters are monotonic so a later attach cannot hide an earlier detach.

Configuration validation bounds identities, channels, and SASL credential sizes before the runtime starts. Desired channels are validated conservatively against `#` and `&` before connecting; live channel classification follows the server-advertised `CHANTYPES`. Snapshots and diagnostics never include credential values or payloads.
