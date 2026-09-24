# `boxscore ask` — Natural-Language Query Layer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `boxscore ask "what did we pay 7 Kings Landscaping last 6 months"` — the model translates the question into local tool calls, the harness executes them against SQLite and renders tables with lineage, and ledger data never leaves the machine unless the user explicitly opts into narration.

**Architecture:** An `AnthropicProvider` (raw HTTP via reqwest — Rust has no official SDK) behind the existing `ModelProvider` module, a `ToolSpec` registry in a new `src/ask.rs` that wraps the SAME `db::` helpers the TUI uses, and a privacy-split flow: default mode makes exactly ONE API call (schema catalog + question in; tool calls out; results rendered locally, never round-tripped), `--narrate` mode runs a bounded agentic loop (≤4 rounds) where tool results return to the model with resident payees redacted. Every exchange logs to the existing `task_runs`/`tool_runs` tables.

**Tech Stack:** reqwest (rustls-tls, json features — no openssl), serde_json, existing sqlx/db helpers, Anthropic Messages API (`POST /v1/messages`, `anthropic-version: 2023-06-01`).

**API facts the implementer must honor (verified against current Anthropic docs 2026-06-11):**
- Default model `claude-opus-4-8`, overridable via `BOXSCORE_MODEL` env var. Auth via `ANTHROPIC_API_KEY` env (header `x-api-key`). Missing key → clear error telling the user to set it; never prompt for or store keys.
- Do NOT send `temperature`/`top_p`/`top_k` (400 on Opus 4.8). Do NOT send a `thinking` field. `max_tokens: 16000`.
- Tools go in `tools: [{name, description, input_schema}]`; responses carry `content` blocks of type `text` and `tool_use` (`{id, name, input}`); check `stop_reason` — handle `"refusal"` (surface stop_details if present, don't retry) and `"max_tokens"` before reading content.
- Continuation turns (narrate mode only): append `{"role":"assistant","content":<full response content>}` then `{"role":"user","content":[{"type":"tool_result","tool_use_id":..., "content": <string>}...]}`. Loop until `stop_reason == "end_turn"` or 4 rounds.
- Explicit HTTP timeout: 120s (repo rule: untimed LLM calls in pipelines are forbidden).
- Log model + input/output token usage (`usage.input_tokens`/`output_tokens`) into the task_run summary.

**Conventions (non-negotiable, established in this codebase):** stdout is for results, logs to stderr via tracing; cargo fmt/test/clippy clean before every commit; commit trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`; PII rule — `is_resident = 1` payees are excluded from vendor aggregations entirely and replaced with `"(resident)"` in any payload sent to the model in narrate mode.

---

## Phase A — Anthropic model provider

**Files:** Modify `boxscore/src/model_provider.rs`, `boxscore/Cargo.toml` (add `reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }`). Test in-module.

- [ ] **A1:** Define the tool-use completion surface alongside the existing trait (keep `summarize` and `StubModelProvider` untouched):

```rust
#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CompletionResponse {
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<String>,
    pub model: String,
    pub usage: Usage,   // { input_tokens: i64, output_tokens: i64 }
}

// Conversation message for multi-turn (narrate) requests.
#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage { pub role: String, pub content: serde_json::Value }

