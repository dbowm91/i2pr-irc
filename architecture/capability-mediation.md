# Capability mediation

Upstream negotiation does not request message-tags, batch, server-time, echo-message, or other features whose downstream semantics are not implemented. It requests only SASL when credentials are configured and the server offers it. Multiline CAP LS is accumulated before the request is sent. Configured SASL PLAIN is rejected when unavailable, and SASL errors fail registration. Credentials and encoded payloads are excluded from Debug and diagnostics.

Downstream CAP LS advertises an empty set and requests receive NAK. The bouncer does not proxy client CAP requests upstream and does not claim downstream semantics it has not implemented.
