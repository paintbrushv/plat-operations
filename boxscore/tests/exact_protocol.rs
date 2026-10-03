use serde_json::{json, Value};
use std::{
    io::Write,
    process::{Command, Stdio},
};

fn call(value: &Value) -> (bool, Value) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_boxscore-exact"))
        .arg("protocol")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(value).unwrap().as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(result.stderr.is_empty());
    (
        result.status.success(),
        serde_json::from_slice(&result.stdout).unwrap(),
    )
}
fn request() -> Value {
    json!({"contract_version":"plat.ops/1","operation":"variance","currency":"USD","expense_convention":"positive_costs",
        "actuals":[{"account_code":"4000","account_name":"Rent","category":"rental income","amount":"0.30"}],
        "budgets":[{"account_code":"4000","account_name":"Rent","category":"rental income","amount":"0.10"}]})
}
#[test]
fn real_binary_returns_exact_money_and_typed_refusals() {
    let (success, result) = call(&request());
    assert!(success);
    assert_eq!(result["result"]["noi_bridge"]["noi_variance"], "0.20");
    assert_eq!(result["producer"]["arithmetic"], "checked_i64_cents");
    for mutation in [json!("0.001"), json!(0.30), json!("NaN")] {
        let mut input = request();
        input["actuals"][0]["amount"] = mutation;
        let (success, result) = call(&input);
        assert!(!success);
        assert_eq!(result["status"], "refused");
    }
    let mut input = request();
    input["database"] = json!("/private/must-not-open.sqlite");
    assert!(!call(&input).0);
    input = request();
    input["contract_version"] = json!("future");
    assert_eq!(call(&input).1["error"]["code"], "CONTRACT_MISMATCH");
}
