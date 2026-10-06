# Initial runtime security boundary

The runtime accepts only a validated `I2pEndpoint` and calls only `I2pStreamProvider` for upstream connections. Local downstream I/O arrives only through `LocalAcceptor`. It adds no DNS, generic TCP, HTTP, proxy, SAM, router-private, or remote-listener path, and the static guard enforces that over the runtime sources as well as core and wire.

Runtime identity strings and configured channels reject CR/LF injection and have explicit length/count ceilings. Client prefixes are rejected before upstream routing. SASL PLAIN secrets are redacted by `Debug`, credential lengths are bounded, and temporary raw/base64 buffers are zeroized after encoding. Raw protocol and SASL payloads are not logged.

Snapshots and diagnostics carry no message payloads, endpoints, or credentials. A shutdown fence prevents queued user traffic from reaching the network after an explicit stop, so cancelled work is never flushed.

## Anonymity qualification (M004)

CTCP/DCC mediation, client-tag policy, and the upstream-fingerprint review are now
implemented and qualified. They live in `ctcp-dcc-policy.md` and `ircv3.md`; the closure
evidence is `plans/closure/bouncer-core/018-status.md`.

Three properties are worth stating here because they are the ones a reader will want to
check first:

- **Nothing local becomes IRC-visible.** No environment-derived hostname, username,
  OS/router version, local path, process id, or machine identifier is inserted into any
  IRC-visible field. No build, router, or version string is exposed by default. The CTCP
  auto-answer is a fixed constant that echoes only the probe's own token.
- **No client is prompted to describe itself.** A CTCP probe from upstream is answered by
  the bouncer and never relayed to a client, because a client that answers one has leaked
  its own software and hostname.
- **No DCC path exists.** `DCC` is recognised only in order to be blocked. No DCC parameter
  is ever parsed into a usable value and no code path turns one into a connection, address,
  or offer.

Raw IRC logging is disabled by default in the strongest available sense: the workspace
depends on no logging facade at all, and no production source writes to stdout or stderr.
There is therefore no sink a raw frame could reach, whether configured deliberately or by
accident.

The boundary still does not make an operator anonymous in general. A user who deliberately
types their own hostname, or runs a client configured to answer probes, is outside what any
bouncer can mediate.
