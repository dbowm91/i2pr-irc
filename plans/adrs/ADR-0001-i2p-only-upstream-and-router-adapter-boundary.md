# ADR-0001: I2P-only upstream and router-adapter boundary

Status: accepted

Date: 2026-10-05

Decision owners: project maintainers

Related specification sections:

- plans/000-long-term-specification.md sections 1, 3, 4, 9, and 11
- plans/001-terminology-and-domain-model.md

Affected roadmaps:

- plans/subsystems/bouncer-core-roadmap.md
- plans/subsystems/i2p-router-integration-roadmap.md

## Context

The project could be a general IRC bouncer with an optional I2P profile or an I2P-only application.

Dual transport would require generic DNS/IP handling, public/private address policy, TLS/PKI, proxy configuration, clearnet brokering, and defenses against accidental fallback from I2P configuration to direct Internet access.

The bundled i2pr use case benefits more from a narrow security proof than from clearnet feature breadth.

## Decision drivers

- make accidental clearnet escape structurally difficult;
- reduce anonymity-sensitive policy and code;
- keep router integration replaceable;
- permit portable SAM operation;
- prevent IRC core from depending on one router implementation;
- support future i2pr app integration without private crate imports.

## Considered options

### Option A — General bouncer plus I2P profile

Broader usefulness, but generic egress remains permanent production authority and I2P safety becomes configuration-dependent.

Rejected.

### Option B — I2P-only with SAM hard-coded into core

Narrow network scope, but IRC state becomes coupled to SAM syntax/lifecycle and future native i2pr integration becomes awkward.

Rejected.

### Option C — I2P-only with a router-neutral stream provider

Narrow authority, portable SAM adapter, deterministic tests, and a clean future i2pr adapter.

Selected.

## Decision

The bouncer core accepts only typed I2pEndpoint values and obtains upstream streams only through I2pStreamProvider.

The core exposes no generic DNS/IP/TCP connector.

Standalone SAM implements I2pStreamProvider and may connect only to explicitly configured local router endpoints under the initial contract.

Future i2pr integration implements the same interface through public managed-app capabilities.

Proposal 170 is not part of the upstream data plane. Future use belongs to a separate optional RouterControlProvider and must be least-privilege.

Downstream local client acceptance is a separate LocalAcceptor capability and does not authorize upstream clearnet networking.

## Consequences

Positive:

- normal configuration cannot switch upstream IRC to clearnet;
- router adapters remain replaceable;
- core state tests can use fake providers;
- i2pr integration can avoid direct socket authority.

Negative:

- the project intentionally cannot serve ordinary clearnet IRC;
- generic IRC client libraries built around host/port connectors may be unsuitable;
- router claims require integration fixtures.

Deferred:

- remote downstream clients over an I2P destination;
- multi-user hosting.

## Security and reliability implications

Static guards and dependency review must detect host resolver/generic upstream connector code outside explicitly approved local-listener/SAM boundaries.

Provider failure is represented independently from IRC failure.

A provider returns only a reliable byte stream and cannot grant general router administration.

## Verification

Milestone 001 must prove core crates contain no generic host-resolution/connect path, I2pEndpoint has no clearnet variant, fake providers drive core tests, and static network-boundary guards include positive controls.

Later router milestones require cross-router evidence before portability claims.

## Supersession

None.
