# Plan 063 — M013-A Network Authentication Profiles

Status: proposed, after M012 closure. Research: 011. ADR: 0009.

Implement explicit independent transport and authentication profiles. Default to plain IRC over typed I2P streams. Provide examples for IRC2P using NickServ and ILITA using configured required SASL PLAIN, without TLS. Only claim live compatibility after authorized CAP observations.

Migrate existing settings without silent behavioral changes. Preserve fail-closed required SASL, bounded registration, credential redaction, network isolation and no generic sockets or DNS.

Tests: CAP-less, SASL accepted/refused, reconnect, no auth downgrade, multiple profiles, old database migration. Run full stable and Rust 1.88 verification when available. Close in plans/closure/irc-enhancements/063-status.md with implementation commits and test evidence.
