# The owned SAM 3.1 adapter

`crates/sam` is the only code in this repository that may open a TCP socket, and the only
code that may open one to a router. It implements the conservative SAM 3.1 STREAM profile
that Plan 030 froze: `HELLO`, `SESSION CREATE`, and `STREAM CONNECT`.

## Authority

The boundary is a type, not a check.

`SamBridgeEndpoint` can only be constructed by its own `parse`, which accepts a numeric
loopback literal and nothing else. `"localhost"` is refused — not as a special case, but
because it is a *name*, and accepting names would mean the resolution decides where to
connect. There is no resolver call anywhere in the crate, and the static boundary scan fails
on `ToSocketAddrs` in every file including the client itself.

Every connect path takes a `SamBridgeEndpoint`, so a future edit cannot reach a non-loopback
router without changing a type that a reviewer will see.

| | |
|---|---|
| Default | `127.0.0.1:7656` |
| Accepted | any loopback literal, IPv4 `127.0.0.0/8` or IPv6 `::1`, port 1–65535 |
| Refused | every non-loopback address, every host name, every URL or path, port 0 |

`Debug` reports only the address family and port, so a diagnostic never repeats the literal.

## The static boundary

`scripts/check-network-boundary.py` scans `crates/sam` like every other crate, with one
difference: it holds `crates/sam/src/client.rs` and `crates/sam/src/fake.rs` exempt from the
blanket socket and DCC predicates, and applies a narrower predicate in exchange.

The narrower predicate permits exactly those two files to name a TCP socket and permits no
other file to name one. Even inside those two, `UdpSocket`, `UnixStream`, `UnixListener`,
`TcpSocket`, and `ToSocketAddrs` are refused. Eight positive controls prove the exemption is
load-bearing, so narrowing it fails the guard rather than quietly relaxing the boundary.

The test-only fake lives inside the crate rather than in `crates/testkit` on purpose: that
way the boundary scan sees it and holds it to the same rule. It binds `127.0.0.1:0` and is
compiled out of every production build behind the `testkit` feature.

## Protocol profile

Three requests, each built from a literal. There is no `send_command`, no option
passthrough, and no parameter through which a caller could add an option to a line. A caller
that needs a fourth request has to add a fourth function, in review.

```
HELLO VERSION MIN=3.1 MAX=3.1
SESSION CREATE STYLE=STREAM ID=<opaque> DESTINATION=TRANSIENT SIGNATURE_TYPE=7
  i2cp.leaseSetEncType=4 i2cp.dontPublishLeaseSet=true
  inbound.quantity=2 outbound.quantity=2
STREAM CONNECT ID=<opaque> DESTINATION=<endpoint> SILENT=false
```

`SIGNATURE_TYPE=7` is the recommended type. The explicit tunnel quantities keep behaviour
independent of whatever the router's defaults are, which is what otherwise differs between
Java I2P and i2pd.

Not implemented, deliberately: `ACCEPT`, `FORWARD`, `DATAGRAM`, `RAW`, primary or
subsessions, SAM authentication, a remote bridge, Destination import/export, and every router
administration API.

## Framing

| Ceiling | Value |
|---|---|
| Control line, terminator included | 4096 |
| Tokens per line | 64 |
| Option key | 64 |
| Option value | 3072 |

**An over-long line is discarded and the reader stays usable.** Bytes are consumed and
counted until a newline arrives; the next line is parsed normally. A reader that grew a
buffer until it found a newline would let anything able to reach the bridge make this
process allocate without bound. Overflow is a state, not an error.

Both `\n` and `\r\n` are accepted. A bare CR is refused rather than treated as a line break:
accepting it would let a peer inject a line boundary.

A `KEY="value"` whose value contains spaces is one token, because `MESSAGE` legitimately
does. An unterminated quote is refused rather than swallowed.

## Typed replies

Replies are typed enums, not a string map, because the two routers genuinely disagree:
`HELLO OK` on one and `HELLO OK VERSION MIN=3.1 MAX=3.1` on the other; a stream failure class
in `MESSAGE` on one and `REASON` on the other. A map would push those rules onto every caller.

Sequencing is strict and lives in the type:

```
new socket -> hello pending -> hello 3.1 -> session pending | stream pending
           -> session active | raw stream
```

`STREAM STATUS RESULT=OK` is the only transition to a raw socket, and
`SamState::is_raw` is the single place that answers "may I stop parsing SAM?" — so the answer
cannot disagree with the parser. Anything else is a typed protocol failure.

`SamError` carries no router text, no session ID, and no Destination. It is eight bytes, which
is the structural proxy for "this enum holds no foreign bytes"; a free-form `MESSAGE` must not
reach an operator's terminal.

## Session identity

32 lowercase hex characters, 128 bits, from the OS CSPRNG. Deliberately not the `NetworkId`:
that is a small monotonic integer, and it would make every configured Network present a
session ID an observer could correlate across time, restarts, and bouncers.

Randomness is a trait with one OS implementation. A failing source is a terminal error, never
a fallback to a counter, a clock, or a hash of process state — an ID that looks random and is
predictable is worse than no session at all. The only OS-random dependency in the workspace is
`getrandom`, MIT OR Apache-2.0, `rust-version` 1.63, below this workspace's 1.88 floor.

## The raw transition

After `RESULT=OK` the socket *is* the stream, and nothing on it is parsed. Bytes the reader had
already consumed past that line are handed to the caller rather than dropped, because a router
may write application bytes in the same TCP segment as the acknowledgement — and losing the
first bytes of an IRC conversation would be a silent, intermittent failure.

The conformance test proves this by sending a payload containing a NUL byte, invalid UTF-8, and
a string that is exactly a `STREAM STATUS RESULT=OK` line, and asserting all of it crosses
unchanged.

## Deadlines

| Phase | Deadline | Why |
|---|---|---|
| bridge connect | 10 s | loopback; a local accept or nothing |
| `HELLO` | 10 s | a router that cannot answer a hello is not cooperating |
| `SESSION CREATE` | 120 s | tunnel build on a cold router is genuinely slow |
| `STREAM CONNECT` | 90 s | above the SAM documentation's ~1-minute router window |

The `STREAM CONNECT` deadline is deliberately *above* the router's own attempt window. A client
deadline shorter than the router's manufactures failures: the router is still working and the
client has already given up. Being longer means the router's own timeout answers first, which
carries a classified reason.

A timeout names its phase. "The router is slow to build tunnels" and "the router did not
answer at all" are different operational problems and get different labels.

**Known gap.** The SAM-side cold path is 10 + 10 + 120 = 140 s against the runtime's
`CONNECT_TIMEOUT` of 120 s. A cold first connect would be cut off while the router was still
building tunnels. Plan 030 section 10 anticipated this and assigned the reconciliation to Plan
031; the mismatch is pinned as a test so it cannot be resolved silently in either direction.

## Cancellation

Every exchange is a plain `async fn` with the socket held across its `await`. There is no
`tokio::spawn`, no cancellation channel, and no join: dropping a pending future drops its
socket, which is the required property, reached without three extra places to be wrong.

## What this crate does not know

A `NetworkId`. Session identity, framing, and deadlines are all router-neutral; composing one
session per configured Network is Plan 031's work, behind `I2pStreamProvider`.

Nothing private is retained. A successful transient `SESSION CREATE` makes the router return
the private Destination it generated. `SessionReply` has no field for it, so it cannot be
stored even by accident; the value is parsed into a zeroizing buffer and dropped.

Related: [network capability ownership](network-boundary.md#provider-scope-and-release),
[network supervisor](network-supervisor.md), [network boundary and egress](network-boundary.md).
