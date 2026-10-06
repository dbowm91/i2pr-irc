# Research 003 — Rust IRC Crate Conformance and Reuse Results

Status: closed

Research plan: `plans/research/002-rust-irc-crate-conformance-plan.md`

Research date: 2026-10-06

Research baseline reviewed: `3de5e66e49826735346a23030374ad96bfdb5a3b`, with Corrective 005
behavior included as the plan required (closure `plans/closure/bouncer-core/005-status.md`,
commit `bdcd0ef`).

Final compared tree: the Corrective 006 closure, which corrects the two defects this research
found (`plans/closure/bouncer-core/006-status.md`).

## 1. Outcome

**The owned wire and state layers are retained. No candidate becomes a production dependency.
No research gate remains open, and M003 is unblocked.**

The default hypothesis in the plan — that the owned codec is kept and external crates are
references — held. It held on evidence, not on preference: each candidate fails at least one
documented criterion in the plan's §8, and the criteria that fail are the ones this project
adopted its own layer to satisfy.

Two i2pr-irc defects were found by this research, both corrected under Corrective 006, and one
of them is a specification-reading defect that only appeared because two independent maintained
implementations disagreed with the owned code.

## 2. Candidate matrix

| candidate | version | latest commit / release | license | MSRV | status |
|---|---|---|---|---|---|
| `ircv3_parse` | 4.0.0 | `5957f9b21c0a`, 2026-03-03 | MIT OR Apache-2.0 | 1.78 | active, 0 stars / 2 forks, 0 open issues |
| `irc-proto` | 1.1.0 | `269b5179dd47`, 2026-01-01 (branch `develop`) | MPL-2.0 | 1.60 | mature, 581 stars, 37 open issues |
| `irc` (high level) | 1.1.0 | same repository | MPL-2.0 | 1.71 | client/network oriented |
| `vinezombie` | 0.3.1 | `176c36eb2b37`, 2025-04-13 | EUPL-1.2-only | 1.70 | self-described work in progress, 0.x |
| `obby-proto` | 0.3.0 | `ba911881f52e`, 2026-09-07 | GPL-3.0-or-later | 1.90 | new (first release 2026-09-07), 5 stars |
| `obby-client` | 0.3.0 | same release | GPL-3.0-or-later | 1.90 | new |

Dependency and safety footprint, measured from the actual resolved trees:

| crate | mandatory dependencies | build script | `unsafe` in `src/` |
|---|---|---|---|
| `ircv3_parse` (`default-features = false, features = ["std"]`) | `bytes`, `memchr`, `thiserror` (+ proc-macro) | none | none |
| `irc-proto` (`default-features = false`) | `thiserror` (+ proc-macro) | none | none |
| `vinezombie` | zero mandatory; optional `crypto` pulls `ring`, `tls-tokio` pulls rustls/tokio | none | none |
| `obby-proto` | `thiserror` (+ proc-macro) | none | none |
| `obby-client` | `obby-proto`, `serde`, `base64`; optional `e2ee` pulls x25519-dalek, ed25519-dalek, chacha20poly1305, hkdf, zeroize | none | none |

Network authority: none of the four candidate families owns sockets, resolvers, TLS, or proxies
in its *low-level* entry point. That is a genuine change from the M001 assessment of 2023 and
is why they were re-examined. It does not change the disposition, because the licensing,
MSRV, and validation-policy criteria fail independently. The high-level `irc` crate still owns
TLS and proxies by default (`tls-native`, `proxy`, `tokio-socks`) and remains unsuitable for
the runtime regardless.

## 3. Protocol vector sources

The corpus in `research/irc-conformance/vectors/` was authored from primary specifications, not
adapted from any crate's fixtures:

| source | used for |
|---|---|
| RFC 2812 §2.3, §3.3, §3.7.2, §4.1.1, §4.2.1, §4.2.2 | message grammar, NUL/CR/LF, JOIN/PART/KICK/NICK, numerics, PING/PONG, ISUPPORT, PREFIX, CHANMODES, RPL_NAMREPLY |
| IRCv3 message-tags | 8191-byte tag prefix, 4094-byte direction-specific tag data, escapes, duplicate keys, empty values, invalid UTF-8 values |
| IRCv3 capability negotiation (v2.2), SASL | CAP 302 flow, registration suspension, LS/REQ/LIST/END semantics, post-registration CAP |
| Modern IRC Client Protocol (the base specification IRCv3 adopted) | `CASEMAPPING` values, `PREFIX`, `CHANTYPES`, `CHANMODES`, trailing parameters, byte-oriented character handling |
| current server implementations in the wild | channel-failure numerics `403`/`405`/`471`/`473`/`474`/`475`/`476` |

Corpus size: 32 message vectors, 14 framing vectors, 28 state vectors, 15 capability vectors.
Every vector carries a specification citation, a stable/draft status, a UTF-8-relevance flag,
and a bouncer-relevance rationale. Two committed runners execute them against the owned
implementation; external comparisons ran in temporary projects outside this workspace so that no
candidate crate entered `Cargo.lock`.

