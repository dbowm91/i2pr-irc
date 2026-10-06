# External differential results — obby-proto 0.3.0 / obby-client 0.3.0

Research date: 2026-10-06
Crates: `obby-proto` 0.3.0 and `obby-client` 0.3.0 (`obbyworld/obby-client`, default branch `main`, release commit `ba911881f52e`, 2026-09-07)
Toolchain: `rustc 1.100.0-nightly (a36d05efa 2026-09-09)` — the crate declares MSRV 1.90, above this project's 1.88 floor
Harness: temporary project outside this workspace, on nightly so the MSRV floor is not violated even for research
Corpus: `research/irc-conformance/vectors/wire.txt`, plus direct casemapping/ISUPPORT/PREFIX/CHANMODES probes

## Message-level comparison

| vector class | owned i2pr-irc | obby-proto 0.3.0 | classification |
|---|---|---|---|
| command/params/trailing form (WV-001..005, 030..032) | agrees | agrees | none |
| unknown command (WV-006) | preserved | preserved | none |
| unknown 3-digit numeric (WV-007) | preserved | preserved | none |
| `353` with `@` visibility (WV-008) | agrees | preserved as parameters | none |
| 15 parameters accepted (WV-009) | agrees | agrees | none |
| exactly 512 bytes including CRLF (WV-010) | agrees | agrees | none |
| opaque/vendor/valueless/duplicate tags (WV-012..016) | agrees | agrees | representation mismatch (duplicate retention) |
| tag escape unescaping (WV-017..019) | agrees | agrees | none |
| invalid UTF-8 (WV-020, WV-021) | byte-oriented | unrepresentable: `Message::parse` takes `&str` | cannot represent due to a UTF-8/API assumption |

## State-primitive comparison — the most valuable part of this research

`obby-proto` is the only candidate that models the state primitives M003 depends on:
`Casemapping`, `Isupport` with `CHANTYPES`, `Prefix`, `ChanModes`, `Tags`, and
`parse_channel_modes`. Direct probes produced:

| primitive | owned | obby-proto | classification |
|---|---|---|---|
| `ascii`: `Bot`≡`bot`; `nick[`≢`nick{`; `nick^`≢`nick~` | agrees | agrees | none |
| `rfc1459`: `Bot`≡`bot`; `nick[`≡`nick{`; `nick]`≡`nick}`; `nick^`≡`nick~` | agrees | agrees | none |
| strict casemapping: `bot~`≢`bot^`, `nick[`≡`nick{` | agrees | agrees once the token is spelled `rfc1459-strict` | see the finding below |
| `CHANTYPES=#&+!` → `+lobby`/`!weird` are channels, `nick`/`bot` are not | agrees | agrees | none |
| `CHANMODES` classes → argument arity | agrees | agrees | none |
| `PREFIX` → membership symbols | agrees | agrees | none |

**Finding R002-O1 (candidate bug, and the origin of C003-F2):** `obby-proto` matches the
Modern IRC spelling `rfc1459-strict` — so it does not have this problem. What it *did*
reveal is that this project's own runtime recognized only the older `strict-rfc1459`
spelling and silently fell back to plain `rfc1459` for a server advertising the Modern IRC
value. `vinezombie` independently uses `rfc1459-strict` as well, so two maintained
implementations agree on the current spelling and the owned runtime was the outlier. The
owned runtime now accepts both, and no longer folds `~`/`^` on a network that declared them
distinct. See `plans/closure/bouncer-core/006-status.md`.

**Malformed-value handling.** `ChanModes::parse` ignores classes past the fourth rather than
rejecting the token; the owned runtime rejects a malformed `CHANMODES` and keeps the previous
classes authoritative. Both fail closed, and the owned behavior is the stricter one. This is
a design difference, not a defect in either.

## Design ideas worth carrying into M003 without importing the architecture

- `CaseFolded` as a distinct newtype so a raw `String` can never be used as an identity key by
  accident. The owned runtime currently compares through `same_nick` at each use site; a
  folded key type would make the class of mistake unrepresentable rather than merely avoided.
- `Isupport::apply` returning a parsed `Token` (with `removed` and accumulated `+=` values) so
  a caller never re-reads the wire grammar. `draft/extended-isupport-0.2` `+=` accumulation is
  directly relevant if M003 ever wants early ISUPPORT.
- A single `Prefix::split`/`char_for_mode`/`mode_for_char`/`rank` surface instead of
  per-call-site prefix parsing. The owned runtime already centralizes this; the reference
  confirms the shape.

None of these require a dependency. They are recorded as design inputs only.

## Dependency, license, and safety assessment

```text
obby-proto v0.3.0
└── thiserror v2.0.21 (proc-macro: proc-macro2, quote, syn)
```

`obby-client` adds `x25519-dalek`, `ed25519-dalek`, `chacha20poly1305`, `hkdf`, `zeroize` for
its optional `e2ee` feature, and `obby-proto` plus `serde` by default. No `build.rs`, no
`unsafe` in `src/`.

Licensing: **GPL-3.0-or-later**. This repository distributes under `MIT OR Apache-2.0`. A
GPL dependency is incompatible with that distribution strategy and cannot be adopted without
a licensing decision that is explicitly outside this research's authority. MSRV 1.90 is also
above this project's 1.88 floor.

## Disposition

**Reference only.** Highest-value behavioral reference of the four candidates for the state
model, because it is the only one that models `Casemapping`/`Isupport`/`Prefix`/`ChanModes`
the way this project needs them. Not usable as a dependency: GPL-3.0-or-later is incompatible
with the repository's `MIT OR Apache-2.0` distribution, MSRV 1.90 exceeds the floor, and the
message parser is `&str`-based like the others. No source, test, or fixture from this crate is
copied into this repository; the research ran it externally and committed only
independently authored inputs and summarized outputs.