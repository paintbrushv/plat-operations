use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ── Existing summarize trait (keep untouched) ─────────────────────────────────

#[allow(clippy::double_must_use)] // async_trait adds must_use to an already must_use Future.
#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn summarize(&self, prompt: &str) -> Result<String>;
}

#[derive(Debug, Default)]
pub struct StubModelProvider;

#[async_trait]
impl ModelProvider for StubModelProvider {
    fn name(&self) -> &'static str {
        "stub-rule-based"
    }

    async fn summarize(&self, prompt: &str) -> Result<String> {
        Ok(format!(
            "Deterministic local summary generated from {} prompt characters. No external model provider was called.",
            prompt.len()
        ))
    }
}

#[derive(Debug, Clone)]
pub enum FutureProviderKind {
    OpenAiCompatible,
    Anthropic,
    LocalModel,
}

// ── Tool-use completion surface ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Usage {
    pub input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CompletionResponse {
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<String>,
    pub model: String,
    pub usage: Usage,
}

/// Conversation message for multi-turn (narrate) requests.
#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: serde_json::Value,
}

#[allow(clippy::double_must_use)] // async_trait adds must_use to an already must_use Future.
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

// ── AnthropicProvider ─────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct AnthropicProvider {
    api_key: String,
    model: String,
    http: reqwest::Client,
}

impl AnthropicProvider {
    /// Errors with a setup hint when ANTHROPIC_API_KEY is unset.
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
            anyhow!(
                "ANTHROPIC_API_KEY is not set. Export it before running `boxscore ask`:\n  export ANTHROPIC_API_KEY=sk-ant-..."
            )
        })?;
        let model =
            std::env::var("BOXSCORE_MODEL").unwrap_or_else(|_| "claude-opus-4-8".to_string());
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?;
        Ok(Self {
            api_key,
            model,
            http,
        })
    }
}

#[async_trait]
impl ToolUseProvider for AnthropicProvider {
    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(
        &self,
        system: &str,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<CompletionResponse> {
        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": 16000,
            "system": system,
            "messages": messages,
        });
        if !tools.is_empty() {
            body["tools"] = serde_json::to_value(tools)?;
        }

        let resp = self
            .http
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            // Read bytes first: gateways can return non-JSON bodies (HTML,
            // plain text) and the raw text is the only diagnostic available.
            let raw = resp.bytes().await.unwrap_or_default();
            let err_body: serde_json::Value = serde_json::from_slice(&raw).unwrap_or_default();
            let msg = err_body
                .pointer("/error/message")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    let text = String::from_utf8_lossy(&raw);
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        "unknown API error".to_string()
                    } else {
                        trimmed.chars().take(300).collect()
                    }
                });
            return Err(anyhow!("Anthropic API error {status}: {msg}"));
        }

        resp.json::<CompletionResponse>().await.map_err(Into::into)
    }
}

// ── ClaudeCodeProvider ────────────────────────────────────────────────────────
//
// Runs asks through the locally installed Claude Code CLI (`claude -p`),
// which authenticates with the operator's Claude subscription — no API key
// required. The subprocess receives the same schema-catalog-plus-question
// prompt the API provider would; tool execution stays local either way.

#[derive(Debug)]
pub struct ClaudeCodeProvider {
    binary: String,
    model: Option<String>,
}

