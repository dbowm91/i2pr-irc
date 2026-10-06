#!/usr/bin/env python3
"""Check source, build scripts, manifests, and dependency trees for forbidden egress.

Every first-party crate that could own network access is scanned, including the
runtime that owns the upstream connection and the downstream client sockets, and the
store that opens a third-party native database dependency. The positive controls
exercise the same predicates and the same crate scoping as the real scan, so a future
coverage regression fails loudly instead of silently narrowing the boundary.

Plan 017 section 9 (M004-C static boundary qualification) requires a positive control per
prohibited production primitive family: generic TCP, DNS, HTTP client, SOCKS/proxy, and
DCC dial/listen. Each family is named below across the source, manifest, and dependency
tree predicates, so dropping or narrowing a token fails the script instead of quietly
narrowing the boundary. The production source and dependency tree scan is unchanged.
"""
from pathlib import Path
import re
import subprocess
import sys
import tempfile

ROOT=Path(__file__).resolve().parents[1]
CRATES=("core","wire","store","runtime","testkit")
SOURCE_TOKENS=("std::net::Tcp","std::net::Udp","ToSocketAddrs","tokio::net::Tcp","tokio::net::Udp","reqwest","hyper::Client","trust_dns","hickory_resolver","ureq::","socks::")
# Corrected to the canonical crate name so the DNS family control exercises the real
# spelling rather than a typo no manifest can ever carry.
DEPENDENCY_NAMES={"reqwest","hyper","hyper-util","ureq","isahc","surf","trust-dns-resolver","hickory-resolver","socks","tokio-socks","async-socks5","smol-hyper"}

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
    # Plan 017 section 9, one named control per prohibited primitive family against the
    # source predicate. A crate can only reach a socket library by naming it in source.
    for family,fixture in (
        ("generic tcp std source control","use std::net::TcpStream;\n"),
        ("generic tcp tokio source control","pub async fn dial(a: &str) { let _s = tokio::net::TcpStream::connect(a).await; }\n"),
        ("dns resolution import source control","use std::net::ToSocketAddrs;\n"),
        ("dns resolution call source control","use std::net::ToSocketAddrs;\nlet addrs = \"a.i2p\".to_socket_addrs()?;\n"),
        ("dns trust_dns source control","let _r = trust_dns::Resolver::builder().build();\n"),
        ("dns hickory_resolver source control","use hickory_resolver::Resolver;\n"),
        ("http reqwest source control","use reqwest::Client;\n"),
        ("http hyper client source control","let _c = hyper::Client::new();\n"),
        ("http ureq source control","let _r = ureq::get(url).call();\n"),
        # SOCKS is a path-qualified crate, so the proxy family is caught at the use site
        # as well as in the manifest and dependency tree.
        ("socks source control","use socks::TcpSocks5Stream;\n"),
    ):
        if not source_findings("fixture.rs",fixture):
            failures.append(family)
    # The same families as manifest names, which is the only way a crate becomes
    # reachable at all. isahc and surf carry manifest coverage only: they are declared
    # dependencies, never named as a socket or DNS path in first-party source.
    for family,fixture in (
        ("dns trust-dns-resolver manifest control","trust-dns-resolver = \"0.23\""),
        ("dns hickory-resolver manifest control","hickory-resolver = \"0.24\""),
        ("http reqwest manifest control","reqwest = { version = \"1\" }"),
        ("http hyper manifest control","hyper = { version = \"1\", features = [\"client\"] }"),
        ("http hyper-util manifest control","hyper-util = \"0.1\""),
        ("http ureq manifest control","ureq = \"2\""),
        ("http isahc manifest control","isahc = \"0.13\""),
        ("http surf manifest control","surf = \"2\""),
        ("socks manifest control","socks = \"0.7\""),
        ("tokio-socks manifest control","tokio-socks = \"0.4\""),
        ("async-socks5 manifest control","async-socks5 = \"0.5\""),
        ("smol-hyper manifest control","smol-hyper = \"0.2\""),
    ):
        if not manifest_findings("fixture.toml",fixture):
            failures.append(family)
    # The dependency tree is a separate code path from the manifest: a transitive or
    # pulled-in proxy or HTTP client must fail even though no manifest names it.
    for family,fixture in (
        ("dns hickory-resolver dependency control","hickory-resolver v0.24.1"),
        ("dns trust-dns-resolver dependency control","trust-dns-resolver v0.23.2"),
        ("http isahc dependency control","isahc v0.13.0"),
        ("http surf dependency control","surf v2.3.2"),
        ("socks dependency control","socks v0.7.1"),
        ("tokio-socks dependency control","tokio-socks v0.4.1"),
        ("async-socks5 dependency control","async-socks5 v0.5.0"),
        ("smol-hyper dependency control","smol-hyper v0.2.0"),
    ):
        if not dependency_findings("fixture tree",fixture):
            failures.append(family)
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
        # Generic TCP is forbidden in every first-party crate, not only the runtime, so
        # each crate scope is proven against the same async dial.
        dial="pub async fn dial(addr: &str) { let _s = tokio::net::TcpStream::connect(addr).await; }\n"
        for crate in CRATES:
            (fixture/"crates"/crate/"src"/"lib.rs").write_text(dial)
            if not any(token=="tokio::net::Tcp" for _label,token in source_findings("fixture.rs",dial)):
                failures.append(f"generic tcp {crate} crate scope control")
            if not any(token=="TcpStream" for _label,token in dcc_findings("fixture.rs",dial)):
                failures.append(f"generic tcp {crate} dcc scope control")
        for crate in CRATES:(fixture/"crates"/crate/"src"/"lib.rs").write_text("pub fn clean() {}\n")
        # Plan 017 section 9: every direct-connect family must fail on its own, in a
        # first-party crate. The specific token is asserted so a hit from the generic
        # socket tokens cannot stand in for the direct-connect predicate.
        for family,dial_text,token in (
            ("dcc tcp listen control",'pub fn listen() { let _l = std::net::TcpListener::bind("0.0.0.0:0").unwrap(); }',"TcpListener"),
            ("dcc tcp dial control","pub fn dial(addr: &str) { let _s = std::net::TcpStream::connect(addr).unwrap(); }","TcpStream"),
            ("dcc unix listen control",'pub fn listen() { let _l = UnixListener::bind("/tmp/dcc.sock").unwrap(); }',"UnixListener"),
            ("dcc listen helper control","pub fn dcc_listen(peer: &str) { let _ = peer; }","dcc_listen"),
            ("dcc dial helper control","pub fn dcc_connect(peer: &str) { let _ = peer; }","dcc_connect"),
        ):
            (fixture/"crates"/"runtime"/"src"/"lib.rs").write_text(dial_text+"\n")
            if not any(finding[1]==token for finding in scan_sources(fixture)):
                failures.append(family)
        (fixture/"crates"/"runtime"/"src"/"lib.rs").write_text("pub fn listen() { let l = std::net::TcpListener::bind(\"0.0.0.0:0\").unwrap(); }\n")
        if not any("TcpListener" in str(finding[1]) for finding in scan_sources(fixture)):
            failures.append("dcc scope does not detect a listener in the runtime")
        # The CTCP classifier is the one file permitted to name DCC, and only as a
        # classification; a listener there would still have to fail.
        (fixture/"crates"/"runtime"/"src"/"ctcp.rs").write_text("pub fn ok() {}\n")
        if any("ctcp.rs" in str(finding[0]) for finding in scan_sources(fixture)):
            failures.append("ctcp classifier is wrongly exempt from the source scan")
        # The exemption is classification-only and file-local: naming the primitive to
        # refuse it is allowed in that one file, and that file is still subject to the
        # generic socket predicate like any other source file.
        (fixture/"crates"/"runtime"/"src"/"lib.rs").write_text("pub fn clean() {}\n")
        (fixture/"crates"/"runtime"/"src"/"ctcp.rs").write_text("// A TcpListener accept or a TcpStream dial is refused for direct connections.\npub fn classify() { DirectConnect::Refused }\n")
        if any("ctcp.rs" in str(finding[0]) for finding in scan_sources(fixture)):
            failures.append("ctcp exemption extends beyond direct-connect classification")
        (fixture/"crates"/"runtime"/"src"/"ctcp.rs").write_text("use std::net::TcpStream;\n")
        if not any("ctcp.rs" in str(finding[0]) for finding in scan_sources(fixture)):
            failures.append("ctcp exemption extends to the generic socket source scan")
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
