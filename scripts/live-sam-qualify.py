#!/usr/bin/env python3
"""Live SAM qualification against an i2pd SAM bridge, with a real inbound accept.

Corrective 033. The Plan 032 harness treated the SESSION CREATE control socket as if it
were the inbound application stream. It is not: SAM has no such rule. An inbound
application must keep its session alive on the control socket and open a *second* socket
to issue STREAM ACCEPT. Without that socket there is no inbound stream at all, so the
"0 bytes traversed" result measured a topology that was never built.

This harness therefore runs the shape the specification describes:

    peer control socket    HELLO / SESSION CREATE          (held open for the session)
    peer accept socket     HELLO / STREAM ACCEPT ID=...     (RESULT=OK, then the peer's
                           Destination line, then raw bidirectional application bytes)

The connecting side is the production Rust client, not this file. It runs as a separate
process (`cargo run --example live-qualify-probe`) that constructs the real
`SamBridgeEndpoint`, the real `SamProvider`, and the real `I2pStreamProvider::connect`,
and keeps one provider instance alive across both streams so session reuse is measured
rather than asserted. This file never speaks SAM on the connecting side, so a mistake in
the owned client cannot hide by being reproduced on both ends.

It fails closed. With no bridge it prints `NOT RUN: no SAM bridge` and exits 2, and it
refuses any endpoint that is not a numeric loopback literal -- including `localhost`,
because accepting a name would let a resolver decide where to connect.

This script opens loopback sockets only. It is a test fixture; nothing in `crates/`
depends on it.
"""

import argparse
import ipaddress
import os
import re
import socket
import subprocess
import sys
import threading
import time

# The peer publishes its LeaseSet. `dontPublishLeaseSet=true` is correct for a client
# that is never reached and wrong for a peer that must be: Plan 032 measured
# `CANT_REACH_PEER MESSAGE="LeaseSet not found"` until this was flipped.
PEER_CREATE_OPTS = (
    "SIGNATURE_TYPE=7 i2cp.leaseSetEncType=4 "
    "i2cp.dontPublishLeaseSet=false inbound.quantity=2 outbound.quantity=2"
)

# Ceiling on the peer-Destination line an accept socket reads before raw data. Matches
# the client's own SAM line ceiling: it is the same kind of line, and an unbounded read
# of a bridge-controlled prelude is how a qualification harness turns into a liability.
MAX_PRELUDE_BYTES = 4096

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PROBE_EXAMPLE = "live-qualify-probe"
# The fixed scope the production probe uses. A test-only NetworkId: it is a local
# lifetime scope and must never reach a router as anything other than the random session
# ID the client mints for it.
PROBE_NETWORK = 1


class Refused(Exception):
    """A configuration or topology violation. Reported, never retried."""


class Endpoint:
    """A numeric loopback SAM bridge address, parsed without a resolver."""

    def __init__(self, host, port):
        self.host = host
        self.port = port

    def __str__(self):
        return f"[{self.host}]:{self.port}" if ":" in self.host else f"{self.host}:{self.port}"

    def connect(self, timeout):
        # `str(self)` rather than a two-tuple, so an IPv6 literal is bracketed for the
        # socket layer exactly as it is here.
        return socket.create_connection((self.host, self.port), timeout=timeout)


def parse_endpoint(text):
    """Accepts only a numeric loopback literal with a usable port.

    Deliberately mirrors `SamBridgeEndpoint::parse` in `crates/sam`. The qualification
    tool must not hold a weaker authority than the code it qualifies: a harness that
    accepts `localhost` would resolve a name to decide where to connect, and the
    boundary the production type enforces by construction would only be enforced here
    by a convention that nothing checks.
    """
    if not text:
        raise Refused("no SAM bridge endpoint configured")
    if "://" in text or "/" in text:
        raise Refused("a URL or path is not a bridge address")
    if ":" not in text:
        raise Refused("bridge endpoint is missing a port")
    host, _, port_text = text.rpartition(":")
    if host.startswith("[") and host.endswith("]"):
        host = host[1:-1]
    if not host:
        raise Refused("bridge endpoint is missing an address")
    if not all(c in "0123456789abcdefABCDEF.:" for c in host):
        # A name of any form. Not special-cased into acceptance: accepting one is what
        # would let a hosts file or a resolver pick the router.
        raise Refused(f"{text!r} is not a numeric address")
    try:
        address = ipaddress.ip_address(host)
    except ValueError:
        raise Refused(f"{text!r} is not a numeric address") from None
    if not address.is_loopback:
        raise Refused(f"{text} is not loopback; this fixture never leaves the host")
    if not port_text.isdigit():
        raise Refused("bridge port must be 1-65535")
    port = int(port_text)
    if not 1 <= port <= 65535:
        raise Refused("bridge port must be 1-65535")
    return Endpoint(host, port)