impl ClaudeCodeProvider {
    pub fn from_env() -> Result<Self> {
        let binary = std::env::var("BOXSCORE_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string());
        if !binary_on_path(&binary) {
            return Err(anyhow!(
                "Claude Code CLI not found on PATH. Install it (https://claude.com/claude-code) or set ANTHROPIC_API_KEY to use the API instead."
            ));
        }
        Ok(Self {
            binary,
            model: std::env::var("BOXSCORE_MODEL").ok(),
        })
    }

    /// Construct against an explicit binary (used by tests with a fake CLI).
    pub fn with_binary(binary: String, model: Option<String>) -> Self {
        Self { binary, model }
    }

    fn build_prompt(system: &str, messages: &[ChatMessage], tools: &[ToolSpec]) -> String {
        let tools_json = serde_json::to_string_pretty(tools).unwrap_or_else(|_| "[]".to_string());
        let mut transcript = String::new();
        for msg in messages {
            let rendered = match &msg.content {
                serde_json::Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            transcript.push_str(&format!("[{}]\n{}\n\n", msg.role, rendered));
        }
        format!(
            "<system>\n{system}\n</system>\n\n\
             You have access to the following read-only tools, described as JSON Schema:\n\
             {tools_json}\n\n\
             Conversation so far:\n{transcript}\
             Respond with ONLY a single JSON object (no markdown fences, no prose outside it) shaped exactly:\n\
             {{\"text\": \"<optional short commentary>\", \"tool_calls\": [{{\"name\": \"<tool name>\", \"input\": {{...}}}}]}}\n\
             Use an empty tool_calls array when no tool is needed and put your answer in \"text\". \
             Never invent data — data questions must be answered through tool calls."
        )
    }
}

/// The JSON envelope `claude -p --output-format json` prints.
#[derive(Debug, Deserialize)]
struct ClaudeCodeEnvelope {
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    result: String,
    #[serde(default)]
    usage: Option<ClaudeCodeUsage>,
}

#[derive(Debug, Deserialize)]
struct ClaudeCodeUsage {
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
}

/// The payload boxscore instructs the model to emit inside `result`.
#[derive(Debug, Deserialize)]
struct AskPayload {
    #[serde(default)]
    text: String,
    #[serde(default)]
    tool_calls: Vec<AskToolCall>,
}

#[derive(Debug, Deserialize)]
struct AskToolCall {
    name: String,
    #[serde(default)]
    input: serde_json::Value,
}

/// Pull the first JSON object out of model text that may carry fences/prose.
fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end >= start).then(|| &text[start..=end])
}

/// Map a Claude Code envelope into the provider-neutral CompletionResponse.
fn envelope_to_completion(envelope: &str, model_label: &str) -> Result<CompletionResponse> {
    let envelope: ClaudeCodeEnvelope = serde_json::from_str(envelope)
        .map_err(|e| anyhow!("could not parse claude -p output envelope: {e}"))?;
    if envelope.is_error {
        return Err(anyhow!("claude -p reported an error: {}", envelope.result));
    }
    let payload_json = extract_json_object(&envelope.result).ok_or_else(|| {
        anyhow!(
            "claude -p result carried no JSON object: {}",
            envelope.result
        )
    })?;
    let payload: AskPayload = serde_json::from_str(payload_json)
        .map_err(|e| anyhow!("could not parse ask payload from claude -p result: {e}"))?;

    let mut content = Vec::new();
    if !payload.text.trim().is_empty() {
        content.push(ContentBlock::Text {
            text: payload.text.clone(),
        });
    }
    for (index, call) in payload.tool_calls.iter().enumerate() {
        content.push(ContentBlock::ToolUse {
            id: format!("local-{index}"),
            name: call.name.clone(),
            input: call.input.clone(),
        });
    }
    let stop_reason = if payload.tool_calls.is_empty() {
        "end_turn"
    } else {
        "tool_use"
    };
    let usage = envelope.usage.unwrap_or(ClaudeCodeUsage {
        input_tokens: 0,
        output_tokens: 0,
    });
    Ok(CompletionResponse {
        content,
        stop_reason: Some(stop_reason.to_string()),
        model: model_label.to_string(),
        usage: Usage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
        },
    })
}

#[async_trait]
impl ToolUseProvider for ClaudeCodeProvider {
    fn model(&self) -> &str {
        self.model
            .as_deref()
            .unwrap_or("claude-code (subscription)")
    }

