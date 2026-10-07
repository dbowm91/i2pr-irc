# Capability mediation

Upstream negotiation requests a small reviewed set — `message-tags`, `server-time`, `batch`,
`labeled-response`, `echo-message`, `extended-join`, `account-notify`, `away-notify`,
`multi-prefix`, `setname` — and only those the server actually offers.
`UpstreamCapabilities::request_set()` is a pure function of the offer, so a generation's
behavior cannot depend on transient client state. Multiline CAP LS is accumulated before the
request is sent. Configured SASL PLAIN is rejected when unavailable, and SASL errors fail
registration. Credentials and encoded payloads are excluded from Debug and diagnostics.

ISUPPORT tokens are recorded rather than interpreted at registration time. `CASEMAPPING`,
`CHANTYPES`, `PREFIX`, and `CHANMODES` are retained with bounded validation and then drive live
channel classification, membership-prefix parsing, and channel-mode argument consumption; an
invalid value leaves the previous mapping authoritative. A token line that arrives in the same
read as the welcome is retained like any other server line.

`CASEMAPPING` accepts `ascii`, `rfc1459`, and both spellings of the strict variant:
`rfc1459-strict` from the Modern IRC Client Protocol and `strict-rfc1459` from the older
RPL_ISUPPORT draft. Any other value keeps the documented `rfc1459` default, because inventing a
mapping the server did not advertise would merge identities silently.

## Downstream advertisement is computed, not static

`CAP LS`, `CAP REQ`, and the `005` welcome all answer from one cell,
`downstream::DOWNSTREAM_ADVERTISED`, so a capability cannot be advertised by one path and
refused by another. That set is non-empty: it holds `message-tags`, `batch`,
`labeled-response`, `server-time`, `standard-replies`, `cap-notify` and
`draft/no-implicit-names`, plus the implemented `draft/chathistory`, `draft/read-marker`,
`draft/pre-away`, `soju.im/search`, `soju.im/bouncer-networks` and
`soju.im/bouncer-networks-notify` adapters.

Two members are **conditional on the upstream negotiation**, because a bouncer that never
negotiated a capability has nothing to mediate: `echo-message`, and the five member-state
capabilities `extended-join`, `account-notify`, `away-notify`, `multi-prefix` and `setname`.
A server may offer a capability and refuse it, so the acknowledgement is the only statement
about the connection actually in hand.

`CAP REQ` is all-or-nothing: a request naming one unavailable capability is NAKed as a whole,
so a client is never left guessing which half took effect.

A `CAP` line from upstream is consumed by the bouncer and never fanned out. Relaying it would
show a local client the upstream's negotiation under the upstream's own prefix, which the
client reads as the server addressing it, and it discloses the upstream connection's shape to
every attached Operator.

Deferring a capability is a reviewable decision and is recorded as such rather than omitted by
accident — `account-tag`, `chghost`, `invite-notify` and `extended-monitor` are deferred with a
stated reason each. See [member state](member-state.md) for those and for the conditional
advertisement rules, and [capabilities and response routing](ircv3.md) for the wider contract.

A downstream client that issued `CAP LS` or `CAP REQ` before registration is still negotiating
and receives no welcome or state projection until it sends `CAP END`; see
[downstream-session.md](downstream-session.md) for the full transition table. Post-registration
CAP is still answered locally and never reopens negotiation.
