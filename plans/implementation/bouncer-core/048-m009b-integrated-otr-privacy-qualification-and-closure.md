# Bouncer Core M009-B / Plan 048 — Integrated OTR Privacy Qualification and M009 Closure

Status: closed

Closure record: plans/closure/bouncer-core/048-status.md

Hard dependency:

- plans/closure/bouncer-core/047-status.md

Source milestone:

- M009 — OTRv3 Transparent-Carriage Compatibility

Primary class: privacy qualification + milestone closure

## 1. Objective

Qualify the complete bouncer-side privacy claim for OTR:

- OTR cryptography remains endpoint-owned;
- the bouncer transports opaque OTRv3 IRC bodies without semantic interference;
- simultaneous clients do not cause shared cryptographic state;
- ambiguous disconnects do not replay OTR chat;
- durable history contains only ciphertext supplied to the bouncer;
- encrypted-store mode protects that retained ciphertext/index at rest.

M009 does not claim cryptographic implementation correctness because i2pr-irc implements no OTR cryptography.

## 2. Deterministic endpoint model

Build test endpoints that distinguish:

- endpoint-only plaintext sentinel;
- opaque OTR-like ciphertext/transcript supplied to the bouncer.

The plaintext sentinel must never be sent into an i2pr-irc API or IRC frame.

Only the opaque transcript enters the bouncer.

This verifies the architectural data boundary; it is not a test of the OTR cipher.

## 3. End-to-end bouncer transcript

Exercise:

1. local session sends OTR query/AKE/data/fragment-shaped PRIVMSG frames;
2. upstream sees exact bodies;
3. remote/upstream sends corresponding opaque frames;
4. all eligible local sessions see exact bodies;
5. history stores exact opaque frames;
6. client detach/reattach/history replay returns ciphertext only.

Do this under both:

- plaintext-store mode, to demonstrate OTR privacy is independent of database encryption;
- M008 encrypted-store mode, to demonstrate defense in depth for retained ciphertext/metadata.

## 4. Plaintext absence

Use a distinctive endpoint-only plaintext value.

Assert it is absent from:

- history events;
- FTS query results/raw corpus;
- config/diagnostics;
- closed encrypted database sentinel scans.

This assertion is meaningful because the fixture never gives the value to the bouncer; closure must say so explicitly and not present it as cryptographic validation.

## 5. Multi-client/instance behavior

At least three attached sessions:

- OTR-aware fixture A;
- OTR-aware fixture B representing a second endpoint instance;
- ordinary legacy client.

Send opaque messages carrying distinct synthetic instance-tag-like content.

Assert:

- bouncer does not parse or route by instance tag;
- ordinary IRC fanout applies;
- no cross-session OTR state exists;
- one slow/disconnected client follows normal session isolation without altering the opaque payload for healthy clients.

## 6. Disconnect/reconnect

Inject ambiguous upstream disconnect after an outbound OTR-bearing PRIVMSG.

Assert:

- frame is not replayed on replacement generation;
- desired IRC state may reconcile as usual;
- endpoint may initiate a new OTR exchange later;
- bouncer does not synthesize OTR retry/refresh traffic.

## 7. History/search UX contract

Document:

- retained OTR transcript is ciphertext;
- CHATHISTORY/history playback may return opaque old OTR frames;
- old ciphertext may not be decryptable by a current endpoint;
- FTS cannot provide plaintext-content search for an E2EE conversation because the bouncer never possessed plaintext.

Do not mislabel ciphertext search as encrypted semantic search.

A future no-history/per-buffer-retention privacy option is separate work.

## 8. External interoperability disposition

Real libotr/irssi/weechat interoperability is useful but cannot be honestly qualified through a production downstream socket until this repository has a standalone/local-listener product path.

Therefore M009 closure does not require embedding libotr or manufacturing FFI solely for a test.

Record external real-client OTR interoperability as a qualification item for the future standalone-listener/client milestone.

M009's claim remains narrowly "transparent carriage," which is completely testable at the accepted-stream/core boundary.

## 9. Documentation language

Allowed claims:

- "OTRv3-transparent";
- "compatible with client-side OTR transport";
- "bouncer stores only OTR ciphertext it receives";
- "OTR remains end-to-end between IRC clients."

Forbidden claims:

- "i2pr-irc implements OTR encryption";
- "bouncer encrypts IRC messages with OTR";
- "encrypted history is decryptable/searchable by the bouncer";
- "OTRv4 supported";
- "group-chat E2EE supported."

## 10. Security review

Confirm:

- no private keys/fingerprints in schema;
- no crypto dependency;
- no unsafe FFI;
- no body logging;
- no capability fingerprint change;
- no replay exception for OTR;
- M008 store key remains unrelated to OTR endpoint keys.

## 11. Verification

Run full current and Rust 1.88 repository verification plus M008 encrypted-store and OTR corpus suites.

No router matrix is required.

## 12. Acceptance criteria

M009 closes only when:

1. deterministic OTRv3 transport corpus remains byte-exact through the bouncer;
2. multi-client fanout introduces no OTR state/coupling;
3. reconnect never replays ambiguous OTR chat;
4. history/replay contains only supplied ciphertext;
5. endpoint-only plaintext never appears in bouncer state/files;
6. encrypted-store mode still satisfies M008 at-rest invariants;
7. docs accurately describe transparent compatibility rather than crypto implementation;
8. no high-severity privacy finding remains.

## 13. Closure evidence

Create plans/closure/bouncer-core/048-status.md containing:

- transcript/corpus matrix;
- multi-client result;
- disconnect/non-replay result;
- history/store result;
- endpoint-only plaintext absence result;
- M008 integration;
- dependency/unsafe/security review;
- current/Rust 1.88 verification;
- explicit M009 closure;
- future external real-client interoperability handoff note.