    async fn complete(
        &self,
        system: &str,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<CompletionResponse> {
        use tokio::io::AsyncWriteExt;

        let prompt = Self::build_prompt(system, messages, tools);
        let mut cmd = tokio::process::Command::new(&self.binary);
        cmd.arg("-p").arg("--output-format").arg("json");
        if let Some(model) = &self.model {
            cmd.arg("--model").arg(model);
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("failed to launch `{}`: {e}", self.binary))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(prompt.as_bytes()).await?;
            drop(stdin); // close stdin so claude -p starts processing
        }

        let output =
            match tokio::time::timeout(Duration::from_secs(180), child.wait_with_output()).await {
                Ok(result) => result?,
                Err(_) => {
                    return Err(anyhow!(
                        "claude -p timed out after 180s; try again or use ANTHROPIC_API_KEY"
                    ))
                }
            };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!(
                "claude -p exited with {}: {}",
                output.status,
                stderr.trim().chars().take(300).collect::<String>()
            ));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        envelope_to_completion(stdout.trim(), self.model())
    }
}

// ── Provider selection ────────────────────────────────────────────────────────

/// Pick the ask provider. Precedence:
/// 1. `BOXSCORE_PROVIDER=api` or `=claude-code` forces one explicitly.
/// 2. `ANTHROPIC_API_KEY` set → API provider.
/// 3. Claude Code CLI on PATH → subscription-backed subprocess provider.
pub fn provider_from_env() -> Result<Box<dyn ToolUseProvider>> {
    match std::env::var("BOXSCORE_PROVIDER").ok().as_deref() {
        Some("api") => return Ok(Box::new(AnthropicProvider::from_env()?)),
        Some("claude-code") => return Ok(Box::new(ClaudeCodeProvider::from_env()?)),
        Some(other) => {
            return Err(anyhow!(
                "unknown BOXSCORE_PROVIDER `{other}` (expected `api` or `claude-code`)"
            ))
        }
        None => {}
    }
    if std::env::var("ANTHROPIC_API_KEY").is_ok() {
        return Ok(Box::new(AnthropicProvider::from_env()?));
    }
    if let Ok(provider) = ClaudeCodeProvider::from_env() {
        return Ok(Box::new(provider));
    }
    Err(anyhow!(
        "no ask provider available. Either export ANTHROPIC_API_KEY=sk-ant-... \
         or install Claude Code (`claude` on PATH) to use your subscription."
    ))
}

