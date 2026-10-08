# Plan 041 Closure — M007-C Eggchaos Multi-Client Adverse Qualification

Status: closed

## Scope and result

Plan 041 qualifies M007's identity and connectivity behavior under deterministic adverse
transport, using external Eggchaos socket shaping plus the repository's in-process runtime
and SAM regressions. M007 is complete. Plans 039, 040, and 041 are closed.

The qualification stays on loopback and does not add a production listener or dependency.
This repository has no standalone downstream TCP listener; external downstream-proxy
placement is deferred to standalone-daemon work as allowed by Plan 041. The accepted-stream
multi-client boundary is covered by existing deterministic tests.

## Eggchaos provenance and scenarios

- CLI: eggchaos 0.2.0, obtained from the upstream source tag because that release is not
  published on crates.io.
- Immutable source commit: `b6a277d5ad4267bd602bc15a4333b14322057b90`.
- Qualified executable SHA-256:
  `339cb51d84050940f57fe7a525e40183604bea63740c52ad90ad06e40d23e5c9`.
- Command: `EGGCHAOS_BIN=/path/to/eggchaos scripts/qualify-m007-eggchaos.py`.
- Scenario definitions: `qualification/m007/scenarios-v1.md`.

The external socket smoke reported PASS for 100 repeated open/close cycles, stable byte
transfer (32,768 bytes), bandwidth-limited transfer (32,768), slow close (32,768), slicing
(32,768), blackhole (zero bytes), disconnect, and stream loss (zero bytes). These scenarios
used loopback only and report counters without retaining or printing traffic payloads.

Scenario A passed through the production `SamProvider`, Eggchaos, and the deterministic fake
SAM bridge. The 20-second healthy baseline and 90-second elevated 50 ms ± 20 ms latency
profile retained one live upstream stream attempt, live Network, provider scope, and healthy
scope. The 256 KiB/s bandwidth cap and 7 ± 3 byte slicing preserved registration and desired
channel join. Peak provider/healthy scopes were 1/1; after Network deletion both returned to
zero. Session creation remained one (cumulative) and no refusal occurred.

## In-process counterpart and client coverage

- `adverse::reconnect_churn_leaves_no_residue`: 120 deterministic reconnect rounds across
  three Networks; resource counts return to baseline.
- `m005c_presence_nick`: preferred-nick collision, fallback, MONITOR/ISON evidence, cooldown,
  and eventual reclaim behavior.
- Controller/admission and multi-client suites: active, legacy, and passive clients, identity
  projection, session-isolated response routes, and isolated slow-client detach.
- `m006c` interoperability and `r001c_sam_core_integration`: registration, fragmentation,
  disconnect, and recovery counterparts.
- `m007_eggchaos::production_sam_provider_registers_through_jitter_bandwidth_and_slicing`
  is explicitly ignored by ordinary test runs and invoked only by the qualification script.

No user chat or other non-idempotent command is replayed across a generation. Service actions
remain bounded and phase-scoped; synthetic fixtures do not claim real NickServ authentication.
No traffic captures, credentials, I2P Destinations, or SAM secrets are included in the
qualification evidence.

## Verification and findings

- `scripts/verify.sh full` passed on the current toolchain, including workspace formatting,
  Clippy, tests, and fuzz smoke.
- `RUSTUP_TOOLCHAIN=1.88 ./scripts/verify.sh full` passed (the declared MSRV floor).
- `scripts/qualify-m007-eggchaos.py` passed with the pinned executable and all scenarios above.
- `scripts/check-network-boundary.py`, `cargo fmt --all -- --check`, and
  `git diff --check` passed.
- No high-severity findings remain. External downstream-listener fault placement is deferred
  with the standalone-daemon milestone; it is not a blocker to M007 closure.

## M007 closure

M007-A service-action sequencing, M007-B preferred-nick/reconnect/multi-client resilience,
and M007-C adverse qualification are complete. Transient nick occupation does not permanently
terminal the Network; preferred nick reclaim follows bounded evidence and cooldown rules;
clients converge on the same current identity; and adverse qualification showed no false
reconnect, ambiguous command replay, or retained provider scopes. M007 is closed.
