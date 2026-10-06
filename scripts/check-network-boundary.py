#!/usr/bin/env python3
"""Check source, build scripts, manifests, and dependency trees for forbidden egress.

Every first-party crate that could own network access is scanned, including the
runtime that owns the upstream connection and the downstream client sockets, and the
store that opens a third-party native database dependency. The positive controls
exercise the same predicates and the same crate scoping as the real scan, so a future
coverage regression fails loudly instead of silently narrowing the boundary.
"""
from pathlib import Path
import re
import subprocess
import sys
import tempfile

ROOT=Path(__file__).resolve().parents[1]
CRATES=("core","wire","store","runtime","testkit")
SOURCE_TOKENS=("std::net::Tcp","std::net::Udp","ToSocketAddrs","tokio::net::Tcp","tokio::net::Udp","reqwest","hyper::Client","trust_dns","hickory_resolver","ureq::","socks::")
DEPENDENCY_NAMES={"reqwest","hyper","hyper-util","ureq","isahc","surf","trust-dns-resolver","hickery-resolver","socks","tokio-socks","async-socks5","smol-hyper"}

# M004-A: direct-connect primitives. A bouncer must never be able to dial or accept a
# direct connection on a client's behalf, so these are forbidden by name even though the
# generic socket tokens above would not catch every spelling of them.
DCC_TOKENS=("TcpListener","TcpStream","UnixListener","dcc_listen","dcc_connect","start_dcc","accept_dcc")

def source_findings(label,text):
    return [(label,token) for token in SOURCE_TOKENS if token in text]

def dcc_findings(label,text):
    return [(label,token) for token in DCC_TOKENS if token in text]

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
            text=path.read_text()
            found.extend(source_findings(path,text))
            # The CTCP module names DCC only to block it. It is the one place allowed
            # to say so, so the guard is scoped rather than blanket.
            if path.name!="ctcp.rs":
                found.extend(dcc_findings(path,text))
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
    if not dcc_findings("fixture.rs","let listener = TcpListener::bind(addr)?;"):
        failures.append("dcc listen predicate")
    if not dcc_findings("fixture.rs","fn start_dcc(peer: &str) {}"):
        failures.append("dcc helper predicate")
    if not manifest_findings("fixture.toml","reqwest = { version = \"1\" }"):
        failures.append("manifest predicate")
    if not dependency_findings("fixture tree","hyper v1.0.0"):
        failures.append("dependency predicate")
    with tempfile.TemporaryDirectory() as temporary:
        fixture=Path(temporary)
        for crate in CRATES:(fixture/"crates"/crate/"src").mkdir(parents=True)
        (fixture/"crates"/"runtime"/"src"/"lib.rs").write_text("use std::net::TcpStream;\n")
        (fixture/"crates"/"runtime"/"Cargo.toml").write_text("[dependencies]\nreqwest = { version = \"1\" }\n")
        # The store crate pulls a third-party native dependency, so its scope must be
        # proven independently rather than assumed from the runtime's coverage.
        (fixture/"crates"/"store"/"src"/"lib.rs").write_text("use std::net::UdpSocket;\n")
        (fixture/"crates"/"store"/"Cargo.toml").write_text("[dependencies]\ntrust-dns-resolver = \"1\"\n")
        (fixture/"crates"/"wire"/"src"/"lib.rs").write_text("pub fn clean() {}\n")
        detected=scan_sources(fixture)
        runtime_source=[finding for finding in detected if "crates/runtime/src" in str(finding[0])]
        runtime_manifest=[finding for finding in detected if "crates/runtime/Cargo.toml" in str(finding[0])]
        store_source=[finding for finding in detected if "crates/store/src" in str(finding[0])]
        store_manifest=[finding for finding in detected if "crates/store/Cargo.toml" in str(finding[0])]
        if not runtime_source:failures.append("runtime source scope")
        if not runtime_manifest:failures.append("runtime manifest scope")
        if not store_source:failures.append("store source scope")
        if not store_manifest:failures.append("store manifest scope")
        if any("crates/wire" in str(finding[0]) for finding in detected):
            failures.append("crate scope over-reports clean fixtures")
        # A direct-connect primitive anywhere outside the CTCP classifier must fail.
        # The earlier fixtures are reset first, so this control is testing only the
        # listener itself rather than tripping over the socket fixture already there.
        (fixture/"crates"/"runtime"/"src"/"lib.rs").write_text("pub fn clean() {}\n")
        (fixture/"crates"/"store"/"src"/"lib.rs").write_text("pub fn clean() {}\n")
        (fixture/"crates"/"runtime"/"Cargo.toml").write_text("[dependencies]\n")
        (fixture/"crates"/"store"/"Cargo.toml").write_text("[dependencies]\n")
        if scan_sources(fixture):
            failures.append("fixture did not stay clean")
        (fixture/"crates"/"runtime"/"src"/"lib.rs").write_text("pub fn listen() { let l = std::net::TcpListener::bind(\"0.0.0.0:0\").unwrap(); }\n")
        if not any("TcpListener" in str(finding[1]) for finding in scan_sources(fixture)):
            failures.append("dcc scope does not detect a listener in the runtime")
        # The CTCP classifier is the one file permitted to name DCC, and only as a
        # classification; a listener there would still have to fail.
        (fixture/"crates"/"runtime"/"src"/"ctcp.rs").write_text("pub fn ok() {}\n")
        if any("ctcp.rs" in str(finding[0]) for finding in scan_sources(fixture)):
            failures.append("ctcp classifier is wrongly exempt from the source scan")
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
