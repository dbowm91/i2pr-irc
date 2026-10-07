# Plan 034 — Rust 1.88 Repository Verification Corrective

Closed 2026-10-07. Outcome: **Corrective 034 closed. The declared Rust 1.88 verification
floor is green repository-wide, on the unchanged MSRV.**

Repository baseline: `140dac560f356aa9cc8985002a5bcf45c96c163d` (Corrective 033 closure).

Raised by: `plans/closure/router-integration/033-status.md` section 8.

Authority: workspace `rust-version = "1.88"`, `plans/002-long-term-roadmap.md` Phase 1
verification floor, `plans/000-long-term-specification.md` verification requirements,
`plans/003-planning-process.md`.

Primary class: verification + maintenance corrective.

## 1. Root cause, which is not "the old Clippy is stricter"

The failure is a lint **group** change, and it runs in the direction that makes a naive
"the newer toolchain is green" reading worthless.

`clippy::uninlined_format_args` is a member of:

| Toolchain | lint group | default level | fires under `-D warnings` with `clippy::all = "warn"` |
|---|---|---|---|
| `1.88.0` | `clippy::style` | warn | **yes — error** |
| `1.89.0` (current stable here) | `clippy::pedantic` | allow | no |

Taken directly from each toolchain's own lint listing (`cargo clippy -- -Whelp`), not
inferred:

~~~
$ rustup run 1.88.0 cargo clippy -p i2pr-irc-core --lib --locked -- -Whelp | grep -i uninlined
    clippy::uninlined-format-args  warn     using non-inlined variables in `format!` calls
    clippy::style  ... clippy::uninlined-format-args ...

$ cargo clippy -p i2pr-irc-core --lib --locked -- -Whelp | grep -i uninlined
    clippy::uninlined-format-args  allow    using non-inlined variables in `format!` calls
           clippy::pedantic  ... clippy::uninlined-format-args ...
~~~

The workspace declares `[workspace.lints.clippy] all = "warn"`, which covers `style` and
not `pedantic`. So the explicit non-inlined form was an error on the declared MSRV and
invisible on the newer toolchain. **The MSRV toolchain was the strict one.**

That is why this defect survived Correctives 033, 032, 031, 030, 029 and Plan 028, and
why `plans/closure/bouncer-core/008-status.md` already recorded the same lint being
addressed in Plan 008: it is a toolchain-window defect, and the repository's own gate
only exercised one side of the window at a time.

The fix is therefore not "make old Clippy happy" and not "suppress the lint". It is to
write the form that is correct on both sides of the group change.

## 2. Complete pre-fix inventory under Rust 1.88.0

Every stage of `scripts/verify.sh full` was run individually under 1.88.0, because
`set -eu` means the failing stage hides the ones behind it and the plan forbids assuming
the first diagnostic is the only one.

| Stage under `rustup run 1.88.0` | Before |
|---|---|
| `./scripts/check-network-boundary.py` | PASS (exit 0) |
| `cargo fmt --all -- --check` | PASS (exit 0) |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | **FAIL (exit 101)** |
| `cargo test --workspace --all-features --locked` | PASS, 892 passed, 0 failed, 40 binaries |
| `./scripts/fuzz-smoke.sh` | PASS (exit 0; `rusqlite 0.40.2` bundled build included) |

Exact pre-fix Clippy diagnostics, complete:

~~~
error: variables can be used directly in the `format!` string
   --> crates/core/src/lib.rs:593:9
    |
593 | /         assert_eq!(
594 | |             format!("{:?}", parsed),
595 | |             "I2pEndpoint([redacted])",
596 | |             "908 characters of key material must never reach a log line"
597 | |         );
    | |_________^
    |
    = help: for further information visit https://rust-lang.github.io/rust-clippy/master/index.html#uninlined_format_args
    = note: `-D clippy::uninlined-format-args` implied by `-D warnings`
help: change this to
    |
594 -             format!("{:?}", parsed),
594 +             format!("{parsed:?}"),
    |

error: variables can be used directly in the `format!` string
   --> crates/runtime/tests/r001c_sam_core_integration.rs:358:9
    |
