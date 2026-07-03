# Security Policy

## Supported versions

Kestrel is pre-production and unaudited. No version is currently recommended for use with real capital. Only the `main` branch receives fixes.

| Version | Supported |
|---|---|
| `main` | Fixes only |
| Tagged releases | None yet |

## Reporting a vulnerability

Report privately. Do not open a public issue for anything exploitable.

- Email the maintainer directly with a description, affected files, and a reproduction if possible.
- Expect an acknowledgement within 72 hours.
- Please allow a remediation window before any public disclosure.

## Scope

In scope:

- Smart contracts in `contracts/src/` — flash-loan callbacks, profit guards, access control, the clone/`initialize` pattern, and the timelock.
- Bot key handling (`SEARCHER_PRIVATE_KEY` via `secrecy::SecretString`), signing, and bundle construction.
- Network exposure of the metrics/control WebSocket (`127.0.0.1:9101` / `9102`) and dashboard.

Out of scope:

- Third-party protocols (Balancer, Curve, MakerDAO DssFlash, Sky PSM, Aave).
- Losses from operating with `SUBMISSION_ENABLED=true` before validating the pipeline in monitor-only mode.

## Operational security

- Never commit `.env`. It is gitignored; verify before every push.
- Keep the executor wallet funded only for a few days of gas.
- Route all `pause`, ownership, and sweep operations through `KestrelTimelock`.
