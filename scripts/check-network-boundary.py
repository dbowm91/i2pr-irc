#!/usr/bin/env python3
"""Enforce the M001 core/wire networking ownership boundary."""
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
FORBIDDEN = ("std::net::Tcp", "std::net::Udp", "ToSocketAddrs", "tokio::net::Tcp", "tokio::net::Udp", "reqwest", "hyper", "trust-dns", "hickory-resolver")

def scan_text(label: str, text: str):
    return [(label, token) for token in FORBIDDEN if token in text]

def violations(base: Path):
    found=[]
    for crate in ("core", "wire"):
        path=base/"crates"/crate
        if not path.exists(): continue
        for source in list(path.glob("src/**/*.rs"))+[path/"Cargo.toml"]:
            found.extend(scan_text(source, source.read_text()))
    return found

def main():
    if violations(ROOT):
        for path,token in violations(ROOT): print(f"forbidden network boundary: {path}: {token}",file=sys.stderr)
        return 1
    # Positive control: the same source scan must reject a prohibited API.
    fixture="std::net::TcpStream::connect(\"example.com\")"
    if not scan_text("positive-control", fixture):
        print("network guard positive control failed",file=sys.stderr);return 1
    return 0
if __name__=="__main__": raise SystemExit(main())
