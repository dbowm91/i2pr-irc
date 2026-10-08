# Research 009 — Privacy, Encrypted Durable State, and OTR Transparency

Status: complete for implementation planning

Research date: 2026-10-08

Repository baseline:

- e66c79a19174d3184edf16a656d4398aeff42715

Related authority:

- plans/000-long-term-specification.md
- plans/001-terminology-and-domain-model.md
- plans/002-long-term-roadmap.md
- plans/subsystems/bouncer-core-roadmap.md
- plans/research/008-m006-m007-irc-interoperability-and-identity-resilience.md
- plans/closure/bouncer-core/042-status.md

External references reviewed:

- SQLCipher upstream README/documentation and Community Edition license
- rusqlite 0.40 / libsqlite3-sys SQLCipher feature/build surfaces
- classic libotr OTRv3 release history and licensing
- OTRv4 specification and libotr-ng implementation status/licensing
- existing IRC OTR clients/plugins as interoperability references

## 1. Purpose

Define the privacy/encryption line without conflating three different security properties:

1. encryption of the bouncer's durable local state at rest;
2. handling of long-lived credentials and service-action secrets;
3. end-to-end encrypted IRC conversations.

The design must preserve the properties that make this bouncer useful:

- always-on upstream connectivity even when no downstream client is attached;
- durable searchable history when the Operator elects to retain it;
- simultaneous independent downstream clients;
- no home-grown cryptographic protocol;
- Rust 1.88;
- bounded single-owner SQLite worker;
- no alternate network egress.

## 2. Current durable-data exposure

The current application layer already redacts secrets in memory-facing APIs:

- StoredSecret is Zeroizing<String>;
- Debug renders [redacted];
- diagnostics/config snapshots omit credential payloads.

The SQLite file itself is nevertheless plaintext.

Sensitive or privacy-relevant durable content includes:

- network_secrets.sasl_username;
- network_secrets.sasl_password;
- registration_actions.payload, including NickServ/service authentication material;
- history_events.payload;
- history_search FTS5 sender/target/body terms;
- Network endpoints, display metadata, desired channels, cursors and read markers.

Therefore field-encrypting only passwords would leave the much larger privacy surface — history and its FTS index — plaintext.

## 3. Threat model for encryption at rest

M008 targets passive durable-media disclosure:

- copied/stolen SQLite database;
- copied backup;
- accidental database upload/commit;
- offline inspection of database pages, journal/WAL, or persistent search index.

M008 does not claim protection against:

- an attacker controlling the live bouncer process;
- root/kernel compromise;
- debugger/memory extraction while the database is open;
- compromise of the device that supplies the store key;
- compromised downstream IRC endpoint;
- plaintext intentionally sent through a non-E2EE conversation.

The database key necessarily exists in process memory while the store is being opened/used.

## 4. Whole-database encryption versus field encryption

### Field-level AEAD

Rejected for the baseline.

It would require separate encryption policy for:

- history payload;
- credentials;
- registration actions;
- potentially target/sender metadata.

More importantly, the current history-search implementation uses an FTS5 side table containing plaintext searchable sender/target/body terms.

Encrypting history bodies while retaining plaintext FTS would leak the content being protected. Encrypting FTS terms would make ordinary FTS5 search impossible and force a new searchable-encryption/index design with its own leakage model.

That is disproportionate complexity for a single-operator local bouncer.

### Whole-database SQLCipher

Selected.

SQLCipher preserves SQLite's relational/FTS model while encrypting the database at the page layer.

Relevant upstream properties:

- SQLCipher is a SQLite fork with database-file encryption, integrity/tamper checks and key derivation support;
- its bundled build supports FTS5;
- SQLCipher behaves as ordinary SQLite when no key is supplied;
- SQLCipher supports keyed open, rekey and export from plaintext SQLite;
- its Community Edition license is BSD-style and compatible with this repository's licensing direction.

