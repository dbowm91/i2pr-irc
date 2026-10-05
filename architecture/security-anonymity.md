# Initial runtime security boundary

The runtime accepts only a validated `I2pEndpoint` and calls only `I2pStreamProvider` for upstream connections. Local downstream I/O arrives only through `LocalAcceptor`. It adds no DNS, generic TCP, HTTP, proxy, SAM, router-private, or remote-listener path.

Runtime identity strings and configured channels reject CR/LF injection and have explicit length/count ceilings. Client prefixes are rejected before upstream routing. SASL PLAIN secrets are redacted by `Debug`, credential lengths are bounded, and temporary raw/base64 buffers are zeroized after encoding. Raw protocol and SASL payloads are not logged.

This is an initial one-client security boundary, not the later M004 anonymity qualification. CTCP/DCC filtering and broader environment metadata review remain later work.
