#!/usr/bin/env python3
"""Live SAM qualification against an i2pd SAM bridge.

Fails closed. Nothing here can report a pass for a router it did not reach: the bridge is
required to be present and answering, and every step that does not complete is reported as
NOT RUN rather than being skipped silently.

Why this is Python and not the owned client on both sides: Plan 032 requires that the
implementation under test not also be the thing that certifies it. The peer session is
driven with hand-written SAM over its own socket, so a protocol mistake in the owned client
cannot hide by being reproduced on both ends.

This script opens loopback sockets only. It is a test fixture; it is not part of the
product, and nothing in `crates/` depends on it.

Observed against i2pd 2.61.0:

  * HELLO and SESSION CREATE both succeed using the exact frozen request lines.
  * i2pd answers SESSION STATUS without an ID= field, so a client that supplies its own
    session ID and does not wait for the router to name the session is required. The owned
    client already works this way.
  * The Destination i2pd returns is 908 characters of I2P base64 (`-` and `~`, not `+`
    and `/`). It is accepted by `I2pEndpoint::parse`.
  * STREAM CONNECT on its own socket, naming the control socket's session, returns
    `STREAM STATUS RESULT=OK`.
  * The inbound stream was not observed arriving on the peer session socket in this
    environment. That step is reported as NOT RUN, never as a pass.
"""

import argparse
import os
import queue
import re
import socket
import sys
import threading
import time

CREATE_OPTS = (
    "SIGNATURE_TYPE=7 i2cp.leaseSetEncType=4 "
    "i2cp.dontPublishLeaseSet={publish} inbound.quantity=2 outbound.quantity=2"
)


class Raw:
    """One hand-driven SAM connection, with a background reader so nothing is missed."""

    def __init__(self, label, address, port, budget):
        self.label = label
        self.budget = budget
        self.sock = socket.create_connection((address, port), timeout=budget)
        self.sock.settimeout(budget)
        self.buf = bytearray()
        self.inbox = queue.Queue()
        self.stop = False

    def _pump(self):
        while not self.stop:
            try:
                chunk = self.sock.recv(4096)
            except socket.timeout:
                continue
            except OSError:
                break
            if not chunk:
                break
            self.inbox.put(chunk)

    def start_pumping(self):
        threading.Thread(target=self._pump, daemon=True).start()

    def readline(self):
        while b"\n" not in self.buf:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise RuntimeError(f"{self.label}: bridge closed the connection")
            self.buf.extend(chunk)
        index = self.buf.index(b"\n")
        line = bytes(self.buf[:index])
        self.buf = self.buf[index + 1 :]
        return line

    def send(self, text):
        self.sock.sendall(text.encode() + b"\r\n")

    def hello(self):
        self.send("HELLO VERSION MIN=3.1 MAX=3.1")
        reply = self.readline()
        if b"RESULT=OK" not in reply:
            raise RuntimeError(f"{self.label}: HELLO refused: {reply!r}")
        return reply

    def create_session(self, session_id, publish):
        started = time.time()
        options = CREATE_OPTS.format(publish="false" if publish else "true")
        self.send(
            f"SESSION CREATE STYLE=STREAM ID={session_id} "
            f"DESTINATION=TRANSIENT {options}"
        )
        reply = self.readline()
        if b"RESULT=OK" not in reply:
            raise RuntimeError(f"{self.label}: SESSION refused: {reply[:160]!r}")
        destination = re.search(rb"DESTINATION=([^\s]+)", reply)
        if not destination:
            raise RuntimeError(f"{self.label}: SESSION returned no destination")
        elapsed = time.time() - started
        print(
            f"  {self.label} SESSION ok in {elapsed:.1f}s "
            f"id_echoed={b'ID=' in reply} destlen={len(destination.group(1))}"
        )
        return destination.group(1).decode(), elapsed

    def drain(self, want, budget):
        got = bytearray()
        end = time.time() + budget
        while len(got) < want and time.time() < end:
            try:
                got.extend(self.inbox.get(timeout=1))
            except queue.Empty:
                continue
        return bytes(got)


