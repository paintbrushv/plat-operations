# OpenAPI contract

`GET /openapi.json` on `boxscore-exact serve` returns an OpenAPI 3.1 document.
It is the written contract for the HTTP API. It is not a GPT Action manifest,
and none is published. ChatGPT and Claude reach the catalog through
[MCP](remote-mcp.md).

The document is generated from `boxscore::exact::catalog::CATALOG` when the
server starts, so it cannot drift from the code:

- There is one `POST /v1/calc/{function}` operation per catalog function.
  `operationId` is the function name and `summary` is `function@version`.
- The request body schema is that function's catalog input schema, verbatim
  (JSON Schema 2020-12, the OpenAPI 3.1 default dialect).
- The `200` response is the provenance envelope, with `result` typed by the
  function's output schema. Its `status` is `calculated` or
  `review_required`.
- The `422` response is the same envelope with `status: refused` and a typed
  `error`.
- On `/v1/calc`, `400`, `401`, `403`, `413`, `415` and `429` return the
  `Error` body `{"error":{"code","message"}}`. `401` carries the
  `WWW-Authenticate` challenge. On `/mcp`, transport refusals (`400`, `406`,
  `413`, `415`) are JSON-RPC errors with `id: null`.
- An unknown function gets `404` with code `UNKNOWN_FUNCTION`.
- `?version=X.Y.Z` pins a function version. A different served version
  refuses with `VERSION_MISMATCH`.
- An empty body means `{}`. `input_sha256` is the SHA-256 of the body bytes
  exactly as received.
- The document also covers `/mcp`, `/health`, `/openapi.json` and
  `/.well-known/oauth-protected-resource`. The last three set
  `security: []`. Everything else needs `bearer`.
- `servers[0].url` is the server's `--public-url`. Nothing else in the
  document depends on configuration.

Each function keeps its own semantic version, which the catalog's
[versioning policy](../EXACT_CENTS.md#calculation-catalog-and-versioning-policy)
governs. `info.version` is the producer version. A consumer should pin
function versions rather than the document.

Fetch a copy to review or diff:

```bash
curl -s http://127.0.0.1:3819/openapi.json > boxscore-exact.openapi.json
```

`tests/exact_mcp_http.rs` checks that the document is OpenAPI `3.1.0` and
that it lists every catalog function. It also checks that each request schema
equals the catalog input schema, that each `200` result schema equals the
output schema, and that every `$ref` resolves.