This protects credentials, action payloads, history and FTS terms together rather than creating a plaintext side index.

## 5. Rust integration findings

The repository already uses rusqlite 0.40.

rusqlite 0.40 exposes:

- bundled-sqlcipher;
- bundled-sqlcipher-vendored-openssl;
- linked sqlcipher.

The bundled SQLCipher build enables:

- SQLITE_HAS_CODEC;
- SQLITE_TEMP_STORE=2;
- SQLITE_ENABLE_FTS5.

The vendored-OpenSSL variant avoids requiring a system OpenSSL installation, but it increases build time/dependency surface.

Important qualification requirement:

- do not assume the backend is shippable across Linux/ARM, macOS and Windows;
- M008-A must prove the selected SQLCipher backend under the repository's supported CI/toolchain matrix and Rust 1.88 before the backend choice is considered closed.

If that matrix fails, stop and revise the backend plan; do not fall back to ad-hoc field crypto merely to keep the milestone moving.

## 6. Store-key ownership

The store crate must consume key material, not discover it.

Do not make crates/store:

- read environment variables;
- read a key file;
- call an OS keyring;
- prompt a terminal;
- talk to a hardware token;
- derive a key from downstream IRC credentials.

Those are process/bootstrap policy concerns and the repository does not yet have its standalone daemon.

The store API should accept injected key material through a redacted/zeroizing type.

Recommended baseline:

- one random 256-bit database key for the single-operator store;
- key material supplied by the future executable/managed-app environment;
- store applies it immediately after opening the SQLCipher connection and before any schema read;
- application copy is zeroized once SQLCipher has accepted the key;
- no key in Debug, errors, tracing, config snapshot, SQL diagnostics or closure fixtures.

Passphrase/keyring/HSM UX is intentionally deferred to the executable layer.

## 7. Why the downstream login cannot be the database key

A password supplied by an attached IRC client is the wrong root key.

The bouncer is designed to remain connected while every downstream client is absent.

Tying durable-state decryption to a logged-in client would produce one of two bad outcomes:

- the bouncer cannot restart/reconnect autonomously until a client logs in; or
- the key must be retained/recoverable elsewhere anyway, eliminating the supposed property.

Therefore the store key belongs to process startup/key management, not downstream session authentication.

## 8. Plaintext mode remains explicit

Encryption at rest should be an option rather than silently changing every existing store.

The SQLCipher-capable backend can serve both:

- Plaintext;
- Encrypted(StoreKey).

Existing Store::open behavior may remain a compatibility plaintext wrapper while a new explicit StoreOpenOptions/StoreEncryption API carries the privacy policy.

A future production executable can choose a safer default once it owns key provisioning and migration UX.

## 9. Keyed-open semantics

For an encrypted store:

1. open the file;
2. apply the key before reading sqlite_master/application/schema data;
3. force an authenticated/decryption read;
4. only then run application-id/schema validation and migrations;
5. wrong key fails closed with a typed redacted error.

Never attempt schema creation/migration against a database whose key has not been validated.

Plaintext and encrypted databases must not be ambiguously auto-guessed in a way that could create a new schema over unreadable encrypted bytes.

## 10. Plaintext-to-encrypted migration

Do not mutate the only plaintext copy in place.

Selected strategy:

- close/quiesce the source store;
- create a distinct destination/temp database under SQLCipher with the new key;
- copy via the documented SQLCipher export mechanism or an equivalently complete SQLite-level copy;
- explicitly preserve/verify application_id, user_version, tables, indexes, FTS content, durable IDs and row counts;
- close and reopen destination with the new key;
- run the same schema/search-index integrity checks;
- only then report the encrypted copy complete.

The source plaintext database remains untouched by the library primitive.

Final replacement/deletion is an operator/bootstrap concern because deleting a file cannot promise physical secure erasure on SSD/flash media.

## 11. Rekey lifecycle

Key rotation is required, but safety outranks convenience.

