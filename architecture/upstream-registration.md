# Upstream registration and capability downgrade

Each `NetworkOwner` negotiates upstream IRC capabilities once per connection generation, before any downstream client is needed. The requested set is a fixed bouncer policy filtered by the server's bounded CAP offer; local clients cannot change it.

## CAP support state

Registration starts in `Negotiating`. A valid `CAP LS`, `CAP ACK`, or `CAP NAK` proves the server supports CAP. A `421` naming the `CAP` command, or a `001` received before any valid CAP response, marks CAP `Unsupported` for that generation.

When CAP is unsupported and no SASL credential is configured, the welcome completes registration. The owner does not send `CAP END` to a server that never demonstrated CAP support. A malformed CAP message follows the normal bounded protocol error path; it is not treated as proof that CAP is unsupported.

## Authentication policy

Without configured SASL, registration proceeds with full CAP, partial CAP, or no CAP. Optional capabilities are requested as one bounded opportunistic set. A NAK disables those optional semantics for the generation and does not make registration fail.

With configured SASL, registration fails closed if CAP is unsupported, SASL is absent, an explicit SASL mechanism list excludes PLAIN, SASL is NAKed, authentication returns 904/905/906/907, or `001` arrives before authentication succeeds. The required `sasl` request is separate from the optional capability request so an optional refusal cannot downgrade authentication. A bare `sasl` offer has unknown mechanisms under IRCv3 SASL 3.2, so the owner attempts the configured PLAIN flow; an explicit mechanism list must include PLAIN.

SASL payloads remain secret-classified and are never added to logs or diagnostics. SASL credentials are not reused for service commands.

## Transport profile

The I2P stream provider supplies an ordered byte stream. IRC is sent directly over that stream. The runtime does not require a TLS connector, automatically upgrade to TLS, or treat missing server TLS as registration failure. This does not add a generic socket or alternate egress path. TLS-over-I2P would require a separately reviewed future design.

## Verification surface

The production-owner registration tests cover modern CAP, no-CAP welcome, `421 CAP`, optional NAK, bare SASL, explicit mechanism refusal, required-SASL NAK/failure, early welcome, and continued failure classification. No client attachment participates in upstream negotiation.
