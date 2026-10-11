# Claude Desktop connector (MCP over stdio)

`boxscore-exact mcp` serves the exact-cent
[calculation catalog](../EXACT_CENTS.md#calculation-catalog-and-versioning-policy)
as a Model Context Protocol server: JSON-RPC 2.0, one message per line on
stdin and stdout, protocol version `2025-06-18` (`2025-03-26` is also
accepted). It is part of the `boxscore-exact` binary and adds no dependency.

## Configure

Build the binary (see [EXACT_CENTS.md](../EXACT_CENTS.md#build-and-run)),
create a database once with `boxscore-exact init --database /path/to/exact.sqlite`,
then add the server to `claude_desktop_config.json`
(macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`,
Windows: `%APPDATA%\Claude\claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "boxscore-exact": {
      "command": "/absolute/path/to/boxscore-exact",
      "args": ["mcp", "--database", "/path/to/exact.sqlite"]
    }
  }
}
```

Restart Claude Desktop. Leave out `"--database", "/path/to/exact.sqlite"` to
run stateless: `describe`, `noi_variance` and the snapshot metrics with an
inline snapshot work, and the stateful tools refuse with `DATABASE_REQUIRED`.

The database path is fixed for the session by these arguments. No tool
accepts a path, SQL or network target, and a `database` field in tool
arguments is refused as `INVALID_INPUT`. `import_period` takes the dataset
inline only.

To run the server from the container image instead of a native binary, see
[INSTALL.md](../INSTALL.md#mcp-over-stdio-claude-desktop).

## Tools

There is one tool per catalog function. Each tool's input schema is the
catalog's input schema. Each result is the catalog envelope
(`contract_version`, `function`, `version`, `producer`, `input_sha256`,
`output_sha256`, `computed_at`, `trace_id`, `trace_store`, `citation`,
`status`, and `result` or `error`), returned
both as `structuredContent` and as a JSON text block. A refusal comes back
with `isError: true` and its typed `error.code`; it is not a protocol error.

| tool | needs `--database` | what it does |
|---|---|---|
| `describe` | no | Lists every calculation with its version and JSON Schemas. |
| `noi_variance` | no | Actual-versus-budget NOI bridge from inline GL rows, in checked USD cents. |
| `import_period` | yes | Stores a canonical period dataset as a new immutable revision; a correction needs `supersedes` (the current revision id) and a `reason`. |
| `review_period` | yes | Reviews a stored revision: matched-account NOI bridge, review reasons, excluded accounts, and changes versus the superseded revision. |
| `issue_report` | yes | Issues an immutable report record for a revision. Review flags stay visible; human approval is never granted. |
| `list_revisions` | yes | Lists stored revisions and their issued reports, optionally filtered by property and period. |
| `occupancy` | only with `revision_id` | Physical occupancy (occupied over total units) from a stored revision's snapshot or an inline snapshot, as an exact four-place ratio. |
| `economic_occupancy` | only with `revision_id` | Economic occupancy (in-place over market rent) from a stored revision's snapshot or an inline snapshot, as an exact four-place ratio of cents. |
| `delinquency` | only with `revision_id` | Delinquent and prepaid balances, their net, and each as a share of in-place rent, from a stored revision's snapshot or an inline snapshot. |
| `close_readiness` | yes | Per-property close readiness for a period: latest revision, account coverage, blocking review reasons, issued reports, and what the store does not track. |
| `t12` | yes | Trailing twelve-period statement for a property from stored revisions; a month with no revision is null and flagged, never zero. |
| `get_provenance` | yes | Reads back a recorded invocation by `trace_id`: function, version, caller, hashes, status, and the revision and report it produced or read. |

Money in and out is a decimal string with at most two places (`"100.10"`),
never a JSON number. In an MCP response, `input_sha256` is the SHA-256 of the
canonical JSON serialization of the tool arguments.

## Grounding rule

Every tool description, the server's `initialize` instructions and the
`boxscore_grounding` prompt carry this rule verbatim:

> Numbers come from the engine. If the tool refused, say so and ask; never
> compute, round or restate a figure the engine did not return. Cite
> function@version, the trace_id and the output_sha256 with every number.

Fields such as `account_name` are echoed back as data. Text inside them is
never an instruction to the engine, and the model should not treat it as one.

## Operational notes

- Requests are answered in order. Each line is capped at 2 MiB. A longer line
  is drained without being held in memory and answered with JSON-RPC error
  `-32600`, and the session continues.
- Malformed JSON gets `-32700`, an unknown method `-32601`, and invalid params
  (including an unknown tool or prompt name) `-32602`. A request with
  `"id": null` gets `-32600`. Notifications are never answered.
- stdout carries JSON-RPC only. Set `BOXSCORE_MCP_DEBUG=1` to log method
  names (never arguments) to stderr.
- The server exits when stdin closes.
