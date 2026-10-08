#!/usr/bin/env python3
"""Exercise every required eggchaos TCP fault against a loopback echo peer."""
from __future__ import annotations

import pathlib
import socket
import socketserver
import subprocess
import tempfile
import threading
import time
import os


class Echo(socketserver.BaseRequestHandler):
    completed = 0
    lock = threading.Lock()

    def handle(self) -> None:
        while True:
            try:
                data = self.request.recv(65536)
            except OSError:
                return
            if not data:
                with self.lock:
                    type(self).completed += 1
                return
            try:
                self.request.sendall(data)
            except OSError:
                return


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def recv_all(sock: socket.socket, expected: int, timeout: float = 8.0) -> bytes:
    sock.settimeout(timeout)
    chunks = bytearray()
    while len(chunks) < expected:
        try:
            part = sock.recv(min(65536, expected - len(chunks)))
        except socket.timeout:
            break
        if not part:
            break
        chunks.extend(part)
    return bytes(chunks)


def main() -> None:
    binary = os.environ["EGGCHAOS_BIN"]
    server = Server(("127.0.0.1", 0), Echo)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    target = f"127.0.0.1:{server.server_address[1]}"
    definitions = [
        ("stable", '[[proxy.fault]]\nid="latency-jitter"\ndirection="upstream"\ntype="latency"\ndelay="20ms"\njitter="10ms"\n'),
        ("bandwidth", '[[proxy.fault]]\nid="rate"\ndirection="upstream"\ntype="bandwidth"\nbytes_per_second=32768\nburst_bytes=4096\n'),
        ("blackhole", '[[proxy.fault]]\nid="sink"\ndirection="upstream"\ntype="blackhole"\n'),
        ("slow-close", '[[proxy.fault]]\nid="shutdown-delay"\ndirection="downstream"\ntype="slow_close"\ndelay="150ms"\n'),
        ("slicing", '[[proxy.fault]]\nid="small-writes"\ndirection="downstream"\ntype="slice"\naverage_size=5\nvariation=2\n'),
        ("disconnect", '[[proxy.fault]]\nid="cut"\ndirection="upstream"\ntype="disconnect"\ndelay="5ms"\n'),
        ("stream-loss", '[[proxy.fault]]\nid="loss"\ndirection="upstream"\ntype="stream-loss"\nloss_rate=1.0\ncorrelation=1.0\n'),
    ]
    blocks = []
    ports = {}
    for name, fault in definitions:
        port = free_port()
        ports[name] = port
        blocks.append(f'[[proxy]]\nname="{name}"\nlisten="127.0.0.1:{port}"\nupstream="{target}"\n{fault}')
    config = "version=1\nseed=410041\n[admin]\nbind=\"127.0.0.1:{0}\"\n".format(free_port()) + "\n".join(blocks)
    with tempfile.TemporaryDirectory(prefix="i2pr-irc-eggchaos-") as directory:
        path = pathlib.Path(directory) / "scenarios.toml"
        path.write_text(config)
        child = subprocess.Popen([binary, "serve", "--config", str(path)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if child.poll() is not None:
                    raise RuntimeError("eggchaos stopped before proxy ports opened")
                try:
                    with socket.create_connection(("127.0.0.1", ports["stable"]), timeout=0.1):
                        break
                except OSError:
                    time.sleep(0.03)
            payload = bytes(range(256)) * 128
            for _ in range(100):
                with socket.create_connection(("127.0.0.1", ports["stable"]), timeout=3) as client:
                    client.settimeout(3)
                    client.sendall(b"reconnect-cycle")
                    if recv_all(client, len(b"reconnect-cycle"), timeout=3) != b"reconnect-cycle":
                        raise AssertionError("repeated stable stream cycle lost bytes")
            if Echo.completed < 100:
                raise AssertionError(f"only {Echo.completed} streams settled")
            print(f"PASS repeated-churn cycles={Echo.completed}")
            for name in ("stable", "bandwidth", "slow-close", "slicing"):
                with socket.create_connection(("127.0.0.1", ports[name]), timeout=3) as client:
                    client.settimeout(10)
                    client.sendall(payload)
                    echoed = recv_all(client, len(payload), timeout=12)
                    if echoed != payload:
                        raise AssertionError(f"{name}: expected {len(payload)} echoed bytes, got {len(echoed)}")
                    if name == "slow-close":
                        start = time.monotonic()
                        client.shutdown(socket.SHUT_WR)
                        client.settimeout(3)
                        while client.recv(1):
                            pass
                        if time.monotonic() - start < 0.10:
                            raise AssertionError("slow-close did not delay stream shutdown")
                print(f"PASS {name} bytes={len(echoed)}")
            with socket.create_connection(("127.0.0.1", ports["blackhole"]), timeout=3) as client:
                client.settimeout(0.25)
                client.sendall(b"synthetic blackhole probe")
                try:
                    data = client.recv(64)
                except socket.timeout:
                    data = b""
                if data:
                    raise AssertionError("blackhole forwarded client bytes")
                print("PASS blackhole bytes=0")
            with socket.create_connection(("127.0.0.1", ports["disconnect"]), timeout=3) as client:
                client.settimeout(3)
                client.sendall(payload)
                recv_all(client, len(payload), timeout=3)
                try:
                    closed = client.recv(1) == b""
                except OSError:
                    closed = True
                if not closed:
                    raise AssertionError("disconnect did not terminate the stream")
                print("PASS disconnect")
            with socket.create_connection(("127.0.0.1", ports["stream-loss"]), timeout=3) as client:
                client.settimeout(0.25)
                client.sendall(payload)
                try:
                    echoed = client.recv(65536)
                except socket.timeout:
                    echoed = b""
                if echoed:
                    raise AssertionError("100% stream-loss forwarded bytes")
                print("PASS stream-loss bytes=0")
        finally:
            child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
            server.shutdown()
            server.server_close()


if __name__ == "__main__":
    main()
