# Downstream session

The first local session is an IRC server endpoint over `LocalAcceptor`. It requires NICK and USER before sending 001; the NICK must match the configured current upstream network nick under negotiated casemapping. It answers client PING locally, handles CAP LS/REQ/END with an empty advertised capability set, and routes a bounded command allowlist upstream. Unsupported commands receive 421. Client-supplied prefixes are rejected, and client tag budgets are checked before re-encoding.

The online state projection retains bounded channel membership, topic, and ISUPPORT sets. On registration it emits the current nick, supported tokens, joined channels, topics, NAMES, and end-of-NAMES numerics. Upstream events continue to be forwarded to the attached session. Disconnecting the local client ends this session; the supervisor owns the connection cleanup.

The runtime does not advertise message-tags, batch, SASL, or other downstream capabilities because it does not implement those semantics for downstream clients.