class Wire:
    """One SAM TCP connection, with exactly one reader and a bounded buffer.

    A single reader thread feeds every byte into one buffer, and every read consumes
    from that buffer. That is what makes the raw transition exact: bytes the reader
    picked up past the last newline are kept and handed to the next read, rather than
    dropped on the floor between a `readline` and a `read_exact`.
    """

    def __init__(self, label, endpoint, budget):
        self.label = label
        self.budget = budget
        self.sock = endpoint.connect(budget)
        self.sock.settimeout(1.0)
        self.buffer = bytearray()
        self.condition = threading.Condition()
        self.closed = False
        self.thread = threading.Thread(target=self._pump, daemon=True)
        self.thread.start()

    def _pump(self):
        while True:
            try:
                chunk = self.sock.recv(4096)
            except socket.timeout:
                continue
            except OSError:
                break
            if not chunk:
                break
            with self.condition:
                self.buffer.extend(chunk)
                self.condition.notify_all()
        with self.condition:
            self.closed = True
            self.condition.notify_all()

    def _fill(self, want, budget):
        deadline = time.monotonic() + budget
        with self.condition:
            while len(self.buffer) < want:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise Refused(f"{self.label}: no data within {budget:.0f}s")
                self.condition.wait(min(remaining, 1.0))
                if self.closed and len(self.buffer) < want:
                    raise Refused(
                        f"{self.label}: bridge closed the connection "
                        f"({len(self.buffer)} of {want} bytes)"
                    )

    def readline(self, budget, ceiling=MAX_PRELUDE_BYTES):
        """One CRLF line, terminator removed, with any trailing bytes preserved."""
        self._fill(1, budget)
        deadline = time.monotonic() + budget
        with self.condition:
            while True:
                index = self.buffer.find(b"\n")
                if index >= 0:
                    if index > ceiling:
                        raise Refused(
                            f"{self.label}: line of {index} bytes exceeds {ceiling}"
                        )
                    line = bytes(self.buffer[:index])
                    del self.buffer[: index + 1]
                    return line.rstrip(b"\r")
                if len(self.buffer) > ceiling:
                    raise Refused(
                        f"{self.label}: no newline within {ceiling} bytes"
                    )
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise Refused(f"{self.label}: no line within {budget:.0f}s")
                self.condition.wait(min(remaining, 1.0))
                if self.closed:
                    raise Refused(f"{self.label}: bridge closed before a line arrived")

    def skip_one_blank(self, budget):
        """Consumes a single empty line if one is waiting, and reports whether it did.

        i2pd 2.61.0 terminates the peer-Destination line and then writes a second,
        empty line before any application byte. The specification does not call for it,
        so it is reported as its own stage rather than absorbed silently: a reader that
        quietly swallowed arbitrary leading bytes would hide a real deviation behind a
        passing run.

        Bounded to exactly one, and only when the byte is already there or arrives within
        the budget. The residual ambiguity is stated plainly in the run summary: a payload
        whose *first* byte is a newline cannot be told apart from this blank line on a
        router that sends one.
        """
        try:
            self._fill(1, budget)
        except Refused:
            return False
        with self.condition:
            if self.buffer[:1] == b"\n":
                del self.buffer[:1]
                return True
            return False

    def read_exact(self, count, budget):
        """Exactly `count` bytes, consuming anything already buffered first."""
        self._fill(count, budget)
        with self.condition:
            taken = bytes(self.buffer[:count])
            del self.buffer[:count]
            return taken

    def send(self, data):
        if isinstance(data, str):
            data = data.encode()
        self.sock.sendall(data)

    def send_line(self, text):
        self.send(text.encode() + b"\r\n")

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass


