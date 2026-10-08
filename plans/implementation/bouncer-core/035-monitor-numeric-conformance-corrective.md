# Bouncer Core Corrective 035 — MONITOR Numeric Conformance

Status: ready for handoff

Repository baseline:

- a5fa47b20b58ddb0e72007eaec72e9fa8e238d8e

Raised by:

- plans/research/008-m006-m007-irc-interoperability-and-identity-resilience.md

Historical authority:

- plans/implementation/bouncer-core/022-m005c-presence-and-preferred-nick-policy.md
- plans/closure/bouncer-core/022-status.md

External authority:

- IRCv3 MONITOR 3.2: https://ircv3.net/specs/core/monitor-3.2.html

Primary class: protocol correctness corrective

## 1. Objective

Correct the live preferred-nick reclaim path so IRCv3 MONITOR numerics are interpreted according to the specification before M006/M007 build further identity behavior on top of them.

This is a narrow correction to an already-closed M005-C feature.

## 2. Finding

Current code and historical Plan 022 closure treat:

- 730 as RPL_MONITOROFFLINE;
- 731 as RPL_MONITORONLINE.

The IRCv3 MONITOR specification defines:

- 730 = RPL_MONONLINE;
- 731 = RPL_MONOFFLINE.

Current consequence:

- receiving 730 for the preferred nick can incorrectly wake a NICK reclaim attempt while the server says the nick is online;
- receiving 731 when the preferred nick actually becomes offline/free is ignored.

Severity: high for keep-nick correctness; bounded to Networks with keep_nick enabled and usable MONITOR support.

## 3. Required semantics

Reclaim evidence becomes:

- 731 listing the preferred nick => positive "nick is offline/free" evidence;
- 730 listing the preferred nick => no reclaim write;
- 303 RPL_ISON with preferred nick absent => positive free evidence;
- 303 with preferred nick present => no reclaim write;
- every other numeric => no reclaim evidence.

The NICK write remains only a request. Success is still confirmed solely by the server's own NICK/state transition.

## 4. Parsing requirements

MONITOR numerics may include:

- recipient nick or *;
- target list in the trailing parameter;
- 730 targets optionally carrying !user@host.

For 730/731 comparison:

- compare nick only;
- use current Network casemapping;
- strip optional hostmask from 730 target before comparison;
- split comma-separated target lists;
- keep all parsing bounded by existing wire/tag/line ceilings.

Do not parse arbitrary server text.

## 5. Regression matrix

Tests must cover:

- 730 preferred nick online => no evidence;
- 731 preferred nick offline => immediate evidence;
- 730 nick!user@host form => no evidence;
- 731 comma-separated list containing preferred => evidence;
- 731 list without preferred => no evidence;
- casemapping equivalence;
- 303 preferred absent => evidence;
- 303 preferred present => no evidence;
- unrelated numeric => no evidence;
- evidence wake causes at most one bounded reclaim write;
- reclaim-write ceiling unchanged;
- stale-generation evidence cannot act on replacement generation.

## 6. Documentation reconciliation

Update:

- architecture/presence-and-nick.md;
- comments in owner.rs/presence.rs;
- Plan 022 interpretation through the Corrective 035 closure record.

Do not rewrite plans/closure/bouncer-core/022-status.md as though the defect were known when it closed.

## 7. Verification

Run:

~~~sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
scripts/check-network-boundary.py
scripts/verify.sh full
rustup run 1.88.0 sh scripts/verify.sh full
~~~

## 8. Acceptance criteria

Corrective 035 closes when:

1. 730 is treated only as online;
2. 731 is the MONITOR offline/free evidence;
3. optional 730 hostmask syntax is handled without false matches;
4. ISON behavior remains correct;
5. no new reclaim write path bypasses generation fencing or ceilings;
6. current and Rust 1.88 full verification pass.

## 9. Closure evidence

Create plans/closure/bouncer-core/035-status.md with:

- before/after numeric matrix;
- regression transcript;
- verification results;
- explicit Plan 036 readiness.
