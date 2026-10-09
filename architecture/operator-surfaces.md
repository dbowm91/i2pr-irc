# Operator diagnostics, configuration snapshots, and registration actions

Three surfaces an Operator uses when something is wrong or when a bouncer has to move to
another machine. They share one property: **none of them can express a secret.**

## Diagnostics

`BouncerServ DIAG` and `DIAG NETWORK <netid>` return the bounded, secret-free state of the
process and of one Network. The shape is defined in `crates/runtime/src/diagnostics.rs` and
nothing else may assemble it.

Two rules govern the whole surface.

**There is no field to render a secret in.** `NetworkDiagnostics` and
`ProcessDiagnostics` have no endpoint, no `Destination`, no SASL value, no registration-action
payload, and no filesystem path in any variant. This is not a redaction pass applied while
rendering; it is the absence of a field, which is stronger than any policy a renderer could
forget to apply. A rendering bug cannot leak what there is nowhere to read from.

**Every list is sampled with an explicit overflow count.** A channel sample that simply
stopped at 64 would be indistinguishable from a Network with six channels, and a reader would
conclude the wrong thing from a report that looks truthful.

### Reading it

Each line is a `NOTICE` tagged `bouncer-diag`, so a client filters on the tag alone and never
has to know which half a line came from:

```
@bouncer-diag :BouncerServ NOTICE bot :state=netid=1 name=lab phase=online generation=3 …
@bouncer-diag :BouncerServ NOTICE bot :counts=visible=12 overflow=0 recorded=419 … dropped=0 …
@bouncer-diag :BouncerServ NOTICE bot :lists=acknowledged=… sample=#one,#two reasons=none …
```

* `state` — preferred/current nickname, generated fallback and reclaim status, away class,
  session counts, reconnect state, and last disposition. Reclaim writes/refusals and the
  cooldown duration are counters, not protocol payloads.
* `counts` — every scalar counter. **Always before the lists.** A Network with a hundred
  channels pushes the counters off a shared line, and the counters are what an Operator
  opened diagnostics to find.
* `lists` — channel samples, the acknowledged-capability fingerprint, rejection reasons.

A line that had to be cut ends with `truncated=1`. That is inside the line rather than a
separate frame because a separate frame is exactly the thing that does not fit.

### Bounds

`MAX_DIAGNOSTIC_LINES` is `CONTROL_QUEUE_CAPACITY`, and that is the number that actually
decides it. A session's control queue holds eight frames and is written with `try_send`, so a
longer reply loses its tail silently — `ControlSurface::write` discards the refusal. A reply
that arrives half-delivered is worse than a shorter complete one, because an Operator cannot
tell which half is missing. When a whole-process report has to leave Networks out, the count
is stated on the process line.

### Away is a class, never a message

`away=manual`, `away=automatic`, `away=present`, `away=unattributed`. The class is published
by the owner from the same `away_decision_with_origin` call that decided the away, so the two
cannot disagree. An away whose origin the generation cannot account for is `unattributed`
rather than guessed at — a bouncer that is away for a reason it cannot explain is a finding,
and labelling it `automatic` would hide exactly that. The away *text* is never exported.

### Stability

These names and meanings are stable: Operator tooling reads them. Adding a field is a
compatible change; renaming or re-meaning one is not, and needs its own plan.

## Configuration snapshots

A versioned, typed local format — not IRC draft syntax, and not a configuration language.
`BouncerServ CONFIG EXPORT` writes it out one `NOTICE` per line; `CONFIG PLAN` proves the
bouncer can read back what it wrote.

```
#i2pr-bouncer-config
version 3
network netid=1 name=lab host=…b32.i2p nick=bot username=user realname=bouncer auto_away=off keep_nick=off actions=1 action_phases=0,1,0
channel target=#one position=0 detached=off relay_detached=none reattach_on=off detach_after_secs=off
```

**The endpoint is exported here even though `BOUNCER NET` withholds it.** That is not an
inconsistency. `BOUNCER NET` renders into an IRC-visible frame on a downstream connection,
where every session that negotiated the capability can read it. This renders into an export
the Operator asked for and holds to store. An I2P destination is the identity of the Network
being moved; an export without it would not be an export.

**No credential, and no way to add one.** The record type has no field for one, and an
attribute naming a secret is refused *by name* — `sasl user=bob` pasted onto a record line is
refused as `RefusedSecret`, not as a malformed attribute, because the Operator needs to hear
that their secret was refused rather than being sent looking for a syntax problem.

**Unknown versions are refused, never guessed.** A format that guessed would import a Network
with fields missing and the Operator would find out from a failed connection.

Version 3 adds optional-per-version channel activity attributes to the versioned snapshot.
Version 1 and 2 channel rows remain accepted with disabled activity defaults. New exports
include `relay_detached`, `reattach_on`, and `detach_after_secs` so a policy round-trip does
not silently reset them.

## Detached channel activity controls

`CHANNEL STATUS <netid> <channel>` includes the durable detached state and all three
activity values. Set them together with
`CHANNEL ACTIVITY <netid> <channel> relay=<none|mentions|all> reattach=<off|message|mention> detach_after=<off|1..86400>`.
The complete policy is validated before the owner commits it, and an invalid or duplicate
attribute is refused. A timed detach changes only local presentation; it never sends an
upstream PART. Automatic reattach uses the owner’s observed channel state and shares the
same presentation policy across attached sessions.

