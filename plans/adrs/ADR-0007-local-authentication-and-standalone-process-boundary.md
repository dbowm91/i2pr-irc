# ADR-0007: Local Authentication and Standalone Process Boundary

Status: accepted
Date: 2026-10-08
Applies to: M010 standalone daemon
Supersedes: none
Authority: plans/000-long-term-specification.md, ADR-0001, ADR-0002, ADR-0003, ADR-0006, Research 010

## Context and decision drivers

M001-M009/R001 are library components, not a safe local IRC service. The core's LocalAcceptor returns a *trusted* ClientId; DownstreamAdmission and BouncerServ assume that identity has already been established. Unix socket permissions and a 127.0.0.1 bind are not sufficient for TCP authentication. IRC CAP LS may precede PASS, and SASL authentication may occur after NICK/USER: discarding or replaying frames is unsafe. The product serves one Operator with multiple durable local ClientIds, not hosted tenants.

## Decision

### 1. Ownership and privilege

Create a dedicated standalone executable/process adapter, separate from runtime/core/sam/store. It owns only local listening, authentication, startup configuration, exclusive state-dir ownership, signals and shutdown. RuntimeController still owns Network lifetimes; NetworkOwner still owns each network's live state. SamProvider remains the only upstream I2P network transport.

Do not add a generic upstream TCP/DNS API, direct I2P socket from daemon, HTTP control server or DCC support.

### 2. Listening

First production baseline: numeric loopback TCP, fail closed on non-loopback IP. Optional Unix-domain socket on Unix platforms with private parent directory and socket permissions. Unix connections must satisfy the same Operator secret as TCP for M010; peer credentials may be qualified as extra defense. No remote listener or implicit host binding.

The daemon alone owns the explicitly allowlisted local accept API. The network-boundary guard must reject generic outbound TCP and DNS in all crates, including the new executable.

### 3. Pre-authentication and IRC registration

No unverified stream may receive a trusted ClientId or reach RuntimeController, BouncerServ or NetworkOwner. A bounded local pre-auth state machine recognizes CAP, NICK, USER, PASS, AUTHENTICATE and PING as needed; other requests are refused without exercising operator authority. Support both PASS and SASL PLAIN for existing IRC clients, with exact one-attempt/limited-attempt semantics, a bounded authentication deadline and bounded initial input.

Use one logical registration state machine across the boundary. The canonical parser/writer may be factored and its state *moved* into a post-auth registration checkpoint. Preserve CAP negotiation (including replies already sent), NICK/USER facts, line decoder, queued bytes, and writer ownership exactly once. Do not synthesize a second registration, replay answered frames, silently downgrade negotiated capabilities or queue arbitrary pre-auth command history. Conditional capabilities must be advertised truthfully: before selecting an authenticated network, only unconditional bouncer capabilities and downstream sasl=PLAIN may be advertised; dynamic upstream-conditional offers require a valid selected Network and authenticated state.

A passive PASS-only first profile may be used by legacy clients; a CAP-first/SASL client must not deadlock waiting for CAP LS/AUTHENTICATE. Reject mixed/conflicting PASS and SASL identities rather than selecting whichever succeeds last.

### 4. One operator, many stable clients

One authenticated local Operator has all local control rights; roles/tenant separation are out of scope. Auth credentials prove Operator authorization. A separately bounded canonical client-profile label (default "default") maps via StoreHandle::create_client to durable ClientId *only after auth*. Reconnecting to the same profile preserves history/cursor lineage, while every attachment gets a fresh SessionId. A client profile is not a separate authorization principal. Where profile is present in a PASS value, specify and test a single unambiguous syntax with bounded grammar; SASL PLAIN authcid may carry the profile; prohibit identity-switch after authentication.

### 5. Secrets and storage

For M010 use a randomly generated, at least 256-bit, high-entropy Operator secret; no human password choice is needed. The bootstrap UX provisions the secret over an operator-controlled local channel, not command-line arguments, logs or IRC-visible fields. If a verifier is stored, compare in constant time; for random tokens, a fixed-length cryptographic digest is appropriate. If human passwords are added later, a new security review must specify Argon2id-equivalent derivation, work factors and concurrency limits.

Database encryption is independently optional and explicitly configured. On encrypted initialization, generate a separate 256-bit SQLCipher StoreKey. No silent key regeneration, plaintext fallback, autodetection, or derivation from the auth secret. Wrong/missing key and suspicious path/permission/ownership problems fail closed. No secret may be displayed in normal diagnostics or config export.

### 6. Process lifecycle

Acquire a nonblocking cross-process state-directory lease before store/controller/network startup, using a reviewed Rust 1.88-compatible implementation. State-dir lock is held until all background components stop. A second instance fails clearly; stale PID file presence alone is not an authority or a reason to delete an active lock.

Start sequence: validate config/path/security -> acquire lock -> read provisioned credentials/key -> open store -> construct provider/controller -> restore supervised Networks -> establish local listener -> signal readiness. Shutdown sequence: stop accepting -> cancel/finish bounded auth sessions -> request controller stop and join -> flush/close Store -> release lock. Where ordering must change, prove it preserves no traffic after stop and deterministic resource release.

### 7. Resource bounds and authorization

Accepted-but-unauthenticated sockets, total tasks, buffered bytes, line length, decode work, SASL payload/chunks, authentication attempts, active sessions and startup/stop wait have explicit process ceilings. Admission permits acquired before spawning task, fail fast when full. Auth failure messages disclose no account-existence or credential details and must not persist clients. Bound resource ceilings and standardized failure dispositions are visible in redacted diagnostics only.

## Consequences

Advantages: existing bouncer core remains reusable under standalone or future i2pr managed-app adapters; conventional IRC clients connect through loopback, and credentials do not become router or storage keys.

Costs: the local boundary requires stateful CAP/SASL handling and new deterministic tests; the daemon must enforce OS file security, same-instance exclusion and process cancellation. On platforms without Unix sockets, TCP remains the portable baseline.

## Compatibility and migration

No durable NetworkId or ClientId schema rewrite required just to add the executable. Existing in-process tests/LocalAcceptor contracts remain valid. If an implementation needs a new typed post-auth checkpoint API, change it additively and qualify the preexisting registration path. Existing M008 encrypted databases retain the same key format.

## Verification and stop conditions

Prove CAP LS -> PASS, PASS -> CAP LS, CAP LS -> CAP REQ sasl -> AUTHENTICATE PLAIN -> CAP END, NICK/USER before authentication, authentication failure after partial CAP, fragmented/coalesced input, double-auth refusal, buffered post-auth bytes, timeout, bounded concurrent idle accepts, wrong ClientId attempts, client cursor recovery, and no BouncerServ before trust.

Use an executable-specific static allowlist with deliberate malicious bind/dial fixtures. Check Rust 1.88/target OS support. Stop if implementation needs unchecked networking, loss of decoder/CAP state, replay of answered commands, permissive unauthenticated control, or key auto-regeneration.

Future policy changes (TCP remotely exposed, unauthenticated Unix bypass, client roles, human-password scheme) require a new ADR rather than loosening this record.