#[async_trait]
pub trait ToolUseProvider: Send + Sync {
    fn model(&self) -> &str;
    async fn complete(
        &self,
        system: &str,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<CompletionResponse>;
}

pub struct AnthropicProvider { /* api_key, model, http: reqwest::Client (120s timeout) */ }
impl AnthropicProvider {
    /// Errors with a setup hint when ANTHROPIC_API_KEY is unset.
    pub fn from_env() -> Result<Self>;
}
```

`complete` POSTs `https://api.anthropic.com/v1/messages` with headers `x-api-key`, `anthropic-version: 2023-06-01`, `content-type: application/json`; body `{model, max_tokens: 16000, system, messages, tools}` (omit `tools` when empty). Non-2xx → error including status and the API's `error.message`. `#[serde(other)] Other` keeps unknown block types from breaking deserialization.

- [ ] **A2 tests (no network):** deserialize a fixture response JSON containing text + tool_use + an unknown block type; `from_env` error message mentions `ANTHROPIC_API_KEY`; request body serialization omits forbidden fields (assert the serialized JSON has no `temperature`/`thinking` keys).
- [ ] **A3:** fmt/test/clippy clean → commit `feat(boxscore): anthropic tool-use provider`.

## Phase B — ask engine, tool registry, vendor spend, CLI

**Files:** Create `boxscore/src/ask.rs` (register in lib.rs). Modify `boxscore/src/db.rs` (one new helper + type), `boxscore/src/cli.rs`. Test: `boxscore/tests/ask_engine.rs`.

- [ ] **B1: `db::vendor_spend`** — the one missing query (also unblocks the future Vendors screen):

```rust
pub struct VendorSpend { pub payee: String, pub txn_count: i64, pub total: f64, pub first_period: String, pub last_period: String }
pub async fn vendor_spend(pool, property_id: Option<&str>, since_period: Option<&str>, payee_contains: Option<&str>, limit: i64) -> Result<Vec<VendorSpend>>;
// gl_transactions WHERE is_resident = 0 AND payee != '' [AND property_id=?] [AND period >= ?]
// [AND payee LIKE ? ESCAPE '\' — reuse the existing escape helper], GROUP BY payee,
// ORDER BY ABS(SUM(amount)) DESC LIMIT ? (cap 200). Tests: residents excluded; LIKE-escape.
```

- [ ] **B2: tool registry in `src/ask.rs`.** Six tools, each = a `ToolSpec` (JSON schema with `additionalProperties: false`, required fields marked) + an async executor returning `(serde_json::Value /*for logging+narrate*/, String /*rendered table for stdout*/)`:

| tool | args | executes |
|---|---|---|
| `list_properties` | — | `db::list_properties` + latest GL period per property |
| `account_activity` | `property` (name, fuzzy via `find_property_by_name`), `period` YYYY-MM | `db::account_activity` |
| `search_transactions` | `query`, `property?`, `limit?` (default 50, cap 200) | `db::search_transactions` |
| `t12_statement` | `property`, `end_period?` | `t12::assemble_t12` |
| `vendor_spend` | `property?`, `since_period?`, `payee_contains?`, `limit?` | `db::vendor_spend` (B1) |
| `close_readiness` | `period` | `close_readiness::assess_portfolio` |

Tool descriptions must be prescriptive about WHEN to call (e.g. vendor_spend: "Call this for any question about payments to a vendor/payee or vendor comparisons"). Unknown property name → executor returns an error string listing valid property names (the model sees nothing in default mode; the user sees the message).

- [ ] **B3: system prompt builder** — assembled fresh per ask from the live db: one paragraph on what Boxscore is; the property catalog (name, unit count, entity codes, GL period range); today's date and current period; rules: "Answer ONLY by calling tools. Never invent numbers. Prefer one tool call; use several only when the question genuinely spans them."

- [ ] **B4: ask flow** `pub async fn run_ask(pool, provider: &dyn ToolUseProvider, question: &str, narrate: bool) -> Result<AskResult>`:
  - Create task_run (`task_type = "ask"`, user_prompt = question).
  - **Default mode (one API call):** send system + question with tools (`tool_choice` omitted/auto). For each `tool_use` block: execute locally, print rendered table to stdout with a `── tool: vendor_spend {...args} ──` header line, log via `tools::log_tool_run`. Print any model `text` blocks as a dim preamble. NOTHING is sent back to the API.
  - **Narrate mode (`--narrate`):** loop ≤4 rounds feeding `tool_result` blocks back (JSON results with resident payees already replaced by `"(resident)"` — write a `redact_residents(&mut Value)` that walks the JSON and replaces `payee` fields when `is_resident == 1`); stop on `end_turn`; final text printed as the answer. First line of stderr output: `narrate mode: query results are sent to the model provider`.
  - `stop_reason == "refusal"` → print the refusal cleanly, complete task_run as `failed`. `max_tokens` → tell the user to retry simpler.
  - Complete task_run with summary `"{n} tool calls · {in} in / {out} out tokens · model {model}"`.
- [ ] **B5: CLI** — `Command::Ask { question: String, #[arg(long)] narrate: bool }`; constructs `AnthropicProvider::from_env()` and runs. Errors (no key, no network) must be one clear line, not a panic.
- [ ] **B6 tests (`tests/ask_engine.rs`, no network):** a `MockProvider` implementing `ToolUseProvider` returning scripted `CompletionResponse`s. Cover: (1) default mode executes a scripted vendor_spend tool_use against a seeded in-memory db, renders the payee table, logs one tool_run row, and makes exactly ONE provider call; (2) narrate mode feeds redacted results back (assert the second request's messages contain `"(resident)"` and not the seeded resident name) and stops on end_turn; (3) refusal handling; (4) unknown tool name from model → graceful error result, not a panic. Plus unit tests for `redact_residents` and the schema of every ToolSpec (parses as JSON, has additionalProperties false).
- [ ] **B7:** fmt/test/clippy → commit `feat(boxscore): boxscore ask natural-language query layer`.

## Phase C — TUI ask palette

**Files:** Modify `boxscore/src/tui/{app,mod,ui}.rs`; test in `boxscore/tests/desk_tui.rs`.

- [ ] **C1:** `:` from any screen opens an ask input line (reuse the ledger `/` capture pattern — a global `ask_input: Option<String>` captured before all other keys; picker and ledger-filter captures take precedence). Enter runs `run_ask` in default mode (no narration from the TUI) with the results captured into `ask_output: Vec<String>` (the rendered tables) shown on a new `Screen::Ask` results view (scrollable with j/k, Esc/q returns to the previous screen — track `previous_screen`). While the request runs the UI blocks; draw a `asking the model…` footer line first by rendering one frame before awaiting.
- [ ] **C2:** If `AnthropicProvider::from_env()` fails, the footer shows `ask requires ANTHROPIC_API_KEY` instead of crashing.
- [ ] **C3:** render tests: `:` input line renders typed text; Ask screen renders fixture output lines; footer hint includes `: ask`. fmt/test/clippy → commit `feat(boxscore): ask palette in the desk TUI`.

## Final
- [ ] code-reviewer agent over the full diff; fix must-fix findings; live smoke `boxscore ask "list properties"` (real API, requires key present — skip gracefully if unset and note it); update SESSION_HANDOFF.md; push.

## Self-review notes
- Privacy contract is the load-bearing design: default mode = one-call/no-results-out is what makes "your GL never leaves the machine" true; narrate is explicit opt-in with redaction. Both are tested without network via MockProvider.
- Mutations stay keyboard-only: every tool in the registry is read-only by construction.
- Type names (`ToolSpec`, `ChatMessage`, `CompletionResponse`, `VendorSpend`) are defined once in Phase A/B1 and referenced identically afterward.
