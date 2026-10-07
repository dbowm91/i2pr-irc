# Deterministic testing

`VirtualClock` schedules monotonic `sleep_until` futures. `advance()` wakes expired sleepers ordered by deadline and timer identity; dropping a sleep removes its pending registration. Core tests poll and advance virtual time without wall-clock sleeps.

`ScriptedStream::pair` creates two ordered stream endpoints with bounded directional buffers (capacity is clamped to 1 MiB). Its script sets short read/write sizes, capacity, initial stalls, byte-boundary EOF/reset, and bounded opt-in write capture (4 KiB maximum). The controller can stall/resume or reset a read while tests are running. Full buffers return pending and wake when the peer consumes bytes. Provider outcomes/records/peer queues are capped at 256; deferred completions are capped at 1024. Provider fixtures return scripted results and expose the peer stream for integration tests. Deferred completions carry connection generations so stale work can be held across replacement and discarded.

Wire tests exercise the exact normal/tagged limits, parameters, tags, all representative split points, concatenation, and deterministic arbitrary byte inputs. The release fuzz-smoke binary repeats arbitrary input and semantic round-trip/bounded-buffer properties. This is a deterministic smoke campaign; it is not coverage-guided fuzzing.

M002's runtime integration suite uses `FakeI2pStreamProvider` and `FakeLocalAcceptor` for CAP/SASL registration, downstream handshake and CAP mediation, channel/topic/mode/member projection, tagged-message filtering, message/query routing, PING/PONG priority, no-replay after ambiguous disconnect, desired-channel reconstruction, stopped-registration cancellation, registration/liveness deadlines, and 100 provider failures before recovery.

The ownership campaign adds evidence for upstream registration and liveness with zero attached clients, client EOF and client `QUIT` ending only that client, a second client reattaching the same generation and seeing state learned while detached, 100 attach/detach cycles staying bounded within one generation, downstream protocol violation and queue overload ending only the client, a saturated client backlog still receiving an answered server PING, upstream failure with and without an attached client, a stale detached session not affecting the next client, and explicit stop being the only path that sends upstream `QUIT`. Provider fixtures also expose the fault controller for a connection so tests can assert on captured upstream writes. Tokio's paused clock drives reconnect, accept-retry, and online deadline tests without wall-clock waiting; paused-time scenarios settle pending work before arming a bounded reader so virtual time cannot outrun the owner.

The M005 suites add, per plan: `m005a_controller_admission` (bounded controller, pre-bind admission, one-shot transfer, ambiguous-commit recovery), `m005b_detached_policy` (durable detach across restart, deferred reveal, fanout redaction), `m005c_presence_nick` (presence transition matrix, bounded deterministic fallback, both reclaim mechanisms, generation fencing), `m005d_bouncer_networks` (discovery, selection, attribute refusal, notifications, and the administration surface), and `m005e_search_history` (search framing, refusal, scoping, retention, restart identity, `AROUND` edges, and liveness under search load).

Two harness rules this repo now depends on, both learned from suites that reported passes
for the wrong reason:

**A search or query assertion reads from a mark, not from the start of the buffer.** A
client attached to a live Network is still being fanned the upstream traffic that the test
just wrote upstream. Asserting against the whole buffer tests what the client was *fanned*
rather than what it was *told*, and the fanned text is nearly always a superset of the
answer — so a negative assertion passes vacuously and a positive one passes for the wrong
reason. Drain first, then mark, then send the request.

**Unit-test the parse path the wire actually takes.** `parse_selectors` was directly
unit-tested and correct; `parse_search` — the function the owner calls — skipped the first
parameter, silently discarding any `in=` scope. Every search still returned plausible
results, scoped to the wrong set. The unit tests could not see it because they started
past the bug, and only the end-to-end suite did.

Two harness properties are load-bearing across those suites and are stated here because a suite that gets them wrong fails in a way that reads like a product bug. A test client buffers everything it has received rather than discarding what follows the frame it waited for: a scripted stream can deliver several frames in one read, and the discarded remainder is exactly the frame the next assertion is about. And an assertion inside a loop matches only what arrived *after* that iteration's request, or the second iteration passes on the first one's answer.

The network-boundary script scans the first-party crates including their tests, so a fixture that reached for a clearnet socket would fail the same campaign the production code does.
