# Reference analysis — vinezombie 0.3.1

Research date: 2026-10-06
Crate: `vinezombie` 0.3.1 (`vinezombie/vinezombie`, default branch `main`, last commit `176c36eb2b37`, 2025-04-13)
Toolchain used: none — this candidate was analyzed by source and documentation review only
Corpus: not executed; see "Why it was not executed"

## Why it was not executed

- License **EUPL-1.2-only**, a network copyleft. Adopting it — even as a dev dependency
  linked into this workspace — would place EUPL obligations on this repository's
  distribution, which is `MIT OR Apache-2.0`. Research invariant 3 forbids copying
  GPL/EUPL code into this repository and invariant 5 forbids adding an incompatible-license
  crate for convenience.
- Upstream states it plainly: "*vinezombie is a work in progress. Use with care. It may have
  bugs, and there will be further breaking 0.x releases.*"
- The last commit is 2025-04-13, roughly 18 months before this research date.

Executing it would produce differential evidence we are not permitted to act on. The
information value is in its design, so it was read instead of run.

## What the source shows

| property | observation | value to this project |
|---|---|---|
| parsing model | zero-copy, `no enums with fallback cases` | confirms the project's instinct that a relay needs unknown commands/numerics to remain representable |
| correctness claim | "without using `unsafe`, it should be impossible to construct a correctly-sized message that, once written, does not parse into the same message" | round-trip fidelity is the right invariant for a relay; the owned codec tests it directly |
| casemapping | `IrcCasemap::{Ascii, Rfc1459, Rfc1459Strict}` parsed from `ascii`, `rfc1459`, **`rfc1459-strict`** | independent confirmation of the Modern IRC spelling; this is what exposed C003-F2 |
| tags and labeled-response | first-class support | relevant design input for M003 |
| SASL | implementation of connection registration including SASL | relevant design input; the project's own SASL sequencing is capability-gated and tested |
| TLS/async helpers | rustls connection helpers, self-signed certificates | would introduce TLS and transport authority; irrelevant to an I2P-only bouncer and explicitly out of bounds |
| dependency posture | "Zero mandatory dependencies; minimal optional dependencies" | the most attractive footprint of the four, but irrelevant while the license blocks use |

## Supply-chain and maintenance assessment

- EUPL-1.2-only: incompatible with `MIT OR Apache-2.0` distribution without a licensing
  decision this project has not made and should not make for a reference-only benefit.
- MSRV 1.70 nominally, but 0.x with an explicit work-in-progress warning and no releases since
  2024-05-02 on crates.io.
- Bus factor appears to be one maintainer (10 stars, 0 forks, 0 open issues on the GitHub
  repository at research time).
- Optional `crypto` feature pulls `ring`; optional `tls-tokio` pulls `rustls`,
  `rustls-native-certs`, `rustls-pemfile`, and `tokio-rustls`. None of this would be used by a
  bouncer that connects only through an I2P stream provider.

## Disposition

**Reference only.** Its casemapping spelling was directly useful — it is one of the two
independent confirmations behind C003-F2 — and its parsing-correctness framing is a useful
statement of intent for the project's own wire contract. Not executable within the license
and threat-model constraints, and not proposed as a dependency at any tier. No source, test,
or fixture from this crate is copied into this repository.