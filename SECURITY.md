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

  With `--oauth-browser-client-id`, the browser workbench signs analysts in
  through the same identity provider as a public client (authorization code
  with PKCE `S256`, no client secret in the page). The browser talks to the
  provider's authorization and token endpoints itself; the server only
  publishes them in `/workbench/auth.json`, names the token endpoint's
  exact URL (not its origin) in the page's `connect-src`, and introspects
  the resulting token like any other. The verifier, `state` and access
  token live in the tab's `sessionStorage` only (never `localStorage`, a
  cookie, the console or a URL), the code is removed from the address bar
  before anything else, and the answer's `state` (and `iss`, RFC 9207, when
  present) must match. No refresh token is kept: expiry is handled on the
  next request, which is not sent; the page asks to sign in again and never
  falls back to an API key meanwhile. `sessionStorage` is shared with every
  page on the same origin, so give the workbench an origin of its own; a
  duplicated tab inherits the token, and signing out in one tab does not
  sign out the other. Signing out of the page does not end the session at
  the identity provider. The authorize request carries `resource=<audience>`
  (RFC 8707), which strict identity providers accept only as an absolute
  URI, so make `--oauth-audience` one.

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