358 | /         assert!(
359 | |             frame.starts_with("NICK ")
360 | |                 || frame.starts_with("USER ")
361 | |                 || frame.starts_with("CAP ")
362 | |                 || frame.starts_with("JOIN "),
363 | |             "the replacement connection carried {:?}, which is not part of a fresh \
364 | |              registration and therefore looks like a replay",
365 | |             frame
366 | |         );
    | |_________^
    |
    = help: for further information visit https://rust-lang.github.io/rust-clippy/master/index.html#uninlined_format_args
    = note: `-D clippy::uninlined-format-args` implied by `-D warnings`

error: could not compile `i2pr-irc-core` (lib test) due to 1 previous error
error: could not compile `i2pr-irc-runtime` (test "r001c_sam_core_integration") due to 1 previous error
~~~

The inventory is exhaustive rather than first-hit, by two independent means:

1. a per-crate run of every workspace member (`wire`, `core`, `sam`, `store`, `testkit`,
   `runtime`, `fuzz-smoke`), each with `--all-targets --all-features -D warnings`;
2. a workspace run with `-W clippy::uninlined_format_args` instead of `-D warnings`, so
   the lint is a warning, compilation is never aborted, and every occurrence in every
   crate is reported at once.

Both returned **two** occurrences, and the same two file:line pairs. A further workspace
run with `-A clippy::uninlined_format_args` passed clean, which establishes that
`uninlined_format_args` was the *only* lint that fired on 1.88.0 — no hidden rustfmt,
compiler, dependency, or build-script incompatibility sits behind it.

| Class required by plan section 4 | Findings |
|---|---|
| source-level Clippy/style incompatibility | **2** (below) |
| rustfmt difference | 0 — `cargo fmt --all -- --check` green on both toolchains |
| compiler language/API incompatibility | 0 — the whole workspace compiles and 892 tests run on 1.88.0 |
| dependency MSRV incompatibility | 0 — the locked tree, including bundled `rusqlite 0.40.2`, builds on 1.88.0 |
| build-script/tooling incompatibility | 0 — `scripts/verify.sh` and `scripts/fuzz-smoke.sh` are unchanged and run as written |

## 3. Findings and disposition

| Finding | Location | Kind | Disposition |
|---|---|---|---|
| **C034-F1** | `crates/core/src/lib.rs:593`, test `endpoint_accepts_a_live_router_i2p_base64_destination` | `clippy::uninlined_format_args` | **Closed.** `format!("{:?}", parsed)` → `format!("{parsed:?}")` |
| **C034-F2** | `crates/runtime/tests/r001c_sam_core_integration.rs:358`, test `a_reconnect_replays_no_upstream_frame_from_the_old_connection` | `clippy::uninlined_format_args` | **Closed.** trailing `frame` argument folded into the format string as `{frame:?}` |

Both findings were introduced by later plans (029's live-router endpoint corpus and 031's
reconnect-no-replay regression respectively) and neither is pre-existing in the sense
Plan 033 meant: Plan 033 reproduced a red MSRV floor correctly, but attributed the work
only to `crates/core`, because it checked the crate it had touched rather than the
repository. **The second finding is outside the file Plan 034 expected**, which is exactly
why the plan demanded a complete inventory rather than a fix to the known first hit.

Neither site is production code:

- `crates/core/src/lib.rs:593` is inside `#[cfg(test)] mod tests`, which begins at
  `crates/core/src/lib.rs:505`;
- `crates/runtime/tests/r001c_sam_core_integration.rs` is an integration test target.

**So this corrective changed zero production behavior.** There was no runtime, protocol,
persistence, identity, storage, or router-semantics surface in scope.

## 4. The corrections

The complete diff. Two files, two hunks, no dependency, no manifest, no script, no lint
configuration:

~~~diff
--- a/crates/core/src/lib.rs
+++ b/crates/core/src/lib.rs
@@ -591,7 +591,7 @@ mod tests {
         assert_eq!(parsed.as_str(), destination);
         assert_eq!(
-            format!("{:?}", parsed),
+            format!("{parsed:?}"),
             "I2pEndpoint([redacted])",
             "908 characters of key material must never reach a log line"
         );
--- a/crates/runtime/tests/r001c_sam_core_integration.rs
+++ b/crates/runtime/tests/r001c_sam_core_integration.rs
@@ -360,9 +360,8 @@ async fn a_reconnect_replays_no_upstream_frame_from_the_old_connection() {
                 || frame.starts_with("CAP ")
                 || frame.starts_with("JOIN "),
-            "the replacement connection carried {:?}, which is not part of a fresh \
-             registration and therefore looks like a replay",
-            frame
+            "the replacement connection carried {frame:?}, which is not part of a fresh \
+             registration and therefore looks like a replay"
         );
     }
