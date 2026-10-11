//! One bounded JSON request on stdin; one JSON response on stdout. No paths.
use super::{
    catalog, error,
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

/// Stateless entry point.
///
/// `operation: "variance"` keeps its original `plat.ops/1` response shape
/// byte for byte. Any other operation names a catalog function; the remaining
/// fields are its input and the response is a provenance envelope whose
/// `input_sha256` covers the request bytes exactly as received. Stateful
/// functions refuse here with `DATABASE_REQUIRED`: the protocol never opens a
/// database or any local path.
pub async fn calculate(bytes: &[u8]) -> Result<Value> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(error("INPUT_LIMIT", "Request exceeds 2 MiB"));
    }
    let mut raw: Value = serde_json::from_slice(bytes)?;
    if raw.get("contract_version").and_then(Value::as_str) != Some(CONTRACT) {
        return Err(error("CONTRACT_MISMATCH", "Unsupported operating protocol"));
    }
    let operation = raw
        .get("operation")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| error("INVALID_INPUT", "Request needs an operation"))?;
    if operation == "variance" {
        let r: Request = serde_json::from_slice(bytes)?;
        debug_assert_eq!(
            (r.contract_version.as_str(), r.operation.as_str()),
            (CONTRACT, "variance")
        );
        if r.currency != "USD" || r.expense_convention != "positive_costs" {
            return Err(error(
                "INVALID_INPUT",
                "Require variance, USD, and positive_costs",
            ));
        }
        let result = variance::compute(&r.actuals, &r.budgets)?;
        return Ok(
            json!({"contract_version":CONTRACT,"status":if result.review_reasons.is_empty() {"calculated"} else {"review_required"},
            "producer":{"name":"boxscore-exact","version":env!("CARGO_PKG_VERSION"),"arithmetic":"checked_i64_cents"},
            "input_sha256":super::digest(bytes),"result":result}),
        );
    }
    let object = raw
        .as_object_mut()
        .ok_or_else(|| error("INVALID_INPUT", "Request must be a JSON object"))?;
    object.remove("contract_version");
    object.remove("operation");
    let version = match object.remove("version") {
        None => None,
        Some(Value::String(version)) => Some(version),
        Some(_) => return Err(error("INVALID_INPUT", "version must be a string")),
    };
    let call = catalog::Call {
        function: operation,
        version,
        input: raw,
        database: None,
    };
    Ok(catalog::invoke_hashed(&call, super::digest(bytes))
        .await
        .to_value())
}
pub fn refusal(e: &super::ExactError) -> Value {
    json!({"contract_version":CONTRACT,"status":"refused","error":{"code":e.code,"message":e.message}})
}
