# Standalone M010-B / Plan 051 Closure

Status: closed
Implementation commits: `856ee28c566fe220e87fcb24f1a0a97ee709ee10`, `1e299c86fd367dea984d1bba09beb4726a18387f`, `0867c3037464e8629fef65709d4dcad220aa8019`, `e5dccad6e77b6d1ca90de79f80d103676c2c6248`, `b196781cd8467b5df144d18f34678f2fdc8a5418`
Predecessor closure: `plans/closure/standalone/050-status.md`

## Requirement-to-evidence

| Requirement | Evidence | Result |
|---|---|---|
| Numeric loopback TCP only | `listener::validate_loopback`, parsed `SocketAddr`, accepted peer recheck, IPv4/IPv6 loopback tests, nonloopback rejection tests | Pass |
| Bounded permit-gated accept and cancellation | `listener::serve` uses 64 owned permits acquired before spawning; stop cancels accept and joins aborted handshakes | Pass |
| Bounded pre-auth resource use | 30 second deadline; 512-byte lines; 4 KiB read buffer; 32 line limit; at most 3 SASL chunks and 1024 decoded bytes | Pass |
| PASS and SASL PLAIN are separate local credentials | `CredentialVerifier` receives a bounded profile/token; PASS uses `profile:token`; SASL validates authzid/authcid/password and uses only the local verifier | Pass |
| CAP/NICK/USER ordering and one-shot state transfer | PASS-first and CAP-first flows; NICK/USER before SASL; `AuthenticatedCheckpoint::into_parts` consumes stream and state once; post-auth bytes remain unread in the checkpoint | Pass |
| No raw secret in errors and transient secret cleanup | Fixed generic 464/904 responses; line, input buffer, SASL payload, decoded PLAIN material and read scratch use zeroizing storage | Pass |
| No unauthenticated privileged reachability | Listener only returns a checkpoint after verified authentication and NICK/USER plus CAP completion; no RuntimeControlHandle is reachable in this module | Pass |
| Network boundary remains narrow | One allowlisted file is limited to `TcpListener::bind`/`accept`; address validation is required; dial, DNS, UDP, Unix and nonloopback fixtures are rejected | Pass |

## Commands executed

- `rtk cargo fmt --all` — passed
- `rtk cargo test -p i2pr-irc-daemon --locked` — 10 passed
- `rtk cargo test -p i2pr-irc-daemon --locked` — 13 passed after handoff integration
- `rtk cargo clippy -p i2pr-irc-daemon --all-targets --all-features --locked -- -D warnings` — passed
- `rtk cargo test -p i2pr-irc-runtime --lib --locked` — 285 passed
- `rtk cargo clippy -p i2pr-irc-runtime --lib --all-features --locked -- -D warnings` — passed
- `rtk python3 scripts/check-network-boundary.py` — passed with daemon exception positive controls

## Security and recovery review

Handshake tasks reserve their semaphore permit before spawn. Rejected or cancelled handshakes drop their socket and permit. PASS/SASL credentials never enter formatted errors. The parser refuses mixed PASS/SASL attempts, repeated auth, unsupported commands and malformed/truncated input. CAP REQ is all-or-nothing and bounded; only capabilities from the runtime's downstream advertisement plus SASL PLAIN are acknowledged. No upstream-conditional capability is advertised before Network selection.

This plan deliberately leaves the authenticated checkpoint private to the daemon executable package and does not grant it controller authority. `serve_local_access` accepts an injected verifier, but the production CLI does not activate it until Plan 053 provisions credentials. Unix-domain listener support is optional and not implemented. This closure is the tested authentication/listener substrate, not a standalone-client capability claim.

## Handoff

Plan 052 consumes the one-shot checkpoint and connects it to existing registration/admission without replaying CAP or registration bytes. Plan 052 is closed. The CLI remains non-listening until secure credential provisioning.