## Local watch rules

`WATCH ADD <netid> <channel|query> <target|*> <keyword|sender> <term>` installs a durable,
Network-scoped literal rule, with target-specific rules tied to a stable `BufferId`.
`WATCH LIST <netid>`, `WATCH DELETE <netid> <id>`, and
`WATCH CLEAR <netid>` inspect or change the set. Each Network is limited to 128 rules and
each term to 128 bytes; there is no regex or script interpreter. Matching happens once on
parsed inbound PRIVMSG/NOTICE events. OTR is skipped. A hit is sent only to attached local
sessions as a NOTICE containing a process-unique sequence and rule id; it omits message text
and destination and is not replayed after reconnect. Matching is coalesced per rule, output
is best-effort through each bounded client queue, and `diag` reports emitted and dropped
notification counts. No external notification path exists.

### Import boundary

Import is **plan-then-apply, per Network**, and is deliberately not transactional across
Networks. The durable store is one bounded worker behind a request queue; a multi-Network
transaction would have to stay open across the owner restarts that each write causes, which is
a second writer on the one durable surface. `ApplyOutcome` reports how far the apply got
rather than implying a rollback.

What *is* guaranteed, and what actually matters: **nothing is written until the entire
snapshot has parsed and validated.** That is a property of the type — the only way to hold a
`ConfigSnapshot` is to have parsed one, and the controller takes the parsed value.

A snapshot whose `netid=N` names a *different* Network than the store's `netid=N` is a
`Conflict` and stops the plan before any write. Such a snapshot came from another bouncer;
importing it under a new identity would produce a configuration that looks restored and is not.

An import never writes a credential, because the format has no field to carry one. It also
never *erases* one: `plan` merges the stored credential into an update, because writing
`sasl: None` through would turn a successful restore into silent data loss. An export is
therefore safe to paste anywhere and safe to apply anywhere, in both directions.

## Registration actions

A mature bouncer re-sends a few things after every successful registration: the modes it wants
on its own nick, an identify line for a services bot. This is the *only* way this build can
emit an action.

```
BouncerServ :ACTION SET 1 mode=+B message=NickServ text=IDENTIFY hunter2
BouncerServ :ACTION SET 1 phase=pre-join message=NickServ text=IDENTIFY hunter2
BouncerServ :ACTION SET 1 phase=fallback-recovery message=NickServ text=RECOVER bot
```

Actions have `pre-join`, `post-join`, or `fallback-recovery` phases. The legacy form
replaces the full action list with post-join actions. Supplying `phase=` replaces only that
phase; an empty phase-specific set clears that phase and preserves the others. Pre-join actions
run before desired JOINs, post-join actions run after them, and fallback recovery runs only
after registration under a generated fallback nick when `keep_nick` is enabled. Existing
actions migrate to post-join.

### It is an allowlist of shapes, not a denylist of commands

`RegistrationAction` is constructed only by `mode` and `message`, and neither accepts a
command name. There is no argument through which `JOIN` or `AUTHENTICATE` could reach a
stored action. The forbidden list exists as a test oracle and as documentation of what was
considered — enforcement is the absence of variants, not the presence of refusals.

Two further shape restrictions, each closing a specific route an allowlist on the command name
alone would leave open:

* a mode string is `sign letter` pairs and nothing else, so a `MODE` action cannot carry a
  trailing parameter;
* a message target must be a nick ending in `Serv`, so a stored action cannot be pointed at a
  person or a channel and replayed to them on every reconnect.

### Replay semantics

Each successful registration generation runs the configured phases in order: pre-join
actions, desired JOINs, post-join actions, then fallback-recovery actions only if registration
used a generated fallback nick and `keep_nick` is enabled. A generation that dies part way
through starts its applicable setup phases over on the next one. These are operator-configured
setup actions, not queued user traffic; a reconnect never resumes at an ambiguous action
index.

### Secrets

An action's text is expected to be a service password, so the payload is secret throughout:
`Debug` renders `[redacted]` at both the runtime and the storage layer, `ACTION STATUS`
reports a count and nothing else, and diagnostics reports a count and nothing else. The value
is a `StoredSecret` from the operator's line to the store and back out, so no intermediate
owned `String` ever holds it.

`text=` takes the **remainder of the line**, not the next whitespace-separated word.
`IDENTIFY hunter2` is one message containing a space, and a parser that took a single word
would store `IDENTIFY` — failing at the service while looking exactly like a working
configuration.

## Storage

Schema 7 adds `registration_actions`; schema 8 adds its constrained phase, defaulting existing
rows to post-join. Both migrations preserve action order. `position` is part of the primary key because replay order is part of the
meaning, and `kind` is `CHECK`-constrained to the two entries the allowlist defines, so a row
written by a future build is refused by SQLite rather than read back as an unknown kind that
something downstream would have to guess at.

## See also

- [bouncer networks and administration](bouncer-networks.md) — the control plane these
  commands live in.
- [control session](control-session.md) — how a session reaches them at all.
- [presence and preferred-nick policy](presence-and-nick.md) — the away classes diagnostics
  reports.
- [storage](storage.md) — the schema and its migrations.
