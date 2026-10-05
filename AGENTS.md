# AGENTS.md

## Authority

Use this order when documents disagree:

1. plans/000-long-term-specification.md and plans/001-terminology-and-domain-model.md
2. accepted ADRs under plans/adrs/
3. the applicable subsystem roadmap
4. the active milestone implementation plan
5. current repository evidence

Repository evidence may require a corrective plan, but it does not silently weaken a canonical invariant.

## Product boundary

i2pr-irc is I2P-only upstream.

Production code MUST NOT add generic hostname/IP connection APIs, system DNS for upstream IRC, clearnet IRC transports, SOCKS or HTTP CONNECT, DCC direct-connect behavior, arbitrary HTTP egress, or a general Proposal 170 administrator credential.

Standalone SAM access is a router adapter, not permission for arbitrary network access. Future i2pr integration must consume public managed-app capabilities rather than private router internals.

## Implementation posture

Correctness precedes feature breadth. All externally controlled lines, tags, collections, queues, history queries, timers, reconnect attempts, diagnostics, and pending requests require explicit ceilings.

Each upstream network has one live owner. Downstream sessions submit bounded typed intents instead of sharing a large mutable lock.

A disconnect after an outbound IRC command may leave delivery ambiguous. Never blindly replay user chat or other non-idempotent commands across a connection generation.

No environment-derived hostname, username, OS/router version, local path, process ID, or machine identifier may be inserted into IRC-visible fields by default. Secrets and SASL payloads must not be logged.

plans/registry.md is the active planning control surface.

## Rust workspace checks

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`
- `scripts/verify.sh quick` or `scripts/verify.sh full`

If implementation requires generic host DNS, generic upstream TCP, arbitrary HTTP egress, or private i2pr internals, stop for architecture review.
