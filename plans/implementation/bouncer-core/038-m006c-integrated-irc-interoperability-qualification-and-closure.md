# Bouncer Core M006-C / Plan 038 — Integrated IRC Interoperability Qualification and M006 Closure

Status: closed

Hard dependency:

- plans/closure/bouncer-core/037-status.md

Source milestone:

- M006 — IRC Interoperability and Capability Downgrade

Primary class: qualification + milestone closure

## 1. Objective

Qualify M006 as one product across legacy/minimal servers, modern IRCv3 servers, and simultaneous heterogeneous downstream clients.

This is a protocol/server-behavior qualification milestone. It does not add router/SAM conformance loops.

## 2. Server profiles

Build deterministic server transcript profiles for:

### Legacy/minimal

- no CAP;
- no SASL;
- no TLS assumption;
- classic 001/005/JOIN/NAMES;
- no MONITOR;
- no labeled-response;
- no message-tags.

### Transitional

- CAP 302;
- partial capability set;
- bare sasl;
- optional capability NAK;
- MONITOR ISUPPORT;
- account-notify but no account-tag;
- message-tags without all tag extensions.

### Modern

- full current reviewed capability set;
- account-tag;
- invite-notify;
- cap-notify NEW/DEL;
- existing history/standard-replies/labeled-response features.

## 3. Downstream client profiles

Run simultaneously:

- legacy client: no CAP;
- basic IRCv3 client: message-tags/server-time;
- modern client: current full advertised set;
- history-sync/passive client.

Prove one client's capability choices never alter upstream negotiation for the generation.

## 4. Required compatibility claims

- no-CAP upstream remains online;
- no-SASL upstream remains online when auth is not configured;
- configured SASL never silently downgrades;
- plain IRC bytes over I2P are canonical;
- account-tag is per-session filtered;
- invite-notify is per-session filtered;
- current history/routing/member-state capabilities still behave;
- CAP NEW/DEL updates downstream advertisement truthfully;
- no unsupported capability is advertised;
- chghost and extended-monitor remain absent.

## 5. Faults during registration

Inject:

- partial CAP LS then EOF;
- 001 before any CAP reply;
- optional CAP NAK;
- SASL NAK;
- SASL timeout;
- fragmented numerics;
- stale generation replacement during negotiation.

No ambiguous downstream user traffic exists/replays before registration completes.

## 6. Multi-client state qualification

During one generation:

- attach legacy and modern clients;
- deliver account-tagged message;
- deliver account-notify transition then unstamped message;
- deliver self and third-party invites;
- detach/reattach one client;
- issue concurrent WHOIS/NAMES;
- ensure response routing remains session-specific;
- ensure state/history remains one Network authority.

## 7. Verification

Run:

~~~sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
scripts/check-network-boundary.py
scripts/fuzz-smoke.sh
scripts/verify.sh full
rustup run 1.88.0 sh scripts/verify.sh full
~~~

No live Java/i2pd/i2pr router matrix is required.

## 8. Documentation

Add/update architecture documentation for:

- registration downgrade;
- auth requirement semantics;
- plain IRC-over-I2P;
- capability mediation/deferred list.

Update roadmap/registry on closure.

## 9. Acceptance criteria

M006 closes only when legacy/no-CAP/no-SASL and modern IRCv3 profiles are all usable under deterministic tests, and no high-severity protocol or multi-client finding remains.

## 10. Closure evidence

Create plans/closure/bouncer-core/038-status.md containing:

- server-profile matrix;
- client-profile matrix;
- auth/downgrade matrix;
- account-tag/invite matrix;
- capability-change matrix;
- verification results;
- unresolved findings;
- explicit M006 closure and Plan 039 readiness.
