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
| `POST /v1/calc/{function}` | bearer | one catalog function; body is its input, response is the envelope |
| `GET /openapi.json` | none | the OpenAPI 3.1 contract; see [openapi.md](openapi.md) |
| `GET /health` | none | `{"status":"ok","producer":{...}}` |
| `GET /.well-known/oauth-protected-resource` | none | RFC 9728 protected-resource metadata |

## Run it

```bash
boxscore-exact serve \
  --bind 127.0.0.1:3819 \
  --database /srv/boxscore/exact.sqlite \
  --api-key-file /etc/boxscore/api-keys \
  --public-url https://boxscore.example.com \
  [--allow-origin https://inspector.example.com]... \
  [--authorization-server https://idp.example.com]... \
  [--rate-limit 60]
```

- `--bind` takes an IP address and port, not a host name. The default is
  `127.0.0.1:3819`. The server refuses to start on a non-loopback address
  unless `--api-key-file` is given. It also refuses a non-loopback
  `--public-url` without `--api-key-file`. A loopback bind behind a same-host
  reverse proxy is still exposed, and this check catches that case.
- Without `--api-key-file` the server needs no token and answers loopback
  only. Use that mode for local development. Do not use it behind a proxy.
- `--database` is fixed for the life of the process. No request can choose
  it. A `database` field in tool arguments or in a `/v1/calc` body is refused
  as `INVALID_INPUT`. Without `--database`, the stateful functions refuse with
  `DATABASE_REQUIRED`.
- `--public-url` is the resource identifier in the metadata and in the
  `WWW-Authenticate` challenge, and the `servers` URL in `/openapi.json`. The
  default is `http://127.0.0.1:<port>`.
- `--rate-limit` sets requests per minute per key. The default is 60. Each key
  is a token bucket with a burst of that size, refilled continuously. Going
  over returns `429` with `Retry-After`. Public routes are not limited.
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
- A missing token and a wrong token get the same `401`:

  ```
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
- Tools, the `boxscore_grounding` prompt, the grounding rule and the
  envelope are the same as on stdio. See
  [claude-desktop.md](claude-desktop.md#tools).

## Origin allowlist (DNS rebinding)

A request with an `Origin` header passes only when that origin matches an
`--allow-origin` value. Matching uses scheme, host and port, ignores case and
drops default ports. Anything else gets `403`, on every route and before
authentication. The default allowlist is empty, so no browser page can call
the server. Server-side clients send no `Origin` and are not affected. Add an
origin only for a browser-based MCP client you run yourself. Each value must
be `scheme://host[:port]` with no path.

## TLS

The server speaks plain HTTP. Terminate TLS at a reverse proxy, bind the
server to loopback and pass `--public-url https://...` and `--api-key-file`.
For Caddy:

```
boxscore.example.com {
    reverse_proxy 127.0.0.1:3819
}
```

The proxy should pass `Authorization` and `Origin` through unchanged and
should not log `Authorization`. Set request-header and idle timeouts at the
proxy; the server does not set them. Keep the proxy's body limit at or above
2 MiB so that the server's own `413` applies.

## Claude.ai and ChatGPT (needs stage 2b)

Claude.ai custom connectors and ChatGPT Developer Mode connectors reach the
server from the vendor's cloud. They authenticate with OAuth, not with a
static key you paste in. Until an OAuth identity provider is in front of this
server (stage 2b, a later issue), use API keys from scripts and Claude Code
only. The setup below applies once stage 2b ships.

1. Run the server behind TLS on a public host name, with
   `--public-url https://boxscore.example.com` and
   `--authorization-server https://idp.example.com` (your OAuth 2.1
   authorization server).
2. Clients find the authorization server from the `WWW-Authenticate`
   challenge and `/.well-known/oauth-protected-resource`, then request scope
   `boxscore:calc`.
3. **Claude.ai**: Settings → Connectors → Add custom connector. Enter the URL
   `https://boxscore.example.com/mcp`. Enter the OAuth client ID and secret
   under Advanced settings if your IdP does not support dynamic client
   registration.
4. **ChatGPT**: enable Developer Mode (Settings → Apps & Connectors →
   Advanced), create a connector with the MCP server URL
   `https://boxscore.example.com/mcp`, and choose OAuth authentication.

There is no GPT Action. `/openapi.json` is the written contract, not an
Action manifest.

In stage 2a, `--authorization-server` only advertises the IdP in metadata. No
OAuth access token is accepted until stage 2b adds its validator behind the
same `TokenValidator` seam as the API keys.

## What is logged

- On stderr: one line at start-up (the bound address and the auth mode), one
  line at shutdown, a warning for a group-readable key file, and start-up
  refusals.
- With `BOXSCORE_MCP_DEBUG=1`: MCP method names, never arguments, as on
  stdio.
- Nothing else. The server writes no access log, request bodies, results or
  tokens. It writes no files apart from what the catalog stores in
  `--database` (and `BOXSCORE_TRACE_DIR`, when set). Each catalog call's trace
  record names its caller as `http:<key_id>` (`http:local` without a key
  file); see [PROVENANCE.md](../PROVENANCE.md). Error messages about the key
  file name line numbers, never secrets.
