# Bouncer Core Corrective 035 — Closure Status

Status: closed

Plan: `plans/implementation/bouncer-core/035-monitor-numeric-conformance-corrective.md`

## Implementation commits

- `657ba0e` — `fix: conform preferred-nick reclaim to MONITOR numerics`
- `bef370b` — `chore: restore current-toolchain clippy verification` (supporting verification repair; behavior-neutral current-Clippy updates)

## Requirement-to-evidence matrix

| Requirement | Evidence |
|---|---|
| 730 means online and cannot trigger reclaim | `monitor_online_is_not_free_evidence_and_offline_lists_match_by_nick`; sends `730 bot :bot!user@host` and observes no `NICK bot` |
| 731 means offline/free | `monitor_evidence_reclaims_the_preferred_nick`; sends `731 bot :bot` and observes the bounded reclaim request |
| Comma-separated MONITOR targets and nick-only matching | `monitor_online_is_not_free_evidence_and_offline_lists_match_by_nick`; `731 bot :other,bot` triggers reclaim |
| ISON 303 still treats absence as free evidence | `note_reclaim_evidence` retains absent-from-ISON semantics; parser still receives bounded parsed parameters |
| Casemapping and generation ownership remain authoritative | Comparisons call `NetworkState::same_nick`; stale-generation regression remains in `a_replaced_generations_reclaim_state_cannot_act_on_its_replacement` |
| Reclaim ceiling is unchanged | `reclaim_writes_are_capped_per_generation` |
| Documentation corrected without rewriting historical Plan 022 closure | `architecture/presence-and-nick.md` records the corrected 730/731 meanings; Plan 022 closure remains historical |

## Verification executed

- `rtk cargo test -p i2pr-irc-runtime --test m005c_presence_nick monitor --locked` — passed (5 tests).
- `rtk scripts/verify.sh full` — passed on the installed toolchain.
- `rtk rustup run 1.88.0 sh scripts/verify.sh full` — passed.

Both full runs include formatting, workspace Clippy with warnings denied, all locked workspace tests, the static network-boundary check, and fuzz smoke. Current Clippy exposed existing new-toolchain diagnostics in leap-year/base64 arithmetic, one sort comparator, a test fixture, and the SAM probe; the supporting commit rewrites these without changing behavior and preserves the Rust 1.88 floor.

## Security and recovery review

MONITOR/ISON input remains parsed through the bounded IRC line/message representation. Only numeric 731 named-target and 303 absent-target observations provide free evidence. Numeric 730 is explicitly non-free evidence, and its optional `!user@host` suffix cannot turn an online target into a match. No human-readable server text is parsed. A reclaim remains a request; the server's own NICK/state transition confirms success. The existing per-generation write bound and generation-local reclaim lifetime remain intact.

Finding: the pre-existing 730/731 inversion is corrected. No other finding was opened by this bounded corrective.

## Disposition

Corrective 035 is closed. Plan 036's hard dependency is discharged and Plan 036 is ready for handoff. Plans 037-041 remain gated by their stated sequential dependencies.