~~~

Both rewrites bring outliers into line with the code around them rather than imposing a
foreign style: `crates/core/src/lib.rs` already uses interpolated arguments throughout
its test module (`assert!(I2pEndpoint::parse(s).is_err(), "{s}")` at line 517), so
`format!("{:?}", parsed)` was the local exception, not the local convention.

### Why each is behavior-neutral

- **C034-F1.** `parsed` is already a bound local of type `I2pEndpoint`. `{parsed:?}`
  captures exactly that binding and applies `Debug` exactly as `{:?}` did. The rendered
  string is identical, so the assertion's comparison operand and the redaction guarantee
  it exists to prove — that 908 characters of key material never reach a log line — are
  unchanged. The `{:?}` specifier and the argument's type are both preserved.
- **C034-F2.** `frame` is already a bound local of type `String`, moved into the format
  string's implicit capture by name instead of by position. The format string itself is
  unchanged apart from `{:?}` → `{frame:?}`, and the trailing `\` line continuation
  (which strips the newline and leading spaces) is preserved. The failure message is
  byte-identical, so a replay regression still reports the offending frame.

In both cases the value was already bound to a clearly named local, so plan section 5's
preferred policy — "prefer named/interpolated format arguments where the value is already
bound" — applied directly and **no lint suppression of any kind was needed**. Plan
section 6's narrow-`#[allow]` path was not used anywhere, so plan section 6's
workspace/global suppression prohibition is satisfied vacuously and `clippy::uninlined_format_args`
remains enabled by `clippy::all = "warn"` on every toolchain.

Plan section 5's rejected alternative was also rejected here: neither argument is a
complex expression, so neither needed a new named local.

## 5. Verification after the corrections

### Package level, Rust 1.88.0

| Command | Result |
|---|---|
| `rustup run 1.88.0 cargo clippy -p i2pr-irc-core --all-targets --all-features --locked -- -D warnings` | PASS, exit 0, no diagnostics |
| `rustup run 1.88.0 cargo test -p i2pr-irc-core --all-features --locked` | PASS, 13 passed, 0 failed |
| `rustup run 1.88.0 cargo clippy -p i2pr-irc-runtime --all-targets --all-features --locked -- -D warnings` | PASS, exit 0, no diagnostics |
| `rustup run 1.88.0 cargo test -p i2pr-irc-runtime --all-features --locked` | PASS, 631 passed, 0 failed |

### Repository level

| Command | Result |
|---|---|
| **`rustup run 1.88.0 sh scripts/verify.sh full`** | **PASS, exit 0** — 892 passed, 0 failed, 40 test binaries, zero `error`/`warning` lines |
| `rustup run 1.88.0 ./scripts/check-network-boundary.py` | PASS, exit 0, including positive controls |
| `rustup run 1.88.0 cargo fmt --all -- --check` | PASS, exit 0 |
| `sh scripts/verify.sh full` (current stable 1.89.0) | **PASS, exit 0** — 892 passed, 0 failed |
| `./scripts/check-network-boundary.py` (current stable) | PASS |

The test count is 892 on both toolchains, before and after the change. The suite size did
not move, which is the direct evidence that the rewrite added, removed, or skipped no
assertion.

`scripts/verify.sh` is byte-identical to its committed form. It still runs the boundary
guard, `cargo fmt --check`, Clippy with `--all-targets --all-features --locked -D warnings`,
the full test suite, and `scripts/fuzz-smoke.sh` in full mode, under whichever toolchain
invokes it. No check was weakened, reordered, or skipped to make the command pass.

## 6. Invariant review

