# Bouncer networks and local administration

How a client that has never heard of this bouncer discovers what Networks exist, picks one,
and changes them.

## The adapter owns no authority

`bouncer_networks.rs` parses and renders. `bouncerserv.rs` parses. Neither performs I/O,
holds state, or can reach a store, a supervisor, or a socket. Everything they produce is a
typed value; `control_session.rs` decides what to do with it.

That split is the reason attribute handling can be reviewed once. The disposition of
`port`, `tls`, `pass`, `state`, and `error` is decided in one table, and the two commands
that accept attributes — `ADDNETWORK` and `CHANGENETWORK` — cannot disagree about the same
name because there is only one table to disagree with.

## Netids are assigned, canonical, and never chosen by a client

A netid is the decimal `NetworkId` with no sign, no padding, and no whitespace, so it
round-trips: a client that writes one back is asserting an identity, and `+7`, `007`, and
` 7` would make "which Network did you mean" ambiguous.

Allocation happens in the controller and nowhere else, through `ControlRequest::CreateNext`.
A caller-chosen identity turns `ADDNETWORK` into a race — two clients creating a Network
pick the same free id, and the loser is refused for a reason that has nothing to do with
what it asked for.

The identity survives a restart because it is the durable record's identity, not a counter:
startup restore reports the same netid the record was stored with, and a client's saved
configuration keeps pointing at the same thing.

## `host` is a typed I2P endpoint or it is nothing

There is no URL parsing, no port extraction, no TLS material, and no path to a resolver in
this surface. An attribute that looks like `irc.example.org:6697` or `https://irc.example.org`
is refused by `I2pEndpoint::parse` on shape alone, before anything could treat it as a
destination. A hostname-form `.i2p` name is accepted, and it is a *name inside I2P* — the
same `I2pEndpoint` type the store validates — not a DNS name this bouncer resolves.

A rendered listing never carries an endpoint, a nickname, or a credential. A Network list
is shown to every session that negotiated the capability, and an I2P destination is an
identity the Operator did not ask to publish.

### One real limitation

A canonical raw `Destination` is 516 characters, and an IRC line is at most 512. No
`BOUNCER ADDNETWORK host=…` line can carry one, so this control surface cannot configure
that form. `MAX_BOUNCER_LINE_BYTES` is therefore the wire's own constant rather than a
larger number that would describe a line the decoder has already refused to deliver. The
`.b32.i2p` and `.i2p` forms both fit comfortably, and those are what a draft client
actually sends.

## Nothing is dropped silently

An attribute this build cannot honour is refused *by name*. `port`, `tls`, and `pass` are
recognised — the draft names them — and refused with "not supported", which is different
from being refused as unknown. `state` and `error` are refused as read-only, because
accepting `state=connected` would leave a client believing it had asked the bouncer to
connect a Network that is in fact disconnected.

Every refusal carries a reason. `FAIL BOUNCER <subcommand> :<reason>` is the draft's
required form, and this build uses it *without* advertising `standard-replies`, because it
does not implement that capability's full semantics. Plan 025 promotes the advertisement
once it does.

## Selection is a registration-time decision

`BOUNCER BIND <netid>` is the one control verb accepted during registration, and it is
accepted only while registration is open. The bindable set is read once from the
controller's snapshot at accept time, so a `FAIL BOUNCER BIND` reaches the client *before*
registration completes rather than only after it. A registered session that sends `BIND` is
refused by name and stays unbound for the rest of its life.

A refusal leaves the session usable as a control connection. Killing the registration would
be a harsher answer than the draft asks for, and a client that miscounted its netid should
be able to correct it rather than reconnect.

## Notifications reconcile, they do not replay

A client that negotiated `soju.im/bouncer-networks-notify` gets a bounded initial batch and
then deltas derived from consecutive `ControlSnapshot`s. There is no event log: a session
that fell behind is told what is true now, not everything that happened. That is what keeps
a slow client's memory bounded however long it stalls, and it is why a client that never
reads costs the process one snapshot and one bounded queue rather than a backlog.

Deltas are compared as rendered text, which is exact — two entries that render identically
are indistinguishable to the client, so announcing one of them would be noise it cannot act
on.

## Channel presentation is the owner's, not the controller's

A channel policy change commits through the owner's durable write and only then changes
what sessions see, so a refusal means no client was told anything. The administrative path
is the same code the client's own `PART :detach` takes, with no requesting session — a
change that arrived through the control surface reports through its reply instead of a
NOTICE.

The controller re-reads the record after such a change. Without that, `channel status`
would answer a question about the world as it was before the command it had just
acknowledged.
