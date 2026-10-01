use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqlitePool;

use crate::db;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PermissionLevel {
    ReadOnly,
    WriteLocal,
    ProposeOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DataAccessLevel {
    PublicSample,
    PropertyFinancials,
    PrivateTenantMinimized,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub permission_level: PermissionLevel,
    pub data_access_level: DataAccessLevel,
}

#[allow(clippy::double_must_use)] // async_trait adds must_use to an already must_use Future.
#[async_trait]
pub trait Tool {
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, input: Value) -> Result<Value>;
}

pub fn registry() -> Vec<ToolDefinition> {
    vec![
        tool(
            "LoadActualsTool",
            "Load actual GL lines for a property period.",
            PermissionLevel::ReadOnly,
        ),
        tool(
            "LoadBudgetsTool",
            "Load budget GL lines for a property period.",
            PermissionLevel::ReadOnly,
        ),
        tool(
            "LoadRentRollTool",
            "Load rent roll operating snapshot.",
            PermissionLevel::ReadOnly,
        ),
        tool(
            "LoadDelinquencyTool",
            "Load delinquency operating snapshot.",
            PermissionLevel::ReadOnly,
        ),
        tool(
            "LoadLeasingTool",
            "Load leasing funnel operating snapshot.",
            PermissionLevel::ReadOnly,
        ),
        tool(
            "LoadCollectionsTool",
            "Load collections and bad debt context snapshot.",
            PermissionLevel::ReadOnly,
        ),
        tool(
            "ComputeVarianceTool",
            "Compute account, revenue, expense, and NOI variance.",
            PermissionLevel::ReadOnly,
        ),
        tool(
            "DetectGapsTool",
            "Detect missing data and low-confidence areas.",
            PermissionLevel::WriteLocal,
        ),
        tool(
            "GenerateQuestionsTool",
            "Turn gaps into prioritized operator questions.",
            PermissionLevel::WriteLocal,
        ),
        tool(
            "UpdateCapabilityBacklogTool",
            "Propose auditable future product capabilities.",
            PermissionLevel::ProposeOnly,
        ),
        tool(
            "WriteMarkdownReportTool",
            "Write a local markdown variance report.",
            PermissionLevel::WriteLocal,
        ),
    ]
}

pub async fn log_tool_run(
    pool: &SqlitePool,
    task_run_id: &str,
    tool_name: &str,
    input: Value,
    output: Option<Value>,
    error: Option<&str>,
) -> Result<()> {
    let success = error.is_none();
    sqlx::query(
        "INSERT INTO tool_runs (id, task_run_id, tool_name, input_json, output_json, success, error_message, started_at, completed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(db::new_id())
    .bind(task_run_id)
    .bind(tool_name)
    .bind(input.to_string())
    .bind(output.map(|value| value.to_string()))
    .bind(if success { 1 } else { 0 })
    .bind(error)
    .bind(db::now_iso())
    .bind(db::now_iso())
    .execute(pool)
    .await?;
    Ok(())
}

fn tool(
    name: &'static str,
    description: &'static str,
    permission_level: PermissionLevel,
) -> ToolDefinition {
    ToolDefinition {
        name,
        description,
        permission_level,
        data_access_level: DataAccessLevel::PropertyFinancials,
    }
}
