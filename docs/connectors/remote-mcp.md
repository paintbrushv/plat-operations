# Remote MCP connector (Streamable HTTP)

`boxscore-exact serve` serves the exact-cent
[calculation catalog](../EXACT_CENTS.md#calculation-catalog-and-versioning-policy)
over HTTP from the same binary and the same dispatcher as the
[stdio connector](claude-desktop.md). The same tool call returns the same
envelope, `result` and `output_sha256` on both transports.

| route | auth | what it is |
|---|---|---|
| `POST /mcp` | bearer | MCP Streamable HTTP, protocol `2025-06-18` (`2025-03-26` accepted) |
| `GET /mcp` | bearer | `405`: no server-initiated SSE stream |
| `POST /v1/calc/{function}` | bearer | one catalog function; body is its input, response is the envelope. `import_period` also takes `profile`, `files` (each file's text as a JSON string) and `options` in place of `dataset` |
| `GET /openapi.json` | none | the OpenAPI 3.1 contract; see [openapi.md](openapi.md) |
| `GET /health` | none | `{"status":"ok","producer":{...}}` |
| `GET /.well-known/oauth-protected-resource` | none | RFC 9728 protected-resource metadata, `resource` = the public URL |
| `GET /.well-known/oauth-protected-resource/mcp` | none | the same document with `resource` = `<public-url>/mcp` |
| `GET /`, `/app.css`, `/app.js` | none | the browser workbench; its data calls use `/v1/calc` with a bearer API key. Not in the OpenAPI contract. See [INSTALL.md](../INSTALL.md#browser-workbench) |

"bearer" means `Authorization: Bearer <token>`, where the token is an API key
(for scripts) or an OAuth 2.1 access token (for claude.ai and ChatGPT
connectors).

## Run it

```bash
boxscore-exact serve \
  --bind 127.0.0.1:3819 \
  --database /srv/boxscore/exact.sqlite \
  --public-url https://boxscore.example.com \
  [--api-key-file /etc/boxscore/api-keys] \
  [--authorization-server https://idp.example.com/oauth2/default \
   --oauth-audience https://boxscore.example.com/mcp \
   --oauth-client-id boxscore-resource-server \
   --oauth-client-secret-file /etc/boxscore/oauth-client-secret \
   [--introspection-endpoint https://idp.example.com/oauth2/default/v1/introspect] \
   [--oauth-scope boxscore:calc]] \
  [--allow-origin https://inspector.example.com]... \
  [--rate-limit 60]
```

- `--bind` takes an IP address and port, not a host name. The default is
  `127.0.0.1:3819`. The server refuses to start on a non-loopback address
  unless at least one validator is configured: `--api-key-file`, OAuth (see
  [below](#oauth-21-claudeai-and-chatgpt-connectors)), or both. It also
  refuses a non-loopback `--public-url` without one. A loopback bind behind a
  same-host reverse proxy is still exposed, and this check catches that case.
- Without a validator the server needs no token and answers loopback only.
  Use that mode for local development. Do not use it behind a proxy.
- `--database` is fixed for the life of the process. No request can choose
  it. A `database` field in tool arguments or in a `/v1/calc` body is refused
  as `INVALID_INPUT`. Without `--database`, the stateful functions refuse with
  `DATABASE_REQUIRED`.
- `--public-url` is the resource identifier in the metadata and in the
  `WWW-Authenticate` challenge, and the `servers` URL in `/openapi.json`. The
  default is `http://127.0.0.1:<port>`. With OAuth on a non-loopback bind it
  is required: remote connectors find the authorization server through the
  URL in the challenge and the metadata, so a loopback default would strand
  them.
- `--rate-limit` sets requests per minute per caller (an API key, or an OAuth
  subject). The default is 60. Each caller is a token bucket with a burst of
  that size, refilled continuously. Going over returns `429` with
  `Retry-After`. Public routes are not limited.
- SIGINT or SIGTERM stops the server gracefully: in-flight requests finish,
  then the process exits 0. A start-up refusal exits 2 with one line on
  stderr.

## API keys

API keys are for scripts and command-line clients. Interactive connectors use
OAuth (see below). The key file holds one `key_id:secret` per line. Blank lines
and `#` comments are ignored, and so is surrounding whitespace, including a
trailing newline or `\r`.

```bash
umask 077
printf 'nightly-close:%s\n' "$(openssl rand -hex 32)" >> /etc/boxscore/api-keys
chmod 600 /etc/boxscore/api-keys
```

- `key_id` is 1 to 64 characters from `A-Z a-z 0-9 . _ -`. It names the
  caller and is never secret.
- The secret is at least 32 characters from `A-Z a-z 0-9 - . _ ~ + /` and `=`.
  Duplicate ids and reused secrets refuse to start.
- On unix, a key file that other users can read or write refuses to start.
  `0600` is expected, and group access prints a warning.
- Send the secret alone: `Authorization: Bearer <secret>`. The server keeps
  only SHA-256 digests of the secrets. It compares the presented token's
  digest with every key in constant time.
- A missing token and a wrong token get the same `401`. On `/mcp` the
  challenge names the `/mcp`-suffixed metadata document (whose `resource` is
  `<public-url>/mcp`); on `/v1/calc` it names the unsuffixed one:

  ```
  WWW-Authenticate: Bearer resource_metadata="https://boxscore.example.com/.well-known/oauth-protected-resource/mcp"
  WWW-Authenticate: Bearer resource_metadata="https://boxscore.example.com/.well-known/oauth-protected-resource"
  ```

- To rotate a key, add the new line, restart, move the scripts over, remove
  the old line and restart again. Keys are read once at start-up.

Example call:

```bash
curl -s https://boxscore.example.com/v1/calc/describe \
  -H "Authorization: Bearer $BOXSCORE_KEY" -X POST
```

Claude Code can use an API key directly:

```bash
claude mcp add --transport http boxscore https://boxscore.example.com/mcp \
  --header "Authorization: Bearer $BOXSCORE_KEY"
```

## OAuth 2.1 (claude.ai and ChatGPT connectors)

Claude.ai custom connectors and ChatGPT Developer Mode connectors reach the
server from the vendor's cloud and sign the user in with OAuth; you do not
paste a static key into them. `boxscore-exact serve` is the OAuth **resource
server** only. Sign-in, consent, multi-factor, client registration and token
issuance all happen at an external **authorization server**: your identity
provider (IdP). The server checks each access token by asking that IdP, with
RFC 7662 token introspection.

### How a request is checked

1. A request without a token, or with one that is not accepted, gets `401`
   and the challenge above. On `/mcp` the challenge names
   `/.well-known/oauth-protected-resource/mcp`, whose `resource` is
   `<public-url>/mcp`, so the client asks the IdP for that audience; on
   `/v1/calc` it names `/.well-known/oauth-protected-resource`. The client
   reads that document, finds the IdP in `authorization_servers` and the
   scope in `scopes_supported`, and signs the user in there.
2. With both validators configured, the token is first compared with the API
   keys (constant time, no network). Only a token that is not an API key goes
   to the IdP.
3. The server POSTs the token to the introspection endpoint
   (`token=...&token_type_hint=access_token`, HTTP Basic with
   `--oauth-client-id` and the secret), with a 5-second timeout. A token over
   4 KiB, or with characters outside the RFC 6750 token alphabet, is refused
   without a call. The server makes at most 16 concurrent and 120 per minute
   introspection calls (cached verdicts do not count); anything beyond
   answers `503` without a call.
4. The token is accepted only if the answer has `active: true`; it is an
   access token (`token_type`, if present, is `Bearer` or `access_token`, and
   `typ` is not `Refresh`, `refresh_token`, `ID` or `id_token`); `exp` is
   present and in the future; `nbf`, if present, is not in the future; `aud`
   contains `--oauth-audience` (`resource` is consulted only when `aud` is
   absent, so a wrong `aud` is refused whatever `resource` says); and `scope`
   contains `--oauth-scope` (default `boxscore:calc`).

| outcome | response |
|---|---|
| accepted | the request runs; the trace record names the caller `http:oauth:<sub>` (or `http:oauth:<client_id>` when the IdP returns no `sub`) |
| inactive, expired, not yet valid, wrong audience, a refresh or ID token, no subject | `401` with the same challenge as a missing token |
| valid but without the scope | `403` with `WWW-Authenticate: Bearer error="insufficient_scope", scope="boxscore:calc", resource_metadata="<public-url>/.well-known/oauth-protected-resource/mcp"` (the unsuffixed document on `/v1/calc`) |
| IdP unreachable, timed out, non-2xx (including a rejected client secret), a redirect, or an answer that is not a JSON object with a boolean `active`; or 16 calls already in flight, or 120 calls in the last minute | `503` with `Retry-After: 5`, and a line on stderr naming the failure, never the token (at most one line every 10 seconds; the next line counts the ones suppressed) |

Verdicts are cached in memory, keyed by the SHA-256 of the token (the token
itself is never stored): an accepted token until its `exp`, at most 5
minutes; a refused one (`401` or `403`) for 30 seconds. A failure (`503`) is
not cached. The cache holds at most 10 000 verdicts. When it is full,
expired verdicts go first, then refused ones oldest first, and accepted ones
only after those, so a flood of unknown tokens cannot push signed-in users
out of the cache.
Consequences: revoking a token at the IdP takes effect here within 5 minutes,
and a token refused because of an IdP misconfiguration stays refused for 30
seconds after you fix it.

Any bearer token that the static API key list does not match is forwarded to
the introspection endpoint. A mistyped or revoked API key therefore reaches
the IdP (and its logs) like any other unknown token. If that matters to you,
run API keys and OAuth on separate server instances.

### Flags

| flag | required | meaning |
|---|---|---|
| `--authorization-server URL` | yes | The IdP's issuer URL, exactly as the IdP's metadata `issuer` says. Advertised in the metadata. One only. `https`, or `http` to a loopback host. |
| `--oauth-audience VALUE` | yes | The value the IdP puts in the token's `aud` (or `resource`) for this server. Use the MCP endpoint URL, for example `https://boxscore.example.com/mcp`, unless your IdP fixes another value. |
| `--oauth-client-id ID` | yes | The client this server uses at the introspection endpoint. |
| `--oauth-client-secret-file PATH` | yes | One line: that client's secret. Same rules as the API key file: other users must not be able to read it (0600). |
| `--introspection-endpoint URL` | no | The RFC 7662 endpoint. When absent, the server reads the issuer's RFC 8414 metadata once at start-up, checks that its `issuer` matches, and uses its `introspection_endpoint`; metadata without one refuses to start. For an issuer with a path (`https://host/tenant`) it tries the RFC 8414 form `https://host/.well-known/oauth-authorization-server/tenant` first and, only on a `404`, the appended form `https://host/tenant/.well-known/oauth-authorization-server`; an issuer without a path has only the second form. Pass the flag explicitly when the IdP serves metadata elsewhere. |
| `--oauth-scope SCOPE` | no | The scope a token must carry; default `boxscore:calc`. |
| `--oauth-browser-client-id ID` | no | The public client IT registers for the browser workbench, so analysts sign in on the page instead of pasting an API key. See [Browser workbench sign-in](#browser-workbench-sign-in). Needs the four required flags above; the issuer's `authorization_endpoint` and `token_endpoint` are then read from its metadata at start-up (also when `--introspection-endpoint` is given). |

`serve` refuses to start on any inconsistent combination: an OAuth flag
without `--authorization-server`; `--authorization-server` without
`--oauth-audience`, `--oauth-client-id` or `--oauth-client-secret-file`; a
client id without a secret file or the reverse; more than one
`--authorization-server`; a plain-`http` issuer or endpoint on a
non-loopback host; a URL carrying a user name, password, query or fragment
(the refusal never repeats the URL); a non-loopback `--bind` without
`--public-url`; a scope with spaces or quotes; an empty or world-readable
secret file; `--oauth-browser-client-id` without the four required OAuth
flags, or with issuer metadata that lacks an `authorization_endpoint` or
`token_endpoint`, or names one that is plain `http` on a non-loopback host
or carries a query. (Before this change, `--authorization-server` alone only
advertised the IdP; it now always enables validation and needs the other
flags.)

The introspection call honours `HTTPS_PROXY`/`HTTP_PROXY` from the
environment (except for a loopback endpoint) and trusts the public web PKI
roots built into the binary (Mozilla's, via `webpki-roots`), not the
operating system store. An IdP behind a private certificate authority is
therefore not reachable yet. [stated: `boxscore/src/exact/oauth.rs`]

### Which identity providers work

This release validates tokens by RFC 7662 introspection only. Local JWT
validation against the IdP's signing keys (JWKS) is out of scope; it would be
another `TokenValidator` behind the same seam in
`boxscore/src/exact/mcp_http.rs`.

| IdP | RFC 7662 introspection | dynamic client registration (DCR) |
|---|---|---|
| Okta (custom authorization server) | Yes: `https://<org>.okta.com/oauth2/<server-id>/v1/introspect` [stated] | Okta's client registration endpoint (`/oauth2/v1/clients`) needs an admin API token, so it is not the open registration that claude.ai or ChatGPT can use on their own. Register the connector's client by hand and paste its id and secret. [stated: Okta developer forum] |
| Keycloak | Yes: `https://<host>/realms/<realm>/protocol/openid-connect/token/introspect`. **Verified with Keycloak 26.0.8** ([recipe](#local-test-with-keycloak-verified-against-keycloak-260-on-2026-10-12)): metadata discovery works through the appended form `/realms/<realm>/.well-known/oauth-authorization-server` (the RFC 8414 form answers `404`); introspection returns `token_type` and `typ` `Bearer` for access tokens and `Refresh` or `ID` for the other tokens, which it also reports active; the *Audience* mapper on the scope puts the audience in `aud` as described; the RFC 8707 `resource` parameter is ignored. | Yes, at `/realms/<realm>/clients-registrations/openid-connect` (listed as `registration_endpoint` in the metadata, verified). Anonymous registration is blocked unless the realm's client registration policies (Trusted Hosts) allow the vendor's hosts. [stated: keycloak.org "Using the client registration service"; not tested] |
| Auth0 | **No.** Auth0 documents no RFC 7662 endpoint; it expects resource servers to validate JWT access tokens against its JWKS. Not usable with this release. [stated: Auth0 community thread "RFC 7662 support", 2021; no later Auth0 document found that adds it] | Yes, "OIDC Dynamic Application Registration" (open, no token) at `/oidc/register`, off by default (Dashboard → Settings → Advanced). [stated: auth0.com/docs, Dynamic Application Registration] |
| Microsoft Entra ID | **No** introspection endpoint; needs JWT validation. Not usable with this release. [stated] | No open DCR. [stated] |

The claims marked [stated] come from the vendors' public documentation and
forums as of October 2026. Keycloak 26.0 is the only IdP tested against
this server, locally and with the password grant (see the
[recipe](#local-test-with-keycloak-verified-against-keycloak-260-on-2026-10-12));
Okta, Auth0 and Entra ID were not tested, and no connector sign-in
(authorization code flow, DCR) was tried with any IdP.

Dynamic client registration is entirely the IdP's job. This server neither
offers nor proxies a registration endpoint, and its metadata does not
mention one. Claude.ai and ChatGPT read the IdP's own metadata
(`registration_endpoint`) to decide whether to register themselves; when the
IdP has none, you register their client by hand and paste the client id and
secret into the connector settings.

### IdP configuration checklist

1. **The connector client** (the one claude.ai or ChatGPT signs users in
   with). Authorization code flow with PKCE. Either let the vendor register
   it through DCR (Keycloak, Auth0) or create it yourself and keep its id and
   secret for the connector settings. Its redirect URI is the vendor's, and
   handling it is the IdP's job, not this server's:
   - claude.ai: `https://claude.ai/api/mcp/auth_callback` [stated by
     third-party connector guides; check Anthropic's help center]
   - ChatGPT: use the redirect URI ChatGPT shows when you create the
     connector. Reported forms are `https://chatgpt.com/connector/oauth/<id>`
     and, in older guides, `https://chatgpt.com/connector_platform_oauth_redirect`.
     [stated by third-party guides; unverified]
2. **The scope.** Define `boxscore:calc` (or your `--oauth-scope`) at the IdP
   and allow the connector client to request it. The token's `scope` must
   list it, or every call gets `403`.
3. **The audience.** Make the IdP put `--oauth-audience` in the access
   token's `aud`. Okta: set the custom authorization server's *Audience* to
   it. Keycloak: add an *Audience* mapper (included custom audience, added to
   the access token and to token introspection) to the `boxscore:calc`
   client scope. Using the MCP URL (`https://boxscore.example.com/mcp`) also
   matches IdPs that turn the RFC 8707 `resource` parameter, which MCP
   clients send, into the audience; Keycloak 26.0 does not (verified), so
   with Keycloak the mapper is the only source of `aud`. With the mapper on
   the scope, a token without the scope has no audience either, and the
   server answers `401`, not `403` (verified).
4. **Introspection credentials.** A separate confidential client for this
   server (no user sign-in, no redirect URI) whose id and secret go into
   `--oauth-client-id` and the file named by `--oauth-client-secret-file`.
   Okta: a confidential app with a client secret, allowed to call the
   custom authorization server's introspection endpoint [stated, unverified].
   Keycloak: a confidential client with client authentication on and every
   flow off.
5. **Token lifetime.** Short access tokens (5 to 60 minutes) with refresh
   tokens. Revocation takes effect here within 5 minutes (the cache cap).
6. **Network.** The server must reach the introspection endpoint over HTTPS;
   allow that one egress in your firewall.

### Browser workbench sign-in

The [browser workbench](../INSTALL.md#browser-workbench) at `<public-url>/`
can sign analysts in through the same IdP, so nobody on a shared deployment
needs an API key. The page runs the OAuth 2.1 authorization code flow with
PKCE itself, as a **public client** (no secret: a secret in a web page is
not a secret). The server's part is small: it tells the page where to go,
and it checks the resulting access token by introspection like any other.

**What IT registers at the IdP** (once per deployment):

1. A **public client** for the page, for example `boxscore-workbench`:
   authorization code flow only (no implicit flow, no password grant, no
   client credentials), **PKCE required with `S256`**, and exactly one
   redirect URI, **`<public-url>/`** with the trailing slash (for example
   `https://boxscore.example.com/`).
2. **CORS on the token endpoint.** The page POSTs the code to the IdP's
   token endpoint from its own origin, so the IdP must answer that origin
   with `Access-Control-Allow-Origin`. Keycloak: the client's *Web Origins*
   (`https://boxscore.example.com`, or `+` for "the redirect URIs'
   origins").
3. **The scope and the audience**, as for a connector (checklist steps 2
   and 3 above): the client may request `boxscore:calc` (or your
   `--oauth-scope`), and the access token's `aud` must contain
   `--oauth-audience`. With Keycloak, giving the client the `boxscore:calc`
   scope as a default scope brings the audience mapper with it.
4. **An origin of its own for the workbench.** Serve it on a host name that
   no other web application shares (not a path beside other apps on one
   host): the token lives in `sessionStorage`, which every page on the same
   origin can read.

Then start `serve` with `--oauth-browser-client-id boxscore-workbench` next
to the OAuth flags. The banner adds
`boxscore-exact serve: workbench sign-in: OAuth via <issuer> (client boxscore-workbench)`.
No token, code or secret is ever printed.

**What the server does.** At start-up it reads the issuer's metadata and
keeps its `authorization_endpoint` and `token_endpoint` (both must be
present and `https`, or `http` to a loopback host, and if the metadata lists
`code_challenge_methods_supported` it must include `S256`; otherwise it
refuses to start, naming the rule and asking you to check the IdP's
metadata). `GET /workbench/auth.json` (public, `no-store`, the page's security
headers) tells the page how to authenticate:

```json
{"mode": "oauth",
 "issuer": "https://idp.example.com/realms/acme",
 "authorization_endpoint": "https://idp.example.com/realms/acme/protocol/openid-connect/auth",
 "token_endpoint": "https://idp.example.com/realms/acme/protocol/openid-connect/token",
 "client_id": "boxscore-workbench", "scope": "openid boxscore:calc",
 "audience": "https://boxscore.example.com/mcp",
 "redirect_uri": "https://boxscore.example.com/"}
```

`mode` is `none` (a loopback server without authentication), `api_key`,
`oauth`, or `api_key_or_oauth` when both are configured (the page then
offers sign-in and the key form). Without `--oauth-browser-client-id` the
document is just the mode, and the page shows the key form as before. The
page's Content-Security-Policy gains the token endpoint's exact URL in
`connect-src`
(`connect-src 'self' https://idp.example.com/realms/acme/protocol/openid-connect/token`:
scheme, host, port and path, no query), so the page can POST to that one
URL and to nothing else at the IdP; nothing else changes. The endpoint's
host must be an IP address or DNS labels of ASCII letters, digits, `-` and
`.`, or the server refuses to start. The server makes no new outbound
call.

**What the page does.**

1. *Sign in with your firm's identity provider*: it draws a 64-character
   PKCE verifier and a 32-character `state` from `crypto.getRandomValues`,
   computes `code_challenge` = base64url(SHA-256(verifier)) with
   `crypto.subtle` (so it needs `https` or a loopback address), keeps
   `{verifier, state, started_at}` in the tab's `sessionStorage`, and goes
   to the authorization endpoint with `response_type=code`, `client_id`,
   `redirect_uri`, `scope`, `state`, `code_challenge`,
   `code_challenge_method=S256` and `resource=<audience>`. `resource` is
   the RFC 8707 resource indicator: IdPs may ignore it (Keycloak does), but
   RFC 8707 requires an absolute URI and strict IdPs refuse anything else,
   so make `--oauth-audience` one (the Keycloak recipe's
   `http://127.0.0.1:3819/mcp` and the example's
   `https://boxscore.example.com/mcp` are). That URL is built only from
   `auth.json`. Sign-in starts only at the redirect URI's own origin: a page
   opened as `http://localhost:3819/` when the redirect URI is
   `http://127.0.0.1:3819/` says "Sign-in works only at
   http://127.0.0.1:3819/; open the workbench there." instead of leaving
   (the answer would land in another origin's `sessionStorage`).
2. Back at `<public-url>/` with `code` or `error` in the query, it first
   removes the query from the address bar and history
   (`history.replaceState`) and deletes the stored verifier and state,
   whatever follows. A callback without `state`, with a `state` that is not
   this tab's (or with none stored: a replay), or with an `iss` (RFC 9207)
   other than the issuer is discarded: "Sign-in response did not match this
   tab; start again."; one older than 10 minutes: "The sign-in took too
   long; start again."; one that arrives while `auth.json` could not be
   loaded: "Sign-in could not be completed because the server's sign-in
   settings could not be loaded; start again." Otherwise it POSTs
   `grant_type=authorization_code`, `code`, `redirect_uri`, `client_id` and
   `code_verifier` as a form to the token endpoint, without cookies and
   without a secret. An IdP error (`error`, `error_description`) is shown as
   text, verbatim.
3. It keeps `access_token` and `expires_at` (now plus `expires_in`) in the
   tab's `sessionStorage` and sends the token as the bearer on every
   `/v1/calc` call (before any API key). A refresh token, if the IdP sends
   one, is neither stored nor used; an ID token is ignored.

**What is stored where.** The verifier and `state`: the tab's
`sessionStorage`, from the click until the page comes back. The access token
and its expiry: the tab's `sessionStorage`, until it expires, is refused,
the analyst signs out, or the tab closes. Nothing in `localStorage`,
cookies, the console or any URL except the authorize request (which carries
the challenge and state, never the verifier or a token). `sessionStorage`
belongs to the origin, not to the page: any other page served from the
same origin can read it, which is why the workbench needs an origin of its
own (step 4 above). A duplicated tab starts with a copy of it, token
included, and from then on the two tabs are separate: signing out in one
does not sign out the other. The server stores only the introspection
verdict, keyed by the token's SHA-256, as for any token.

**Expiry and sign-out.** The page does not watch the clock: expiry is
handled on the next request. When the token's `expires_at` has passed by
then, the page drops the token, sends nothing (not under an API key
either), shows the sign-in panel and says "Your sign-in expired; sign in
again." (the IdP usually signs the analyst straight back in while its own
session lasts). A `401` from the server to the token is handled the same
way. Until the analyst signs in again or chooses *Sign out*, the tab sends
no request: an API key in the same tab is used only when no token was kept
in it, or after *Sign out*. A `403 INSUFFICIENT_SCOPE` is shown verbatim
with a note that the IdP did not grant `boxscore:calc`. *Sign out* forgets
the token in this tab only; the session at the IdP is not ended, so on a
shared computer sign out there too.

**Verified** against Keycloak 26.0.8 on 2026-10-14 with the
`boxscore-workbench` client in
[keycloak-realm.example.json](keycloak-realm.example.json) (see
[below](#browser-sign-in-with-keycloak-verified-2026-10-14)). Okta,
Microsoft Entra ID and Auth0 were not tried; Entra ID and Auth0 cannot be
used with this release anyway (no introspection), and whether Okta's token
endpoint answers a browser origin (CORS, a "SPA" application with the
origin trusted) is unverified.

### claude.ai custom connector (unverified)

Not tried with a real claude.ai account; this is the expected flow from
Anthropic's and third-party connector documentation. The server side was
checked against Keycloak 26.0 on one machine ([below](#local-test-with-keycloak-verified-against-keycloak-260-on-2026-10-12)), but no
connector sign-in (authorization code flow, DCR, a public endpoint) was.

1. Run the server behind TLS on a public host name with the OAuth flags
   above and `--public-url https://boxscore.example.com`. Check from outside:
   `curl https://boxscore.example.com/.well-known/oauth-protected-resource`
   names your IdP.
2. In claude.ai: Settings → Connectors → *Add custom connector* (on Team and
   Enterprise plans an owner adds it under Organization settings, then each
   member connects). Name: `boxscore`. URL:
   `https://boxscore.example.com/mcp`.
3. Under *Advanced settings*, paste the connector client's *OAuth Client ID*
   and *OAuth Client Secret* if your IdP has no open DCR (Okta). Leave them
   empty when it does (Keycloak with a Trusted Hosts policy for claude.ai,
   Auth0 once JWT validation exists). A client that registers itself
   through Keycloak's DCR gets the realm's default client scopes, so make
   `boxscore:calc` a realm default or optional client scope that the
   registration policy allows [stated; not tested].
4. Click *Connect*. The user is sent to your IdP's sign-in page, signs in,
   and sees a consent screen for scope `boxscore:calc` (whether consent is
   shown is the IdP's setting). The browser returns to claude.ai, and the
   connector lists the catalog tools.
5. A call that fails with `403 insufficient_scope` means the token has the
   audience but lacks the scope: fix step 2 of the checklist and reconnect.
   Repeated sign-in prompts mean `401`: usually the audience (checklist
   step 3), but with Keycloak, where the audience comes from the scope's
   mapper, a missing scope also shows up as `401` (verified locally). With
   Keycloak also check that the IdP's host name is fixed (`KC_HOSTNAME`):
   Keycloak takes the issuer from the request's host name and reports a
   token issued under another host name as inactive (verified locally).

### ChatGPT Developer Mode connector (unverified)

Not tried with a real ChatGPT account, and no connector sign-in was tried
with any IdP; only the server side was checked against Keycloak locally.

1. As for claude.ai, step 1.
2. In ChatGPT: Settings → Apps & Connectors → Advanced settings → turn on
   *Developer mode*. Then *Create* a connector: name `boxscore`, a short
   description, MCP server URL `https://boxscore.example.com/mcp`,
   authentication *OAuth*. Paste a client id and secret if your IdP has no
   open DCR.
3. Allowlist the redirect URI ChatGPT shows at the IdP (checklist step 1),
   then connect. The user signs in at your IdP and grants `boxscore:calc`.
4. In a chat, enable the connector from the tools menu and ask for a
   calculation. A `403 insufficient_scope` or repeated sign-in prompts have
   the same causes as for claude.ai.

There is no GPT Action. `/openapi.json` is the written contract, not an
Action manifest.

### Local test with a mock introspection endpoint (verified)

A 40-line stand-in IdP for trying the flags on one machine. It knows two
synthetic tokens and is not an OAuth server: it signs nobody in.

```python
# mock-introspection.py: a stand-in OAuth authorization server for local tests
# only. It serves RFC 8414 metadata and an RFC 7662 introspection endpoint for
# two fixed, synthetic tokens. Never put it in front of real data.
import base64, json, time
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import parse_qs

PORT = 9999
ISSUER = f"http://127.0.0.1:{PORT}"
CLIENT = "Basic " + base64.b64encode(b"boxscore-rs:local-test-secret").decode()
AUDIENCE = "http://127.0.0.1:3819/mcp"
TOKENS = {
    "local-good-token": {"active": True, "sub": "alice", "aud": AUDIENCE, "scope": "openid boxscore:calc"},
    "local-no-scope-token": {"active": True, "sub": "bob", "aud": AUDIENCE, "scope": "openid"},
}


class Handler(BaseHTTPRequestHandler):
    def reply(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path == "/.well-known/oauth-authorization-server":
            self.reply(200, {"issuer": ISSUER, "introspection_endpoint": ISSUER + "/introspect"})
        else:
            self.reply(404, {})

    def do_POST(self):
        if self.path != "/introspect" or self.headers.get("Authorization") != CLIENT:
            return self.reply(401, {"error": "invalid_client"})
        length = int(self.headers.get("Content-Length", 0))
        form = parse_qs(self.rfile.read(length).decode())
        claims = TOKENS.get(form.get("token", [""])[0])
        self.reply(200, dict(claims, exp=int(time.time()) + 300) if claims else {"active": False})

    def log_message(self, *args):
        pass  # print nothing, tokens included


HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
```

```bash
python3 mock-introspection.py &
(umask 077; printf 'local-test-secret\n' > client-secret)
# start the mock first: serve reads its metadata once at start-up
boxscore-exact serve --bind 127.0.0.1:3819 \
  --authorization-server http://127.0.0.1:9999 \
  --oauth-client-id boxscore-rs --oauth-client-secret-file ./client-secret \
  --oauth-audience http://127.0.0.1:3819/mcp &
for t in local-good-token local-no-scope-token local-unknown-token; do
  curl -s -o /dev/null -w "$t %{http_code}\n" -X POST \
    http://127.0.0.1:3819/v1/calc/describe -H "Authorization: Bearer $t"
done
# local-good-token 200
# local-no-scope-token 403
# local-unknown-token 401
```

Stop the mock (`kill %1`) and send a token the server has not cached: the
answer is `503` with `Retry-After: 5`, and stderr shows
`OAuth introspection at http://127.0.0.1:9999/introspect failed (connection failed); answered 503`.
`local-good-token` keeps working from the cache until its verdict expires.
(Run on Linux with Python 3.12 when this page was written.)

### Local test with Keycloak (verified against Keycloak 26.0 on 2026-10-12)

Run on 2026-10-12 on Linux against Keycloak 26.0.8, the last 26.0 release (the one the `26.0` image tag tracks),
with every command below and the outputs shown. One deviation: the Docker
socket was not usable on the test host, so Keycloak ran from the official
`keycloak-26.0.8.tar.gz` release on Java 21 (the second form in step 1)
instead of in the container. The `docker run` line is the container
equivalent of the same server and was not run. Everything here is
synthetic: the realm, the user `alice`, and the secrets in
[keycloak-realm.example.json](keycloak-realm.example.json), which are fake
and must never be used on a reachable Keycloak.

The realm file defines:

- client scope `boxscore:calc`, with an *Audience* mapper that adds
  `http://127.0.0.1:3819/mcp` to the access token's `aud` and to the
  introspection answer;
- client scope `basic` with the `sub` mapper. A realm import that lists
  `clientScopes` replaces Keycloak's built-in scopes (`basic`, `profile`,
  `email`, `roles`, ...), and without `basic` the tokens have no `sub`;
- `boxscore-server`: confidential, every flow off; this server's
  introspection credentials;
- `boxscore-test`: confidential, direct access grants (password grant) on,
  default scopes `basic` and `boxscore:calc`. It stands in for the
  connector, which would use the authorization code flow;
- `boxscore-noscope`: like `boxscore-test` but without `boxscore:calc`. It
  carries its own audience mapper, so its tokens have the right `aud` and
  only the scope is missing (the `403` case);
- `boxscore-workbench`: public, standard flow (authorization code) only,
  PKCE `S256` required, redirect URI `http://127.0.0.1:3819/`, web origin
  `http://127.0.0.1:3819`, default scopes `basic` and `boxscore:calc`: the
  [browser workbench sign-in](#browser-workbench-sign-in). The page also
  asks for `openid`, which Keycloak grants as the OpenID Connect scope; it is
  not a client scope (listing it under `defaultClientScopes` only logs
  "Referenced client scope 'openid' doesn't exist. Ignoring");
- user `alice`, password `fake-alice-password`.

Run from the repository root, with `boxscore-exact` on the `PATH` (see
[QUICKSTART.md](../../QUICKSTART.md), Path B), `curl` and `jq`.

**1. Start Keycloak** on loopback with the realm imported:

```bash
docker run -d --name kc -p 127.0.0.1:8080:8080 \
  -e KC_BOOTSTRAP_ADMIN_USERNAME=admin -e KC_BOOTSTRAP_ADMIN_PASSWORD=admin \
  -v "$PWD/docs/connectors/keycloak-realm.example.json:/opt/keycloak/data/import/boxscore-realm.json:ro" \
  quay.io/keycloak/keycloak:26.0 start-dev --import-realm

# or, as run for this page, the release tarball (needs Java 17 or later):
#   tar -xzf keycloak-26.0.8.tar.gz
#   mkdir -p keycloak-26.0.8/data/import
#   cp docs/connectors/keycloak-realm.example.json keycloak-26.0.8/data/import/
#   KC_BOOTSTRAP_ADMIN_USERNAME=admin KC_BOOTSTRAP_ADMIN_PASSWORD=admin \
#     keycloak-26.0.8/bin/kc.sh start-dev --import-realm --http-host=127.0.0.1 --http-port=8080 &

until curl -sf http://127.0.0.1:8080/realms/boxscore/.well-known/oauth-authorization-server >/dev/null; do sleep 2; done
```

Use `127.0.0.1` everywhere, never `localhost`. Keycloak derives the issuer
from the host name of each request, and a token minted at
`http://localhost:8080` was answered `"active": false` when introspected at
`http://127.0.0.1:8080`, so the server refused it with `401`. A real
deployment fixes the host name with `KC_HOSTNAME`.

**2. Start the server** on a fresh database with the quickstart sample month:

```bash
boxscore-exact init --database ./kc-test.sqlite
boxscore-exact import-csv --database ./kc-test.sqlite \
  --actuals docs/quickstart/actuals.csv --budgets docs/quickstart/budgets.csv \
  --snapshot docs/quickstart/snapshot.json \
  --property "Sample Gardens" --period 2026-05 --units 24
(umask 077; printf 'fake-server-secret-change-me\n' > ./kc-client-secret)
boxscore-exact serve --bind 127.0.0.1:3819 --database ./kc-test.sqlite \
  --authorization-server http://127.0.0.1:8080/realms/boxscore \
  --oauth-audience http://127.0.0.1:3819/mcp \
  --oauth-client-id boxscore-server --oauth-client-secret-file ./kc-client-secret &
until curl -sf http://127.0.0.1:3819/health >/dev/null; do sleep 1; done
# boxscore-exact serve: listening on http://127.0.0.1:3819 (auth: OAuth introspection)
# boxscore-exact serve: OAuth issuer http://127.0.0.1:8080/realms/boxscore, introspection http://127.0.0.1:8080/realms/boxscore/protocol/openid-connect/token/introspect, audience http://127.0.0.1:3819/mcp, scope boxscore:calc
```

No `--introspection-endpoint` is needed. Keycloak answers `404` at the
RFC 8414 form (`/.well-known/oauth-authorization-server/realms/boxscore`)
and serves the metadata at the appended form
(`/realms/boxscore/.well-known/oauth-authorization-server`); the server
falls back to it, the `issuer` matches, and the banner shows the discovered
endpoint.

**3. Get tokens** for `alice` with the password grant:

```bash
KC=http://127.0.0.1:8080/realms/boxscore/protocol/openid-connect
token() {  # $1 = client:secret, $2 = scope; prints the access token
  curl -s -u "$1" -d grant_type=password -d username=alice \
    -d password=fake-alice-password -d "scope=$2" "$KC/token" | jq -r .access_token
}
TOKEN=$(token boxscore-test:fake-test-secret-change-me 'openid boxscore:calc')
NOSCOPE=$(token boxscore-noscope:fake-noscope-secret-change-me openid)
```

The token response says `"token_type":"Bearer"`, `"expires_in":300` and
`"scope":"openid boxscore:calc"` (`"scope":"openid"` for `boxscore-noscope`).
Asking `boxscore-noscope` for `boxscore:calc` is refused by Keycloak with
`invalid_scope`, not silently dropped.

**4. Check.** Each command is followed by what it returned:

```bash
curl -s http://127.0.0.1:3819/.well-known/oauth-protected-resource/mcp
# {"authorization_servers":["http://127.0.0.1:8080/realms/boxscore"],"bearer_methods_supported":["header"],"resource":"http://127.0.0.1:3819/mcp","scopes_supported":["boxscore:calc"]}

INIT='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"curl","version":"0"}}}'
curl -s -o /dev/null -D - -X POST http://127.0.0.1:3819/mcp \
  -H 'Content-Type: application/json' -d "$INIT"            # no token
# HTTP/1.1 401 Unauthorized   (other headers left out here and below)
# www-authenticate: Bearer resource_metadata="http://127.0.0.1:3819/.well-known/oauth-protected-resource/mcp"

curl -s -o /dev/null -w '%{http_code}\n' -X POST http://127.0.0.1:3819/mcp \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d "$INIT"
# 200   (the body is the initialize result: serverInfo boxscore-exact, protocolVersion 2025-06-18)

ARGS='{"currency":"USD","expense_convention":"positive_costs",
  "actuals":[{"account_code":"4000","account_name":"rent","category":"rental income","amount":"100.10"},
             {"account_code":"5000","account_name":"payroll","category":"payroll","amount":"30.00"}],
  "budgets":[{"account_code":"4000","account_name":"rent","category":"rental income","amount":"90.05"},
             {"account_code":"5000","account_name":"payroll","category":"payroll","amount":"25.00"}]}'
CALL=$(curl -s -X POST http://127.0.0.1:3819/mcp \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"noi_variance\",\"arguments\":$ARGS}}")
echo "$CALL" | jq -c '.result | {isError, status: .structuredContent.status, trace_id: .structuredContent.trace_id,
                                 noi_variance: .structuredContent.result.noi_bridge.noi_variance}'
# {"isError":false,"status":"calculated","trace_id":"43273dce-ffa9-4708-ab9d-586226e7ac67","noi_variance":"5.05"}

curl -s -o /dev/null -D - -X POST http://127.0.0.1:3819/mcp \
  -H "Authorization: Bearer $NOSCOPE" -H 'Content-Type: application/json' -d "$INIT"
# HTTP/1.1 403 Forbidden
# www-authenticate: Bearer error="insufficient_scope", scope="boxscore:calc", resource_metadata="http://127.0.0.1:3819/.well-known/oauth-protected-resource/mcp"

curl -s -X POST http://127.0.0.1:3819/v1/calc/noi_variance \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d "$ARGS" \
  | jq -c '{status, trace_id, noi_variance: .result.noi_bridge.noi_variance}'
# {"status":"calculated","trace_id":"ad2c5255-76f4-4336-8668-81285af8bc17","noi_variance":"5.05"}

TRACE_ID=$(echo "$CALL" | jq -r .result.structuredContent.trace_id)
boxscore-exact trace --database ./kc-test.sqlite --id "$TRACE_ID" | jq -r .result.trace.caller
# http:oauth:d3c6397a-07b8-4242-9664-5e45b750b46b   (alice's Keycloak user id; yours differs)
```

The `/v1/calc` call with `$NOSCOPE` gets the same `403`, naming the
unsuffixed metadata document. `review_period` on the imported revision
returned `200` with `noi_variance` `-5100.00`, as in the quickstart. Your
trace ids differ.

**What Keycloak's introspection returns.** The server's own view, with
`boxscore-server`'s credentials (`jti` and `sid` left out; keys sorted here, Keycloak's order differs):

```bash
curl -s -u boxscore-server:fake-server-secret-change-me -d "token=$TOKEN" \
  "$KC/token/introspect" | jq 'del(.jti, .sid)'
```

```json
{
  "active": true,
  "aud": "http://127.0.0.1:3819/mcp",
  "azp": "boxscore-test",
  "client_id": "boxscore-test",
  "exp": 1791806008,
  "iat": 1791805708,
  "iss": "http://127.0.0.1:8080/realms/boxscore",
  "scope": "openid boxscore:calc",
  "sub": "d3c6397a-07b8-4242-9664-5e45b750b46b",
  "token_type": "Bearer",
  "typ": "Bearer",
  "username": "alice"
}
```

- `aud` is a single string when the audience mapper adds the only audience;
  it becomes an array once another audience is added (for example by the
  built-in `roles` scope, which adds `account`). The server accepts both.
- `scope` lists `boxscore:calc` but not `basic`, whose
  `include.in.token.scope` is off.
- Keycloak 26.0 also answers `"active": true` for alice's **refresh token**
  and **ID token**, even with `token_type_hint=access_token`, and both carry
  `http://127.0.0.1:3819/mcp` in `aud`; the refresh token's `scope` even
  includes `boxscore:calc`. Only `token_type`/`typ` tell them apart:
  `"Refresh"` and `"ID"`. The server refuses both with `401` on that check;
  this is the case it exists for.
- Keycloak 26.0 ignores the RFC 8707 `resource` parameter: a token request
  with `resource=http://127.0.0.1:3819/mcp` and without the scope got no
  `aud` at all. The audience comes only from the mapper.
- A token without `boxscore:calc` from a client that has no audience mapper
  of its own (here the realm's built-in `admin-cli`) has no `aud`, so the
  server answers `401`, not `403`: with the mapper on the scope, as in the
  [checklist](#idp-configuration-checklist), a missing scope also means a
  missing audience.
- Introspection with a wrong client secret is answered `401` by Keycloak,
  which the server reports as `503` (see the outcome table).

**Failure check.** With Keycloak stopped, a token the server had not cached
got `503` with `retry-after: 5`, and stderr said
`OAuth introspection at http://127.0.0.1:8080/realms/boxscore/protocol/openid-connect/token/introspect failed (connection failed); answered 503`;
alice's already accepted token kept working from the cache. Neither the
token nor the secret appeared in the server's output.

#### Browser sign-in with Keycloak (verified 2026-10-14)

Same Keycloak 26.0.8 tarball, Java 21, realm file as committed, fresh
database with the quickstart month; the server started as in step 2 with
`--oauth-browser-client-id boxscore-workbench` added. The banner's last line
was
`boxscore-exact serve: workbench sign-in: OAuth via http://127.0.0.1:8080/realms/boxscore (client boxscore-workbench)`,
and `curl -si http://127.0.0.1:3819/workbench/auth.json` answered `200`
with the page's headers, `connect-src 'self' http://127.0.0.1:8080`
(the token endpoint's origin, as the page then named it; see the re-run
below for the exact-URL form), and
`mode` `oauth`, the two Keycloak endpoints under
`/realms/boxscore/protocol/openid-connect/`, `client_id`
`boxscore-workbench`, `scope` `openid boxscore:calc`, `audience`
`http://127.0.0.1:3819/mcp` and `redirect_uri` `http://127.0.0.1:3819/`.

No browser was available on the test host, so the page's own `app.js` ran
in jsdom 24 under Node 24 (`crypto.subtle` from Node's WebCrypto; a fetch
shim that sent `Origin: http://127.0.0.1:3819`, enforced the page's
`connect-src` and refused a cross-origin answer without a matching
`Access-Control-Allow-Origin`), and `curl` with a cookie jar played the
user at Keycloak's login form. What happened:

- Before sign-in the page showed the sign-in panel (no key form) and
  `HTTP 401 UNAUTHORIZED · A valid bearer token is required · Sign in to continue.`
- *Sign in* produced exactly one navigation, to
  `http://127.0.0.1:8080/realms/boxscore/protocol/openid-connect/auth` with
  `response_type=code`, `client_id=boxscore-workbench`,
  `redirect_uri=http://127.0.0.1:3819/`, `scope=openid boxscore:calc`, a
  32-character `state`, a 43-character `code_challenge` equal to the
  base64url SHA-256 of the stored 64-character verifier,
  `code_challenge_method=S256` and `resource=http://127.0.0.1:3819/mcp`;
  `sessionStorage` then held only the pending entry.
- `curl` fetched that URL (Keycloak's login page), posted `alice` and
  `fake-alice-password` to the form's action and got `302` to
  `http://127.0.0.1:3819/?state=...&session_state=...&iss=...&code=...`
  with the same `state`.
- Loading that URL in the same tab (its `sessionStorage` kept), the page
  POSTed the five form fields to the token endpoint (`200`, Keycloak
  answered `Access-Control-Allow-Origin: http://127.0.0.1:3819`), then
  `describe` and `list_revisions` with the bearer (both `200`), and showed
  "1 revision loaded" and "Signed in through
  http://127.0.0.1:8080/realms/boxscore; the sign-in expires at ..." (300
  seconds). The address was `http://127.0.0.1:3819/` with no query;
  `sessionStorage` held only the token entry, and the verifier appeared in
  no stored value. The page logged nothing to the console.
- `list_revisions` with that token from `curl`: `200`, `calculated`, one
  revision.
- Loading the same callback URL again: "Sign-in response did not match
  this tab; start again.", no token request, query removed.
- A stored token whose `expires_at` had passed, and a token the server
  refused (`401`): dropped, "Your sign-in expired; sign in again."
- A token from `boxscore-noscope`: `HTTP 403 INSUFFICIENT_SCOPE · The access
  token does not carry the required scope · The identity provider did not
  grant boxscore:calc to this sign-in: ask IT to add it to the workbench
  client.`
- *Sign out*: token gone, "Signed out of this page. Your session at the
  identity provider is not ended: ...".
- A real code redeemed with a different verifier: Keycloak answered
  `invalid_grant` / `PKCE verification failed: Code mismatch`, shown as
  "The identity provider refused the sign-in: invalid_grant: PKCE
  verification failed: Code mismatch"; an `error_description` with markup
  was shown as text.

Not covered by this run: a real browser's CORS check and CSP
enforcement (the shim imitated both), Keycloak's consent screen (the client
has consent off), and any IdP other than Keycloak.

**Re-run the same day** after the review fixes (fresh Keycloak 26.0.8 from
the tarball, the committed realm file, fresh database), with the shim now
matching `connect-src` by exact URL as CSP3 does:

- Start-up passed the new PKCE check (Keycloak's metadata lists
  `code_challenge_methods_supported` `["plain","S256"]`), and the page's
  policy was
  `connect-src 'self' http://127.0.0.1:8080/realms/boxscore/protocol/openid-connect/token`.
  The sign-in, the exchange (`200`, `Access-Control-Allow-Origin:
  http://127.0.0.1:3819`), `describe` and `list_revisions` (`200`) went as
  above; Keycloak's callback carries `iss`
  (`http://127.0.0.1:8080/realms/boxscore`), which matched. A fetch from the
  page to Keycloak's `userinfo` endpoint was refused by `connect-src`.
  `auth.json` and the resource metadata were fetched without a bearer.
- Expiry mid-session, with an API key also stored in the tab: signed in and
  loaded, the token's `expires_at` passed, and *Reload* sent nothing,
  neither the token nor the key; the panel switched from "Signed in
  through ..." to "Your sign-in expired; sign in again." with the sign-in
  button (and *Sign out*), and the status read "Could not list revisions:
  Nothing was sent. Your sign-in expired; sign in again." After *Sign out*
  the next *Reload* went out with the key. A page loaded with an expired
  token sent no API request at all; a token the server refused (`401`) left
  the same "expired" state, so no later request fell back to a key.
- Opened at `http://localhost:3819/`: *Sign in* did not navigate and said
  "Sign-in works only at http://127.0.0.1:3819/; open the workbench there."
- A callback with another issuer's `iss`, and one without `state`: "Sign-in
  response did not match this tab; start again.", query removed, pending
  entry dropped, no token request.

Clean up with `docker rm -f kc` (or stop `kc.sh`), stop the server, and
delete `kc-test.sqlite` and `kc-client-secret`.

## MCP over HTTP

- Each `POST /mcp` carries one JSON-RPC message with
  `Content-Type: application/json`. Clients may send
  `Accept: application/json, text/event-stream`. The server always answers
  `application/json` and never streams. An `Accept` header that rules out
  JSON gets `406`.
- A request gets `200` with its JSON-RPC response, including JSON-RPC errors
  for that request. A notification or a client response gets `202` with an
  empty body.
- A batch (a JSON array), a parse error or a `null` id gets `400` with a
  JSON-RPC error (`-32600` or `-32700`, `id: null`).
- An `MCP-Protocol-Version` header outside the supported versions gets `400`.
  A missing header is allowed.
- Bodies over 2 MiB get `413`.
- The server is stateless. It issues no `Mcp-Session-Id`, keeps no
  per-session state, and answers `GET` and `DELETE /mcp` with `405`.
- Tools, the `boxscore_grounding` and `start_here` prompts, the grounding
  rule and the envelope are the same as on stdio. See
  [claude-desktop.md](claude-desktop.md#tools) and
  [claude-desktop.md](claude-desktop.md#prompts).

## Origin allowlist (DNS rebinding)

A request with an `Origin` header passes only when that origin is the
server's own or matches an `--allow-origin` value. The server's own origins
are the origin of `--public-url` and, when the public URL or the bind address
is loopback, `http://127.0.0.1:<port>`, `http://localhost:<port>` and
`http://[::1]:<port>`; they let the workbench page served at `/` call the
API. Matching uses scheme, host and port, ignores case and drops default
ports. Anything else gets `403`, on every route and before authentication.
The default allowlist is empty, so no page from another origin can call the
server. Server-side clients send no `Origin` and are not affected; the
claude.ai and ChatGPT connectors call from the vendors' servers, not from
the browser. Add an origin only for a browser-based MCP client you run
yourself. Each value must be `scheme://host[:port]` with no path.

## TLS

The server speaks plain HTTP. Terminate TLS at a reverse proxy, bind the
server to loopback and pass `--public-url https://...` with `--api-key-file`,
OAuth, or both. For Caddy:

```
boxscore.example.com {
    reverse_proxy 127.0.0.1:3819
}
```

The proxy should pass `Authorization` and `Origin` through unchanged and
should not log `Authorization`. Set request-header and idle timeouts at the
proxy; the server does not set them. Keep the proxy's body limit at or above
2 MiB so that the server's own `413` applies. With OAuth, also rate-limit
unauthenticated requests at the proxy: each new unknown token costs one
introspection call (refusals are then cached for 30 seconds), and the
server's own per-caller rate limit applies only after authentication. The
server caps those calls itself (16 concurrent, 120 a minute, then `503`),
but a flood that reaches the cap also delays signed-in users whose verdict
is not cached.

## What is logged

- On stderr: one line at start-up (the bound address and the auth mode), a
  second line with OAuth (issuer, introspection endpoint, audience, scope),
  a third with browser sign-in (issuer and the page's client id),
  one line at shutdown, a warning for a group-readable key or secret file,
  start-up refusals, and a line for a failed or refused introspection call
  (the endpoint and a failure category such as `timed out`,
  `connection failed`, `HTTP status 500` or the call limit reached), at most
  one every 10 seconds; the next printed line says how many similar failures
  were suppressed in between.
- With `BOXSCORE_MCP_DEBUG=1`: MCP method names, never arguments, as on
  stdio.
- Nothing else. The server writes no access log, request bodies, results,
  tokens or client secrets. It writes no files apart from what the catalog
  stores in `--database` (and `BOXSCORE_TRACE_DIR`, when set). Each catalog
  call's trace record names its caller as `http:<key_id>` for an API key,
  `http:oauth:<subject>` for an OAuth token (`http:local` without a
  validator); see [PROVENANCE.md](../PROVENANCE.md). The OAuth subject is the
  IdP's `sub`, which some IdPs make an opaque id and others an e-mail
  address. Error messages about the key and secret files name line numbers
  or modes, never secrets.
