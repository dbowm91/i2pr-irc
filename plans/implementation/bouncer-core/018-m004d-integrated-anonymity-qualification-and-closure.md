# Bouncer Core M004-D — Integrated Anonymity Qualification and M004 Closure

Status: blocked

Blocker:

- `plans/closure/bouncer-core/017-status.md` accepted

Source milestone:

- Bouncer Core M004

Primary class: invariant + capability qualification

## 1. Objective

Close M004 only after the core can truthfully be described as suitable for anonymity-network operation at the router-neutral byte-stream boundary.

This final pass integrates Corrective 014 and M004-A/B/C, performs the final privacy/security review, reconciles documentation/planning, and records the evidence needed to unblock M005.

## 2. Required final claims

M004 closure must prove all of the following:

### Anonymity/protocol

- DCC cannot invoke network behavior;
- DCC cannot reach a local client as an actionable request;
- ACTION remains correct;
- reviewed CTCP policy is complete;
- local client software/version/time/environment cannot leak through CTCP auto-replies;
- client-only tags follow explicit deny policy;
- CLIENTTAGDENY is truthful;
- no ambient host identity values become IRC-visible;
- secret/redaction tests pass;
- raw protocol logging is disabled by default.

### Upstream fingerprint

- upstream CAP request set is stable for the same server offer regardless of downstream clients;
- CTCP remote-visible behavior is fixed by bouncer policy, not attached-client brand;
- no build/router/OS/version string is exposed by default.

### Multi-client correctness

- Corrective 014 response routing is live end to end;
- concurrent query replies reach only the requesting SessionId;
- stale sessions/generations cannot receive replies.

### Reconnect/adverse operation

- global scheduler gates startup and retry;
- connect concurrency and start rate are bounded;
- fairness/no starvation proven;
- terminal failures do not consume infinite retries;
- many-Network outage/recovery remains bounded;
- slow client/store pressure cannot starve PING/PONG/control;
- ambiguous user traffic is never replayed;
- stale generation work cannot mutate current state.

### Durability/recovery

- restart/crash preserves committed DesiredState/history/cursors/read markers;
- stale ObservedState is not restored;
- schema v2 remains valid and migration history intact.

### Resource recovery

- tasks/queues/routes/batches/reconnect waiters return to expected bounded baseline after campaigns;
- no hidden unbounded retry or buffering structure exists.

### Structural network boundary

- static guard proves no generic DNS/TCP/HTTP/proxy/DCC production path;
- I2pStreamProvider remains the sole upstream stream authority.

## 3. Final audit

Perform source/dependency review for:

- `std::env` / hostname/user lookup;
- generic socket/resolver imports;
- HTTP/proxy dependencies;
- debug/display paths for Secret/credentials;
- raw IRC logging;
- CTCP/DCC code;
- tag mediation;
- scheduler cancellation;
- unsafe code;
- build scripts/transitive changes introduced since M003.

Document every production dependency added during M004. No new dependency should be unexplained.

## 4. Documentation reconciliation

Update:

- canonical docs only if clarification is needed without changing product direction;
- anonymity/privacy architecture;
- CTCP/DCC policy matrix;
- client-tag policy;
- reconnect scheduler architecture;
- fault model/qualification docs;
- bouncer-core roadmap;
- active registry;
- README status where appropriate.

Remove stale Corrective-013/M004-planning language.

Preserve historical closure records; do not rewrite them.

## 5. Verification

Expected final commands:

~~~sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo tree --locked -e all
scripts/check-network-boundary.py
scripts/fuzz-smoke.sh
scripts/verify.sh full
rustup run 1.88.0 sh scripts/verify.sh full
~~~

Also run the full M004 deterministic qualification target from Plan 017.

## 6. Closure artifact

Create:

- `plans/closure/bouncer-core/018-status.md`

It must reference:

- Corrective 014 closure;
- M004-A/015 closure;
- M004-B/016 closure;
- M004-C/017 closure.

Include:

- final CTCP/DCC matrix;
- client-tag/CLIENTTAGDENY matrix;
- environment/secret negative matrix;
- upstream fingerprint matrix;
- response-routing matrix;
- reconnect budget/fairness matrix;
- adverse fault matrix;
- restart/crash matrix;
- resource baseline/peak/settled table;
- static network-boundary evidence;
- dependency/MSRV review;
- unresolved findings/severity;
- explicit M005 readiness decision.

## 7. Acceptance criteria

M004 closes only when every roadmap exit condition is evidenced and no unresolved high-severity anonymity, alternate-egress, reconnect-herd, response-routing or resource-bound finding remains.

## 8. Stop conditions

Do not close M004 with:

- a known DCC/direct-network path;
- client-dependent upstream fingerprint;
- environment/secret leakage;
- dead response-routing machinery;
- unbounded reconnect admission;
- starvation;
- task/queue/resource leak;
- hidden replay of ambiguous user traffic;
- a raised Rust MSRV;
- missing predecessor closure.

Register a corrective instead.

## 9. Post-closure sequencing

If M004 closes cleanly:

- M005 becomes planning/research eligible;
- router R001 remains blocked until M005 closure;
- no router-specific implementation is authorized by M004 closure alone.
