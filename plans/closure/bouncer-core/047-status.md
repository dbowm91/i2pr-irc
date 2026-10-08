# Bouncer Core M009-A / Plan 047 Closure — OTRv3 Opaque Carriage

Status: closed

Implementation commit: `4bd1b88ad0241a2a93e2414a34d5dfa120c26fa3` — `test(runtime): qualify opaque OTRv3 carriage invariants`

## Corpus and byte preservation

The synthetic, bounded corpus represents an OTRv3 query (`?OTRv3?`), encoded protocol data (`?OTR:`), distinct `?OTR,` fragments, base64-like punctuation, an OTR whitespace capability suffix, and an IRC line at the 512-byte ceiling. No private keys or OTR cryptographic fixtures are used; the bouncer does not validate OTR syntax.

| Path | Evidence | Result |
|---|---|---|
| Generic wire parse/encode | Query, encoded data, fragments, whitespace suffix, TAB/trailing spaces round-trip byte-exactly | Pass |
| Maximum-size framing | Near-limit incoming opaque protocol line is exactly 512 bytes and re-encodes unchanged | Pass |
| Upstream to downstream | Query body reaches modern, message-tag-only, and legacy sessions byte-exactly | Pass |
| Fragment sequence | Two separate frames remain distinct and ordered | Pass |
| Downstream to upstream | PRIVMSG and NOTICE bodies preserve punctuation, TAB, and trailing spaces | Pass |
| Maximum-size downstream frame | 512-byte client line reaches upstream byte-exactly | Pass |
| IRCv3 tags | Negotiated server-time/account-tag/custom tags are mediated per session without changing OTR body | Pass |

The OTR strings are ordinary opaque trailing parameters. No special-case parser, whitespace trimming, UTF-8 conversion, protocol unescaping, concatenation, or fragment generation was added.

## Multi-client, history, and replay

| Invariant | Evidence | Result |
|---|---|---|
| Ordinary multi-client fanout | Three distinct ClientIds (modern, message-tag-only, and legacy) receive the same body; tag differences follow existing negotiated policy | Pass |
| History stores opaque event | Store query returns all three observed messages; first message's trailing bytes and both fragment bodies remain exact | Pass |
| Outgoing replay disposition | Runtime classifies OTR-bearing user chat as `NonReplayable`; `survives_disconnect()` is false | Pass |
| Ambiguous reconnect | OTR frame is confirmed written on the first upstream stream, the stream then disconnects, and the replacement stream contains no OTR replay | Pass |
| Capability surface | Downstream capability list contains no OTR capability; existing fingerprint tests prove downstream negotiation does not change upstream negotiation | Pass |

## Security and dependency boundary

The production implementation adds no OTR endpoint, identity key, fingerprint, SMP, ratchet, DH, session state, or plaintext handling. Static search across crate manifests and production Rust sources found no libotr/libotr-ng dependency, OTR FFI, or OTR module. The bouncer continues to apply its existing CTCP/DCC, client-tag, and diagnostic policies; `?OTR` payloads are relayed as normal chat and message bodies are not added to logs.

## Verification

Passed:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `scripts/check-network-boundary.py`
- `scripts/verify.sh full`
- `rustup run 1.88.0 sh scripts/verify.sh full`
- focused OTR carriage, outbound reconnect, non-replay classification, wire round-trip, and near-limit tests

Both full verification runs include formatting, clippy, all workspace tests, static network-boundary checks, and fuzz smoke. No external network/router was required.

## Handoff

No unresolved Plan 047 finding remains. Product behavior is accurately described as OTRv3-transparent transport for OTR-capable endpoints; the bouncer does not provide end-to-end encryption to ordinary clients. Plan 048's integrated privacy qualification and M009 closure are unblocked and marked ready. External real-client/libotr interoperability remains outside this milestone because no production downstream listener exists yet.
