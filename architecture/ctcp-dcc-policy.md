# Anonymity protocol mediation

The bouncer mediates CTCP and DCC in both directions. It never forwards a frame it has not
classified, and it never hands a client a probe that would prompt the client to describe
itself.

## Why mediation exists

Two directions are dangerous, for different reasons.

A **downstream-to-upstream** CTCP *reply* is the fingerprint risk. A local client asked by
another user for its version, hostname, or client software answers automatically. If that
answer reached the upstream server it would become this Operator's fingerprint, describing
the local machine rather than the bouncer. So every metadata reply is blocked.

An **upstream-to-downstream** CTCP probe is the disclosure risk. A DCC request reaching a
client is an offer to open a direct connection, and a `VERSION`/`CLIENTINFO` probe reaching
a client is a prompt to auto-reveal its software. Both leave this process, so neither is
allowed to arrive.

## The policies

`crate::ctcp` owns classification; `crate::ircv3` owns tag mediation. Both are deny by
default: anything not explicitly classified by the reviewed policy is refused, so a CTCP
command word added by a future specification is blocked until someone has read it.

Outbound (client to upstream) allows only ordinary text, `ACTION`, and `PING` in both
query and reply form. Everything else is blocked, including all metadata replies.

Inbound (upstream to client) delivers ordinary text and `ACTION` as ordinary chat, and
answers an upstream `PING` itself as a fixed `NOTICE` to the prober. The token is echoed
from the probe and nothing about this host is added. The probe is never relayed to a
client, because a client that answers it is a client that has just leaked itself.

`DCC` is recognised only in order to be blocked. No DCC parameter is ever parsed into a
usable value, and no code path turns one into a connection, address, or offer.

`ACTION` in a `NOTICE` is treated as ordinary text rather than as an action, because
clients do emit that shape and an action invites a reply.

A missing closing CTCP delimiter is tolerated, as clients commonly omit it; refusing would
drop an ordinary-looking action.

## Client tags

Client-only tags are denied wholesale. The bouncer advertises `CLIENTTAGDENY=*` and that
is literally true: every client-supplied tag is stripped before forwarding, and any tag the
bouncer has never seen is stripped too. Advertising a narrower allowlist would be a promise
it does not keep, and a client calibrates its behaviour from that token.

The single exception is `label`, which is this bouncer's own correlation mechanism. It is
consumed by the response router, replaced with an opaque generation-local token, and
restored only to the client that sent it. It never reaches the server.

Tag mediation runs on every forwarded frame, not only chat, so a client cannot smuggle a
forged `msgid` onto a `MODE` or a `NICK`. It does not depend on what the client negotiated:
what a client asked for says nothing about whether the tags it sent may be trusted.

## What this does not cover

Blocking a CTCP does not make a bouncer anonymous. A user who deliberately types their
hostname, or runs a client configured to answer probes, is outside this boundary. The
claim defended here is narrower and testable: this process contributes nothing to the
upstream fingerprint, and no local client is prompted to contribute anything either.

See `security-anonymity.md` for the wider boundary and `ircv3.md` for the tag protocol
detail.