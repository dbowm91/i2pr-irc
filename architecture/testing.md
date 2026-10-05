# Deterministic testing

`VirtualClock` schedules monotonic `sleep_until` futures. `advance()` wakes expired sleepers ordered by deadline and timer identity; dropping a sleep removes its pending registration. Core tests poll and advance virtual time without wall-clock sleeps.

`ScriptedStream::pair` creates two ordered stream endpoints with bounded directional buffers (capacity is clamped to 1 MiB). Its script sets short read/write sizes, capacity, initial stalls, byte-boundary EOF/reset, and bounded opt-in write capture (4 KiB maximum). The controller can stall/resume or reset a read while tests are running. Full buffers return pending and wake when the peer consumes bytes. Provider outcomes/records/peer queues are capped at 256; deferred completions are capped at 1024. Provider fixtures return scripted results and expose the peer stream for integration tests. Deferred completions carry connection generations so stale work can be held across replacement and discarded.

Wire tests exercise the exact normal/tagged limits, parameters, tags, all representative split points, concatenation, and deterministic arbitrary byte inputs. The release fuzz-smoke binary repeats arbitrary input and semantic round-trip/bounded-buffer properties. This is a deterministic smoke campaign; it is not coverage-guided fuzzing.

M002's current runtime remains an unqualified registration attempt, not a functioning bouncer.
