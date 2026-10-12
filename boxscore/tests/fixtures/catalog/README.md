# Catalog conformance corpus

Golden cases for `boxscore/tests/exact_conformance.rs`. Every case runs four
ways and all four must return the same envelope:

1. `catalog::invoke`
2. `mcp::Server::handle` with a `tools/call` (what `boxscore-exact mcp` runs)
3. `POST /v1/calc/{function}` through the axum router
4. `POST /mcp` (`tools/call`) through the axum router

The router uses the loopback configuration `boxscore-exact serve` builds
without `--api-key-file` (no authentication). The paths must agree on
`status`, `result`, `error`, `function`, `version`, `producer`,
`input_sha256` and `output_sha256`; match the golden; and every `result` must
satisfy the function's published output schema (the test carries a small
structural validator and names the path that does not conform).

Every call gets its own `trace_id` and `citation`, so those two, plus
`computed_at`, `trace_store` and `trace_warning`, are never compared across
paths. Each path is only checked for shape: `trace_id` is an id, `citation`
names `function@version` and the trace id, `trace_store` is one of `database`,
`jsonl`, `none`.

All data is synthetic. Money is always a decimal string.

## Adding a case

Drop two files in the function's directory; no Rust changes:

```
<function>/<case>.input.json      the input object, exactly what a caller sends
<function>/<case>.expected.json   the golden
```

`<function>` must be a catalog function name; a misspelled directory fails the
layout test. Create the golden from the engine's current output, then review it
by hand before committing:

```
BOXSCORE_UPDATE_GOLDENS=1 cargo test --test exact_conformance
```

The input must be a JSON object. A stateless function runs with no database.
A stateful function runs against a freshly created, empty exact-cent database
(one copy per path), so single cases for stateful functions are refusals on an
empty database or empty listings; anything that needs prior state is a chain.

## `expected.json`

```json
{
  "version": "1.0.0",
  "status": "calculated",
  "result": { "...": "..." },
  "output_sha256": "..."
}
```

| key | meaning |
|---|---|
| `version` | required. The function version the golden was generated at. |
| `status` | required. `calculated`, `review_required` or `refused`. |
| `result` | required unless `refused`. Compared exactly (keys and values). |
| `error` | required when `refused`: `{"code": "..."}`. Only `code` is pinned. |
| `output_sha256` | optional for single cases. Present for deterministic results. |

Unknown keys are rejected.

## Chains

`chain/<case>/steps.json` is an ordered list of steps run against one evolving
database, for behavior that needs state (import, review, issue, correction,
list). A chain case covers every function it calls.

```json
{
  "description": "what the story shows",
  "steps": [
    {
      "name": "import original",
      "function": "import_period",
      "input": { "dataset": { "...": "..." } },
      "capture": { "rev1": "/result/revision_id" },
      "expected": { "version": "1.0.0", "status": "calculated",
                    "result": { "revision_id": "<id>" }, "output_sha256": "<sha256>" }
    },
    { "name": "review original", "function": "review_period",
      "input": { "revision_id": "${rev1}" }, "expected": { "...": "..." } }
  ]
}
```

- `capture` maps a name to a JSON Pointer into the step's envelope (for
  example `/result/revision_id`, or `/trace_id` to feed `get_provenance`). A later
  `input` may use a string that is exactly `"${name}"`, replaced by the captured
  id.
- Each step runs on all four paths against separate copies of the database as
  it was before the step (so the paths do not interfere); the direct path's
  database becomes the starting point of the next step.
- Values the engine generates are replaced by placeholders and compared by
  shape. Money values, statuses and everything else compare exactly.

| placeholder | matches |
|---|---|
| `"<id>"` | a non-empty id string (letters, digits, `-`, `_`) |
| `"<id=name>"` | the id captured as `name` earlier in the chain |
| `"<timestamp>"` | an RFC 3339 timestamp |
| `"<sha256>"` | 64 lowercase hex digits |
| `"<string>"` | any non-empty string (used for `producer_version`, which follows the crate release) |

Regenerating a chain writes placeholders for the keys `revision_id`,
`report_id`, `id`, `parent_id`, `supersedes_revision`, `trace_id` (ids;
`<id=name>` when the value is a captured one), `created_at`, `issued_at`,
`started_at`, `finished_at` (timestamps), `input_sha256`, `body_sha256`,
`output_sha256` (hashes) and `producer_version` (`<string>`). `output_sha256`
of a chain step is `<sha256>`.

## Versions and regenerating goldens

`expected.json` pins `version`. Three situations:

- **A number changes and the version does not.** The test fails with the path
  that differs. This is deliberate: a changed value needs a version bump
  (see the versioning policy in `docs/EXACT_CENTS.md`).
- **The version was bumped.** The test fails with `STALE GOLDEN` naming the
  function. Regenerate the affected goldens with
  `BOXSCORE_UPDATE_GOLDENS=1 cargo test --test exact_conformance`, then review
  the diff.
- **A golden is missing.** The test fails and says how to create it.

`BOXSCORE_UPDATE_GOLDENS=1` writes only goldens that are missing or whose pinned
`version` differs from the served one. It never rewrites a golden whose version
is unchanged, so regenerating cannot hide a regression, and it writes nothing
for a step on which the paths disagreed. Chain files are rewritten as a whole
(keys sorted, two-space indent).

`describe` lists the whole catalog, so adding a function or bumping any
function's version changes `describe`'s result: bump `describe`'s own version
too (a minor bump: it adds an entry) and regenerate its golden.

## Coverage

The test fails, naming the function, if a catalog function has neither a case
directory nor a step in any chain. Add a case when you add a function.