fn binary_on_path(binary: &str) -> bool {
    if binary.contains('/') {
        return std::path::Path::new(binary).is_file();
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(binary).is_file())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
        "id": "msg_01XFDUDYJgAACzvnptvVoYEL",
        "type": "message",
        "role": "assistant",
        "model": "claude-opus-4-8",
        "content": [
            {"type": "text", "text": "Here is the vendor data."},
            {"type": "tool_use", "id": "toolu_abc", "name": "vendor_spend", "input": {"payee_contains": "Kings"}},
            {"type": "unknown_future_block", "data": 42}
        ],
        "stop_reason": "tool_use",
        "stop_sequence": null,
        "usage": {"input_tokens": 100, "output_tokens": 50}
    }"#;

    #[test]
    fn deserialize_fixture_response() {
        let resp: CompletionResponse =
            serde_json::from_str(FIXTURE).expect("should deserialize without error");
        assert_eq!(resp.model, "claude-opus-4-8");
        assert_eq!(resp.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(resp.usage.input_tokens, 100);
        assert_eq!(resp.usage.output_tokens, 50);
        assert_eq!(resp.content.len(), 3);

        // Text block
        assert!(
            matches!(&resp.content[0], ContentBlock::Text { text } if text == "Here is the vendor data.")
        );

        // ToolUse block
        match &resp.content[1] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "toolu_abc");
                assert_eq!(name, "vendor_spend");
                assert_eq!(input["payee_contains"], "Kings");
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }

        // Unknown block falls through to Other variant
        assert!(matches!(&resp.content[2], ContentBlock::Other));
    }

    #[test]
    fn from_env_error_mentions_anthropic_api_key() {
        // Temporarily unset to verify the error message.
        let saved = std::env::var("ANTHROPIC_API_KEY").ok();
        std::env::remove_var("ANTHROPIC_API_KEY");

        let err = AnthropicProvider::from_env().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("ANTHROPIC_API_KEY"),
            "error should mention ANTHROPIC_API_KEY; got: {msg}"
        );

        // Restore if it was set.
        if let Some(v) = saved {
            std::env::set_var("ANTHROPIC_API_KEY", v);
        }
    }

    #[test]
    fn request_body_omits_forbidden_fields() {
        // Build a body the way complete() would.
        let body = serde_json::json!({
            "model": "claude-opus-4-8",
            "max_tokens": 16000,
            "system": "test",
            "messages": [],
        });
        let serialized = body.to_string();
        assert!(
            !serialized.contains("\"temperature\""),
            "temperature must not appear in the request body"
        );
        assert!(
            !serialized.contains("\"thinking\""),
            "thinking must not appear in the request body"
        );
        assert!(
            !serialized.contains("\"top_p\""),
            "top_p must not appear in the request body"
        );
        assert!(
            !serialized.contains("\"top_k\""),
            "top_k must not appear in the request body"
        );
    }

    #[test]
    fn extracts_json_object_from_fenced_or_prosey_text() {
        assert_eq!(
            extract_json_object("```json\n{\"a\": 1}\n```"),
            Some("{\"a\": 1}")
        );
        assert_eq!(
            extract_json_object("Here you go: {\"text\": \"hi\", \"tool_calls\": []} done"),
            Some("{\"text\": \"hi\", \"tool_calls\": []}")
        );
        assert_eq!(extract_json_object("no json here"), None);
    }

    #[test]
    fn maps_claude_code_envelope_to_completion() {
        let envelope = r#"{
            "type": "result", "subtype": "success", "is_error": false,
            "result": "{\"text\": \"checking vendors\", \"tool_calls\": [{\"name\": \"vendor_spend\", \"input\": {\"payee_contains\": \"7 Kings\"}}]}",
            "session_id": "abc", "usage": {"input_tokens": 12, "output_tokens": 34}
        }"#;
        let completion = envelope_to_completion(envelope, "claude-code (subscription)").unwrap();
        assert_eq!(completion.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(completion.usage.input_tokens, 12);
        let tool = completion
            .content
            .iter()
            .find_map(|b| match b {
                ContentBlock::ToolUse { name, input, .. } => Some((name.clone(), input.clone())),
                _ => None,
            })
            .expect("tool_use block present");
        assert_eq!(tool.0, "vendor_spend");
        assert_eq!(tool.1["payee_contains"], "7 Kings");
    }

    #[test]
    fn claude_code_envelope_error_is_surfaced() {
        let envelope = r#"{"type":"result","is_error":true,"result":"usage limit reached"}"#;
        let err = envelope_to_completion(envelope, "x").unwrap_err();
        assert!(err.to_string().contains("usage limit reached"));
    }

    #[test]
    fn text_only_payload_maps_to_end_turn() {
        let envelope =
            r#"{"is_error":false,"result":"{\"text\": \"three properties\", \"tool_calls\": []}"}"#;
        let completion = envelope_to_completion(envelope, "x").unwrap();
        assert_eq!(completion.stop_reason.as_deref(), Some("end_turn"));
        assert!(
            matches!(&completion.content[0], ContentBlock::Text { text } if text == "three properties")
        );
    }

    #[tokio::test]
    async fn claude_code_provider_round_trips_through_a_fake_cli() {
        // A fake `claude` that ignores stdin and prints a canned envelope.
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("claude");
        std::fs::write(
            &script,
            "#!/bin/sh\ncat > /dev/null\nprintf '%s' '{\"is_error\":false,\"result\":\"{\\\"text\\\": \\\"ok\\\", \\\"tool_calls\\\": []}\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}'\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let provider = ClaudeCodeProvider::with_binary(script.to_string_lossy().to_string(), None);
        let completion = provider.complete("system", &[], &[]).await.unwrap();
        assert_eq!(completion.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(completion.usage.output_tokens, 2);
    }
}
