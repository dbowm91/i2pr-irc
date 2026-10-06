# External differential results — irc-proto 1.1.0

Research date: 2026-10-06
Crate: `irc-proto` 1.1.0 (`aatxe/irc`, default branch `develop`, last commit `269b5179dd47`, 2026-01-01)
Toolchain: `rustc 1.89.0 (29483883e 2025-08-04)`, `default-features = false` (the crate's default feature set pulls in `tokio` and `tokio-util`)
Harness: temporary project outside this workspace
Corpus: `research/irc-conformance/vectors/wire.txt` (30 comparable vectors, 2 unrepresentable)

## Verdict per vector class

| vector class | owned i2pr-irc | irc-proto 1.1.0 | classification |
|---|---|---|---|
| command/params/trailing form (WV-001..005, 030..032) | agrees | agrees; trailing stays a separate enum field | representation mismatch |
| unknown command (WV-006) | preserved | preserved as `Command::Raw` | none |
| unknown 3-digit numeric (WV-007) | preserved | mapped to a typed `Command::Response` variant | representation mismatch |
| `353` with `@` visibility (WV-008) | agrees | preserved as parameters | none |
| 15 parameters accepted (WV-009) | agrees | agrees | none |
| exactly 512 bytes including CRLF (WV-010) | agrees | agrees | none |
| opaque/vendor/valueless tags (WV-012..015) | agrees | agrees | none |
| duplicate tag keys (WV-016) | final value retained | both retained | representation mismatch |
| tag escape unescaping (WV-017..019) | agrees | agrees | none |
| invalid UTF-8 (WV-020, WV-021) | byte-oriented | unrepresentable: `FromStr for Message` takes `&str` | cannot represent due to a UTF-8/API assumption |
| NUL in a line (WV-022) | rejected | **accepted** as `Command::Raw("PRIV\0MSG", …)` | external crate more permissive |
| embedded CR (WV-023) | rejected | **accepted**, CR retained inside a parameter | external crate more permissive |
| embedded LF (WV-024) | rejected | **accepted** | external crate more permissive |
| 513-byte body (WV-025) | rejected | **accepted** | external crate more permissive |
| 16 parameters (WV-026) | rejected | **accepted, and mis-split**: `#p15 #p16` collapses into one parameter | external crate more permissive **and wrong** |
| parameter longer than 512 bytes (WV-027) | rejected | **accepted** | external crate more permissive |
| prefix with no command (WV-028) | rejected | **accepted** as `Command::Raw("", [])` | external crate more permissive |
| empty line (WV-029) | rejected | rejected | agrees |

Targeted probes beyond the corpus:

| probe | owned | irc-proto |
|---|---|---|
| unknown numeric `999` | preserved | preserved as `Raw("999", …)` |
| four-digit `9999` | rejected | **accepted** as `Raw("9999", …)` |
| two-digit `99` | rejected | **accepted** as `Raw("99", …)` |
| bare prefix `:srv` | rejected | **accepted** as `Raw("", [])` |
| tag key containing a space | rejected | **accepted**, mis-split as `Raw("with=1", ["srv 001 bot :hi"])` |
| tag prefix of 8212 bytes | rejected | **accepted** |
| `key=` versus bare `key` | both normalize to no value | keeps `Some("")` distinguishable from `None` |

## Findings that matter for a bouncer

1. **The 15-parameter ceiling is not enforced and exceeding it corrupts the parse.** Sixteen
   parameters become fifteen, with the last two merged into one token. A bouncer that
   relayed or interpreted such a message would silently mis-attribute content.
2. **No CR/LF/NUL policy.** All three are accepted and retained, which is a message-forgery
   surface when a relay re-emits the parsed parameters.
3. **No length ceilings.** 512/8191/512-token enforcement is entirely the caller's problem.
4. **Four-digit and two-digit numerics are accepted** as raw commands, which weakens the
   command-token grammar the project treats as a relayable invariant.
5. **Typed `Command`/`Response` enums** make well-known commands ergonomic, but they are also
   the crate's main reuse obstacle for a bouncer: a relay must handle every command, and the
   `Raw` fallback is where the correctness risk concentrates.

## Dependency and safety footprint

`irc-proto` 1.1.0 with `default-features = false`:

```text
irc-proto v1.1.0
└── thiserror v1.0.69 (proc-macro: proc-macro2, quote, syn)
```

With default features it additionally pulls `tokio` and `tokio-util`. No `build.rs`, no `unsafe`
in `src/`. MSRV 1.60, license `MPL-2.0`.

## Disposition

**Reference only.** Useful as an interoperability reference for how a mature client crate
models commands, prefixes, and tag escapes, and its typed enums are a good checklist of the
commands a bouncer must not drop. Not a production dependency: it fails several of this
project's documented safety invariants (CR/LF/NUL, length ceilings, the parameter ceiling),
it cannot represent invalid UTF-8, and MPL-2.0 would need a licensing decision this research
does not make. The parameter-merge behavior above is recorded as an upstream observation,
not a patch: no code from this crate is copied into this repository.