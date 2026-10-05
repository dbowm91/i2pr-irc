# Capability mediation

Upstream negotiation does not request message-tags, batch, server-time, echo-message, or other features whose downstream semantics are not implemented. It requests only SASL when credentials are configured and the server offers it. Multiline CAP LS is accumulated before the request is sent. Configured SASL PLAIN is rejected when unavailable, and SASL errors fail registration. Credentials and encoded payloads are excluded from Debug and diagnostics.

ISUPPORT tokens are recorded rather than interpreted at registration time. `CASEMAPPING`, `CHANTYPES`, `PREFIX`, and `CHANMODES` are retained with bounded validation and then drive live channel classification, membership-prefix parsing, and channel-mode argument consumption; an invalid value leaves the previous mapping authoritative. A token line that arrives in the same read as the welcome is retained like any other server line.

Downstream CAP LS advertises an empty set and requests receive NAK. The bouncer does not proxy client CAP requests upstream and does not claim downstream semantics it has not implemented.
