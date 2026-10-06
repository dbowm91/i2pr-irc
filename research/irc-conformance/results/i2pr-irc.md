# External differential results — i2pr-irc owned wire codec

Research date: 2026-10-06
Toolchain: `rustc 1.89.0 (29483883e 2025-08-04)`
Corpus: `research/irc-conformance/vectors/` (32 message vectors, 14 framing vectors)
Runner: `cargo test -p i2pr-irc-wire --test conformance` (committed) and `cargo test -p i2pr-irc-runtime --test conformance` (committed)

Every vector in `wire.txt` and `framing.txt` passes against the owned codec, and every
vector in `state.txt` and `cap.txt` passes against the owned state model and session.
Three defects were found by this corpus during the research and corrected under
`plans/implementation/bouncer-core/006-framing-recovery-corrective.md`; see
`plans/research/003-rust-irc-crate-conformance-results.md`.

## Behavioral summary

| property | owned behavior | evidence |
|---|---|---|
| 512-byte body ceiling including CRLF | accepted at exactly 512, rejected at 513 | `WV-010`, `WV-025` |
| IRCv3 8191-byte tag-prefix ceiling | accepted at 8191, rejected beyond | `WV-012`, probe "tag prefix beyond 8191 bytes" |
| direction-specific 4094-byte tag-data budget | enforced separately per direction | `tag_data_budget_max_and_max_plus_one` |
| 15-parameter ceiling | accepted at 15, rejected at 16 | `WV-009`, `WV-026` |
| 512-byte token/prefix ceiling | rejected beyond | `WV-027`, probe "513-byte token" |
| 128-tag-key ceiling | enforced | `tag_count_max_and_max_plus_one` |
| NUL anywhere in a line | rejected | `WV-022`, `WF-005`, `WF-010` |
| embedded CR | rejected | `WV-023` |
| embedded LF | rejected | `WV-024` |
| command token grammar | alphabetic or exactly three digits; `9999`, `99`, `9PRIV` rejected | `a_command_must_be_alphabetic_or_a_three_digit_numeric` |
| unknown command / unknown numeric | preserved verbatim and re-encodable | `WV-006`, `WV-007`, `unknown_numerics_are_representable_and_relayable` |
| opaque tag keys, case sensitive | preserved | `WV-012`, `WV-013`, `WV-014` |
| duplicate tag keys | final value retained | `WV-016` |
| empty tag value | normalized to a key with no value | `WV-015` |
| tag escapes (`\;` `\s` `\\` `\r` `\n`) | unescaped on parse | `WV-017` |
| invalid tag escape | backslash discarded | `WV-018` |
| final lone backslash in a tag value | emits no byte | `WV-019` |
| invalid UTF-8 in an ordinary parameter | kept as bytes, never transcoded | `WV-020` |
| invalid UTF-8 in a tag value | key retained, value dropped | `WV-021` |
| incremental framing | arbitrary chunk splits decode once, unterminated tail is withheld | `WF-001`, `WF-003`, `WF-008`, `WF-009` |
| framing recovery after an over-long line | one error, recovery at that line's own LF | `WF-004`, `an_over_long_line_is_discarded_through_its_own_terminator_only` |
| per-push decode overload | 256 decoded then one explicit `TooManyMessages` | `WF-014`, `line_decoder_bounds_outputs_per_push` |
| hostile-input state | deterministic arbitrary bytes never panic or exceed the bound | `deterministic_arbitrary_bytes_never_panic_or_exceed_decoder_bound` |

## State and registration summary

| property | owned behavior | evidence |
|---|---|---|
| `CASEMAPPING=ascii` / `rfc1459` / `rfc1459-strict` / `strict-rfc1459` | each honored; unknown values keep the documented `rfc1459` default | `SV-001`, `SV-002`, `SV-003`, `SV-003b`, `both_strict_casemapping_spellings_are_honored` |
| `CHANTYPES` | replaces live channel classification | `SV-004`, `SV-005` |
| `PREFIX` | replaces the membership symbol set; unadvertised symbols stay part of the nick | `SV-006`, `SV-007`, `SV-016` |
| `CHANMODES` | classes decide argument consumption; malformed value leaves the previous classes authoritative | `SV-008`, `SV-009` |
| undeclared mode letter | marks the snapshot explicitly incomplete instead of guessing arity | `SV-010` |
| authoritative `324` | restores completeness | `SV-011` |
| `353` visibility `=`, `*`, `@` | all accepted; never consumed as a member symbol | `SV-012`, `SV-013`, `SV-014` |
| unknown visibility field | ignored without corrupting membership | `SV-015` |
| written JOIN | records a bounded attempt, creates no membership | `SV-017`, `a_written_desired_join_is_not_observed_membership` |
| standard channel-failure numerics | classified without membership, intent preserved | `SV-018`, `SV-019`, `SV-020` |
| self PART / KICK | authoritative removal of membership | `SV-021`, `SV-022` |
| topic beyond the local ceiling | omitted, never truncated | `SV-027` |
| downstream CAP negotiation | registration held until `CAP END` | `SV`-independent: `CV-002`–`CV-011` |
| client CAP traffic | never forwarded upstream | asserted by the CAP runner for every vector |
| advertised downstream capabilities | empty | `CV-006` |

## Deliberate stricter policies, not defects

| policy | why |
|---|---|
| fixed local ceilings (512/8191/4094/15/128/256) | required so a hostile peer cannot grow memory; upstream servers may have their own limits, and those are the server's business |
| byte-oriented ordinary fields | RFC 2812 section 2.3 does not require UTF-8; transcode-on-parse causes mojibake |
| invalid UTF-8 tag value drops the value but keeps the key | the IRCv3 message-tags specification explicitly permits dropping unusable tag data |
| unknown `CASEMAPPING` value keeps `rfc1459` | matches the specification's documented default; claiming an unknown strictness would be an invented assumption |
| a projection larger than a client's bounded queue ends that client | bounded and explicit beats unbounded buffering |