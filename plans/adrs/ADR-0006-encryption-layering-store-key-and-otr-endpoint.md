# ADR-0006 — Encryption Layering, Store-Key Ownership, and OTR Endpoint Boundary

Status: accepted

Date: 2026-10-08

Related:

- plans/research/009-m008-m009-privacy-encryption-and-otr.md
- plans/adrs/ADR-0002-bounded-sqlite-persistence-history-order-and-session-identity.md
- plans/adrs/ADR-0003-process-runtime-control-and-pre-bind-downstream-admission.md

## Context

The bouncer now retains several classes of privacy-sensitive durable data:

- IRC history;
- FTS5 search terms derived from that history;
- SASL credentials;
- service-action payloads such as NickServ authentication commands;
- network/channel/account metadata.

Application-level redaction prevents ordinary logs/Debug/config export from printing secrets, but the SQLite database itself remains plaintext.

Separately, the product wants optional end-to-end encrypted conversations such as OTR. Those two requirements protect against different attackers and should not share one cryptographic endpoint.

A design that field-encrypts message bodies but leaves FTS terms plaintext does not provide meaningful history-at-rest privacy. A design that terminates OTR in the bouncer gives the bouncer conversation plaintext and ceases to be endpoint-to-endpoint encryption.

## Decision

### 1. Durable-state encryption uses whole-database SQLCipher

M008 uses a SQLCipher-capable SQLite backend rather than field-level message/credential encryption.

The encrypted database covers, together:

- credentials;
- registration-action payloads;
- history;
- FTS search terms;
- durable configuration/state.

Existing relational and FTS semantics remain available after the database is unlocked.

The exact bundled/linking mode is qualified by Plan 044 before the backend choice is considered shippable on all supported platforms.

### 2. Encryption-at-rest is optional and explicit

The store has an explicit open policy:

- plaintext SQLite-compatible mode;
- SQLCipher encrypted mode with supplied key material.

Existing databases are never silently converted merely because an encryption-capable build is installed.

### 3. The store consumes a key; it does not discover one

The store layer accepts injected key material through a redacted/zeroizing value.

The store layer does not:

- read environment variables;
- read key files;
- call platform keyrings;
- prompt users;
- contact hardware security devices;
- derive the key from downstream IRC authentication.

Key acquisition belongs to process/bootstrap integration, which can later differ between a standalone executable and the i2pr managed-app environment.

### 4. The baseline database key is independent random key material

The storage API is designed around opaque high-entropy key material rather than requiring the bouncer to derive a database key from an Operator password.

This preserves always-on restart/reconnect behavior when no downstream client is attached.

Future executable-level passphrase/keyring/HSM workflows may derive or unwrap this key outside crates/store.

### 5. Key application precedes every schema read

For encrypted open:

1. open connection;
2. apply SQLCipher key;
3. force a keyed read that proves the database can be decrypted;
4. only then perform application-id/schema/index checks and migrations.

A wrong key is a fail-closed typed/redacted startup error.

The implementation must never attempt to initialize a fresh schema over a database that is merely unreadable because the wrong key was supplied.

### 6. Encryption migration is source-preserving

Plaintext-to-encrypted migration writes and verifies a separate destination database before any source replacement/deletion.

Key rotation uses the same source-preserving principle unless Plan 045 proves an in-place SQLCipher rekey has an equal or stronger interruption/recovery story.

The library does not claim secure physical deletion of the plaintext source.

### 7. OTR terminates at IRC clients, never at the bouncer

For OTR conversations:

- local OTR-capable client is an endpoint;
- remote OTR-capable client is the other endpoint;
- i2pr-irc is an opaque IRC transport/bouncer in between.

The bouncer does not hold:

- OTR identity/private keys;
- fingerprints/trust decisions;
- OTR session/ratchet state;
- decrypted OTR message bodies.

### 8. OTRv3 is the compatibility baseline; OTRv4 is deferred

M009 qualifies transparent carriage of classic OTRv3 wire content because that is the deployed compatibility target for established IRC OTR clients and supports multiple endpoint instances.

M009 does not implement OTR cryptography and does not link libotr.

OTRv4 is deferred because its specification remains draft/revisable and embedding its native implementation would add substantial dependencies while placing crypto at the wrong architectural layer.

A future built-in IRC client may revisit OTRv4 as an endpoint concern.

### 9. OTR history is ciphertext history

When durable history is enabled, OTR-bearing messages are retained as the opaque ciphertext observed on IRC.

The bouncer does not produce a parallel plaintext copy or plaintext search index.

Historical OTR ciphertext may not be decryptable by a later endpoint/session; no guarantee is made otherwise.

## Considered alternatives

### Encrypt only credential fields

Rejected. History and FTS remain plaintext.

### Encrypt message bodies with application-level AEAD

Rejected for the baseline. FTS/search would either leak plaintext terms or require a new searchable-encryption design.

### One encrypted database per Network

Rejected. This is a single-operator bouncer and would multiply worker/migration/key-management complexity without defending against the live process that owns all Networks.

### Database key derived from downstream client password

Rejected. It breaks always-on autonomous restart/reconnect or merely forces the same key to be stored elsewhere.

### Store crate reads an environment variable/keyring

Rejected. Key source is process/deployment policy and must not contaminate the persistence primitive.

### Bouncer-terminated OTR

Rejected. It makes the bouncer an E2EE endpoint and gives it plaintext, creating exactly the trust expansion OTR is meant to avoid.

### Implement OTR ourselves in Rust

Rejected. The bouncer does not need cryptographic OTR code to carry OTR, and custom cryptographic protocol implementation is outside project risk tolerance.

### Link libotr/libotr-ng into the bouncer

Rejected for the baseline. It adds native crypto/FFI/licensing/build surface while providing no security benefit to an opaque transport.

## Consequences

- SQLCipher becomes the selected encrypted-store substrate if Plan 044 passes packaging/MSRV qualification.
- store-open API gains an explicit encryption policy/key value;
- future standalone/i2pr integration must provide a key source if encrypted storage is enabled;
- FTS/search can continue unchanged after unlock;
- migration/rekey tooling becomes explicit work;
- OTR-capable downstream clients remain independently responsible for cryptographic state;
- simultaneous clients do not share bouncer-owned OTR state;
- the bouncer can advertise transparent OTR compatibility without claiming to implement cryptography.

## Security implications

Encryption at rest protects only when the key is not compromised with the database.

The encrypted-store feature does not protect plaintext from a live compromised process.

OTR protects conversation content from the bouncer only because the bouncer never terminates it.

Metadata remains observable to the bouncer as required to route IRC:

- network;
- target;
- sender/prefix;
- timestamps;
- message lengths/timing;
- IRC tags supplied by the server/client according to existing mediation.

No claim of metadata-hiding E2EE is made.

## Verification

M008 must prove:

- wrong-key refusal;
- plaintext and encrypted open are unambiguous;
- credentials/history/FTS sentinels are absent from closed encrypted files/side files;
- search/history semantics survive encrypted restart and key rotation;
- current stable/Rust 1.88/platform builds remain supported.

M009 must prove:

- OTR-looking trailing payload survives bouncer parse/mediation/history/replay byte-exactly where IRC framing permits;
- no OTR crypto/session state is introduced;
- simultaneous clients receive ordinary routed ciphertext without state merging;
- outbound OTR-bearing chat is not replayed across ambiguous disconnects;
- retained history contains only ciphertext supplied to the bouncer, never fixture plaintext known only to endpoint tests.

## Supersession

None.