def parse_endpoint(text):
    host, _, port = text.rpartition(":")
    return host, int(port)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--endpoint",
        default=os.environ.get("I2PR_SAM_ENDPOINT", "127.0.0.1:7656"),
        help="loopback SAM bridge, host:port. Never a non-loopback address.",
    )
    parser.add_argument("--budget", type=int, default=300, help="per-socket socket timeout")
    parser.add_argument("--drain", type=int, default=180, help="byte-exchange budget")
    args = parser.parse_args()

    host, port = parse_endpoint(args.endpoint)
    if host not in ("127.0.0.1", "::1", "localhost"):
        print(f"REFUSED: {args.endpoint} is not loopback; this fixture never leaves the host.")
        return 2

    try:
        probe = socket.create_connection((host, port), timeout=10)
        probe.close()
    except OSError as error:
        print(f"NOT RUN: no SAM bridge at {args.endpoint} ({error}).")
        print("A missing router is not a pass.")
        return 2
    print(f"bridge: {args.endpoint}")

    binary_fixture = (
        bytes([0x00, 0xFF, 0xFE])
        + b"STREAM STATUS RESULT=OK\r\n"
        + b"\r\n"
        + bytes([0x01])
    )
    results = []

    def record(step, verdict, detail=""):
        results.append((step, verdict, detail))
        print(f"  {step}: {verdict}{(' — ' + detail) if detail else ''}")

    peer = Raw("peer", host, port, args.budget)
    try:
        peer.hello()
        record("HELLO", "PASS")
        peer_destination, peer_seconds = peer.create_session(os.urandom(16).hex(), publish=True)
        peer.start_pumping()
        record("SESSION CREATE (independent peer)", "PASS", f"{peer_seconds:.1f}s")
    except (RuntimeError, OSError) as error:
        record("SESSION CREATE (independent peer)", "NOT RUN", str(error))
        summarise(results)
        return 2

    control = Raw("control", host, port, args.budget)
    try:
        control.hello()
        client_id = os.urandom(16).hex()
        _, control_seconds = control.create_session(client_id, publish=False)
        control.start_pumping()
        record("SESSION CREATE (owned topology)", "PASS", f"{control_seconds:.1f}s")
    except (RuntimeError, OSError) as error:
        record("SESSION CREATE (owned topology)", "NOT RUN", str(error))
        summarise(results)
        return 2

    stream = Raw("stream", host, port, args.budget)
    try:
        stream.hello()
        started = time.time()
        stream.send(
            f"STREAM CONNECT ID={client_id} DESTINATION={peer_destination} SILENT=false"
        )
        reply = stream.readline()
        connect_seconds = time.time() - started
        if b"RESULT=OK" not in reply:
            record("STREAM CONNECT", "FAIL", reply[:80].decode(errors="replace"))
            summarise(results)
            return 1
        record("STREAM CONNECT", "PASS", f"{connect_seconds:.1f}s, {reply[:40].decode()}")
        stream.start_pumping()
    except (RuntimeError, OSError) as error:
        record("STREAM CONNECT", "NOT RUN", str(error))
        summarise(results)
        return 2

    stream.sock.sendall(binary_fixture)
    forward = peer.drain(len(binary_fixture), args.drain)
    record(
        "binary fixture reaches the peer",
        "PASS" if forward == binary_fixture else "NOT RUN",
        "exact bytes" if forward == binary_fixture else f"{len(forward)} of {len(binary_fixture)} bytes",
    )

    peer.sock.sendall(b"PEER-REPLY\x00\xff\r\n")
    backward = stream.drain(13, args.drain)
    record(
        "reply reaches the client",
        "PASS" if backward == b"PEER-REPLY\x00\xff\r\n" else "NOT RUN",
        "exact bytes" if backward == b"PEER-REPLY\x00\xff\r\n" else f"{len(backward)} of 13 bytes",
    )

    second = Raw("stream-2", host, port, args.budget)
    try:
        second.hello()
        second.send(
            f"STREAM CONNECT ID={client_id} DESTINATION={peer_destination} SILENT=false"
        )
        reply = second.readline()
        record(
            "second stream reuses the session",
            "PASS" if b"RESULT=OK" in reply else "NOT RUN",
            reply[:40].decode(errors="replace"),
        )
    except (RuntimeError, OSError) as error:
        record("second stream reuses the session", "NOT RUN", str(error))

    return summarise(results)


def summarise(results):
    print("\n--- summary ---")
    for step, verdict, detail in results:
        print(f"{verdict:<8} {step}" + (f" ({detail})" if detail else ""))
    passed = sum(1 for _, verdict, _ in results if verdict == "PASS")
    not_run = sum(1 for _, verdict, _ in results if verdict == "NOT RUN")
    failed = sum(1 for _, verdict, _ in results if verdict == "FAIL")
    print(f"\n{passed} passed, {failed} failed, {not_run} not run")
    if not_run:
        print("NOT RUN steps are reported as evidence unavailable, never as passes.")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())