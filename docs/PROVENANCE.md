# Provenance: trace ids and citations

Every call into the `boxscore-exact` calculation catalog gets a trace id, and
its envelope carries a one-line citation. When the host runs with a database,
the call is also written to an append-only `exact_invocations` table. This
covers the CLI (`call`, `trace`), the stateless `protocol` and any connector
built on `catalog::invoke`. Successful calls and refusals are both recorded.
The citation lets a reader find the call again and check the number in a memo
against what the engine computed.

## What a citation says

```
boxscore issue_report@1.1.0 · trace 3f0c9b52-6a0e-4d0b-9a77-1c2d3e4f5a6b · input sha256:1b7638a0c2d4… · output sha256:ce5487f1e2a3… · revision 8e1d… · report 41a9…
```

| part | meaning |
|---|---|
| `issue_report@1.1.0` | the catalog function and the exact version that served the call (`@unknown` if no such function exists) |
| `trace <id>` | the full UUID v4 `trace_id` for this call; use it to look the record up |
| `input sha256:…` | first 12 hex characters of `input_sha256`, the hash of the input bytes as the transport received them (canonical JSON for in-process calls) |
| `output sha256:…` | first 12 hex characters of `output_sha256`, the hash of the canonical JSON `result` |
| `refused <CODE>` | appears instead of the output hash when the engine refused, e.g. `refused NOT_FOUND` |
| `revision <id>` | the stored revision the call created or read: `import_period`, `review_period` and `issue_report` return it, and a snapshot metric run with `revision_id` read it |
| `report <id>` | the immutable report record `issue_report` created |

The envelope also has `trace_id` and `trace_store`. `trace_store` is
`"database"`, `"jsonl"` or `"none"` and says where the record was written.
If a record should have been written and was not, the envelope also has a
`trace_warning`.

## Where the record goes

- **The host supplied a database** (`--database`, or a connector's configured
  path). The record is inserted into `exact_invocations` in that database.
  The insert happens after the calculation's own writes have committed, as a
  separate statement. The table has the same UPDATE/DELETE refusal triggers as
  the other exact tables.
- **No database, and `BOXSCORE_TRACE_DIR` names an existing directory.** The
  same fields are appended as one JSON line to
  `$BOXSCORE_TRACE_DIR/invocations.jsonl`. The file is created with mode 0600.
  This includes stateless `protocol` calls: the directory comes from the host
  environment, never from the request.
- **Otherwise** nothing is written, and `trace_store` is `"none"`.

Tracing never fails a calculation. Suppose the database is missing, was built
for another schema, or is read-only, or the trace directory is missing or not
writable. The result is still returned, with `trace_store: "none"` and a
`trace_warning` that explains the cause. A stateful function refuses for the
same database problem anyway, and that refusal is in `error`.

Each record has these fields: `trace_id`, `function`, `version`,
`producer_version`, `caller` (`cli`, `protocol`, `mcp`, `http:<key_id>`, or
`unknown` if a host did not set it), `input_sha256`, `output_sha256` (null for a refusal),
`status` (`calculated`, `review_required` or `refused`), `error_code`,
`revision_id`, `report_id`, `input_json`, `started_at` and `finished_at`.

### Retention controls

| variable | effect |
|---|---|
| `BOXSCORE_TRACE_INPUTS=1` | also store the canonical JSON of the input in `input_json`. Without it `input_json` is NULL and only the hash is kept, so the trace stores no operating numbers. Whoever runs the host decides whether to set it. |
| `BOXSCORE_TRACE_DIR=<dir>` | the directory for the JSONL trace when no database is in use. |

`input_json` is the canonical serialization: keys sorted, no whitespace. When
a transport hashed the raw bytes it received, `sha256(input_json)` can differ
from `input_sha256`, because the two may differ only in formatting. Re-running
the function on `input_json` reproduces `output_sha256`.

## How to verify a citation

1. Look the record up by its trace id:

   ```bash
   boxscore-exact trace --database ./exact.sqlite --id 3f0c9b52-6a0e-4d0b-9a77-1c2d3e4f5a6b
   # equivalently
   echo '{"trace_id":"3f0c9b52-…"}' | boxscore-exact call --database ./exact.sqlite --function get_provenance
   ```

   `get_provenance@1.0.0` returns `{"trace": {...}, "revision": {...}|null,
   "report": {...}|null}`. `trace` is the stored record. `revision` is set when
   the record names a revision, and gives its `id`, `property`, `period`,
   `revision`, `parent_id`, `reason`, `input_sha256` and `sources`. `report` is
   set when the record names a report, and gives its `id`, `created_at` and
   `body_sha256`. An unknown id refuses with `NOT_FOUND`.
2. Check that the record's function, version and the hash prefixes match the
   citation.
3. Reproduce the number:
   - For a stored revision, run `review_period` with the `revision_id`. Review
     checks the stored data against its `input_sha256` first and refuses
     `INTEGRITY_ERROR` if a row was changed. For `review_period` and the
     snapshot metrics, the same revision and the same `function@version` give
     the same `output_sha256`.
   - For an inline calculation, re-run the function on the original input
     (from `input_json` if it was retained, otherwise from your own copy) and
     compare `output_sha256`.
   - For an issued report, compare `sha256` of the stored report body with
     `report.body_sha256`. An issued report includes its own `report_id` and
     `issued_at`, so issuing again creates a new report with a different hash.
     Compare against the record; do not compare against a fresh issue. From
     `issue_report@1.1.0`, a superseded revision refuses (`SUPERSEDED`), so a
     report on a revision that has since been corrected is a record of what
     was issued before the correction, not a current figure.

## What a citation does not prove

- **It is an integrity check, not access control.** The hashes and triggers
  catch accidental or casual changes. Anyone with write access to the SQLite
  file or the JSONL file can still drop the triggers, rewrite rows or delete
  lines. Protect the files with operating-system permissions and backups.
- **It proves what the engine computed, not that the inputs were right.** A
  citation shows that this `function@version` turned these input bytes into
  this result. It does not show that the vendor export was complete, that
  accounts were mapped correctly, or that the right period was imported. The
  revision `sources` record which files were used: hashes the server computed
  of the bytes it read, plus anything the caller sent under
  `sources.supplied`, which is stored as sent and not verified. They do not
  prove that those files are correct.
- **It is not approval.** Reviews and reports still say
  `human_approval: not_granted`.
- **Missing records are possible.** A call with `trace_store: "none"` left no
  record. Calls made outside the catalog are not traced: the legacy `variance`
  protocol operation and the direct `import`, `import-csv`, `review`, `issue`
  and migration subcommands.

## Schema version 2

`exact_invocations` raised the exact-cent database schema (`PRAGMA
user_version`) from 1 to 2. Nothing had been published at schema 1, so there
is no in-place upgrade. Opening a schema-1 database refuses with
`SCHEMA_MISMATCH`, and the message says that the database was created by
schema 1. To continue, either re-import its periods into a new database
(`boxscore-exact init`, then `import` / `import-csv` / `import_period`), or
rebuild it by reviewed copy migration from the original legacy source.
