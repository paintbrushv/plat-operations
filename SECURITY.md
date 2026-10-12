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
    unless a validator is configured: API keys (`--api-key-file`, for
    scripts), OAuth 2.1 access tokens checked by RFC 7662 introspection at
    your identity provider (for claude.ai and ChatGPT connectors), or both.
    With a validator it requires a bearer token on every route except
    `/health`, `/openapi.json` and the OAuth protected-resource metadata. It
    also rejects any `Origin` that is not allowlisted and rate-limits each
    caller. It speaks plain HTTP, so terminate TLS at a reverse proxy. See
    [docs/connectors/remote-mcp.md](docs/connectors/remote-mcp.md).
- Without network egress, except one kind you switch on: with OAuth
  configured, `serve` sends bearer tokens that are not API keys to your
  identity provider's introspection endpoint (HTTPS, 5-second timeout, no
  redirects), authenticated with a client secret read from a 0600 file.
  Tokens and the secret are never logged or stored; verdicts are cached
  under the token's SHA-256. See
  [docs/DATA_FLOW.md](docs/DATA_FLOW.md#4-what-leaves-the-host).

  Any bearer token that the static API key list does not match is forwarded
  to the introspection endpoint, so a mistyped or revoked API key reaches
  the identity provider. If that matters to you, run API keys and OAuth on
  separate server instances.
- Not a JWT validator. OAuth access tokens are checked by introspection only,
  so identity providers without an RFC 7662 endpoint (Microsoft Entra ID,
  Auth0) cannot be used yet.
- Not hardened for multi-tenant or hostile-network deployment. Neither server
  isolates tenants. Expose `boxscore-exact serve` only with API keys or OAuth
  behind TLS, and never expose the legacy API. With OAuth, rate-limit
  unauthenticated requests at the proxy: each unknown token costs one call to
  the identity provider before the server's own per-caller limit applies.
  The server itself makes at most 16 concurrent and 120 per minute
  introspection calls and answers `503` beyond that, so a flood cannot
  amplify against the identity provider, but it can delay new sign-ins.