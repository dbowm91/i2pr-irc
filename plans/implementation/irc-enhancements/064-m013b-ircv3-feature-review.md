# Plan 064 — M013-B IRCv3 CHGHOST, Event Playback and Redaction Disposition

Status: proposed; depends on Plan 063 closure and current spec-version review.
Date: 2026-10-09
Primary class: protocol compatibility invariant + conditional capability.
Authority: Research 011, ADR-0008/0009, `architecture/member-state.md`, `architecture/chathistory.md`.

## Objective and evidence
Close the differences from soju/ZNC that help legacy and modern IRC clients without overstating support for a draft. M005/M006 already mediate many member-state capabilities but CHGHOST was withheld because old-client fallback can synthesize misleading membership. Existing history persists only PRIVMSG/NOTICE and cannot truthfully promise event playback. Redaction must not be conflated with forensic erase.

## Prerequisite research and feature decisions
Freeze current relevant IRCv3 draft versions and primary examples; test behavior of modern/legacy clients; record ACCEPT, DEFER or REJECT separately for CHGHOST, event playback and redaction before changing advertised CAP. Maintain upstream capability requests as fixed per-generation policy independent of attached downstream clients. No copying external implementation sources.

## Ordered implementation (only accepted semantics)
1. CHGHOST: reconcile old/new hostmask and observed NAMES/WHO state, include case-fold/duplicate membership and multiple local client capabilities. Assess whether legacy clients can be kept consistent without false QUIT/JOIN/MODE. If not, continue to withhold and document accurately.
2. Event playback: design explicit typed history event categories with durable ordering, visibility and cursor semantics. Do not relabel previous PRIVMSG/NOTICE-only rows into JOIN/PART/NICK history. No-history and ephemeral modes govern such events, and replay cannot claim a fresh generation's state from old observations.
3. Redaction: assess draft stability, msgid provenance and authorized server direction; if implemented, reconcile live fanout, Store rows, FTS terms and query outputs consistently. Do not claim erased SSD/WAL/backup bytes or retroactively retract delivered content from other clients.
4. Add per-session CAP advertisement/NAK, CAP DEL and conditional rewrite/withhold behavior only for fully supported semantics, protecting local client history identity and upstream CAP fingerprint.
5. Add explicit migration/rollback and bounded query/fanout/synthetic-event budgets for every feature that introduces durable state. On unsupported or unknown variants reject cleanly without state divergence.

## Failure and restart contract
An unauthenticated/invalid upstream feature event cannot be used to fabricate channel membership or erase unrelated history. On interrupted migration, wrong version, partial BATCH, stale generation or Store pressure, fail closed on the extension but keep ordinary IRC baseline correct. No new network authority, generic DNS/TCP, HTTP, DCC or plugin host.

## Verification, acceptance and closure
Run mixed legacy+IRCv3 client transcripts under CHGHOST state changes, JOIN/NAMES reconciliation, capability withdrawal, event search/replay after restart, redaction across FTS and multi-client history, retention/no-history/OTR, malformed tags, burst pressure and simulated router stalls. Run focused tests, full stable and Rust 1.88 checks if available, static no-egress controls. Each extension is accepted only when advertised semantics are completely evidenced; otherwise its deferral is a successful research outcome, not an implementation success claim.

Stop for privacy leakage, fabricated state, unbounded work or false CAP advertisement. Close through `plans/closure/irc-enhancements/064-status.md` with accepted/deferred matrix, SHAs, tests executed, failure/recovery assessment, open risks and registry disposition.
