# Workspace overview

The workspace separates `i2pr-irc-wire` (bounded protocol bytes), `i2pr-irc-core` (IDs and network/time contracts), `i2pr-irc-testkit` (deterministic short-I/O stream), and `i2pr-irc-runtime` (Tokio composition). `i2pr-irc-fuzz-smoke` feeds bounded arbitrary byte inputs through both wire entry points.

Only the runtime consumes `I2pStreamProvider`; core and wire own no concrete sockets or resolver. M001 is evidence-closed. The runtime provides the active M002 single-network vertical: it owns one upstream Network across connection generations and a zero-or-one local client attachment, so upstream registration, liveness, and retained state survive local-client absence. Observed channel, member, and mode state is driven by advertised ISUPPORT values and is projected only when it is truthful. It remains short of a complete daemon and has no concrete router integration.
