//! One bounded JSON request on stdin; one JSON response on stdout. No paths.
use super::{
    error,
    variance::{self, Line},
    Result, CONTRACT,
};
use serde::Deserialize;
use serde_json::{json, Value};

pub const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    contract_version: String,
    operation: String,
    currency: String,
    expense_convention: String,
    actuals: Vec<Line>,
    budgets: Vec<Line>,
}

pub fn calculate(bytes: &[u8]) -> Result<Value> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(error("INPUT_LIMIT", "Request exceeds 2 MiB"));
    }
    let r: Request = serde_json::from_slice(bytes)?;
    if r.contract_version != CONTRACT {
        return Err(error("CONTRACT_MISMATCH", "Unsupported operating protocol"));
    }
    if r.operation != "variance" || r.currency != "USD" || r.expense_convention != "positive_costs"
    {
        return Err(error(
            "INVALID_INPUT",
            "Require variance, USD, and positive_costs",
        ));
    }
    let result = variance::compute(&r.actuals, &r.budgets)?;
    Ok(
        json!({"contract_version":CONTRACT,"status":if result.review_reasons.is_empty() {"calculated"} else {"review_required"},
        "producer":{"name":"boxscore-exact","version":env!("CARGO_PKG_VERSION"),"arithmetic":"checked_i64_cents"},
        "input_sha256":super::digest(bytes),"result":result}),
    )
}
pub fn refusal(e: &super::ExactError) -> Value {
    json!({"contract_version":CONTRACT,"status":"refused","error":{"code":e.code,"message":e.message}})
}