def hello(wire):
    wire.send_line("HELLO VERSION MIN=3.1 MAX=3.1")
    reply = wire.readline(wire.budget)
    if b"RESULT=OK" not in reply and b"HELLO OK" not in reply:
        raise Refused(f"{wire.label}: HELLO refused: {reply[:120]!r}")
    return reply


def peer_session(wire, session_id):
    """SESSION CREATE on the control socket, which then stays open for the session.

    `session_id` is supplied by the caller because the accept on the second socket has to
    name the *same* session. Minting it in here and again at the accept would produce a
    `STREAM ACCEPT` for a session the router never created — which is exactly what i2pd
    answers with `INVALID_ID`, and exactly why this run is worth reading rather than
    assuming.

    Returns the Destination the router minted. The session ID is never printed: it is the
    one value that would let a reader correlate two Networks from logs.
    """
    started = time.monotonic()
    wire.send_line(
        f"SESSION CREATE STYLE=STREAM ID={session_id} "
        f"DESTINATION=TRANSIENT {PEER_CREATE_OPTS}"
    )
    reply = wire.readline(wire.budget)
    if b"RESULT=OK" not in reply:
        raise Refused(f"{wire.label}: SESSION refused: {reply[:160]!r}")
    destination = re.search(rb"DESTINATION=([^\s]+)", reply)
    if not destination:
        raise Refused(f"{wire.label}: SESSION returned no destination")
    return destination.group(1).decode(), time.monotonic() - started, b"ID=" in reply


def open_accept(wire, session_id, budget):
    """STREAM ACCEPT on a second socket, then the peer's Destination line.

    Two protocol lines arrive before any application byte: `STREAM STATUS RESULT=OK`
    acknowledging the accept, and then -- because SILENT=false -- the base64 public
    destination of the peer that connected. Only after both is the socket raw.
    """
    hello(wire)
    wire.send_line(f"STREAM ACCEPT ID={session_id} SILENT=false")
    status = wire.readline(budget)
    if b"RESULT=OK" not in status:
        raise Refused(f"{wire.label}: STREAM ACCEPT refused: {status[:160]!r}")
    return status


def build_probe():
    """Compiles the production-side probe from the real `i2pr-irc-sam` crate."""
    subprocess.run(
        [
            "cargo", "build", "--locked", "--quiet",
            "--package", "i2pr-irc-sam", "--example", PROBE_EXAMPLE,
        ],
        cwd=REPO_ROOT, check=True,
    )
    return os.path.join(REPO_ROOT, "target", "debug", "examples", PROBE_EXAMPLE)


def start_exchange(probe, destination, payload, reply):
    """Instructs the production probe to open one stream and exchange bytes.

    Returns without waiting for the answer. The provider writes its payload and then
    blocks for the reply, so the peer side has to be driven *before* this probe's result
    line is read. Waiting for the line first would deadlock both halves against each
    other and report a budget timeout for what is really an ordering mistake here.
    """
    command = (
        f"CONNECT DESTINATION={destination} "
        f"FORWARD={payload.hex()} REPLY={reply.hex()}"
    )
    started = time.monotonic()
    # Text mode on both ends, so this is a str. The Destination and the hex fixtures are
    # all ASCII, so nothing is lost by that choice.
    probe.stdin.write(command + "\n")
    probe.stdin.flush()
    return started


def collect_exchange(probe, reply):
    """Reads the probe's verdict and checks it against what was actually sent.

    `probe` stays the same process across both streams, so the second stream reuses the
    session the first one created. The probe verifies both directions itself and reports
    `reverse_exact`; this function checks the same answer rather than trusting a single
    verdict from the code under test.
    """
    line = probe.stdout.readline()
    if not line:
        raise Refused("the production probe exited before answering")
    fields = line.strip().split()
    if len(fields) < 3 or fields[0] != "RESULT" or fields[1] != "CONNECT":
        raise Refused(f"unreadable probe result: {' '.join(fields)[:200]}")
    if fields[2] != "PASS":
        raise Refused(f"production connect failed: {' '.join(fields[3:])[:200]}")
    attributes = parse_attributes(fields[3:])
    if attributes.get("forward_exact") != "true":
        raise Refused("the production client did not write the exact payload")
    if attributes.get("reverse_exact") != "true":
        raise Refused("the reply did not reach the production client byte for byte")
    if attributes.get("reverse") != reply.hex():
        raise Refused(
            "the production client reported different reply bytes than were sent"
        )
    return attributes


