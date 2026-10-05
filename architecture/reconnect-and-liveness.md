# Reconnect and liveness

Provider connection attempts use a 120-second bound; upstream registration uses a 180-second bound; CAP/SASL negotiation reads use a 90-second bound; stream writes use a 30-second bound. Online liveness sends a generation-scoped PING every 60 seconds and disconnects after 120 seconds without the matching PONG. PING/PONG traffic is handled directly by the network owner.

Failed generations use exponential backoff from 1 second to 300 seconds with deterministic bounded jitter derived from the attempt generation. A generation that remains online for five minutes resets the attempt count. Stop cancels connection, registration, online, and backoff futures. These are initial operational defaults, not IRC protocol requirements.

The implementation uses Tokio's monotonic timer for these runtime deadlines. A paused-time integration test advances the online probe and proves a missing matching PONG causes a fresh generation. The M001 core `Clock`/`Timer` abstraction remains available for a later runtime adapter.
