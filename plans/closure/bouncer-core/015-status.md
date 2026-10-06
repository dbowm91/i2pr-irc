# Bouncer Core M004-A — Anonymity Protocol Mediation

Status: closed

Implements: `plans/implementation/bouncer-core/015-m004a-anonymity-protocol-mediation.md`

Research authority: `plans/research/005-m004-anonymity-and-adverse-network-research.md`

Repository baseline: `3c31f8b` ("Close Corrective 014 and unblock M004-A and M004-B")

Implementation commit: `1410154` ("Mediate anonymity-sensitive protocol inputs at one boundary")

## Objective

Prove an attached local client cannot alter the bouncer's remote metadata
fingerprint or create a direct-network path, while ACTION and safe intentional IRC
behaviour continue to work.

## Specifications reviewed

| Extension | URL | What it settled |
|---|---|---|
| message-tags (CLIENTTAGDENY) | https://ircv3.net/specs/extensions/message-tags.html | `CLIENTTAGDENY` is an **ISUPPORT 005 token**, not a CAP capability, and `*` means every client-only tag is blocked |

`CLIENTTAGDENY=*` is emitted in the `005` projection. The client-only allowlist is
empty, so anything narrower would be a promise the bouncer does not keep.

## CTCP matrix

`crates/runtime/src/ctcp.rs` parses the trailing parameter of PRIVMSG/NOTICE. CTCP is
recognised only when the body **begins** with `0x01`; text merely containing one is
ordinary text, so a sentence cannot smuggle a command through. The final delimiter is
tolerated when absent, because clients commonly omit it.

### Upstream -> downstream

| Inbound | Result |
|---|---|
| `ACTION` | fan out as ordinary chat |
| `PING` | bouncer answers privately; the query is never handed to a client |
| `DCC` (any form) | suppressed |
| `VERSION` `TIME` `USERINFO` `SOURCE` `FINGER` `CLIENTINFO` | suppressed |
| unknown command | suppressed |
| `NOTICE` reply of any kind | suppressed |
| malformed (starts with delimiter, unreadable) | suppressed |

### Downstream -> upstream

| Outbound | Result |
|---|---|
| `ACTION` | forward |
| `PING` query or reply | forward |
| metadata **reply** (`VERSION`, `TIME`, `USERINFO`, `SOURCE`, `FINGER`, `CLIENTINFO`) | blocked |
| `DCC` in any form | blocked |
| unknown CTCP reply | blocked |

The asymmetry is the point: a metadata *reply* travelling upstream is how a local
client's software and hostname become this Operator's fingerprint. Blocking it is the
whole reason the inbound direction is not enough.

## DCC structural suppression

`Ctcp::Dcc` deliberately carries **no parameters**. The parser recognises the command
name only to classify it as blocked, so no host/port pair is ever produced and there is
no value for a dialer or listener to be built from. `DCC CHAT`, `DCC SEND`, `DCC RESUME`
and `DCC ACCEPT` all parse to the same parameterless value.

`scripts/check-network-boundary.py` gained a direct-connect predicate
(`TcpListener`, `TcpStream`, `UnixListener`, `dcc_listen`, `dcc_connect`, `start_dcc`,
`accept_dcc`) with positive controls proving that a listener or a DCC helper anywhere in
the runtime fails the check, and that the one file permitted to name DCC — the CTCP
classifier — is still subject to the source scan.

## Tag matrix

Deny by default. A client tag is untrusted input the server will believe.

| Client tag | Result |
|---|---|
| `+typing`, `+vendor/x`, any client-only tag | removed (allowlist empty) |
| `msgid` (even well-formed) | removed |
| `time` (even canonical) | removed |
| any other unprefixed tag | removed |
| `label` | **retained** — consumed by the response router, translated to an opaque upstream token, restored only to its owner |
| malformed tag value | removed |

The label is the one exception because it is not client metadata: it is this bouncer's
own correlation mechanism. Corrective 014 already guarantees it is translated and never
reaches the server in the client's spelling.

Two pre-existing tests asserted the old permissive policy — a well-formed client
`msgid` forwarded, a well-formed client `time` forwarded. Both were rewritten to assert
deny-by-default, not deleted.

### Capability promotion, and why it is safe now

Corrective 014 deliberately withheld `message-tags`, `batch` and `labeled-response`
downstream pending a mediator. That mediator now exists, so the tag surface is promoted
and per-session tag delivery is implemented: a session that negotiated `message-tags`
receives tags, and one that did not never receives a tagged frame.

`server-time` and `echo-message` remain withheld under reviewable constants. Neither has
implemented downstream semantics: this build synthesises `server-time` for history
replay but does not promise it on the live stream, and has no echo-confirmation path.

## Capability / fingerprint matrix

