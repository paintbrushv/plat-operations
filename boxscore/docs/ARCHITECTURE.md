# Architecture

Boxscore separates the replaceable model from the durable product harness.

## Layers

- CLI and API: Clap commands and Axum JSON endpoints.
- Persistence: SQLx with SQLite by default.
- Data registry: normalized tables for properties, periods, GL actuals/budgets, rent roll, delinquency, and leasing snapshots.
- Tool registry: typed metadata and logged tool runs for every analysis stage.
- Variance engine: deterministic revenue, expense, and NOI variance calculations.
- Gap engine: identifies missing data and low-confidence analysis areas.
- Question engine: turns gaps into prioritized operator questions.
- Memory layer: records specific reusable facts with confidence scores.
- Evolution engine: turns repeated gaps and missing data into capability proposals.
- Report writer: creates evidence-aware markdown reports under `reports/generated/`.

## Model Boundary

`ModelProvider` is a trait. v0.1 ships with `StubModelProvider`, which is deterministic and local. Future providers can implement OpenAI-compatible, Anthropic, or local-model adapters without changing the harness.

## Safety Boundary

Boxscore may propose capabilities, migrations, tools, and questions. It must not silently self-modify production code, delete data, transmit private data externally, or deploy.
