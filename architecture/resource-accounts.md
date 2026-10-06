# Resource accounting

Qualification needs to answer one question repeatedly: after a campaign has churned
through many Networks and sessions, has everything it touched come back down? A counter
that only moves forward cannot answer that, and a log of every sample could not be stored
without becoming the thing being measured.

`crate::resource` answers it with a `ResourceLedger` that keeps exactly two values per
gauge: its **current** value and the **highest** it has ever reached. A campaign takes a
baseline, drives load, reads the peak, settles, and reads again. Settled must equal
baseline.

## Why two numbers and not a history

Keeping a time series would let a campaign reconstruct behaviour, but it makes the
accounting structure grow with run time — which is precisely what a resource-bound claim
is supposed to rule out. Two fixed `usize` slots per gauge have a constant cost no matter
how long the process runs or how many campaigns it serves.

Peak folding happens inside `snapshot()` rather than at each observation, so a caller
cannot forget to update it. There is no public "record peak" operation to omit.

## Process-wide gauges are measured, not summed

`store_queue`, `reconnect_waiters`, and `in_flight_connects` are **not** per-Network. There
is one store queue and one reconnect scheduler for the whole process. Summing them across
Networks would multiply a single queue by the Network count and report a total that does
not exist.

They are therefore read live from the `StoreHandle` and the `ReconnectScheduler` rather than
mirrored into the ledger. A mirror would be a second copy that could disagree with the thing
it is measuring.

For the same reason, session queue depths report the **deepest single session**, not a sum:
one client falling behind is a bounded per-client condition, and summing it across sessions
would make a well-understood backpressure look like an attack.

## Bounded by construction

The gauge set is a closed struct, not a map keyed by a caller-supplied string, and the
tracked Network count is capped by the same ceiling that bounds supervision
(`MAX_TRACKED_NETWORKS`). A Network beyond that ceiling is refused, not recorded.

`LedgerRefused` is a diagnostic refusal, never a Network failure. Refusing to report a
gauge must not stop the bouncer, because the Network is real and the diagnostic is not.
The error carries no detail: a message naming the refused Network would turn a diagnostic
into an enumeration of what the operator is running.

`NetworkOwner::drop` calls `resources.forget()`, so a stopped owner stops being counted.
Without that, a long-lived process would accumulate ledger entries for Networks that no
longer exist.

## Counts only

Every field is a `usize` or a `NetworkId`. There is deliberately no field that could hold a
nick, an endpoint, a message, or a credential, so reading the ledger cannot disclose what
the bouncer is carrying. The same constraint holds for `NetworkSnapshot`.

See `network-ownership.md` for what each gauge describes and `reconnect-budget.md` for the
scheduler whose admission is gated here.