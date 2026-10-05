#!/usr/bin/env python3
"""Check source, build scripts, manifests, and dependency trees for forbidden egress.

Every first-party crate that could own network access is scanned, including the
runtime that owns the upstream connection and the downstream client sockets. The
positive controls exercise the same predicates and the same crate scoping as the
real scan, so a future coverage regression fails loudly instead of silently
narrowing the boundary.
"""
from pathlib import Path
import re
import subprocess
import sys
import tempfile

ROOT=Path(__file__).resolve().parents[1]
CRATES=("core","wire","runtime","testkit")
SOURCE_TOKENS=("std::net::Tcp","std::net::Udp","ToSocketAddrs","tokio::net::Tcp","tokio::net::Udp","reqwest","hyper::Client","trust_dns","hickory_resolver","ureq::","socks::")
DEPENDENCY_NAMES={"reqwest","hyper","hyper-util","ureq","isahc","surf","trust-dns-resolver","hickory-resolver","socks","tokio-socks","async-socks5","smol-hyper"}

def source_findings(label,text):
    return [(label,token) for token in SOURCE_TOKENS if token in text]

def manifest_findings(label,text):
    names=re.findall(r"(?m)^\s*([A-Za-z0-9_-]+)\s*=\s*\{?",text)
    return [(label,name) for name in names if name.replace('_','-').lower() in DEPENDENCY_NAMES]

def dependency_findings(label,tree):
    found=[]
    for line in tree.splitlines():
        name=line.strip().split()[0] if line.strip() else ""
        normalized=name.split('@')[0].lower()
        if normalized in DEPENDENCY_NAMES: found.append((label,normalized))
    return found

def scan_sources(root,crates=CRATES):
    found=[]
    for crate in crates:
        base=root/"crates"/crate
        if not base.exists():continue
        for path in sorted(base.rglob("*.rs")):
            found.extend(source_findings(path,path.read_text()))
        for path in (base/"Cargo.toml",base/"build.rs"):
            if path.exists():found.extend(manifest_findings(path,path.read_text()))
    return found

def scan_dependency_trees(root,crates=CRATES):
    found=[]
    for crate in crates:
        package=f"i2pr-irc-{crate}"
        result=subprocess.run(["cargo","tree","--locked","--target","all","--prefix","none","-p",package,"-e","all"],cwd=root,text=True,capture_output=True)
        if result.returncode:raise RuntimeError(f"cargo tree failed for {package}: {result.stderr.strip()}")
        found.extend(dependency_findings(package,result.stdout))
    return found

def positive_control_failures():
    """Each control fails unless the real scan would also fail."""
    failures=[]
    if not source_findings("fixture.rs","use std::net::TcpStream;"):
        failures.append("source predicate")
    if not manifest_findings("fixture.toml","reqwest = { version = \"1\" }"):
        failures.append("manifest predicate")
    if not dependency_findings("fixture tree","hyper v1.0.0"):
        failures.append("dependency predicate")
    with tempfile.TemporaryDirectory() as temporary:
        fixture=Path(temporary)
        for crate in CRATES:(fixture/"crates"/crate/"src").mkdir(parents=True)
        (fixture/"crates"/"runtime"/"src"/"lib.rs").write_text("use std::net::TcpStream;\n")
        (fixture/"crates"/"runtime"/"Cargo.toml").write_text("[dependencies]\nreqwest = { version = \"1\" }\n")
        (fixture/"crates"/"wire"/"src"/"lib.rs").write_text("pub fn clean() {}\n")
        detected=scan_sources(fixture)
        runtime_source=[finding for finding in detected if "crates/runtime/src" in str(finding[0])]
        runtime_manifest=[finding for finding in detected if "crates/runtime/Cargo.toml" in str(finding[0])]
        if not runtime_source:failures.append("runtime source scope")
        if not runtime_manifest:failures.append("runtime manifest scope")
        if any("crates/wire" in str(finding[0]) for finding in detected):
            failures.append("crate scope over-reports clean fixtures")
    return failures

def main():
    failures=positive_control_failures()
    if failures:
        print("network boundary positive control failed: "+", ".join(failures),file=sys.stderr)
        return 1
    try:
        found=scan_sources(ROOT)+scan_dependency_trees(ROOT)
    except RuntimeError as error:
        print(error,file=sys.stderr)
        return 1
    if found:
        for path,token in found:print(f"forbidden network ownership in {path}: {token}",file=sys.stderr)
        return 1
    return 0
if __name__=="__main__":raise SystemExit(main())