Preferred implementation is copy-and-verify from old-key encrypted source to new-key encrypted destination using the same migration substrate.

SQLCipher's PRAGMA rekey is an available mechanism, but an in-place-only workflow provides a weaker recovery story after power loss.

M008-B should select the safest bounded implementation after testing both. It must never leave the only usable database silently destroyed after an interrupted rotation.

## 12. FTS/search implications

Whole-database encryption allows the existing FTS5 schema and query semantics to remain unchanged after the database is unlocked.

Required qualification:

- history search behaves identically before/after encryption;
- FTS index survives restart/export/rekey;
- known plaintext search terms are absent from closed encrypted database files and durable side files;
- index-consistency startup checks continue to work.

No searchable-encryption protocol is introduced.

## 13. OTR security boundary

End-to-end conversation encryption is a different layer.

Decision:

**the bouncer must not terminate OTR.**

OTR endpoints are downstream user clients and remote user clients.

Conceptual path:

~~~
local OTR-capable IRC client
        |
        | OTR ciphertext inside IRC PRIVMSG
        v
     i2pr-irc
        |
        | same opaque ciphertext
        v
     I2P IRC
        |
        v
remote OTR-capable IRC client
~~~

The bouncer may see IRC routing metadata and ciphertext length/timing, but it must not hold OTR private keys, fingerprints, ratchet/session state or decrypted message bodies.

If the bouncer terminated OTR:

- the bouncer would become an E2EE endpoint;
- a live bouncer compromise could read conversation plaintext;
- simultaneous clients would have to share or multiplex cryptographic session state;
- history/search would silently change from ciphertext storage to plaintext storage.

That is not the privacy property requested.

## 14. OTR version decision

### OTRv3

Selected as the interoperability baseline.

Classic libotr 4.x implements OTRv3 and explicitly added instance tags for multiple simultaneous logins.

OTRv3 is therefore the useful compatibility target for IRC clients/plugins that already speak classic OTR.

No libotr production dependency is required in the bouncer because the bouncer does not perform OTR cryptography.

### OTRv4

Deferred.

The upstream OTRv4 specification still identifies itself as a draft under revision.

libotr-ng is a native C library with a materially larger dependency surface, is not inherently thread-safe, and itself depends on classic libotr plus other crypto libraries.

The bouncer gains no security benefit from embedding it because the correct endpoint is the client.

A future built-in IRC client may revisit OTRv4 independently.

## 15. OTR transport requirements

M009 is accurately named **OTRv3 transparent-carriage compatibility**, not "OTR implementation."

The bouncer must prove that it does not break OTR-bearing IRC traffic.

Required corpus includes:

- OTR query messages;
- OTR AKE/encoded messages;
- OTR data messages;
- OTR fragmented messages;
- OTR whitespace capability tags where representable in an IRC trailing parameter;
- messages at relevant IRC line boundaries.

Rules:

- preserve message trailing-body bytes exactly through parse/encode and mediation;
- never reassemble/refragment OTR protocol fragments;
- never parse OTR cryptographic fields into bouncer state;
- never derive identity from OTR instance tags;
- never log/decode OTR payloads;
- normal IRC tag mediation may change IRC message tags but not the OTR body;
- OTR-bearing outbound chat remains NonReplayable across ambiguous disconnects.

## 16. Multi-client OTR behavior

All attached bouncer clients share one upstream IRC identity, but each OTR-capable client can be its own OTR endpoint.

The bouncer should:

- fan out the same incoming opaque OTR payload according to ordinary IRC routing;
- not decide which local OTR instance should consume it;
- not merge OTR sessions;
- not suppress an OTR frame because another attached client does not understand it;
- not make upstream capability negotiation depend on OTR-aware attached clients.

OTRv3 instance tags are endpoint protocol content and remain opaque to the bouncer.

## 17. OTR and durable history

If history retention is enabled, the bouncer stores what it observed on IRC: OTR ciphertext.

It must never store decrypted OTR plaintext because it never possesses it.