| # | Invariant | Evidence |
|---|---|---|
| 1 | Rust 1.88 remains the declared MSRV | `Cargo.toml` `rust-version = "1.88"` unchanged; no manifest touched |
| 2 | Current stable verification remains green | `sh scripts/verify.sh full` exit 0 |
| 3 | No production behavior change required | both findings are `#[cfg(test)]`/integration-test code; diff touches no production path |
| 4 | No global suppression of `clippy::uninlined_format_args` | no `#[allow]` added anywhere; `clippy::all = "warn"` unchanged |
| 5 | `-D warnings` not weakened | unchanged in `verify.sh` and in every command run |
| 6 | `--all-targets`, `--all-features`, `--locked` not removed | unchanged in `verify.sh` and in every command run |
| 7 | `crates/core` not special-cased out of the MSRV run | the 1.88 run covers the whole workspace, `crates/core` included |
| 8 | Source rewrite preserves exact test semantics | `{:?}` and argument binding preserved in both sites; 892/892 tests pass on both toolchains |
| 9 | No dependency version changes | `git diff` over `Cargo.lock`, `Cargo.toml`, `crates/*/Cargo.toml` is empty; the locked tree builds and runs on 1.88.0 |
| 10 | R001 live i2pd evidence and SAM/network-boundary semantics unchanged | `crates/sam/**` untouched; boundary guard green on both toolchains |

## 7. Stop conditions

None of plan section 11's stop conditions was reached.

- no dependency stopped supporting 1.88 — `rusqlite 0.40.2` with bundled SQLite and the
  rest of the locked tree build and run under 1.88.0;
- no language or library usage had to change to compile on 1.88;
- the workspace MSRV was not raised, and no toolchain file was added;
- `scripts/verify.sh` needed no portability change;
- the cleanup stayed localized to two test-formatting sites in two crates, with no
  unrelated subsystem touched.

The MSRV remains `1.88`. The lint group changed underneath the repository; the source was
made correct on both sides of that change, which is the opposite of raising the floor to
dodge it.

## 8. Planning reconciliation

- `plans/implementation/bouncer-core/034-rust-1-88-verification-corrective.md`: status
  line set to closed.
- `plans/closure/bouncer-core/034-status.md`: this record.
- `plans/subsystems/bouncer-core-roadmap.md`: the Corrective 034 current-state paragraph
  and the milestone-status row updated to closed.
- `plans/registry.md`: Corrective 034 moved from active/ready to recently closed; the
  Bouncer core roadmap row and the immediate-handoff section updated.

**Plan 033's historical closure is untouched.** Its section 8 statement that
`rustup run 1.88.0 sh scripts/verify.sh full` did not pass, and that the failure was
pre-existing and outside its scope, remains exactly as written. That statement was and is
true: Plan 034 changed no SAM code, and Plan 033's live i2pd 2.61.0 evidence is unchanged.
Plan 034 adds the missing repository-wide dimension, which Plan 033's crate-scoped check
did not cover.

## 9. Future-plan disposition

| Plan | Status after Corrective 034 | Changed? |
|---|---|---|
| R002 — i2pr Managed-App Adapter | still blocked | **No** |
| R003 — Proposal 170/control integration | still research-blocked | **No** |
| Bouncer core M006+ | none registered | **No** |

Nothing is unblocked by this corrective, and the registry says so explicitly rather than
implying otherwise.

R002's blocker is its own: a managed-app interface, an i2pr integration consuming public
managed-app capabilities, and stable public i2pr app I2P-stream/local-listener/lifecycle
contracts. Corrective 034 supplied none of those. What it did remove is a repository-wide
defect that would have been copied into any future plan's closure evidence: R002 can now
be authored knowing that a green `rustup run 1.88.0 sh scripts/verify.sh full` is a
property this repository actually holds, rather than a check that fails before it reaches
R002's own work. That is a precondition for authoring a plan confidently, not a dependency
of R002, and it is deliberately not recorded as a change to R002's status.

R003 remains research-blocked on a concrete product need. MSRV was never a factor there.

The verification floor is now genuinely dual-toolchain: both the declared MSRV and current
stable run the full suite green from one tree, which no prior closure in this repository
could claim.

## 10. What a reader should take from this

The defect was invisible to the toolchain this repository verifies with most often, and
visible only to the one it declares. Every closure before this one ran the Rust 1.88 floor
narrowly — per crate, per touched file, or `cargo check` — and Plan 033, which correctly
identified the red floor, checked only `crates/core` and therefore missed the second site
in `crates/runtime`. The evidence was assembled correctly each time and still did not add
up to the whole.

The durable rule this corrective enforces is the one its own plan stated: run the full
command under the declared floor and let it reach completion. A narrow green is not a
floor, and the toolchain that is newer is not automatically the toolchain that is
stricter.