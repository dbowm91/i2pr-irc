# Bouncer Core M006-B / Plan 037 — Account-Tag and Invite-Notify Mediation

Status: closed

Hard dependency:

- plans/closure/bouncer-core/036-status.md

Research authority:

- plans/research/008-m006-m007-irc-interoperability-and-identity-resilience.md

Primary class: IRCv3 capability + invariant

## 1. Objective

Promote the two deferred IRCv3 capabilities that fit the existing observed-state/projection model without fabricating state:

- account-tag;
- invite-notify.

Keep chghost and extended-monitor explicitly deferred with updated rationale.

## 2. account-tag policy

Request account-tag upstream only when offered.

Advertise account-tag downstream only when the current upstream generation acknowledged it.

For an incoming upstream message:

- if it carries an account tag and the downstream session negotiated account-tag, preserve it subject to existing message-tag bounds;
- if the session did not negotiate account-tag, remove only the account tag while preserving other tags appropriate for that session;
- if the upstream message contains no account tag, never synthesize one from cached MemberEntry account state.

The bouncer therefore forwards server-authenticated account metadata but never invents it.

This applies to all relevant upstream user-originated commands, not only PRIVMSG/NOTICE.

## 3. Tag-surface integration

The current fanout order is:

1. member mediation;
2. tag-form selection;
3. per-session fanout.

Account-tag filtering belongs in the per-session tag surface, after any message rewrite whose prefix/command is still the same observed event.

Required combinations:

- no message-tags => no tags;
- message-tags but no account-tag => account removed, other permitted tags remain;
- account-tag negotiated => account preserved;
- server-time without account-tag => time preserved according to existing rule;
- account-tag without generic message-tags still works according to IRCv3 capability dependency semantics.

Do not let client-originated unprefixed account tags pass upstream. Existing client tag policy remains authoritative.

## 4. invite-notify policy

Request invite-notify upstream only when offered.

Advertise downstream only when upstream acknowledged it.

Classify incoming INVITE:

### Self-target invite

If target is the bouncer/current Network nick, it is ordinary IRC traffic.

Deliver it to attached sessions regardless of invite-notify negotiation, subject to normal session fanout, because withholding the Operator's own invite would change baseline IRC semantics.

### Third-party invite notification

If target is another user, the bouncer only receives this because invite-notify or equivalent server behavior exposed it.

Deliver only to sessions that negotiated invite-notify.

Do not persist third-party invite notification as desired state.

History policy should remain explicit: unless current history classification already stores INVITE events, do not add durable INVITE history in this plan.

## 5. chghost disposition

Remain deferred.

Do not request upstream and do not advertise downstream in M006.

Reason:

- requesting it changes server output;
- clients without the capability need a compatibility projection;
- spec-compatible fallback may require synthetic QUIT/JOIN/MODE;
- current bouncer invariant does not allow invented membership claims;
- partial fallback would leave multi-client state inconsistent.

Record this as deliberate deferred work, not an omission.

## 6. extended-monitor disposition

Remain deferred.

Do not advertise a downstream MONITOR broker merely because the bouncer internally uses MONITOR for its own preferred nick.

A future implementation requires:

- per-session monitor sets;
- bounded merged upstream subscription;
- routing of monitor metadata to interested sessions;
- interaction with account/away/chghost/setname;
- privacy review.

M006 does not need it.

## 7. Capability change behavior

CAP NEW/DEL mid-generation:

- NEW may allow the bouncer to request a newly available capability only according to the existing stable upstream policy;
- DEL removes it from enabled state;
- downstream cap-notify must truthfully advertise/remove account-tag or invite-notify availability as the live generation changes;
- no attached client changes the upstream request policy.

## 8. Multi-client tests

At minimum:

- client A negotiates account-tag, client B only message-tags, client C no tags;
- same upstream PRIVMSG is rendered three correct ways;
- account tag is never synthesized from cached account-notify state;
- third-party invite reaches only invite-notify client;
- self-target invite reaches all appropriate clients;
- client attach after prior account state gets no fabricated account tag on later unstamped messages;
- CAP DEL removes downstream capability truthfully;
- slow client isolation remains intact.

## 9. Verification

Run full workspace/current + Rust 1.88 verification and existing M005-F/M005-G suites.

## 10. Acceptance criteria

Plan 037 closes when account-tag and invite-notify are fully mediated per session without introducing synthetic live state, while chghost and extended-monitor remain deliberately unavailable.

## 11. Closure evidence

Create plans/closure/bouncer-core/037-status.md with:

- capability matrix;
- tag filtering matrix;
- invite routing matrix;
- deferred-capability rationale;
- multi-client evidence;
- Plan 038 readiness.