`the_upstream_registration_is_identical_for_every_client_mix` drives four runs of a
single Network and asserts the upstream byte stream is byte-identical for:

- no clients attached;
- one legacy client;
- one client that negotiated `message-tags`;
- three clients, one of which negotiated `labeled-response`.

CTCP policy replies are fixed bouncer policy, so they cannot vary with client brand or
version: the `PING` answer is the reflected bounded token or the fixed placeholder, and
nothing about who is attached enters it.

## Environment / secret evidence

The workspace forbids `unsafe`, so the environment cannot be poisoned in-process.
`no_host_environment_value_reaches_the_wire_or_a_diagnostic` therefore asserts against
the **real** host values of `USER`, `LOGNAME`, `HOSTNAME`, `HOME`, `TMPDIR` and `PWD`.
That is the stronger check: these are the exact strings a leak would expose, they are
unique to this machine, and a match is attributable rather than merely suspicious.

Each is asserted absent from the registration, the upstream byte stream, the downstream
replies, and the structured snapshot — after exercising a CTCP probe and a routed query
so the diagnostic path is non-empty. `a_sasl_secret_never_reaches_a_diagnostic` covers
the credential case separately.

## Testing

`crates/runtime/tests/privacy.rs` adds 15 tests against fake I2P providers and scripted
streams. `scripts/verify.sh full` exits 0; 286 tests pass across 20 test binaries, up
from 271 before this plan.

| Area | Evidence |
|---|---|
| upstream probes suppressed | 7 commands, each asserted counted and absent from the client |
| upstream DCC suppressed | no `DCC`, no port, no delimiter reaches the client |
| upstream PING answered by the bouncer | reply addressed to the probe's sender; client sees no CTCP |
| ACTION both directions | inbound fanned out, outbound forwarded |
| client metadata replies blocked | 7 replies, upstream drained and asserted clean |
| client DCC blocked | 4 shapes, upstream drained and asserted clean |
| tags denied | forged msgid, client time, two client-only tags all absent upstream |
| tag pressure bounded | 64 tagged frames stay inside the queue; client not detached |
| fingerprint stability | four client mixes, byte-identical upstream |
| environment | six real host values absent from four surfaces |
| static boundary | the guard itself is run from the test, including its positive controls |

## Defects found and fixed while implementing

1. **`inbound_action` classified ordinary text as suppressible.** The first draft
   matched only `Action`, so every plain PRIVMSG fell through to `Suppress`. Six
   existing integration tests failed immediately. Ordinary text now fans out, and a test
   pins it.
2. **`outbound_action` had the same defect**, which blocked *all* client frames including
   `JOIN`/`PART` — `client_frames_blocked` reached 600 in a test that sends ordinary
   traffic. Fixed identically.
3. **Two network implementations existed.** See UF-015-1.
4. **The conformance corpus had gone stale.** `CV-005` used `message-tags` as its
   example of an unimplemented capability and `CV-006` asserted an empty advertisement.
   Both now use `echo-message` (genuinely withheld) and assert the served set, preserving
   each vector's intent rather than deleting it.

## Unresolved findings

### UF-015-1 — a second, legacy network implementation exists (medium, non-blocking)

`crates/runtime/src/lib.rs` contains `NetworkSupervisor`, a complete single-network
implementation with its own upstream loop, CAP mediator and fanout. It is referenced by
nothing outside its own `#[cfg(test)]` module; the production path is
`catalog::NetworkSupervisor` over `owner::NetworkOwner`.

Two consequences were addressed here:

- Both now apply the **same** CTCP privacy policy and the same per-session tag rule, so
  two implementations cannot disagree about what a client may observe. M004-A does not
  leave the legacy path as a way to bypass the privacy boundary.
- Every M004-A live test targets the production path explicitly. Nothing in this closure
  record certifies the legacy supervisor.

Removing the duplicate is follow-on work. It is flagged rather than done because it is
an architectural decision, not an M004-A privacy fix, and M004-C's adversarial
qualification is where the cost of two implementations would be measured.

### No other open findings

No finding is partially closed and no invariant was weakened to achieve closure.

## Stop conditions

None triggered. The policy required no downstream client-brand identification, no host
environment inspection, and no new socket or network API. In particular the environment
test *reads* host values to assert their absence; it never uses one to construct a reply.

## M004-C readiness contribution

M004-C inherits:

- a parsed, classified, and directionally mediated CTCP boundary in front of every
  client and every upstream write;
- a deny-by-default tag mediator with an empty client-only allowlist;
- a static guard with direct-connect controls and positive controls;
- an explicit fingerprint-stability matrix that M004-C can re-run under adverse
  conditions;
- the known-duplicate-implementation finding, which M004-C should re-check because a
  second code path is exactly the kind of thing an adversarial campaign can diverge on.

M004-C may proceed when M004-B closes.