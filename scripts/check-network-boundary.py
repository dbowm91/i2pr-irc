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

Plan 018 section 3 (M004-D final audit) found that `fuzz-smoke` was absent from the crate
list. That crate is a binary rather than a library, but M004 gave it a dependency on the
runtime, so anything it can reach the network through is reachable from it. A boundary that
depends on which crate a file happens to live in is not a boundary.
"""
from pathlib import Path
import re
import subprocess
import sys
import tempfile

ROOT=Path(__file__).resolve().parents[1]
CRATES=("core","wire","store","runtime","sam","testkit","fuzz-smoke")
# R001-B: `crates/sam` is the only production path permitted to open a TCP socket, and
# only ever to a loopback SAM bridge. It is scanned like every other crate for the generic
# primitives, and separately for socket authority, because "loopback only" is a claim this
# script has to be able to fail rather than a comment in a source file.
SAM_CRATE="sam"
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

# R001-B section 13. Every symbol that can reach a socket, named exhaustively so that
# renaming one fails the control instead of quietly widening the crate's authority.
SAM_SOCKET_TOKENS=("TcpStream","TcpListener","tokio::net","TcpSocket","UdpSocket","UnixStream","UnixListener")
# The only two files permitted to name a socket: the client that must connect, and the
# test-only fake that must listen. Anything else is an authority the crate did not need.
SAM_SOCKET_ALLOWLIST=("src/client.rs","src/fake.rs")

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

def _sam_allowlisted(path):
    """Whether `path` is one of the two SAM files permitted to name a TCP socket."""
    tail=path.as_posix().split(f"crates/{SAM_CRATE}/",1)[-1]
    return any(tail==allowed for allowed in SAM_SOCKET_ALLOWLIST)

def sam_findings(path,text):
    """Socket authority inside the SAM crate, which is allowed exactly two files.

    Compared on the path *relative to `crates/sam/src`*, not on an absolute suffix, so a
    fixture tree laid out anywhere on disk is held to the same rule as the real crate.
    """
    relative=path.as_posix()
    if not relative.endswith(".rs"):
        return []
    if not _sam_allowlisted(path):
        return [(relative,token) for token in SAM_SOCKET_TOKENS if token in text]
    # Even in an allowed file, a *generic* socket is not acceptable: only a loopback
    # TCP connect in the client and a loopback bind in the fake.
    findings=[]
    # The generic primitives stay forbidden even in an allowlisted file. A resolver is
    # the important one: "loopback only" is enforced by never resolving anything, so a
    # name that reached `to_socket_addrs` would defeat the whole type-level boundary.
    for token in ("UdpSocket","UnixStream","UnixListener","TcpSocket","ToSocketAddrs"):
        if token in text:
            findings.append((relative,token))
    if path.as_posix().split(f"crates/{SAM_CRATE}/",1)[-1]=="src/fake.rs" and "TcpStream" in text:
        # The fake may listen and accept, and may connect back only in a test.
        if "TcpListener" not in text:
            findings.append((relative,"TcpStream without a listener"))
    return findings

def scan_sources(root,crates=CRATES):
    found=[]
    for crate in crates:
        base=root/"crates"/crate
        if not base.exists():continue
        for path in sorted(base.rglob("*.rs")):
            text=path.read_text()
            # R001-B: `tokio::net` is in the generic socket predicate and the SAM client
            # has to name it. Exempted for exactly the two allowlisted files, and
            # compensated by `sam_findings`, which is the stricter predicate: it permits
            # only those two files and only TCP, so the exemption cannot become a door to
            # DNS, UDP, or a unix socket.
            if not (crate==SAM_CRATE and _sam_allowlisted(path)):
                found.extend(source_findings(path,text))
            # The CTCP module names DCC only to block it. It is the one place allowed
            # to say so, so the guard is scoped rather than blanket.
            #
            # R001-B: the SAM client and its test-only fake are the second exemption, for
            # a different reason. They must name a TCP socket to do their job, so the
            # blanket DCC predicate would flag the crate that is *supposed* to own the
            # only socket authority in the workspace. The exemption is compensated by
            # `sam_findings`, which is stricter: it names the exact two files allowed to
            # hold a socket and the exact socket families even those may not use. A
            # blanket `path.name` exemption with nothing behind it would be a hole; this
            # one has a narrower predicate in front of it.
            exempt = path.name=="ctcp.rs" or (crate==SAM_CRATE and _sam_allowlisted(path))
            if not exempt:
                found.extend(dcc_findings(path,text))
            if crate==SAM_CRATE:
                found.extend(sam_findings(path,text))
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
    # R001-B section 13 positive controls. Each must fail on its own, so a future
    # narrowing of the SAM allowlist fails the script instead of quietly relaxing the
    # boundary it exists to enforce.
    for family,relpath,text,token in (
        ("sam tcp authority outside the allowlist control","src/endpoint.rs","pub fn open() { let _s = TcpStream::connect(addr).await; }","TcpStream"),
        ("sam listener authority outside the allowlist control","src/protocol.rs","pub fn listen() { let _l = TcpListener::bind(addr).unwrap(); }","TcpListener"),
        ("sam udp in an allowlisted file control","src/client.rs","pub fn send() { let _s = UdpSocket::bind(addr).unwrap(); }","UdpSocket"),
        ("sam unix in an allowlisted file control","src/client.rs","pub fn dial() { let _s = UnixStream::connect(path).unwrap(); }","UnixStream"),
        ("sam raw-socket control","src/client.rs","pub fn send() { let _s = TcpSocket::new_v4().unwrap(); }","TcpSocket"),
        ("sam udp in the fake control","src/fake.rs","pub fn send() { let _s = UdpSocket::bind(addr).unwrap(); }","UdpSocket"),
        ("sam resolver authority control","src/client.rs","pub fn resolve() { let _a = ToSocketAddrs::to_socket_addrs(addr); }","ToSocketAddrs"),
        ("sam dns in a non-allowlisted file control","src/session.rs","pub fn resolve() { let _a = ToSocketAddrs::to_socket_addrs(addr); }","ToSocketAddrs"),
    ):
        with tempfile.TemporaryDirectory() as temporary:
            # `scan_sources` expects a workspace root containing `crates/<name>`.
            root=Path(temporary)/"root"
            (root/"crates"/SAM_CRATE/"src").mkdir(parents=True)
            (root/"crates"/SAM_CRATE/relpath).write_text(text+"\n")
            found=scan_sources(root,crates=(SAM_CRATE,))
            if not any(finding[1]==token for finding in found):
                failures.append(family)
    # The allowlist itself must be load-bearing: a socket in the two permitted files is
    # fine, and in any other file it is not. Proven by removing one and expecting a hit.
    with tempfile.TemporaryDirectory() as temporary:
        fixture=Path(temporary)
        (fixture/"crates"/"sam"/"src").mkdir(parents=True)
        (fixture/"crates"/"sam"/"src"/"client.rs").write_text("pub fn connect() { let _s = TcpStream::connect(addr).await; }\n")
        (fixture/"crates"/"sam"/"src"/"fake.rs").write_text("pub fn listen() { let _l = TcpListener::bind(addr).await.unwrap(); }\n")
        if scan_sources(fixture,crates=(SAM_CRATE,)):
            failures.append("sam fixture did not stay clean")
        (fixture/"crates"/"sam"/"src"/"session.rs").write_text("pub fn open() { let _s = TcpStream::connect(addr).await; }\n")
        if not scan_sources(fixture,crates=(SAM_CRATE,)):
            failures.append("sam socket authority outside the allowlist is not detected")
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
