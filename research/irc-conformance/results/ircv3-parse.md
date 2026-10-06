# External differential results — ircv3_parse 4.0.0

Research date: 2026-10-06
Crate: `ircv3_parse` 4.0.0 (`m3idnotfree/ircv3_parse`, default branch `main`, last commit `5957f9b21c0a`, 2026-03-03)
Toolchain: `rustc 1.89.0 (29483883e 2025-08-04)`, features `default-features = false, features = ["std"]`
Harness: temporary project outside this workspace, so the crate never entered this repository's `Cargo.lock`
Corpus: `research/irc-conformance/vectors/wire.txt` (30 comparable vectors, 2 unrepresentable)

## Verdict per vector class

| vector class | owned i2pr-irc | ircv3_parse 4.0.0 | classification |
|---|---|---|---|
| command/params/trailing form (WV-001..005, 030..032) | agrees | agrees | representation mismatch only (IRCv3 splits middles/trailing; i2pr-irc keeps one ordered parameter list) |
| unknown command and unknown 3-digit numeric (WV-006, 007) | agrees | agrees | none |
| `353` with `@` visibility (WV-008) | agrees | agrees (visibility stays an opaque parameter) | none |
| 15 parameters accepted (WV-009) | agrees | agrees | none |
| exactly 512 bytes including CRLF (WV-010) | agrees | agrees | none |
| empty trailing parameter (WV-004, 011) | agrees | agrees | none |
| opaque/vendor/valueless/duplicate tags (WV-012..016) | agrees | agrees; duplicate keys are all retained rather than collapsed to the final value | representation mismatch |
| tag escape unescaping (WV-017) | unescapes | leaves `a\:b\sc` escaped in the parsed value; the crate exposes a separate `unescape()` the caller must apply | external crate requires an explicit step |
| invalid tag escape (WV-018) | discards the backslash | leaves the escape sequence in the value | external crate requires an explicit step |
| final lone backslash (WV-019) | emits no byte | retains the trailing backslash in the value | external crate requires an explicit step |
| invalid UTF-8 in an ordinary parameter (WV-020) | byte-oriented, preserved | unrepresentable: `parse()` takes `&str` | cannot represent due to a UTF-8/API assumption |
| invalid UTF-8 in a tag value (WV-021) | key retained, value dropped | unrepresentable: `parse()` takes `&str` | cannot represent due to a UTF-8/API assumption |
| NUL in a line (WV-022) | rejected as `InvalidFraming` | rejected, but as a grammar error ("PARAM must be followed by a space"), not by a NUL policy | agrees in outcome, disagrees in reason |
| embedded CR (WV-023) | rejected | accepted, keeping `\r` inside a parameter | external crate more permissive |
| embedded LF (WV-024) | rejected | accepted | external crate more permissive |
| 513-byte body (WV-025) | rejected `TooLong` | accepted | external crate more permissive |
| 16 parameters (WV-026) | rejected | accepted | external crate more permissive |
| parameter longer than 512 bytes (WV-027) | rejected | accepted | external crate more permissive |
| prefix with no command (WV-028) | rejected `InvalidFraming` | rejected | agrees |
| empty line (WV-029) | rejected | rejected | agrees |
| tag prefix beyond 8191 bytes (probe) | rejected `TooLong` | accepted | external crate more permissive |

Targeted probes beyond the corpus:

| probe | owned | ircv3_parse |
|---|---|---|
| unknown numeric `999` | accepted, preserved | accepted, preserved |
| four-digit `9999` | rejected | rejected ("numeric command must be exactly 3 digits") |
| two-digit `99` | rejected | rejected |
| tag key containing a space | rejected | rejected |
| tag prefix of 8212 bytes | rejected | accepted |
| `key=` versus bare `key` | both normalize to a key with no value | distinguishes `TagValue::Empty` from `TagValue::Flag` |

## What this says about production adoption

`ircv3_parse` is a careful, allocation-light, `#![forbid(unsafe_code)]`-clean parser with a
stricter grammar than most peers, and it agrees with the owned codec on every well-formed
message in the corpus. It is materially weaker on the properties this project adopted its
own wire layer for:

1. **No length or count ceilings anywhere.** A caller must impose 512/8191/15/128 itself.
   The project's canonical wire contract requires those ceilings to be enforced inside the
   codec so no caller can forget them.
2. **No embedded CR/LF policy.** Accepted input containing CR or LF inside a parameter is a
   message-forgery surface for a relay that re-emits the parsed parameter.
3. **`&str` in, `&str` out.** Invalid UTF-8 cannot be represented at all, which is exactly
   the case the owned byte-oriented policy exists for (Research invariant 7).

Adopting it in production would therefore mean re-imposing every ceiling and the CR/LF policy
around it, which is more code than the owned codec, not less. As a **dev-only differential
oracle** it is genuinely useful: its grammar is stricter than the owned codec in several
places, so disagreement in either direction is worth a look.

## Dependency and safety footprint

`ircv3_parse` 4.0.0 with `default-features = false, features = ["std"]`:

```text
ircv3_parse v4.0.0
├── bytes v1.12.1
├── memchr v2.8.3
└── thiserror v2.0.21 (proc-macro: proc-macro2, quote, syn)
```

No `build.rs`, no `unsafe` in `src/`. Optional features add `ircv3_parse_derive` (proc-macro)
and `serde`. MSRV 1.78, license `MIT OR Apache-2.0`.

## Disposition

**Dev/research-only differential oracle.** Not a production dependency: it would require the
caller to reimplement the project's safety ceilings and it cannot represent invalid UTF-8.
No source, test, or fixture from this crate is copied into this repository.