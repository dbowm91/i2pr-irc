# i2pr-irc Terminology and Domain Model

Status: canonical terminology authority

## Operator

The one logical local owner of a bouncer process in the initial product generation. It is not an IRC account, OS username, router identity, or I2P Destination.

## Network

A durable configuration for one upstream IRC service reached through I2P. It has stable identity, display name, typed I2P endpoint, IRC identity/auth policy, desired channels, reconnect policy, and feature overrides. It is not one socket.

## NetworkSupervisor

The exclusive live owner of one Network's mutable upstream state across connection generations.

## ConnectionGeneration

A monotonically changing identity for one upstream transport generation. Events from stale generations cannot mutate a replacement generation.

## I2pEndpoint

A typed upstream locator resolvable only by an I2P provider. It may represent a valid I2P name, base32 address, or canonical Destination form supported by the provider. It is never interpreted through host DNS and is not a generic URL.

## I2pStreamProvider

The router-neutral data-plane interface that obtains an ordered reliable byte stream to an I2pEndpoint. SAM and future i2pr app integration implement this boundary. It exposes neither generic TCP nor router administration.

## RouterControlProvider

An optional separate control-plane interface for narrowly scoped router operations. Proposal 170 integration, if any, belongs here and is not required for basic IRC.

## DownstreamSession

One attached local IRC client.

## ClientId

A durable local identifier distinguishing client lineages for history/read-state purposes. It is not sent upstream unless an explicit reviewed extension requires it.

## Buffer

A durable conversation target, normally a channel or direct-message peer, always scoped by NetworkId.

## HistoryEvent

One durable event for a Buffer, with local order plus source metadata such as server-time/msgid.

## Cursor

A durable per-client/global marker into HistoryEvent order. IRCv3 read-marker is a projection, not storage identity.

## DesiredState

Durable operator intent such as configured networks and channels.

## ObservedState

Live state learned from the current upstream generation. Restart/reconnect reconciles DesiredState with a fresh ObservedState.

## UpstreamIntent

A typed operation submitted to a NetworkSupervisor. It records replay safety. User chat defaults to non-replayable across ambiguous failure.

## ResponseRoute

Metadata used to route request-specific replies back to the initiating downstream client. Labeled-response is preferred where available.

## AnonymityPolicy

The normative policy for CTCP, DCC, client tags, local metadata, and alternate egress. It is part of the product, not an optional profile.

## LocalAcceptor

The downstream-only interface supplying local client streams. Standalone implementations use local IPC/loopback; future i2pr operation may receive accepted streams through the app capability channel.

## SAM adapter

The standalone router adapter mapping I2pStreamProvider to SAM STREAM. It is restricted to configured local router endpoints by default.

## i2pr managed-app adapter

The future adapter mapping I2pStreamProvider and LocalAcceptor to public i2pr managed-app capabilities. It must not import private router crates.

## Capability mediation

Independent upstream negotiation and downstream advertisement based on semantics the bouncer itself can guarantee, rather than transparent CAP forwarding.
