# Bouncer Core M008-B / Plan 045 — Encrypted Store Migration and Key Rotation

Status: ready

Hard dependency:

- plans/closure/bouncer-core/044-status.md

Research authority:

- plans/research/009-m008-m009-privacy-encryption-and-otr.md

Architecture authority:

- plans/adrs/ADR-0006-encryption-layering-store-key-and-otr-endpoint.md

Primary class: security migration + durability

## 1. Objective

Provide source-preserving conversion of an existing plaintext i2pr-irc store into an encrypted SQLCipher store and a recoverable key-rotation workflow.

No migration may destroy the only known-good source database before a keyed destination has been independently reopened and verified.

## 2. Migration primitive

Add a bounded offline/store-quiesced primitive conceptually equivalent to:

~~~text
export_encrypted_copy(source, source_policy, destination, new_key)
~~~

Required behavior:

- source is opened and validated under its declared policy;
- destination must not already contain unrelated data;
- destination is created under the new encrypted key;
- complete database content is copied with SQLCipher's supported export mechanism or an equivalently complete SQLite-level copy;
- destination application metadata is explicitly set/verified;
- destination is closed;
- destination is reopened through the ordinary encrypted Store-open path;
- ordinary schema/search-index integrity checks pass;
- only then is success returned.

The primitive does not delete or overwrite the source.

## 3. Plaintext -> encrypted

Support schema-8 and every currently supported predecessor that the normal store can migrate.

Preferred sequencing:

1. open/migrate source normally;
2. quiesce source worker;
3. export to distinct sibling/temp destination;
4. verify encrypted destination;
5. return verified destination path/report.

The caller later decides whether/how to replace the source.

Do not claim secure deletion of the plaintext file.

## 4. Key rotation

Support encrypted old-key -> encrypted new-key.

Prefer the same copy-and-verify substrate because it leaves the old encrypted source recoverable until the new one is proven.

Plan 045 may use PRAGMA rekey only if interruption/power-loss tests establish an equal or stronger recovery story.

No successful return until the destination/new-key database passes the same ordinary Store-open/integrity checks.

## 5. Complete durable-state preservation

Migration must preserve exactly:

- NetworkId and ClientId identities;
- network configuration;
- network_secrets;
- desired channels/detached state;
- registration actions and phase/order;
- history event IDs/order/payloads/timestamps/msgids;
- buffers;
- cursors;
- read markers;
- FTS5 search rows;
- application_id;
- user_version/current schema;
- promised indexes.

Use row/count/hash/sentinel verification where appropriate.

## 6. FTS handling

Do not trust table presence alone.

After export:

- ordinary startup search-index consistency check passes;
- known search fixtures return identical event IDs/order;
- retention/search references still work;
- no plaintext sidecar FTS rebuild is emitted outside the destination database.

If SQLCipher export does not carry FTS state exactly, rebuild it inside the encrypted destination and verify before success.

## 7. File safety

Destination handling must be conservative:

- create with restrictive permissions where the platform supports them;
- refuse accidental overwrite;
- keep temp path in same filesystem when future atomic replacement is intended;
- sync destination file and relevant directory metadata where available;
- remove failed temporary encrypted destination best-effort;
- never remove source automatically.

Cross-platform path/rename behavior must be documented, but final installation swap UX belongs to the standalone process layer.

## 8. Error handling and secrecy

All migration errors remain redacted.

Never include:

- old/new key;
- SASL password;
- registration-action payload;
- history payload;
- SQL statement containing key material.

A wrong source key fails before destination mutation.

A destination failure leaves source readable under its original policy/key.

## 9. Concurrency

Migration is offline relative to the Store worker.

Do not copy a database that is concurrently accepting writes.

The API must require ownership/quiescence rather than hoping WAL state is stable.

No new multi-writer model is introduced.

## 10. Tests

At minimum:

- schema-8 plaintext -> encrypted copy;
- older supported schema -> normal migration -> encrypted copy;
- encrypted old-key -> encrypted new-key;
- wrong old key leaves source untouched;
- destination exists => refusal;
- injected failure during export => source intact;
- injected failure before verification => source intact;
- correct key reopens destination;
- old/wrong key does not;
- IDs/history/cursors/markers/actions/secrets preserved;
- FTS query parity before/after;
- large bounded history copied without one giant in-memory materialization.

## 11. Acceptance criteria

Plan 045 closes only when:

- plaintext-to-encrypted conversion is source-preserving;
- key rotation has a recoverable source-preserving path;
- destination verification uses the normal keyed Store open;
- full durable and FTS semantics survive exactly;
- no migration path deletes plaintext/source automatically;
- failure leaves the original usable.

## 12. Stop conditions

Stop for ADR/corrective if:

- SQLCipher export cannot preserve/rebuild required FTS/schema state safely;
- safe rotation requires an in-place-only destructive operation;
- migration requires a second unbounded history representation in memory;
- cross-filesystem replacement becomes part of the store primitive.

## 13. Closure evidence

Create plans/closure/bouncer-core/045-status.md with:

- migration/rotation matrix;
- failure-injection matrix;
- durable-state parity table;
- FTS parity;
- filesystem safety notes;
- Plan 046 readiness.
