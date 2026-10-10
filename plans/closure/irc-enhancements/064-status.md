# Plan 064 — M013-B IRCv3 Feature Disposition

Status: closed; CHGHOST, event playback, and message redaction deferred
Implementation commit: none; no feature implementation was accepted
Closure commit: `ac4e3e4` — `docs(plans): close Plan 064 and start TLS preflight`
Date: 2026-10-10

## Requirement-to-evidence matrix

| Requirement | Evidence and result |
|---|---|
| Review current primary IRCv3 specifications | Reviewed the current [chghost](https://ircv3.net/specs/extensions/chghost), [chathistory/event-playback](https://ircv3.net/specs/extensions/chathistory), and [message-redaction](https://ircv3.net/specs/extensions/message-redaction) pages on 2026-10-10. Chghost is standardized; event-playback and message-redaction are explicitly work-in-progress and their pages warn against production implementation. |
| Preserve truthful CHGHOST semantics for modern and legacy clients | DEFER. The standardized specification calls for QUIT/JOIN/MODE fallback for clients without the capability. Existing `architecture/member-state.md` explains why synthesizing membership would violate this bouncer's projection invariant. `crates/runtime/tests/m005g_member_state.rs` checks deferred capabilities are not offered. |
| Avoid fabricated historical state | DEFER event-playback. Existing storage/replay persists PRIVMSG/NOTICE only; `architecture/chathistory.md` and `crates/runtime/tests/chathistory.rs` establish that no JOIN/PART/NICK event can be replayed. No old rows were relabeled and no CAP was added. |
| Preserve message provenance and avoid false erasure claims | DEFER message-redaction. The reviewed spec is draft and describes cosmetic deletion without operational security guarantees. The current model has no authorized redaction provenance/tombstones connecting live fanout, retained history, search and backup semantics. No REDACT command/CAP or erasure claim was added. |
| Boundaries and verification | No code or schema changed, so no tests were added or run for this plan. The decision retains current capability withholding and ordinary baseline behavior. Plan 063 stable and Rust 1.88 full verification had passed immediately before this review. No network authority changed. |

## Security, recovery, and limits

The three deferrals avoid false membership, fabricated history, and misleading deletion expectations. Capability negotiation remains unchanged. Reconsider the two drafts after specification stabilization; CHGHOST requires a complete state reconciliation design that preserves the projection invariant. No modern/legacy client matrix was run because no implementation was accepted, and no claim of interoperability beyond existing tests is made.

## Registry and roadmap disposition

Plan 064 is closed with a DEFER disposition for all three features. Plan 065 is unblocked for its bounded research preflight only; the plan itself continues to prohibit implementation until authenticated TLS-inside-I2P and client-certificate qualification is feasible. Plan 066 remains proposed pending Plan 065's shipped-or-deferred result. Plan 054 and Plan 062's separate live/external evidence conditions are unchanged.
