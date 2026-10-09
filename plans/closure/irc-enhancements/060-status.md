# Plan 060 — M012-B Closure Status

Status: closed with production implementation deferred
Disposition commit: `cd71105` — `docs(plans): defer draft chathistory and ready failover`
Closure commit: recorded by the immediately following exact-closure commit
Date: 2026-10-09

## Requirement-to-evidence matrix

| Requirement | Evidence and result |
|---|---|
| Determine whether upstream CHATHISTORY may be shipped | Research 011 records the official IRCv3 CHATHISTORY specification and BATCH review. The CHATHISTORY draft remains work in progress and explicitly warns against production use. The plan therefore defers all production CAP requests and replay implementation until that warning is withdrawn or a stable finalized specification is available. |
| Do not infer deployed IRC2P/ILITA support | No authorized live transcript was collected. Fake-server fixtures cannot establish current deployment capability, access control, or history semantics. No support claim is made. |
| Preserve existing behavior and privacy | No runtime code or schema changed. Existing local Store-backed CHATHISTORY, NoHistory, Ephemeral, OTR, and upstream CAP behavior are unchanged. No history is fabricated, fetched, or retained by this disposition. |
| Unblock eligible successor work | Plan 061 source review confirmed typed `I2pEndpoint`, a single `NetworkOwner` per `NetworkId`, and the `NetworkId`-scoped `I2pStreamProvider`; its readiness gate now requires explicit operator trust-equivalence and credential-scope attestation. |

## Security, recovery, and limitations

The defer decision avoids deploying a protocol whose current official draft says not to use it in production. It also avoids treating a direct capability advertisement or a fake IRCd as evidence of IRC2P/ILITA interoperability. There are no runtime changes, migrations, queues, or new recovery behaviors in this closure. A successor must re-review the then-current official specification, capability spelling, reference negotiation, access-control semantics, and supported server evidence before implementation.

## Verification

- `rtk git diff --check` — passed for the disposition changes.
- No code tests or full verification scripts were run because this closure changes plans and registry only; no runtime implementation was produced.
- Official specification review and source links are recorded in `plans/research/011-irc-privacy-resilience-authentication.md`.

## Registry and roadmap disposition

Plan 060 is closed with its production feature deferred. Plans 055-059 retain their existing closures. Plan 061 is ready for core implementation under an explicit operator-declared same-Network trust-equivalence gate; no deployed federation or live service support is implied. Plan 062 remains proposed until Plans 059-061 are closed or formally deferred with evidence.
