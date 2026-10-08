#!/usr/bin/env python3
"""Run the explicit eggchaos M007 socket qualification; ordinary verify skips it.

Output is deliberately sectioned so a reader can tell apart evidence of different
strength. A single undifferentiated "PASS" line would let the weakest evidence in the run
be read as the strongest, which is how a qualification claim quietly overstates itself.
"""
from __future__ import annotations

import hashlib
import os
import pathlib
import shutil
import subprocess
import sys

PIN = "b6a277d5ad4267bd602bc15a4333b14322057b90"
REPO = pathlib.Path(__file__).resolve().parents[1]


def section(title: str) -> None:
    print(f"\n=== {title} ===", flush=True)


def main() -> int:
    binary = os.environ.get("EGGCHAOS_BIN") or shutil.which("eggchaos")
    if not binary:
        print("NOT RUN: install eggchaos-cli v0.2.0 from the pinned source revision; see qualification/m007/scenarios-v2.md")
        return 0
    resolved = pathlib.Path(binary).resolve()
    version = subprocess.run([str(resolved), "version"], check=True, text=True, capture_output=True).stdout.strip()
    digest = hashlib.sha256(resolved.read_bytes()).hexdigest()
    if "0.2.0" not in version:
        raise SystemExit(f"eggchaos version mismatch: expected 0.2.0, got {version!r}")
    print(f"eggchaos={version} source_commit={PIN} binary_sha256={digest}", flush=True)

    section("generic eggchaos fault smoke (loopback echo peer; fault-tool capability only)")
    fault_smoke = subprocess.run([sys.executable, str(REPO / "qualification/m007/fault-smoke.py")], cwd=REPO)
    if fault_smoke.returncode:
        return fault_smoke.returncode

    section(
        "stream-loss disposition\n"
        "NOT QUALIFIED AS PACKET LOSS. eggchaos stream-loss drops arbitrary application\n"
        "bytes with no TCP semantics, so whether a connection survives it depends on which\n"
        "byte was dropped. It is exercised above against a generic echo peer as a\n"
        "fault-tool capability and is deliberately not asserted through the product path."
    )

    for name, needle in (
        ("product-path latency/bandwidth/slicing", "production_sam_provider_registers_through_jitter_bandwidth_and_slicing"),
        ("product-path blackhole recovery", "a_blackholed_generation_ends_under_liveness_and_recovers_through_the_product_path"),
        ("product-path disconnect recovery", "a_hard_disconnect_replaces_the_generation_and_recovers_through_the_product_path"),
        ("product-path replay disposition", "a_disruptive_recovery_replays_no_ambiguous_user_traffic"),
    ):
        section(f"{name} (SamProvider -> eggchaos -> fake SAM bridge)")
        outcome = subprocess.run(
            [
                "cargo", "test", "-p", "i2pr-irc-runtime", "--test", "m007_eggchaos", "--",
                "--ignored", "--nocapture", "--exact", needle,
            ],
            cwd=REPO,
            # Only the pinned binary comes from the environment. The proxy and admin
            # addresses are allocated inside the test and released immediately before the
            # proxy binds them, which is the only window in which they are actually
            # reserved. Handing them over from here would leave them unheld across a
            # process spawn, where any outbound connection can claim one.
            env={**os.environ, "EGGCHAOS_BIN": str(resolved)},
        )
        if outcome.returncode:
            return outcome.returncode

    print("\nQUALIFICATION COMPLETE: generic smoke, product-path shaping, blackhole recovery, disconnect recovery, and replay disposition all PASS", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())