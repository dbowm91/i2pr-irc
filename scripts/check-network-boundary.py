#!/usr/bin/env python3
"""Check source, build scripts, manifests, and dependency trees for core/wire egress."""
from pathlib import Path
import re
import subprocess
import sys

ROOT=Path(__file__).resolve().parents[1]
SOURCE_TOKENS=("std::net::Tcp","std::net::Udp","ToSocketAddrs","tokio::net::Tcp","tokio::net::Udp","reqwest","hyper::Client","trust_dns","hickory_resolver","ureq::","socks::")
DEPENDENCY_NAMES={"reqwest","hyper","hyper-util","ureq","isahc","surf","trust-dns-resolver","hickory-resolver","socks","tokio-socks","async-socks5","smol-hyper"}

def source_findings(label:str,text:str):
    return [(label,token) for token in SOURCE_TOKENS if token in text]

def manifest_findings(label:str,text:str):
    names=re.findall(r"(?m)^\s*([A-Za-z0-9_-]+)\s*=\s*\{?",text)
    return [(label,name) for name in names if name.replace('_','-').lower() in DEPENDENCY_NAMES]

def dependency_findings(label:str,tree:str):
    found=[]
    for line in tree.splitlines():
        name=line.strip().split()[0] if line.strip() else ""
        normalized=name.split('@')[0].lower()
        if normalized in DEPENDENCY_NAMES: found.append((label,normalized))
    return found

def scan_workspace(root:Path):
    found=[]
    for crate in ("core","wire"):
        base=root/"crates"/crate
        if not base.exists():continue
        for path in base.rglob("*.rs"):
            found.extend(source_findings(path, path.read_text()))
        for path in (base/"Cargo.toml",base/"build.rs"):
            if path.exists():found.extend(manifest_findings(path,path.read_text()))
        package=f"i2pr-irc-{crate}"
        result=subprocess.run(["cargo","tree","--locked","--target","all","--prefix","none","-p",package,"-e","all"],cwd=root,text=True,capture_output=True)
        if result.returncode:raise RuntimeError(f"cargo tree failed for {package}: {result.stderr.strip()}")
        found.extend(dependency_findings(package,result.stdout))
    return found

def main():
    # Each positive-control category uses the same predicate as the real workspace scan.
    if not source_findings("fixture.rs","use std::net::TcpStream;"):
        print("source positive control failed",file=sys.stderr);return 1
    if not manifest_findings("fixture.toml","reqwest = { version = \"1\" }"):
        print("manifest positive control failed",file=sys.stderr);return 1
    if not dependency_findings("fixture tree","hyper v1.0.0"):
        print("dependency positive control failed",file=sys.stderr);return 1
    try: found=scan_workspace(ROOT)
    except RuntimeError as error: print(error,file=sys.stderr);return 1
    if found:
        for path,token in found:print(f"forbidden network ownership in {path}: {token}",file=sys.stderr)
        return 1
    return 0
if __name__=="__main__":raise SystemExit(main())