def probe_command(probe, command):
    probe.stdin.write(command + "\n")
    probe.stdin.flush()
    line = probe.stdout.readline()
    if not line:
        raise Refused(f"the production probe exited during {command!r}")
    return line.strip().split()


# Every step verdict this run produced, in order. Module-level so `record` can be a
# plain function rather than a closure; a closure would make the helper below and the
# caller share a name for two different things.
RESULTS = []


def describe_bytes(expected, actual):
    """A bounded hex view of a byte mismatch.

    Only ever applied to this file's own synthetic fixtures, never to router or peer
    output, so the ceiling here is about a readable diff rather than about secrecy.
    """
    limit = min(len(expected), len(actual), 32)
    return f"expected {expected[:limit].hex()} got {actual[:limit].hex()}"


def record(step, verdict, detail=""):
    RESULTS.append((step, verdict, detail))
    print(f"  {step}: {verdict}{(' — ' + detail) if detail else ''}")


def parse_attributes(fields):
    return dict(field.split("=", 1) for field in fields if "=" in field)


def run(args):
    endpoint = parse_endpoint(args.endpoint)

    try:
        probe_conn = endpoint.connect(10)
        probe_conn.close()
    except OSError as error:
        print(f"NOT RUN: no SAM bridge at {endpoint} ({error}).")
        print("A missing router is not a pass.")
        return 2
    print(f"bridge: {endpoint}")

    binary_fixture = (
        bytes([0x00, 0xFF, 0xFE])
        + b"STREAM STATUS RESULT=OK\r\n"
        + b"\r\n"
        + bytes([0x01])
    )
    reply_fixture = b"PEER-REPLY\x00\xff\r\n"
    second_fixture = b"\x00\xfeSECOND\xff"
    second_reply = b"OK\x00"

    RESULTS.clear()

    # ------------------------------------------------ independent peer session
    control = Wire("peer-control", endpoint, args.budget)
    session_id = os.urandom(16).hex()
    destination = None
    try:
        hello(control)
        record("HELLO (independent peer)", "PASS")
        destination, seconds, echoed = peer_session(control, session_id)
        record(
            "SESSION CREATE (independent peer control socket)",
            "PASS",
            f"{seconds:.1f}s, id_echoed={echoed}, destlen={len(destination)}",
        )
    except (Refused, OSError) as error:
        record("SESSION CREATE (independent peer control socket)", "NOT RUN", str(error))
        return summarise(RESULTS)

    # ------------------------------------------------------ production probe
    try:
        probe_path = args.probe or build_probe()
    except (subprocess.CalledProcessError, OSError) as error:
        record("production probe build", "NOT RUN", str(error))
        return summarise(RESULTS)

    try:
        probe = subprocess.Popen(
            [probe_path, "--endpoint", str(endpoint), "--network", str(PROBE_NETWORK)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1,
        )
    except OSError as error:
        record("production probe start", "NOT RUN", str(error))
        return summarise(RESULTS)

    try:
        # ------------------------------------------------ first stream: full exchange
        accept = Wire("peer-accept-1", endpoint, args.budget)
        try:
            open_accept(accept, session_id, args.budget)
            record("STREAM ACCEPT armed on a separate socket", "PASS")

            opened = start_exchange(
                probe, destination, binary_fixture, reply_fixture
            )

            peer_line = accept.readline(args.drain)
            if len(peer_line) < MIN_PEER_DESTINATION:
                record(
                    "inbound peer-Destination line",
                    "FAIL",
                    f"{len(peer_line)} characters is not a Destination",
                )
                return summarise(RESULTS)
            record(
                "inbound peer-Destination line",
                "PASS",
                f"{len(peer_line)} characters",
            )
            blank = accept.skip_one_blank(args.drain)
            record(
                "router blank line after the Destination line",
                "PASS" if blank else "PASS",
                "i2pd 2.61.0 sent one; skipped exactly one"
                if blank
                else "none sent",
            )

            forward = accept.read_exact(len(binary_fixture), args.drain)
            record(
                "binary fixture reaches the peer (NUL, invalid UTF-8, CRLF, SAM text)",
                "PASS" if forward == binary_fixture else "FAIL",
                "exact bytes" if forward == binary_fixture
                else f"{len(forward)} of {len(binary_fixture)} bytes, "
                + describe_bytes(binary_fixture, forward),
            )
            if forward != binary_fixture:
                return summarise(RESULTS)

            # Answer before reading the probe's verdict: the provider is blocked waiting
            # for exactly these bytes.
            accept.send(reply_fixture)
            attributes = collect_exchange(probe, reply_fixture)
            record(
                "production STREAM CONNECT (real SamProvider)",
                "PASS",
                f"{time.monotonic() - opened:.1f}s, "
                f"{attributes.get('forward_bytes')} bytes out, "
                f"{attributes.get('reply_bytes')} bytes back",
            )
            record("reply reaches the production client", "PASS", "exact bytes")
        finally:
            accept.close()

        # --------------------------------- second stream over the same provider
        accept2 = Wire("peer-accept-2", endpoint, args.budget)
        try:
            open_accept(accept2, session_id, args.budget)
            record("second STREAM ACCEPT armed", "PASS")

            opened = start_exchange(
                probe, destination, second_fixture, second_reply
            )
            accept2.readline(args.drain)
            accept2.skip_one_blank(args.drain)
            forward2 = accept2.read_exact(len(second_fixture), args.drain)
            record(
                "second payload reaches the peer",
                "PASS" if forward2 == second_fixture else "FAIL",
                "exact bytes" if forward2 == second_fixture
                else f"{len(forward2)} of {len(second_fixture)} bytes, "
                + describe_bytes(second_fixture, forward2),
            )
            if forward2 != second_fixture:
                return summarise(RESULTS)
            accept2.send(second_reply)
            collect_exchange(probe, second_reply)
            record(
                "second STREAM CONNECT (same provider process)",
                "PASS",
                f"{time.monotonic() - opened:.1f}s",
            )
        finally:
            accept2.close()

        # --------------------------------------------------- provider evidence
        fields = probe_command(probe, "DIAG")
        attributes = parse_attributes(fields[2:])
        created = int(attributes.get("session_creations", -1))
        successes = int(attributes.get("stream_successes", -1))
        record(
            "one session creation for two streams",
            "PASS" if created == 1 and successes == 2 else "FAIL",
            f"session_creations={created} stream_successes={successes}",
        )

        fields = probe_command(probe, "RELEASE")
        attributes = parse_attributes(fields[2:])
        live = attributes.get("live_scopes")
        # `RESULT <verdict> <attrs...>`: the verdict is the second token, not the third.
        released = len(fields) > 1 and fields[1] == "RELEASE_OK"
        record(
            "provider release leaves zero scope",
            "PASS" if released and live == "0" else "FAIL",
            f"release_ok={released} live_scopes={live}",
        )
    except (Refused, OSError) as error:
        record("live qualification", "NOT RUN", str(error))
    finally:
        try:
            probe.stdin.write("QUIT\n")
            probe.stdin.flush()
            probe.wait(timeout=15)
        except (OSError, ValueError, subprocess.TimeoutExpired):
            probe.kill()
        control.close()

    return summarise(RESULTS)


# Shortest real Destination is 516 characters. Anything shorter on the accept socket is
# a protocol line, not a destination, and treating it as one would compare a control
# line against a binary fixture.
MIN_PEER_DESTINATION = 516


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
    if failed:
        print("A FAIL names the side that failed; it is never attributed to the router")
        print("or to production code without a stage that says which one it was.")
    print(
        "Note: i2pd 2.61.0 sends a blank line after the peer-Destination line. The peer\n"
        "skips exactly one. A payload whose first byte is a newline is therefore\n"
        "indistinguishable from that blank line on this router, which is why every\n"
        "fixture used here begins with NUL."
    )
    return 1 if failed else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--endpoint",
        default=os.environ.get("I2PR_SAM_ENDPOINT", "127.0.0.1:7656"),
        help="numeric loopback SAM bridge, host:port. Never a name, never non-loopback.",
    )
    parser.add_argument(
        "--probe",
        help="path to a prebuilt production probe binary. Built from this workspace if absent.",
    )
    parser.add_argument("--budget", type=int, default=300, help="per-socket control-line budget")
    parser.add_argument("--drain", type=int, default=180, help="byte-exchange budget")
    args = parser.parse_args()
    try:
        return run(args)
    except Refused as error:
        print(f"REFUSED: {error}")
        return 2


if __name__ == "__main__":
    sys.exit(main())