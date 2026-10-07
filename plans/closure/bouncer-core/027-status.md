# Plan 027 — M005-H Operator Diagnostics, Configuration Snapshots, and Constrained Registration Actions — Closure

Closed 2026-10-07. Outcome: **closed, with one pre-existing finding recorded and one
boundary stated rather than papered over.**

## Outcome

An Operator can now read the bounded state of a bouncer while something is wrong, move its
non-secret configuration between machines through a versioned local format, and configure a
small audited set of post-registration commands — without gaining arbitrary raw-command
execution and without any of the three surfaces being able to express a secret.

| Work package | Where | Evidence |
|---|---|---|
| A diagnostic DTO/projection | `crates/runtime/src/diagnostics.rs` | 11 unit tests |
| B exact retry observability | `NetworkSnapshot::next_retry_delay`, published at the backoff site | `diagnostics_report_the_running_state_of_a_live_network` |
| C config snapshot schema/export | `config_snapshot.rs`, `ControlRequest::ExportConfig`, `CONFIG EXPORT` | 17 unit + 3 integration tests |
| D bounded import validation/application | `config_snapshot::plan`, `ControlRequest::ImportConfig` | `an_import_that_names_a_different_network_under_one_identity_stops_without_writing_it` |
| E secret/redaction classification | no credential field exists in either type | `diagnostics_carry_no_endpoint_and_no_credential`, `a_configuration_export_carries_no_credential`, `an_import_does_not_erase_a_stored_credential` |
| F registration-action model/store migration | `action.rs`, schema 7 `registration_actions` | 11 unit + 5 store qualification tests |
| G post-registration replay runner | `NetworkOwner::registration_action_frames` | `a_stored_action_is_replayed_after_a_successful_registration`, `a_reconnect_replays_the_action_sequence_intentionally` |
| H operator service/control integration | `BouncerServ DIAG`, `CONFIG`, `ACTION` | 21 integration tests |
| I docs/closure | `architecture/operator-surfaces.md` | this record |

## Diagnostics

### Redaction matrix

| Surface | Endpoint | Destination | SASL value | SASL name | Action payload | Path |
|---|---|---|---|---|---|---|
| `DIAG` process half | absent | absent | absent | absent | absent | absent |
| `DIAG NETWORK` | absent | absent | absent | absent | absent | absent |
| `CONFIG EXPORT` | **present** | **present** | absent | absent | absent | absent |
| `ACTION STATUS` | absent | absent | absent | absent | absent | absent |

The endpoint row is the interesting one and is a decision, not an omission.
`BOUNCER NET` withholds an I2P destination because it renders into an IRC-visible frame that
every session which negotiated the capability can read. `CONFIG EXPORT` includes it because an
export without the identity of the Network being moved is not an export. The difference is who
holds the output, and the test asserts the whole reply either way.

The absence of a SASL *name* from diagnostics is also deliberate. It is not secret, and
`SASL STATUS` reports it on purpose; it is simply not a diagnostic field, so a reader scanning
a report does not have to decide which surface is safe to paste.

The guarantee is structural: neither report type has a field a secret could be read out of.
That is stronger than a redaction pass, and it is what
`diagnostics_carry_no_endpoint_and_no_credential` asserts by giving the bouncer a real
credential and a real endpoint, authenticating upstream with SASL PLAIN, and then searching
the entire reply.

### Bounding

| Bound | Value | Why that value |
|---|---|---|
| Lines per reply | `CONTROL_QUEUE_CAPACITY` (8) | a longer reply loses its tail to `try_send` with nothing written to say so |
| Lines per Network | 3 (`state`, `counts`, `lists`) | the whole of one Network does not fit one line |
| Bytes per line | 380 | below the wire ceiling with room for the `NOTICE` prefix |
| Channels per sample | 64, plus an overflow count | a list that merely stops is indistinguishable from a short one |
| Capabilities per sample | 64 | one length standing for two different lists would couple their bounds |
| Rejections per sample | 16 | a rejection is rare and each carries a fixed reason |

Counts precede lists within a Network. A Network with a hundred joined channels pushes the
counters off a shared line, and the counters are what an Operator opened diagnostics to find;
`a_line_that_had_to_be_cut_says_so` is what stops that ordering from being undone later.

### Away classification

`present` / `automatic` / `manual` / `unattributed`. The class comes from the owner's
`away_decision_with_origin`, the same call that decided the away, so the class cannot drift
from the decision. An away whose origin the generation cannot account for is `unattributed`,
not `automatic`: a bouncer that is away for a reason it cannot explain is a finding, and
labelling it `automatic` would hide exactly that.