## 4. Differential behavior matrix

Full per-candidate detail is in `research/irc-conformance/results/`. Summary of where the
implementations differ on identical inputs:

| behavior | owned | ircv3_parse 4.0.0 | irc-proto 1.1.0 | obby-proto 0.3.0 | classification |
|---|---|---|---|---|---|
| 512-byte body ceiling | enforced | not enforced | not enforced | not enforced | owned stricter |
| 8191-byte tag prefix | enforced | not enforced | not enforced | not enforced | owned stricter |
| 15-parameter ceiling | enforced | not enforced | not enforced; **mis-parses past 15** | not enforced | owned stricter; `irc-proto` additionally wrong |
| 512-byte token/prefix | enforced | not enforced | not enforced | not enforced | owned stricter |
| embedded CR | rejected | **accepted** | **accepted** | not probed | external more permissive |
| embedded LF | rejected | **accepted** | **accepted** | not probed | external more permissive |
| NUL | rejected | rejected incidentally | **accepted** | not probed | external more permissive |
| 2/4-digit numeric | rejected | rejected | **accepted** | not probed | owned and `ircv3_parse` stricter |
| prefix with no command | rejected | rejected | **accepted** | not probed | owned stricter |
| invalid UTF-8 anywhere | byte-oriented | unrepresentable (`&str`) | unrepresentable (`&str`) | unrepresentable (`&str`) | owned policy required by invariant 7 |
| tag escape unescaping | unescapes on parse | caller must call `unescape()` | unescapes | unescapes | representation difference |
| `key=` vs bare `key` | both become no value | distinguishes `Empty`/`Flag` | keeps `Some("")` | — | representation difference |
| duplicate tag keys | final value retained | all retained | all retained | all retained | representation difference |
| unknown command / numeric | preserved | preserved | preserved (`Raw`) | preserved | none |
| round-trip fidelity | parse/encode asserted in tests | not applicable (no owned encoder) | `to_string` lossy for `Raw` forms | — | none material |
| casemapping (ascii/rfc1459/strict) | agrees with both other state-model implementations | — | — | agrees | none |
| `CHANTYPES` / `PREFIX` / `CHANMODES` | agrees | — | — | agrees | none |
| malformed `CHANMODES` | rejected, previous value retained | — | — | ignores classes past the fourth | owned stricter; both fail closed |

Agreement among libraries was never used as a correctness rule. Where ircv3_parse and irc-proto
disagreed with each other (CR/LF handling, numeric grammar), the specification decided it and
the owned behavior was recorded as the stricter one.

## 5. Every i2pr-irc discrepancy found

Three discrepancies were found. Two were defects and are corrected; one was a corpus-authoring
error, recorded for completeness because the process matters more than the count.

| id | discrepancy | verdict | disposition |
|---|---|---|---|
| C003-F1 | the incremental decoder discarded the line following an over-long line, because the length ceiling was applied before the line terminator | **bug**, medium | corrected: `self.dropping = *byte != b'\n';` |
| C003-F2 | `CASEMAPPING=rfc1459-strict` — the Modern IRC Client Protocol spelling — was unrecognized and silently fell back to `rfc1459`, folding `~`/`^` that the server declared distinct | **bug**, medium | corrected: both spellings select the strict mapping |
| — | corpus vector `WV-026` initially contained 15 parameters while claiming 16 | corpus-authoring error, not an implementation defect | vector corrected; the runner now fails on any expectation that does not hold, which is how the error surfaced |

No discrepancy was found in the join/membership model, the CAP registration state machine, the
ISUPPORT-driven interpretation, or the projection truthfulness rules: all 28 state vectors and
all 15 capability vectors passed on first execution against the corrected Corrective 005
behavior.

## 6. Candidate dispositions

| candidate | disposition | reasoning |
|---|---|---|
| `ircv3_parse` 4.0.0 | **dev/research-only differential oracle** | License and MSRV are acceptable, but it enforces none of the project's documented ceilings and accepts embedded CR/LF. Production adoption would require re-imposing every ceiling around it — more code than the owned codec, and a caller that can forget one. Valuable as a second opinion: its grammar is stricter than the owned codec in several places. Not added to this workspace. |
| `irc-proto` 1.1.0 | **reference only** | Fails the CR/LF/NUL policy, every length ceiling, and the parameter ceiling (which it also mis-parses). MPL-2.0 would need a licensing decision this research does not make. Useful as an interoperability checklist of commands a relay must not drop. |
| `irc` 1.1.0 (high level) | **excluded** | Client/network oriented; default features pull `native-tls` and it can pull `tokio-socks`. Bringing it near the runtime would add exactly the transport authority the I2P-only boundary forbids. |
| `vinezombie` 0.3.1 | **reference only** | EUPL-1.2 network copyleft is incompatible with `MIT OR Apache-2.0` distribution; self-described work in progress; last commit 2025-04-13. Its `rfc1459-strict` spelling was directly useful and is one of the two confirmations behind C003-F2. Not executed. |
| `obby-proto` 0.3.0 | **reference only** | Highest-value behavioral reference of the four — the only candidate that models `Casemapping`/`Isupport`/`Prefix`/`ChanModes` the way this project needs. GPL-3.0-or-later is incompatible with the distribution strategy and MSRV 1.90 exceeds the 1.88 floor. Ran externally on nightly; no code copied. |
| `obby-client` 0.3.0 | **reference only** | Same license and MSRV as `obby-proto`, plus optional end-to-end encryption dependencies the project has no use for. |

