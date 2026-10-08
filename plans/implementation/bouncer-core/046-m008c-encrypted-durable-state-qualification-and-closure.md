# Bouncer Core M008-C / Plan 046 — Encrypted Durable-State Qualification and M008 Closure

Status: active

Hard dependency:

- plans/closure/bouncer-core/045-status.md

Source milestone:

- M008 — Encrypted Durable State

Primary class: security qualification + milestone closure

## 1. Objective

Qualify the encrypted-store option as protection against passive durable-file disclosure without overstating protection against a compromised live process.

## 2. Sentinel corpus

Create synthetic distinctive sentinels for:

- SASL username/password;
- PreJoin/FallbackRecovery registration-action payload;
- endpoint/display/channel metadata;
- history PRIVMSG body;
- history sender/target;
- FTS-only search term;
- msgid/time metadata where useful.

Write through ordinary Store APIs under encrypted mode.

No real user secret appears in fixtures.

## 3. Closed-file inspection

After clean store shutdown inspect:

- main database file;
- -wal if present;
- -shm if present where meaningful;
- rollback journal if present;
- migration temp/destination files used by the test.

Assert sensitive sentinels and ordinary SQLite plaintext schema signatures expected to be encrypted are not recoverable as raw byte substrings from encrypted database content.

This is evidence of at-rest behavior, not a cryptanalytic proof.

Plaintext control fixture must demonstrate the same scanner can find selected sentinels in an ordinary plaintext store so the negative test is discriminating.

## 4. Functional parity

With the correct key after restart:

- load Networks/secrets/actions;
- replay/query history;
- FTS search;
- CHATHISTORY-related references;
- cursor/read-marker reads;
- retention/compaction;
- config snapshot secret omission

must behave as in plaintext mode.

Wrong key must fail before any result is served.

## 5. Migration and rotation qualification

Exercise:

- plaintext schema-8 -> encrypted key A;
- restart with key A;
- encrypted key A -> encrypted key B;
- key A rejected on rotated destination;
- key B accepted;
- source files retained according to Plan 045;
- parity across both transitions.

Inject failures at selected export/verification points and prove source recoverability.

## 6. Key leakage review

Static/runtime review must find no key value in:

- Debug;
- StoreError;
- tracing/log fixtures;
- config snapshots;
- panic messages;
- migration reports;
- closure output.

Search source for accidental formatting/serialization of StoreKey.

The closure may record key fingerprints only if derived using a one-way test-only digest and there is a concrete need; otherwise record no key-derived value.

## 7. Build/platform matrix

Qualify the selected SQLCipher backend on the repository's supported packaging direction:

- Linux;
- macOS;
- Windows;
- Rust 1.88 floor.

Include Linux ARM/aarch64 build evidence where existing CI/cross tooling supports it because SBC deployment is a primary target.

If a platform is not operationally available, do not report it as passed. A missing platform required by the declared support matrix blocks M008 closure or triggers a narrowed documented support decision.

## 8. Performance sanity

Encryption need not equal plaintext performance, but capture a bounded comparative smoke baseline for:

- store open;
- history append batch;
- representative FTS query.

The goal is to detect accidental catastrophic regressions, not establish a microbenchmark SLA.

## 9. Threat-model documentation

Operator docs must state:

Encryption protects the durable database when the key is not compromised with it.

It does not protect against:

- a live process compromise;
- a key stored alongside the database and stolen at the same time;
- endpoint compromise;
- plaintext IRC conversations;
- metadata exposed over ordinary application operation.

No "end-to-end encryption" language may describe SQLCipher.

## 10. Verification

Run full current and Rust 1.88 verification, plus encrypted-store qualification.

No external network/router is required.

## 11. Acceptance criteria

M008 closes only when:

1. encrypted file/side-file scans do not expose the sentinel corpus;
2. the plaintext positive control proves the scanner can detect it;
3. correct-key restart preserves full store/search semantics;
4. wrong-key open fails closed;
5. plaintext->encrypted and key rotation preserve all durable state;
6. supported platform/MSRV build evidence is complete;
7. no key leak is found;
8. threat-model docs do not overclaim.

## 12. Closure evidence

Create plans/closure/bouncer-core/046-status.md with:

- sentinel/file matrix;
- functional parity matrix;
- migration/rotation evidence;
- dependency/platform/MSRV matrix;
- performance sanity numbers;
- threat-model statement;
- explicit M008 closure and Plan 047 readiness.
