#!/usr/bin/env python3
"""Run the explicit eggchaos M007 socket qualification; ordinary verify skips it."""
from __future__ import annotations

import hashlib
import os
import pathlib
import shutil
import socket
import subprocess
import sys

PIN = "b6a277d5ad4267bd602bc15a4333b14322057b90"
REPO = pathlib.Path(__file__).resolve().parents[1]


def loopback_address() -> str:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return f"127.0.0.1:{listener.getsockname()[1]}"


def main() -> int:
    binary = os.environ.get("EGGCHAOS_BIN") or shutil.which("eggchaos")
    if not binary:
        print("NOT RUN: install eggchaos-cli v0.2.0 from the pinned source revision; see qualification/m007/scenarios-v1.md")
        return 0
    resolved = pathlib.Path(binary).resolve()
    version = subprocess.run([str(resolved), "version"], check=True, text=True, capture_output=True).stdout.strip()
    digest = hashlib.sha256(resolved.read_bytes()).hexdigest()
    if "0.2.0" not in version:
        raise SystemExit(f"eggchaos version mismatch: expected 0.2.0, got {version!r}")
    print(f"eggchaos={version} source_commit={PIN} binary_sha256={digest}", flush=True)
    proxy_address = loopback_address()
    admin_address = loopback_address()
    fault_smoke = subprocess.run([sys.executable, str(REPO / "qualification/m007/fault-smoke.py")], cwd=REPO)
    if fault_smoke.returncode:
        return fault_smoke.returncode
    command = [
        "rtk", "proxy", "env", f"EGGCHAOS_BIN={resolved}",
        f"EGGCHAOS_PROXY_ADDR={proxy_address}", f"EGGCHAOS_ADMIN_ADDR={admin_address}", "cargo", "test",
        "-p", "i2pr-irc-runtime", "--test", "m007_eggchaos", "--", "--ignored", "--nocapture",
    ]
    return subprocess.run(command, cwd=REPO).returncode


if __name__ == "__main__":
    sys.exit(main())
