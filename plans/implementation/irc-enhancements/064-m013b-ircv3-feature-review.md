# Plan 064 — M013-B IRCv3 Compatibility Review

Status: proposed, after Plan 063 closure. Research 011, ADR-0008 and ADR-0009 apply.

Review deferred CHGHOST, event playback, and message redaction against current IRCv3 specifications. Make feature-by-feature ACCEPT or DEFER decisions before implementation. Implement only truthful per-client negotiation and mediation. CHGHOST must not invent upstream membership, and event playback requires correctly recorded event classes rather than pretending PRIVMSG/NOTICE represent everything. Redaction is a display operation and cannot promise secure destruction of old copies. Index cleanup must follow applicable retention policy.

Tests: mixed modern/legacy clients, state updates, restart and history replay, CAP changes, bounded fanout, bad upstream input, no-history/ephemeral buffers, and search index changes. Preserve one NetworkOwner and I2P-only upstream. Run full stable and Rust 1.88 verification if available. Commit accepted/deferred rationale and closure evidence with commit IDs, actual tests and findings to plans/closure/irc-enhancements/064-status.md.
