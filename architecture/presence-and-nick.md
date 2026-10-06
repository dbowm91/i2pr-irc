# Presence and preferred-nick policy

Two questions recur in every mature bouncer: *is the Operator here?* and *is this the nick I wanted?* Both are easy to answer badly. Answering presence with a socket count makes a background history-sync client look like a human, and answering a nickname collision with a random suffix derived from the machine turns a bouncer into a host beacon.

This document describes how `i2pr-irc-runtime` answers both without consulting the host.

## Presence is a policy, not an observation

The durable record holds exactly two presence switches, both migrated **disabled**:

| Field | Meaning |
|---|---|
| `networks.auto_away` | go away automatically when no *active* session remains |
| `networks.keep_nick` | try to return to the configured nick after a collision |

Both default to off so an upgraded binary emits no new upstream `AWAY` or `NICK` traffic merely because it was replaced. Live away state and the live current nick are **not** stored: they are observations of one connection, and persisting them would let a restart resume a claim about the world that has not been checked.

The automatic away text is a bouncer-owned constant (`presence::AUTO_AWAY_TEXT`), not an Operator-configurable string. An away message is an IRC-visible field, and validating an arbitrary configured string on every path that can reach one — including an upstream re-application after reconnect — is a standing invitation to get a control character into a frame. The bouncer picks the wording; the Operator picks whether it happens at all.

## Sessions are classified, not counted

`SessionPresence` is `Active` or `Passive`. It is a fact about a *session*, set when the session attaches and changed when that session says something, never a derived count of attached sockets.

`draft/pre-away` is what makes `Passive` meaningful. A background client negotiates the cap and may declare itself passive:

```text
CAP REQ :draft/pre-away
PASSIVE
CAP END
```

That declaration is the entire reason the draft exists. A client that had to announce itself passive *after* connecting would make the bouncer flap away and back for every such client — declare, attach, declare again — and a bouncer whose away state flaps is a bouncer the network learns to ignore.

`SessionReader::registration_intent` emits the declaration before `RequestProjection` and requires the decoded buffer to drain before completing registration, which is what makes a pre-registration `PASSIVE` mean anything. An unnegotiated `PASSIVE` or `ACTIVE` is refused, not silently absorbed: a client must not be able to silence the Operator's presence with a command whose consequences it did not ask for.

## Precedence, and what actually emits

```text
manual away set     -> away, with the Operator's own words
manual away cleared -> back, if any session is active
otherwise           -> away iff no active session exists
```

Only *transitions* write `AWAY` upstream. Repeated equivalent events are idempotent, because "exactly once" is the property that keeps a Network from being shaped by how many clients happen to be attached.

Two consequences are worth stating separately:

- **A manual away survives unrelated traffic.** It is owner-scoped, so it outlives a connection and is re-applied upstream after the next successful registration. A client attaching or detaching cannot clear it.
- **Observed upstream away is generation-scoped.** It is dropped on reconnect and re-derived, because what the server believed about the previous connection is not a fact about this one.

Presence is evaluated at generation start (after waiting attachments are applied) and after every session event. It is deliberately **not** evaluated on attach: a session that declared itself passive during registration has not yet been heard from, and evaluating on attach would count it as an Operator and then take that away.

## Collisions are answered, not waited out

A `433` during registration is handled inside the registration loop, so the bouncer answers in the same window as the refusal. Sitting until the generic registration ceiling would turn a two-second collision into a thirty-second stall that is indistinguishable from a dead network.

The fallback sequence is a pure function of the configured nick and the server's advertised `NICKLEN`:

```text
bot -> bot_1 -> bot_2 -> bot_3
```

truncated to the advertised length, clamped to `[1, 64]`, capped at `MAX_FALLBACK_NICK_ATTEMPTS`. There is no random component and no host input of any kind — no hostname, username, OS or router version, local path, process id, or machine identifier. `NickFallback::prime()` is required: registration already offered the preferred nick, so the first refusal must be answered with a *different* name rather than the same one twice.

Exhaustion is a typed terminal failure (`RuntimeError::NickExhausted`). It marks the Network terminal rather than retrying, because a sequence already refused once per candidate will produce identical upstream traffic on every retry while consuming a process-wide connect permit each time. Configuration or a reconcile is the only thing that can change the answer.

## Reclaim: evidence, on a clock that clients cannot move

When the current nick differs from the configured one and `keep_nick` is on, the generation opens a reclaim attempt and chooses one mechanism, once:

- `MONITOR + <preferred>` when ISUPPORT advertises a usable `MONITOR` limit, read from `MONITOR=<limit>` rather than from CAP so the upstream capability fingerprint stays client-independent;
- otherwise a bounded `ISON <preferred>` probe, which is the standard query every server understands.

`MONITOR=0` means the feature is advertised and *disabled*, so it falls back to probing. Waiting for notifications a disabled feature never sends would wait forever. A limit above `MAX_MONITOR_TARGETS` also falls back: the bouncer will not watch more nicks than its own ceiling allows, and the bouncer only ever watches one.

The clock is a generation-owned `tokio::time::interval` and nothing else moves it. A client attaching does not poll upstream faster — if it did, the bouncer's upstream behaviour would depend on which local sessions happen to exist.

Evidence moves the *timing* of a write, never its permission. Accepted evidence wakes the reclaim select arm through a generation-local `Notify`, so a server that has just told the bouncer the preferred nick came free is not made to wait out the interval:

- `730` (RPL_MONITOROFFLINE) names the nick that went **offline**, so the preferred one being *named* is the evidence;
- `303` (RPL_ISON) lists nicks that are **online**, so the preferred one being *absent* is the evidence.

Reading both as an online list would make a nick coming free look like a reason to keep waiting for it. Every other line — including a `731` reporting the preferred nick on-line — is recorded as nothing rather than as negative evidence that would suppress a future write.

A `NICK <preferred>` is a request. Only the server's own frame confirms it, so the bouncer never treats its own write as success.

Reclaim state is a plain local in `run_generation` and is dropped when the generation is replaced. A probe scheduled by a connection that has since died cannot act on the connection that replaced it, and `MAX_RECLAIM_WRITES_PER_GENERATION` bounds how often one generation may claim at all.

## Clients are projected under the nick actually held

A downstream session is refused unless its claimed nick fold-matches the nick this Network currently holds. After a collision that means a client registering as `bot` is refused and told why on its own socket; the bouncer does not silently project a client under an identity it does not hold. A silent close would be indistinguishable from a network fault, and a client that reconnected on the same stale configuration would have no way to tell that the *configuration* was what changed.