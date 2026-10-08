# Bouncer Core M009-B / Plan 048 Closure — Integrated OTR Privacy Qualification

Status: closed

Implementation commit: `530d3c87a78c77a2977d353402d1e94ff982f7a9` — `test(runtime): qualify integrated OTR privacy with SQLCipher`

## Integrated transcript and deterministic endpoint boundary

The endpoint-only plaintext fixture is the single searchable token `endpointonlyplaintext16e2`. The test keeps it in the test process only; it is never passed to Store, RuntimeController, a client stream, or an upstream stream. The only transcript supplied to i2pr-irc consists of synthetic OTR-shaped IRC trailing bodies. These fixtures verify the bouncer's data boundary, not OTR cryptographic correctness.

| Direction/path | Evidence | Result |
|---|---|---|
| Client to upstream | Query/AKE-shaped `PRIVMSG` body reaches upstream byte-exactly | Pass |
| Upstream to clients | Data and two distinct ordered fragment-shaped messages reach two endpoint fixtures and one legacy session | Pass |
| Session metadata policy | Tag/capability presentation differs only according to each session's existing negotiation; each OTR body remains unchanged | Pass |
| Session isolation | One client detaches; both healthy endpoint fixtures receive the following opaque message unchanged | Pass |
| New attachment history replay | A newly registered session's `CHATHISTORY LATEST` contains only the supplied ciphertext-shaped messages and no endpoint-only sentinel | Pass |
| Durable history after process shutdown | Store query returns the exact opaque event payloads, including the post-detach event | Pass |
| FTS | Search for an opaque-transcript term returns ciphertext-bearing hits whose indexed sender/target/body fields omit the endpoint-only plaintext | Pass |

The earlier Plan 047 corpus separately proves whitespace-bearing bodies, `NOTICE`, exact 512-byte framing, ordinary plaintext-store operation, cross-session fanout, and non-replay after an ambiguous upstream disconnect. Together, Plans 047 and 048 establish transport behavior in plaintext-store mode and defense in depth in encrypted-store mode.

## Encrypted durable state and restart

Plan 048 runs the production RuntimeController against an explicitly keyed SQLCipher file Store. It shuts down the runtime and store, scans the main database and present `-wal`, `-shm`, and rollback-journal sidecars, then reopens the same database with the key and queries the retained transcript.

| Check | Result |
|---|---|
| Endpoint-only plaintext sentinel in history or replay | Absent; it was not supplied to the bouncer |
| FTS search result fields | Contain only indexed opaque message content and no endpoint-only plaintext |
| Plain SQLite file signature in closed encrypted files | Absent |
| Correct-key restart and retained history query | Pass |
| SQLCipher at-rest sentinels and plaintext positive control | Pass in Plan 046 encrypted-store qualification |
| Encrypted-store migration/rotation, wrong-key rejection, and failure cleanup | Pass in Plan 045 qualification |

The encrypted integration qualifies storage protection for ciphertext and metadata the bouncer actually retains. It does not validate the OTR cipher, endpoint keys, or plaintext secrecy inside either endpoint. SQLCipher's Store key remains independent of OTR endpoint keys.

## Multi-client, disconnect, and history/search contract

Plan 047 verifies that OTR-bearing user chat remains `NonReplayable`, does not survive disconnect, and is absent on the replacement upstream generation after an ambiguous disconnect. Desired IRC state may reconcile through the ordinary generation path; the bouncer synthesizes no OTR retry or refresh traffic. The three-client integration also confirms that the bouncer does not route on synthetic instance-tag-like body text.

Retained OTR transcript is ciphertext. CHATHISTORY can replay old ciphertext that a current endpoint can no longer decrypt. FTS indexes only received message content and cannot search plaintext for an end-to-end encrypted conversation whose plaintext never entered the bouncer. This limitation is recorded in `architecture/history-search.md`; ciphertext search is not described as semantic search. A no-history or per-buffer-retention option remains separate work.

## Security, dependency, and capability review

- No OTR keys, fingerprints, SMP state, or cryptographic session state were added to schema or runtime state.
- No OTR crypto dependency, OTR module, or unsafe OTR FFI exists in production crates. Static manifest/source search found no libotr or libotr-ng integration.
- No IRC body logging was added. Existing raw-protocol logging absence qualification passes.
- Downstream CAP surface remains unchanged and contains no OTR capability. Plan 047's upstream capability fingerprint test remains green.
- No exception was added to ambiguous-command replay policy.
- SQLCipher protects retained database pages at rest; it does not provide end-to-end encryption.

No high-severity privacy finding remains within M009's transparent-carriage scope.

## Verification

Passed on the final qualification tree:

- `cargo fmt --all -- --check` (as part of full verification)
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` (as part of full verification)
- `scripts/check-network-boundary.py` (as part of full verification)
- `scripts/verify.sh full`
- `rustup run 1.88.0 sh scripts/verify.sh full`
- focused `encrypted_store_otr_transcript_fanout_history_and_restart_stay_opaque` under current stable and Rust 1.88.0
- existing OTR corpus, downstream non-replay, SQLCipher migration/rotation, and encrypted sentinel qualification suites
- static manifest/source search for libotr/libotr-ng, OTR FFI, and an OTR module

The first Rust 1.88 full run had one transient `SessionCreate` timeout in the unrelated SAM fragmented-reply conformance test. That test passed in isolation, and a complete Rust 1.88 full verification rerun passed. Native runtime qualification was on macOS; the existing Plan 046 cross-target matrix records Linux/Windows compile checks only, not runtime checks.

## M009 disposition and handoff

M009 is closed. Allowed product language is “OTRv3-transparent” and “compatible with client-side OTR transport”; i2pr-irc stores only the ciphertext it receives and does not implement OTR encryption. No claim is made for OTRv4, group-chat E2EE, or cryptographic implementation correctness.

No next Bouncer Core implementation plan is currently registered or dependency-ready. External libotr/irssi/weechat interoperability remains a future standalone-listener qualification because this repository has no production downstream socket listener. The separate Router R002 plan remains blocked on stable public i2pr managed-app stream/listener/lifecycle contracts and its prerequisites; M009 does not change that blocker. No other eligible plan was available to advance.
