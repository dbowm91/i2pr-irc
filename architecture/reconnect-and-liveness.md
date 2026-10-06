# Reconnect and liveness

Provider connection attempts use a 120-second bound; upstream registration uses a 180-second bound; CAP/SASL negotiation reads use a 90-second bound; stream writes use a 30-second bound. Online liveness sends a generation-scoped PING every 60 seconds and ends the generation after 120 seconds without the matching PONG. Only a PONG that answers an outstanding probe satisfies liveness; an unsolicited or mismatched PONG is a protocol failure. PING/PONG traffic is handled directly by the generation actor and does not depend on any client.

Liveness and upstream state processing run with zero attached clients. A local client attach or detach does not pause the probe, and a probe deadline ends the generation exactly as it would with a client attached. Failed generations use exponential backoff from 1 second to 300 seconds with deterministic bounded jitter derived from the attempt generation. A generation that remains online for five minutes resets the attempt count. Stop cancels connection, registration, online, and backoff futures.

A failed local accept is a local-only failure. It leaves the generation online, records an `accept` error class, and retries after a bounded 50 ms delay. The acceptor is not polled while that delay is pending, so an acceptor with no waiting client cannot spin the owner.

Explicit stop raises a shutdown fence before sending its single bounded upstream `QUIT`, so no queued user traffic reaches the network after the fence. Failed generations are aborted instead, and user messages are never queued across the failure boundary.

The implementation uses Tokio's monotonic timer for these runtime deadlines. Paused-time integration tests advance the online probe with no client attached and prove a missing matching PONG causes a fresh generation. The M001 core `Clock`/`Timer` abstraction remains available for a later runtime adapter.

Backoff bounds how often *one* Network retries. It does nothing about the case that
actually hurts on a router restart, where every Network fails together and retries together.
A process-wide `ReconnectScheduler` gates that separately — see `reconnect-budget.md`.
