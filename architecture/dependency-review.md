# Dependency review

Rust 1.88 / edition 2024 is the workspace floor. Production dependencies are Tokio (async runtime, synchronization, timers and byte I/O), async-trait (provider object contract), thiserror (typed errors), base64 (SASL PLAIN wire encoding), and zeroize (credential drop cleanup). Wire has no dependencies. Core uses async-trait and Tokio byte-I/O traits. No IRC client/codec, network client, resolver, or storage dependency is present.