## 7. Supply-chain and security assessment

| concern | assessment |
|---|---|
| candidate build scripts | none of the four candidate crates ships a `build.rs` |
| `unsafe` | none found in any candidate `src/` |
| proc-macro surface | `thiserror` only for all candidates; `obby-proto`/`obby-client` add `serde`, `ts-rs` optionally |
| network ownership in low-level crates | none; all four parse `&str` and own no transport |
| text-oriented API | all four take `&str`, which is the single most important security finding: they cannot represent the invalid-UTF-8 case the owned byte policy exists for |
| license compatibility with `MIT OR Apache-2.0` | `ircv3_parse` yes; `irc-proto` MPL-2.0 unresolved for this project's distribution; `vinezombie` EUPL-1.2 no; `obby-*` GPL-3.0-or-later no |
| code copied into this repository | none. Vectors were authored from specifications; external harnesses ran outside the workspace and only summarized outputs are committed |
| `Cargo.lock` impact | unchanged; no candidate crate was added as a dependency |

## 8. M003-facing IRCv3 semantics

M003 intends labeled-response, server-time, batch, history, cursors, and multi-client routing.
What this research establishes for that work:

- The owned codec already carries opaque tags with correct escaping, duplicate collapse, and
  direction-specific budgets, so M003 can enable capabilities without a new wire layer.
- Draft-only features remain version-labelled. The corpus records status per vector, and
  `+draft/…` capability names are ordinary parameters (vector `WV-032`), so a draft capability
  can never be silently treated as stable.
- `soju.im/bouncer-networks`-style multiple-upstream semantics are a *design* concern, not a
  wire concern: they constrain how many generations and identities exist, which is exactly the
  owner model Corrective 004 and 005 established. No candidate crate offers a bouncer-side
  abstraction; every candidate is client-shaped, so none of them reduces that design risk.
- The state-primitive model M003 will persist is validated against the best current reference
  implementation (`obby-proto`) and agrees with it, with one corrected defect found in the
  process.

## 9. Recommendation

**Retain `i2pr-irc-wire`, `i2pr-irc-core::Casemapping`, and the owned state model as production
authority. Do not replace any owned layer.**

The evidence for keeping the owned layers is not inertia:

1. Every candidate would require the caller to re-impose all four documented ceilings. The
   ceilings *are* the safety property.
2. No candidate can represent invalid UTF-8, so adopting one weakens a policy this project
   treats as canonical.
3. No candidate is server/bouncer-shaped; adopting a client abstraction for the bouncer side
   is the specific mismatch the plan warned about.
4. Two of four are license-incompatible with the distribution strategy and one exceeds the MSRV
   floor.

What changed because of this research:

1. A durable, independently authored conformance corpus now exists and runs on every
   `cargo test` (`research/irc-conformance/`, two committed runners).
2. Two real defects were found and corrected (Corrective 006).
3. `ircv3_parse` is identified as the right second opinion for future wire questions, to be run
   externally rather than depended upon.

## 10. M003 readiness decision

Both gates the roadmap placed on M003 are now closed:

1. **Corrective 005** closed with no high-severity blocker — `plans/closure/bouncer-core/005-status.md`.
2. **Research 003** records a clear disposition — this document — with no unresolved
   correctness defect affecting M003 design. The two defects this research found were corrected
   and closed in `plans/closure/bouncer-core/006-status.md`.

`plans/registry.md` therefore moves M003 from blocked to unblocked for implementation planning.
Unblocked is not the same as ready: there is still no M003 implementation handoff, and this
research does not authorize writing one. M004, M005, and the router milestones remain sequenced
behind M003.

The M003 handoff boundary, now including this corrective:

- persistence may store the generation-owned state model and durable operator intent;
- it must not restore pending or rejected join attempts as membership;
- it must not reintroduce coupled upstream/downstream lifetime;
- it must not assume mode state the runtime marked incomplete;
- it must key identities with a folded identity type that honors both strict casemapping
  spellings, rather than ad hoc string comparison;
- it must not assume a framing position that was recovered from an over-long line corresponds
  to a received message;
- the conformance corpus is the regression gate for any change to the owned wire or state
  layers.