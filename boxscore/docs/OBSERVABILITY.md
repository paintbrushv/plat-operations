# Observability

Boxscore uses `tracing`.

Logged events include:

- Task starts and completions.
- Tool calls.
- Ingestion counts.
- Validation failures.
- Gap creation.
- Question creation.
- Capability proposals.
- API requests through Tower HTTP tracing.
- Errors.

The `task_runs` and `tool_runs` tables provide durable local observability beyond process logs.
