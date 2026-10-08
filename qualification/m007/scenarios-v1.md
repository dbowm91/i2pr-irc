# M007 Eggchaos Scenario Set v1

These scenarios use loopback only. The upstream scenario places the pinned eggchaos 0.2.0
stream proxy between the production `SamProvider` and the deterministic fake SAM bridge.
The proxy shapes both SAM control exchanges and the raw IRC stream; evidence records only
state and counters, never payload captures.

| ID | Profile | External/in-process evidence |
|---|---|---|
| A | 20 s healthy at 10 ms ± 5 ms latency, then 90 s elevated at 50 ms ± 20 ms; 256 KiB/s bandwidth cap; 7 ± 3 byte slicing. Stream attempts remain at one through both windows. Provider scope baseline/peak/settled is 0/1/0; healthy scopes are 0/1/0; session creation is 0/1/1 cumulative. | `m007_eggchaos::production_sam_provider_registers_through_jitter_bandwidth_and_slicing` |
| B | blackhole, recovery, disconnect, and generation replacement | existing `adverse` and `r001c_sam_core_integration` deterministic socket suites |
| C | bandwidth starvation and frame slicing | `m005c_presence_nick`, `m006c` interoperability, and scenario A |
| D | 120 reconnect rounds across three Networks under paused Tokio time | `adverse::reconnect_churn_leaves_no_residue` |
| D2 | 100 deterministic accepted-stream open/close cycles through the stable eggchaos loopback proxy | `fault-smoke.py` repeated-churn phase |
| E | preferred collision, fallback, 731/303 evidence, reclaim refusal cooldown, and recovery | `m005c_presence_nick` registration/reclaim qualification |
| F | active + legacy + passive sessions, local NICK transition, alias attach, and isolated slow-client detach | controller/admission, multi-client, and `adverse` suites |

This repository has no standalone downstream TCP listener. The external downstream-proxy
placement is therefore deferred to the standalone-daemon milestone as Plan 041 allows. The
accepted-stream boundary remains covered by in-process multi-client tests.

The 0.2.0 CLI is not published on crates.io. The qualification runner pins the upstream Git
tag's immutable commit and prints the executable SHA-256. It does not add eggchaos to Cargo.
