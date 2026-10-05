# i2pr-irc Planning and Agent-Handoff Process

Status: normative planning governance

This process follows CodeGG's planning model: stable canonical direction, subsystem roadmaps, bounded implementation plans, active registry, ADRs for durable decisions, and evidence-based closure.

## 1. Planning horizons

Long-term documents define product identity, invariants, ownership, non-goals, security properties, and sequencing.

Implementation plans define one bounded outcome against a repository baseline.

Implementation may discover evidence requiring a long-term correction but may not silently weaken long-term direction.

## 2. Canonical documents

- plans/000-long-term-specification.md
- plans/001-terminology-and-domain-model.md
- plans/002-long-term-roadmap.md
- this document

Ordinary implementation does not rewrite canonical direction merely to fit current code.

## 3. ADRs

Use an ADR for security/trust boundaries, durable ownership, public API/protocol, persistence consistency, router integration contracts, authentication/authorization, or product non-goals.

Accepted ADRs are historical; supersede rather than rewrite.

## 4. Subsystem roadmaps

A roadmap defines purpose/ownership, work classes, non-goals, current state, target architecture, dependencies, milestones, cross-cutting requirements, verification, risks, completion definition, and status.

It avoids commit-specific mechanical edits.

## 5. Implementation plans

A plan must include objective, readiness, current evidence, invariants, scope, required production changes, ordered work packages, failure/restart/contention semantics, compatibility, tests, verification, docs, acceptance, stop conditions, and closure evidence.

Only dependency-ready work is normally marked ready for handoff.

## 6. Closure

A milestone closes only through plans/closure/<subsystem>/NNN-status.md containing exact implementation commits, requirement-to-evidence matrix, commands actually executed, security/recovery review, findings, and roadmap disposition.

Compilation alone is not closure. Infrastructure alone does not close a user-visible capability.

## 7. Work classes

Invariant: a property that must remain true.

Infrastructure: internal machinery used by capabilities.

Capability: behavior useful to an operator/client/integration consumer.

Polish: ergonomics, diagnostics, performance cleanup, or documentation.

Each milestone has one primary class.

## 8. Dependencies

Hard: implementation cannot correctly start first.

Interface: work may proceed against a stable written contract or test double.

Soft: parallel implementation is possible but integration depends on another milestone.

Operational: code may land but a claim/release depends on external evidence.

## 9. Corrective passes

Partial/failed closure creates a new corrective plan. Correctives reference original evidence, enumerate findings, explain missed verification, and add regression evidence.

## 10. Agent handoff

Before coding, inspect current state, confirm dependencies, preserve invariants and unrelated work, run narrow tests first, and report incomplete scope rather than expanding it.

If a task requires generic host DNS, generic upstream TCP, arbitrary HTTP egress, or private i2pr internals, stop for architecture review.

## 11. Planning review

A plan is ready only when ownership, dependencies, restart/disconnect behavior, anonymity/security effects, resource bounds, tests, and closure criteria are explicit.
