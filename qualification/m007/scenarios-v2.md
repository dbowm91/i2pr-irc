# M007 Eggchaos Scenario Set v2

These scenarios use loopback only. The upstream scenarios place the pinned eggchaos 0.2.0
stream proxy between the production `SamProvider` and the deterministic fake SAM bridge.
The proxy shapes both SAM control exchanges and the raw IRC stream; evidence records only
state and counters, never payload captures.

Version 2 adds the product-path destructive scenarios Corrective 042 required. Version 1
covered them only with in-process socket suites and a generic echo peer, which is weaker
evidence than recovery across a real process/socket boundary. `scenarios-v1.md` is
retained unchanged as the historical evidence document for the Plan 041 closure; where the
two disagree, this document is the current one.

## Evidence strength, stated explicitly

The runner prints these as separate sections because a single undifferentiated result line
lets the weakest evidence in a run be read as the strongest.

| Tier | What it establishes | Where |
|---|---|---|
| Generic fault smoke | the pinned fault tool does what it claims on a loopback echo peer | `fault-smoke.py` |
| Product-path shaping | latency/jitter/bandwidth/slicing do not cause a false reconnect on the production provider | scenario A1 |
| Product-path blackhole recovery | a silent transport is detected on a deadline and recovered from | scenario A2 |
| Product-path disconnect recovery | an immediate reset is replaced and recovered from | scenario A3 |
| Replay disposition | no ambiguous user frame crosses a generation boundary | scenario A4 |

Nothing above is claimed for the accepted-stream multi-client boundary: this repository has
no standalone downstream listener, and Corrective 042 forbids adding one. Those sessions
stay covered by the in-process owner suites (`m005f`, `adverse`, `m005i`), which attach
real sessions to a real owner.

## Scenarios

| ID | Profile | External/in-process evidence |
|---|---|---|
| A1 | 20 s healthy at 10 ms ± 5 ms latency, then 90 s elevated at 50 ms ± 20 ms; 256 KiB/s bandwidth cap; 7 ± 3 byte slicing. Stream attempts remain at one through both windows. Provider scope baseline/peak/settled is 0/1/0; healthy scopes are 0/1/0; session creation is 0/1/1 cumulative. | `m007_eggchaos::production_sam_provider_registers_through_jitter_bandwidth_and_slicing` |
| A2 | downstream blackhole: asserted *not* to end the generation within 20 s (a silent transport is not a dead one), then asserted to end it within the bounded liveness ceiling, then healed and recovered. | `m007_eggchaos::a_blackholed_generation_ends_under_liveness_and_recovers_through_the_product_path` |
| A3 | upstream hard reset on the active path: generation loss observed, bounded backoff, replacement generation, reconciled desired state, and a router session that is not rebuilt by an IRC reconnect. | `m007_eggchaos::a_hard_disconnect_replaces_the_generation_and_recovers_through_the_product_path` |
| A4 | the frames a replacement generation writes are only re-identification, desired membership, and its own liveness probe; anything else would be an ambiguous user command replayed across a boundary whose delivery is unknown. | `m007_eggchaos::a_disruptive_recovery_replays_no_ambiguous_user_traffic` |
| B | 120 reconnect rounds across three Networks under paused Tokio time | `adverse::reconnect_churn_leaves_no_residue` |
| D2 | 100 deterministic accepted-stream open/close cycles through the stable eggchaos loopback proxy | `fault-smoke.py` repeated-churn phase |
| E | preferred collision, fallback, 730/731/303 evidence, reclaim refusal cooldown, and recovery | `m005c_presence_nick` registration/reclaim qualification, including the Corrective 042 event-versus-snapshot matrix |
| F | active + legacy + passive sessions, local NICK transition, alias attach, and isolated slow-client detach | controller/admission, multi-client, and `adverse` suites |

A4's allowlist includes the `PING` the generation writes on a fresh connection. That frame
carries nothing a client ever typed, so allowing it is not a relaxation of the claim — it
is the difference between a frame the bouncer generates and a frame a user would have had
to type.

## Stream-loss disposition

**Qualified as a fault-tool capability only. Not qualified as packet loss.**

eggchaos stream-loss drops arbitrary application bytes with no TCP semantics: it has no
notion of a segment, a checksum, or a retransmission, so whether a connection survives
depends on *which* byte happened to be dropped. Dropping one byte of a SAM command line
leaves the client waiting for a reply that can no longer arrive; dropping one byte of an
IRC frame leaves a prefix the receiver cannot parse. Neither produces the behaviour real
packet loss produces on a TCP path, where the transport retransmits and the application
usually never notices.

Asserting reconnect semantics through that fault would therefore be asserting something
about byte-drop luck rather than about the bouncer, so it is exercised against a generic
echo peer (`fault-smoke.py`, 100% loss on the upstream direction) and is deliberately not
asserted through the product path. Real loss behaviour remains covered by the in-process
`slicing` and `bandwidth` paths in A1 and by the `adverse` suite, which are the honest
substitutes for it here.

## Fixture notes

Three things about the fixture are load-bearing, and each was found by a failure that looked
like broken recovery:

- **The fake bridge has no background reader.** IRC bytes are only read when the test pumps
  them, so a fixture that only inspects the recorded log is inspecting a buffer nobody
  filled. A standing in-process IRC server pumps and answers each new registration once.
- **Faults are armed by probability, not by redefinition.** Disruptive faults are declared
  with `probability = 0.0` and armed through the pinned admin control plane, so a fault
  exists and is addressable by id without ever being active by accident.
- **A graceful termination of one direction is not a disconnect.** eggchaos's default is a
  half-close, which leaves the client's socket half-open: writes vanish with no error and no
  EOF, so the bouncer waits in `registering` forever and the scenario reads as "recovery
  never happens" when nothing about recovery was tested. Scenario A3 uses `hard_reset`.

The bridge is scripted with several router sessions rather than one. A fault that takes the
SAM control connection down legitimately forces re-identification before a reconnect can
succeed, and a fixture able to answer only one session would then be measuring its own
exhaustion.

## Boundaries

The 0.2.0 CLI is not published on crates.io. The qualification runner pins the upstream Git
tag's immutable commit and prints the executable SHA-256. It does not add eggchaos to Cargo,
and ordinary `verify.sh` never invokes it.