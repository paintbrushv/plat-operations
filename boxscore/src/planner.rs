use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub name: String,
    pub tool_name: String,
    pub purpose: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPlan {
    pub task_type: String,
    pub steps: Vec<PlanStep>,
}

pub struct TaskPlanner;

impl TaskPlanner {
    pub fn ops_variance_plan() -> TaskPlan {
        TaskPlan {
            task_type: "ops_variance_analysis".to_string(),
            steps: vec![
                step(
                    "load_actuals",
                    "LoadActualsTool",
                    "Load actual GL data for the selected property and period.",
                ),
                step(
                    "load_budgets",
                    "LoadBudgetsTool",
                    "Load budget GL data for the selected property and period.",
                ),
                step(
                    "load_operating_snapshots",
                    "LoadRentRollTool",
                    "Load rent roll, delinquency, and leasing snapshots where available.",
                ),
                step(
                    "compute_variance",
                    "ComputeVarianceTool",
                    "Compute revenue, expense, NOI, and account variance.",
                ),
                step(
                    "detect_gaps",
                    "DetectGapsTool",
                    "Identify missing data and low-confidence areas.",
                ),
                step(
                    "generate_questions",
                    "GenerateQuestionsTool",
                    "Ask the operator for the most useful missing context.",
                ),
                step(
                    "update_capabilities",
                    "UpdateCapabilityBacklogTool",
                    "Create proposal-first product improvements from repeated gaps.",
                ),
                step(
                    "write_report",
                    "WriteMarkdownReportTool",
                    "Write a local evidence-aware markdown report.",
                ),
            ],
        }
    }
}

fn step(name: &str, tool_name: &str, purpose: &str) -> PlanStep {
    PlanStep {
        name: name.to_string(),
        tool_name: tool_name.to_string(),
        purpose: purpose.to_string(),
    }
}