Consequences:

- history replay can contain old OTR ciphertext;
- an OTR endpoint may be unable or unwilling to decrypt historical ciphertext after session/key evolution;
- FTS search cannot provide meaningful plaintext search over an OTR conversation;
- the bouncer must not claim that encrypted conversation history is searchable plaintext.

M009 does not add OTR-specific plaintext indexing.

A future per-buffer "do not retain history" privacy policy may be useful, but it is a separate retention-policy feature and is not required for transparent OTR carriage.

## 18. OTR dependency/licensing disposition

No production OTR cryptography dependency is selected.

Reference implementations are suitable for external/corpus qualification only.

Classic libotr is LGPL-family library code with GPL-licensed surrounding tools/files in its repository; libotr-ng is LGPL-2.1-or-later but brings a larger native dependency set.

Avoiding a linked production dependency:

- preserves i2pr-irc's current Rust-only core dependency posture;
- avoids unsafe FFI in a workspace that forbids unsafe code;
- avoids converting the bouncer into a cryptographic endpoint;
- avoids licensing/build complexity that provides no bouncer-side security benefit.

## 19. Implementation decomposition

### Corrective 043 — deterministic member-state test synchronization

Verification-only prerequisite discovered by Corrective 042. Ready first.

### M008-A / Plan 044 — SQLCipher backend and keyed-store foundation

- qualify bundled SQLCipher build/dependencies on supported toolchains;
- add injected zeroizing StoreKey;
- explicit plaintext/encrypted store-open policy;
- apply/validate key before schema access;
- wrong-key failure/redaction;
- preserve FTS5.

### M008-B / Plan 045 — encrypted migration and key rotation

- plaintext -> encrypted copy-and-verify;
- encrypted old-key -> new-key safe rotation;
- source-preserving failure behavior;
- complete schema/FTS/identity validation.

### M008-C / Plan 046 — encrypted durable-state qualification and M008 closure

- credentials/actions/history/FTS sentinel-at-rest checks;
- restart/search/rekey qualification;
- Rust 1.88 + current + platform build matrix;
- dependency/license record.

### M009-A / Plan 047 — OTRv3 opaque-carriage and multi-client invariants

- OTR corpus through wire/runtime/history paths;
- exact body preservation;
- non-replay;
- no OTR state/crypto in bouncer;
- multi-client fanout invariants.

### M009-B / Plan 048 — integrated OTR privacy qualification and M009 closure

- end-to-end opaque transcript qualification with deterministic reference corpus;
- encrypted-store interaction after M008;
- prove stored bouncer history contains ciphertext, never fixture plaintext;
- qualify disconnect/reconnect/non-replay behavior;
- document external real-client interoperability as a later standalone-listener qualification if no production listener exists yet.

## 20. Sequencing

Strict sequence:

Corrective 043
  -> Plan 044
  -> Plan 045
  -> Plan 046 / M008 closure
  -> Plan 047
  -> Plan 048 / M009 closure

The privacy plans may be registered now but stay blocked behind Corrective 043.

This sequencing gives one implementation handoff at a time and lets M009 qualification use the completed encrypted-store substrate where relevant.

## 21. Explicit non-goals

M008/M009 do not include:

- deriving the database key from an IRC client login;
- storing the database key inside the same SQLite database;
- OS keyring/HSM UI;
- standalone-daemon CLI;
- per-network database files;
- per-field searchable encryption;
- custom cryptographic algorithms;
- bouncer-terminated OTR;
- bouncer OTR fingerprints/private keys;
- OTRv4 implementation;
- Signal/OMEMO implementation;
- group-chat E2EE;
- remote hosted/multi-user bouncer service;
- secure physical erasure claims.

## 22. Readiness

Corrective 043 is ready immediately.

Plans 044-048 are architecturally specified but must remain blocked in sequence until Corrective 043 closes.

No upstream i2pr contract is required for M008/M009.