### Failures

A `DIAG NETWORK` for an identity with no live owner is refused with `UnknownNetwork`, mapped to
`no network with id N` — not answered with an empty successful report, because an Operator
reading "no problems" out of a typo is the failure this must not have.

## Configuration snapshots

### Round-trip and failure matrix

| Case | Result |
|---|---|
| export → parse → render | fixed point; the byte sequence is identical |
| two exports of the same configuration | byte-identical (Networks sorted by identity, channels by position) |
| an unknown version | refused, never guessed |
| a later `version` line | ignored: the version is read by position, not by search |
| text that is not a snapshot | refused before it is read |
| a missing attribute | named — `MissingAttribute("nick")`, not "invalid input" |
| an invalid value | named — `netid`, `host`, `nick`, or `boolean` |
| a channel before any Network | `OrphanChannel` |
| two records claiming one identity | `DuplicateNetwork` |
| more lines than the runtime supervises | `TooManyLines` |
| an `netid` naming a different Network | `Conflict`, and the plan stops **before any write** |
| a credential in any spelling | `RefusedSecret`, by name |

### The stated boundary

**Import is plan-then-apply, per Network, and is not transactional across Networks.** The
durable store is one bounded worker behind a request queue. A multi-Network transaction would
have to stay open across the owner restarts that each write causes, and that is a second
writer on the one durable surface. The plan explicitly permits this boundary provided it is
documented rather than disguised.

What is guaranteed instead is the property that actually matters: **nothing is written until
the entire snapshot has parsed and validated.** That is a property of the type — the only way
to hold a `ConfigSnapshot` is to have parsed one, and `ControlRequest::ImportConfig` takes the
parsed value. `ApplyOutcome` reports how far the apply got and which Network stopped it, so a
partial apply is visible rather than implied.

### The credential asymmetry

An import never writes a credential, because the format has no field to carry one. It also
never erases one: `plan` merges the stored credential into an update. An export is therefore
safe to paste anywhere and safe to apply anywhere, in both directions. Both halves are tested
(`a_configuration_export_carries_no_credential`,
`an_import_does_not_erase_a_stored_credential`).

## Registration actions

### Security matrix

| Attempted | Result |
|---|---|
| `NICK`, `JOIN`, `PART`, `QUIT`, `CAP`, `AUTHENTICATE`, `BOUNCER` | refused — no variant can express them |
| `OPER`, `KILL`, `SQUIT`, `CONNECT`, `DIE`, `RESTART`, `WALLOPS`, `DCC` | refused — same |
| a raw or prefixed line (`raw=`, `line=`, `:bot!u@h JOIN`) | refused — no attribute for one exists |
| a client tag on a stored action | refused — no attribute for one exists |
| `MODE +B #channel` | refused — a mode string is `sign letter` pairs and nothing else |
| `message=bot text=…` (a person, not a service) | refused — `a message action must target a service` |
| more than 8 actions | refused; nothing is stored |
| more than 512 bytes across a set | refused; nothing is stored |
| `ACTION STATUS` | a count only — no target, no payload |
| `Debug` of a `ServCommand` or a stored action | `[redacted]`, at both the runtime and the storage layer |
| a refusal | names the attribute; never repeats the value |

Enforcement is the **absence of variants**, not the presence of refusals:
`RegistrationAction` is constructed only by `mode` and `message`, and neither accepts a
command name. The forbidden list is a test oracle and a record of what was considered, and a
test asserts the two sets never overlap.

### Replay semantics

Actions are replayed after **every** successful registration generation, after the JOINs.
This is intentional per-generation setup, not an ambiguous-user-message retry: nobody typed it
at a moment whose delivery is in doubt, an identify line sent twice on reconnect is the
intended behaviour rather than a duplicate, and every action is idempotent by construction,
which is what makes replaying it correct. A generation that dies part way through starts the
sequence over on the next one — never resumed, never truncated to the prefix it got through.

`text=` takes the remainder of the line, not the next word. `IDENTIFY hunter2` is one message
containing a space; a parser that took a single word would store `IDENTIFY`, failing at the
service while looking exactly like a working configuration.

## Defects found and fixed

Nine, six in new code and three in code this plan made load-bearing.

