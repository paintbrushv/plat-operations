# Security Policy

## Reporting a vulnerability

Please report vulnerabilities privately by opening a GitHub security advisory
("Report a vulnerability" under the Security tab of this repository). Do not open
a public issue for security problems.

## Data confidentiality

Boxscore is local-first by default: no private data leaves the machine unless
explicitly configured. Never commit real GL, rent-roll, receivables, or any
tenant/PII data. `.env`, local databases, private data, and private report
directories are gitignored — keep it that way.

## What this project is NOT

- Not a system that exfiltrates data: there is no telemetry, no remote logging,
  and no default network egress. There are two HTTP servers, and both bind to
  loopback by default:
  - The legacy `boxscore` API (`BOXSCORE_BIND_ADDR=127.0.0.1:3818`) has no
    authentication. It is for a single analyst on one machine and must stay
    on loopback.
  - The exact-cent `boxscore-exact serve` (`127.0.0.1:3819`) serves the
    calculation catalog over MCP and `/v1/calc`. It refuses to bind a
    non-loopback address, or to advertise a non-loopback `--public-url`,
    unless `--api-key-file` is given. With a key file it requires a bearer
    token on every route except `/health`, `/openapi.json` and the OAuth
    protected-resource metadata. It also rejects any `Origin` that is not
    allowlisted and rate-limits each key. It speaks plain HTTP, so terminate
    TLS at a reverse proxy. See
    [docs/connectors/remote-mcp.md](docs/connectors/remote-mcp.md).
- Not hardened for multi-tenant or hostile-network deployment. Neither server
  isolates tenants. Expose `boxscore-exact serve` only with API keys (and,
  from stage 2b, OAuth) behind TLS, and never expose the legacy API.