1. **Every diagnostics frame was built without a CRLF.** `queue_line` refuses any line without
   one and `ControlSurface::write` discards the refusal, so the reply vanished without a trace.
2. **The process lines carried their section as the tag name**, so a client filtering on
   `bouncer-diag` would have missed them.
3. **A long channel sample pushed the counters off the end of a shared line**, hiding exactly
   the history numbers an Operator opens diagnostics to find.
4. **`bounded()` could cut its own truncation marker off the end of its output**, and could
   split a multi-byte character.
5. **A 13-line report silently lost its tail** to the 8-frame control queue. The first version
   of this rendering was larger than the queue it was written to, and `write` swallowed the
   refusal.
6. **An import erased a stored credential.** `to_record` produced `sasl: None` because the
   format cannot represent one, and the plan wrote that field straight through — turning a
   successful restore into silent data loss. Caught by
   `an_import_does_not_erase_a_stored_credential`.
7. **The export was unreassemblable.** It reused the help text's arbitrary 96-byte chunking,
   which splits a line across NOTICEs, so a client could not tell where one record ended.
8. **A pasted `sasl user=bob` was reported as a malformed attribute** rather than as the
   refused secret it was, because the check ran after the `=` split and the field had no `=`.
9. **`ACTION SET` could not express an identify line at all.** `text=` was visited by the
   attribute loop and refused as an unknown field, and the text could only ever be one
   whitespace-free word.

## Recorded findings

### Pre-existing: a generation teardown takes about two minutes to be noticed

`a_reconnect_replays_the_action_sequence_intentionally` measures **120.9 s** between the
upstream stream ending and the owner asking for a new generation, against a `CONNECT_TIMEOUT`
of 120 s. A probe with **no registration actions at all** measures the same 120.9 s, so this
is entirely independent of Plan 027: the owner does not begin a new generation until roughly a
`CONNECT_TIMEOUT` after the upstream ends, rather than reacting to the end-of-stream.

A bouncer that takes two minutes to notice its upstream died is not mature, and this is the
largest single operational gap found across Plans 020-027. It is **not** fixed here: the
generation loop is not this plan's surface, and changing its teardown detection needs its own
investigation and plan rather than a drive-by edit inside a feature closure.

The test is kept despite its two-minute cost, and the cost is documented on it, because a
replay on reconnect is a claim only a real reconnect can establish. Asserting it against a
simulated generation would be asserting the fixture.

### Stated boundary: import is not transactional across Networks

Recorded above under "The stated boundary". It is a deliberate limit of this plan, permitted
by its own fallback clause, not an oversight.

### Recorded limit: the whole-process report carries at most two Networks

A consequence of the control-queue bound rather than a design preference. `DIAG NETWORK` always
delivers a Network's complete report; `DIAG` reports the process plus as many Networks fit and
states `networks_omitted=` on the process line. An Operator wanting all of them asks per
Network.

## Verification

- `cargo fmt --all -- --check` — pass
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — pass
- `./scripts/check-network-boundary.py` — pass
- `cargo test -p i2pr-irc-store` — pass (14 unit + 65 qualification, 5 new)
- `cargo test -p i2pr-irc-runtime --lib` — pass (11 action + 11 diagnostic + 17 snapshot
  + 8 command-matrix unit tests, all new)
- `cargo test -p i2pr-irc-runtime --test m005h_diagnostics` — pass (21 tests)
- `cargo test --workspace --all-features` — pass

## M005-I readiness

Plan 028 (M005-I, integrated mature-bouncer qualification and M005 closure) is **unblocked and
dependency-ready**. Everything it integrates is landed and tested:

- the bounded `RuntimeController` and pre-bind `DownstreamAdmission` (Plan 020), so qualification
  can drive one process through both attach paths;
- durable detach, presence and preferred-nick policy, and the `soju.im/bouncer-networks` plus
  `BouncerServ` administration surface (Plans 021-023);
- indexed search and `CHATHISTORY` completion, and the downstream IRCv3 polish and member-state
  mediation (Plans 024-026);
- the bounded diagnostics projection, the versioned configuration snapshot, and the allowlisted
  registration actions with schema 7 (this plan).

Plan 028 inherits two items it should resolve rather than merely re-measure:

1. the ~120 s generation-teardown delay recorded above, which is a real maturity gap and which
   the known pre-existing `adverse.rs` startup-herd teardown flake sits next to;
2. the cross-cutting qualification sweep — the privacy/redaction campaign, the restart
   campaign, and the anonymity review this plan performed only per-surface.