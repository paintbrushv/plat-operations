//! Source adapters: the canonical CSV and JSON profiles reproduce the legacy
//! `import-csv` and `import` subcommands byte for byte, every revision records
//! the profile that produced it, and a vendor adapter plugs in through the
//! same trait. Synthetic data only.
use boxscore::exact::{
    catalog::{self, Call},
    digest,
    ingest::{
        self, AdapterInputs, AdapterSpec, FileSpec, OptionSpec, SourceAdapter, CANONICAL_CSV,
        CANONICAL_JSON,
    },
    store::{self, Dataset, Snapshot},
    variance::Line,
    Result,
};
use serde_json::{json, Value};
use std::{path::PathBuf, process::Command};

const ACTUALS: &str = "account_code,account_name,category,amount\n4000,Rent,rental income,100.10\n6000,Repairs,repairs,12.34\n";
const BUDGETS: &str = "account_code,account_name,category,amount\n4000,Rent,rental income,90.05\n6000,Repairs,repairs,10.00\n";
const SNAPSHOT: &str = r#"{"as_of_date":"2026-05-31","occupied_units":9,"vacant_units":1,"down_units":0,"market_rent_total":"110.00","in_place_rent_total":"100.10","delinquent_amount":null,"prepaid_amount":null,"concessions_amount":null}"#;

fn line(code: &str, name: &str, category: &str, amount: &str) -> Line {
    Line {
        account_code: code.into(),
        account_name: name.into(),
        category: category.into(),
        amount: amount.parse().unwrap(),
    }
}

/// The dataset the fixtures above describe, built by hand so the expected
/// `input_sha256` does not depend on any adapter code.
fn expected_dataset() -> Dataset {
    Dataset {
        property: "Synthetic Oak".into(),
        period: "2026-05".into(),
        currency: "USD".into(),
        expense_convention: "positive_costs".into(),
        unit_count: 10,
        actuals: vec![
            line("4000", "Rent", "rental income", "100.10"),
            line("6000", "Repairs", "repairs", "12.34"),
        ],
        budgets: vec![
            line("4000", "Rent", "rental income", "90.05"),
            line("6000", "Repairs", "repairs", "10.00"),
        ],
        snapshot: Some(Snapshot {
            as_of_date: "2026-05-31".into(),
            occupied_units: 9,
            vacant_units: 1,
            down_units: 0,
            market_rent_total: Some("110.00".parse().unwrap()),
            in_place_rent_total: Some("100.10".parse().unwrap()),
            delinquent_amount: None,
            prepaid_amount: None,
            concessions_amount: None,
        }),
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::write(root.join("actuals.csv"), ACTUALS).unwrap();
        std::fs::write(root.join("budgets.csv"), BUDGETS).unwrap();
        std::fs::write(root.join("snapshot.json"), SNAPSHOT).unwrap();
        std::fs::write(
            root.join("period.json"),
            serde_json::to_vec(&expected_dataset()).unwrap(),
        )
        .unwrap();
        Fixture { _dir: dir, root }
    }
    fn path(&self, name: &str) -> String {
        self.root.join(name).to_str().unwrap().to_string()
    }
    /// A fresh database created through the real binary.
    fn database(&self, name: &str) -> String {
        let db = self.path(name);
        let (code, created) = run(&["init", "--database", &db]);
        assert_eq!((code, created["status"].as_str()), (0, Some("created")));
        db
    }
    fn csv_inputs(&self) -> AdapterInputs {
        AdapterInputs::default()
            .with_file("actuals", self.path("actuals.csv"))
            .with_file("budgets", self.path("budgets.csv"))
            .with_file("snapshot", self.path("snapshot.json"))
            .with_option("property", "Synthetic Oak")
            .with_option("period", "2026-05")
            .with_option("units", "10")
    }
}

fn run(args: &[&str]) -> (i32, Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_boxscore-exact"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        output.status.code().unwrap(),
        serde_json::from_slice(&output.stdout).unwrap(),
    )
}

fn imported(result: (i32, Value)) -> String {
    let (code, body) = result;
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["status"], "imported");
    body["revision_id"].as_str().unwrap().to_string()
}

fn review(db: &str, revision: &str) -> Value {
    let (code, body) = run(&["review", "--database", db, "--revision", revision]);
    assert_eq!(code, 0, "{body}");
    body
}

/// `input_sha256` the pre-adapter binary (d49f111) stored for these fixtures
/// through `import-csv` (with snapshot) and through `import` of `period.json`.
/// Pinned so a change to `Dataset` serialization cannot pass unnoticed.
const PRE_ADAPTER_INPUT_SHA256: &str =
    "51c39007acbc598bc81e2a11ded6e7e8aa6d1d3854bd418c4b361ec7dff673b5";

fn expected_hash() -> String {
    let hash = digest(&serde_json::to_vec(&expected_dataset()).unwrap());
    assert_eq!(hash, PRE_ADAPTER_INPUT_SHA256);
    hash
}

// (a) canonical.csv/1 versus the legacy import-csv subcommand.
#[test]
fn canonical_csv_profile_matches_legacy_import_csv_byte_for_byte() {
    let f = Fixture::new();
    let legacy_db = f.database("legacy.sqlite");
    let legacy = imported(run(&[
        "import-csv",
        "--database",
        &legacy_db,
        "--actuals",
        &f.path("actuals.csv"),
        "--budgets",
        &f.path("budgets.csv"),
        "--snapshot",
        &f.path("snapshot.json"),
        "--property",
        "Synthetic Oak",
        "--period",
        "2026-05",
        "--units",
        "10",
    ]));
    let adapter_db = f.database("adapter.sqlite");
    let through = imported(run(&[
        "import-with",
        "--database",
        &adapter_db,
        "--profile",
        "canonical.csv/1",
        "--file",
        &format!("actuals={}", f.path("actuals.csv")),
        "--file",
        &format!("budgets={}", f.path("budgets.csv")),
        "--file",
        &format!("snapshot={}", f.path("snapshot.json")),
        "--option",
        "property=Synthetic Oak",
        "--option",
        "period=2026-05",
        "--option",
        "units=10",
    ]));
    let a = review(&legacy_db, &legacy);
    let b = review(&adapter_db, &through);
    assert_eq!(a["input_sha256"], expected_hash());
    assert_eq!(a["input_sha256"], b["input_sha256"]);
    let expected_sources = json!({
        "profile": "canonical.csv/1",
        "actuals_sha256": digest(ACTUALS.as_bytes()),
        "budgets_sha256": digest(BUDGETS.as_bytes()),
        "snapshot_sha256": digest(SNAPSHOT.as_bytes()),
    });
    assert_eq!(a["sources"], expected_sources);
    assert_eq!(b["sources"], expected_sources);
    assert_eq!(a["variance"], b["variance"]);

    // Same content through either front end into one database is idempotent.
    let again = imported(run(&[
        "import-with",
        "--database",
        &legacy_db,
        "--profile",
        "canonical.csv/1",
        "--file",
        &format!("actuals={}", f.path("actuals.csv")),
        "--file",
        &format!("budgets={}", f.path("budgets.csv")),
        "--file",
        &format!("snapshot={}", f.path("snapshot.json")),
        "--option",
        "property=Synthetic Oak",
        "--option",
        "period=2026-05",
        "--option",
        "units=10",
    ]));
    assert_eq!(again, legacy);

    // Without the optional snapshot, snapshot_sha256 stays null as before.
    let no_snapshot_db = f.database("nosnap.sqlite");
    let bare = imported(run(&[
        "import-csv",
        "--database",
        &no_snapshot_db,
        "--actuals",
        &f.path("actuals.csv"),
        "--budgets",
        &f.path("budgets.csv"),
        "--property",
        "Synthetic Oak",
        "--period",
        "2026-05",
        "--units",
        "10",
    ]));
    let bare = review(&no_snapshot_db, &bare);
    assert_eq!(bare["sources"]["snapshot_sha256"], Value::Null);
    assert_eq!(bare["sources"]["profile"], "canonical.csv/1");
}

// (b) canonical.json/1 versus the legacy import subcommand.
#[test]
fn canonical_json_profile_matches_legacy_import() {
    let f = Fixture::new();
    let legacy_db = f.database("legacy.sqlite");
    let legacy = imported(run(&[
        "import",
        "--database",
        &legacy_db,
        "--input",
        &f.path("period.json"),
    ]));
    let adapter_db = f.database("adapter.sqlite");
    let through = imported(run(&[
        "import-with",
        "--database",
        &adapter_db,
        "--profile",
        "canonical.json/1",
        "--file",
        &format!("input={}", f.path("period.json")),
    ]));
    let a = review(&legacy_db, &legacy);
    let b = review(&adapter_db, &through);
    assert_eq!(a["input_sha256"], expected_hash());
    assert_eq!(a["input_sha256"], b["input_sha256"]);
    let bytes = std::fs::read(f.path("period.json")).unwrap();
    let expected_sources = json!({
        "profile": "canonical.json/1",
        "canonical_json_sha256": digest(&bytes),
    });
    assert_eq!(a["sources"], expected_sources);
    assert_eq!(b["sources"], expected_sources);

    // Corrections still go through supersedes + reason.
    let mut corrected = expected_dataset();
    corrected.actuals[0].amount = "100.12".parse().unwrap();
    std::fs::write(
        f.path("corrected.json"),
        serde_json::to_vec(&corrected).unwrap(),
    )
    .unwrap();
    let (code, stale) = run(&[
        "import-with",
        "--database",
        &adapter_db,
        "--profile",
        "canonical.json/1",
        "--file",
        &format!("input={}", f.path("corrected.json")),
    ]);
    assert_eq!(
        (code, stale["error"]["code"].as_str()),
        (2, Some("STALE_REVISION"))
    );
    let second = imported(run(&[
        "import-with",
        "--database",
        &adapter_db,
        "--profile",
        "canonical.json/1",
        "--file",
        &format!("input={}", f.path("corrected.json")),
        "--supersedes",
        &through,
        "--reason",
        "Synthetic two-cent correction",
    ]));
    let reviewed = review(&adapter_db, &second);
    assert_eq!(reviewed["changes"]["actual_noi"], "0.02");
    assert_eq!(reviewed["sources"]["profile"], "canonical.json/1");
}

/// A vendor adapter that exists only in this test: a pipe-delimited ledger
/// with a side column, so one file carries both actuals and budgets.
struct AcmeGl;

impl SourceAdapter for AcmeGl {
    fn profile(&self) -> &'static str {
        "acme.gl/1"
    }
    fn spec(&self) -> AdapterSpec {
        AdapterSpec {
            profile: "acme.gl/1",
            summary: "Synthetic Acme ledger: side|account|name|category|amount",
            files: vec![FileSpec {
                name: "ledger",
                required: true,
                description: "Actual and budget rows in one file",
                shape: "side|account|name|category|amount",
            }],
            options: vec![
                OptionSpec {
                    name: "property",
                    required: true,
                    description: "Synthetic property name",
                },
                OptionSpec {
                    name: "period",
                    required: true,
                    description: "YYYY-MM",
                },
            ],
            refuses: vec!["INVALID_INPUT: a side other than A or B"],
        }
    }
    fn read(&self, inputs: &AdapterInputs) -> Result<(Dataset, Value)> {
        let bytes = inputs.require_file("ledger")?;
        let text = String::from_utf8(bytes.clone())
            .map_err(|_| boxscore::exact::error("INVALID_INPUT", "Ledger must be UTF-8"))?;
        let (mut actuals, mut budgets) = (Vec::new(), Vec::new());
        for row in text.lines().filter(|l| !l.is_empty()) {
            let cells: Vec<&str> = row.split('|').collect();
            let [side, code, name, category, amount] = cells[..] else {
                return Err(boxscore::exact::error(
                    "INVALID_INPUT",
                    "Five cells per row",
                ));
            };
            let parsed = Line {
                account_code: code.into(),
                account_name: name.into(),
                category: category.into(),
                amount: amount.parse()?,
            };
            match side {
                "A" => actuals.push(parsed),
                "B" => budgets.push(parsed),
                _ => {
                    return Err(boxscore::exact::error(
                        "INVALID_INPUT",
                        "Side must be A or B",
                    ))
                }
            }
        }
        let data = Dataset {
            property: inputs.require_option("property")?.into(),
            period: inputs.require_option("period")?.into(),
            currency: "USD".into(),
            expense_convention: "positive_costs".into(),
            unit_count: 10,
            actuals,
            budgets,
            snapshot: None,
        };
        // No `profile` here: read_with stamps it.
        Ok((data, json!({"ledger_sha256": digest(&bytes)})))
    }
}

/// Fails closed: a profile without a version is never accepted.
struct Unversioned;

impl SourceAdapter for Unversioned {
    fn profile(&self) -> &'static str {
        "acme"
    }
    fn spec(&self) -> AdapterSpec {
        AdapterSpec {
            profile: "acme",
            summary: "",
            files: vec![],
            options: vec![],
            refuses: vec![],
        }
    }
    fn read(&self, _: &AdapterInputs) -> Result<(Dataset, Value)> {
        unreachable!("read_with refuses before reading")
    }
}

// (c) a vendor adapter plugs into the same path and is recorded on the revision.
#[tokio::test]
async fn test_only_vendor_adapter_plugs_in_and_is_recorded() {
    let f = Fixture::new();
    std::fs::write(
        f.path("acme.txt"),
        "A|4000|Rent|rental income|100.10\nB|4000|Rent|rental income|90.05\n",
    )
    .unwrap();
    let inputs = AdapterInputs::default()
        .with_file("ledger", f.path("acme.txt"))
        .with_option("property", "Synthetic Acme")
        .with_option("period", "2026-05");
    let (data, sources) = ingest::read_with(&AcmeGl, &inputs).unwrap();
    assert_eq!(sources["profile"], "acme.gl/1");

    let db = f.root.join("acme.sqlite");
    let pool = store::create(&db).await.unwrap();
    let id = store::import(&pool, &data, None, None, &sources)
        .await
        .unwrap();
    let reviewed = store::review(&pool, &id).await.unwrap();
    pool.close().await;
    assert_eq!(reviewed["sources"]["profile"], "acme.gl/1");
    assert_eq!(reviewed["variance"]["noi_bridge"]["noi_variance"], "10.05");

    // Not registered: the host cannot select it by id, and the refusal lists
    // what is registered.
    let refused = ingest::adapter("acme.gl/1").err().unwrap();
    assert_eq!(refused.code, "UNKNOWN_PROFILE");
    assert!(
        refused.message.contains("canonical.csv/1"),
        "{}",
        refused.message
    );
    assert!(
        refused.message.contains("canonical.json/1"),
        "{}",
        refused.message
    );
    // Profile ids match exactly; no prefix or version guessing.
    for near in [
        "canonical.csv",
        "canonical.csv/2",
        "CANONICAL.CSV/1",
        "canonical.csv/1 ",
    ] {
        assert_eq!(
            ingest::adapter(near).err().unwrap().code,
            "UNKNOWN_PROFILE",
            "{near}"
        );
    }

    let unversioned = ingest::read_with(&Unversioned, &AdapterInputs::default())
        .err()
        .unwrap();
    assert_eq!(unversioned.code, "ADAPTER_CONTRACT");
}

fn refusal(inputs: &AdapterInputs, profile: &str) -> (String, String) {
    let adapter = ingest::adapter(profile).unwrap();
    let e = ingest::read_with(adapter, inputs).err().unwrap();
    (e.code.to_string(), e.message)
}

// (d) inputs that do not match the spec refuse with typed errors.
#[test]
fn inputs_outside_the_spec_refuse_with_typed_errors() {
    let f = Fixture::new();
    let mut missing = f.csv_inputs();
    missing.files.remove("budgets");
    let (code, message) = refusal(&missing, CANONICAL_CSV);
    assert_eq!(code, "MISSING_FILE");
    assert!(message.contains("budgets"), "{message}");

    let (code, message) = refusal(
        &f.csv_inputs().with_option("currency", "EUR"),
        CANONICAL_CSV,
    );
    assert_eq!(code, "UNKNOWN_OPTION");
    assert!(message.contains("currency"), "{message}");

    let (code, message) = refusal(
        &f.csv_inputs()
            .with_file("rentroll", f.path("snapshot.json")),
        CANONICAL_CSV,
    );
    assert_eq!(code, "UNKNOWN_FILE");
    assert!(
        message.contains("rentroll") && message.contains("actuals"),
        "{message}"
    );

    let mut no_units = f.csv_inputs();
    no_units.options.remove("units");
    let (code, message) = refusal(&no_units, CANONICAL_CSV);
    assert_eq!(code, "MISSING_OPTION");
    assert!(message.contains("units"), "{message}");

    let (code, _) = refusal(&f.csv_inputs().with_option("units", "ten"), CANONICAL_CSV);
    assert_eq!(code, "INVALID_INPUT");

    // canonical.json/1 takes no options at all.
    let json_inputs = AdapterInputs::default()
        .with_file("input", f.path("period.json"))
        .with_option("property", "Synthetic Oak");
    let (code, message) = refusal(&json_inputs, CANONICAL_JSON);
    assert_eq!(code, "UNKNOWN_OPTION");
    assert!(message.contains("property"), "{message}");

    // The 2 MiB cap applies per file, as before.
    std::fs::write(f.path("huge.csv"), vec![b'a'; 2 * 1024 * 1024 + 1]).unwrap();
    let (code, _) = refusal(
        &f.csv_inputs().with_file("actuals", f.path("huge.csv")),
        CANONICAL_CSV,
    );
    assert_eq!(code, "INPUT_LIMIT");

    // Same refusals through the binary, which never opens the database first.
    let db = f.database("refusals.sqlite");
    let (code, body) = run(&[
        "import-with",
        "--database",
        &db,
        "--profile",
        "canonical.csv/1",
        "--file",
        &format!("actuals={}", f.path("actuals.csv")),
        "--option",
        "property=Synthetic Oak",
        "--option",
        "period=2026-05",
        "--option",
        "units=10",
    ]);
    assert_eq!(
        (code, body["error"]["code"].as_str()),
        (2, Some("MISSING_FILE"))
    );
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("budgets"));
    let (code, body) = run(&["import-with", "--database", &db, "--profile", "yardi.gl/1"]);
    assert_eq!(
        (code, body["error"]["code"].as_str()),
        (2, Some("UNKNOWN_PROFILE"))
    );
    let (code, body) = run(&[
        "import-with",
        "--database",
        &db,
        "--profile",
        "canonical.json/1",
        "--file",
        "input",
    ]);
    assert_eq!(
        (code, body["error"]["code"].as_str()),
        (2, Some("INVALID_INPUT"))
    );
    let (code, body) = run(&[
        "import-with",
        "--database",
        &db,
        "--profile",
        "canonical.json/1",
        "--file",
        &format!("input={}", f.path("period.json")),
        "--file",
        &format!("input={}", f.path("period.json")),
    ]);
    assert_eq!(
        (code, body["error"]["code"].as_str()),
        (2, Some("INVALID_INPUT"))
    );
    let (code, listed) = run(&["call", "--database", &db, "--function", "list_revisions"]);
    assert_eq!(code, 0);
    assert_eq!(listed["result"]["revisions"], json!([]));
}

fn profiles(adapters: &Value) -> Vec<&str> {
    adapters
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["profile"].as_str().unwrap())
        .collect()
}

// (e) the adapters subcommand prints both specs.
#[test]
fn adapters_subcommand_prints_every_spec() {
    let (code, body) = run(&["adapters"]);
    assert_eq!(code, 0);
    assert_eq!(body["contract_version"], "plat.ops/1");
    assert_eq!(
        profiles(&body["adapters"]),
        ["canonical.csv/1", "canonical.json/1"]
    );
    assert_eq!(body["adapters"], ingest::describe_adapters());
    let csv = &body["adapters"][0];
    let files: Vec<(&str, bool)> = csv["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["name"].as_str().unwrap(),
                f["required"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        files,
        [("actuals", true), ("budgets", true), ("snapshot", false)]
    );
    assert_eq!(
        csv["files"][0]["shape"],
        "account_code,account_name,category,amount"
    );
    let options: Vec<&str> = csv["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["name"].as_str().unwrap())
        .collect();
    assert_eq!(options, ["property", "period", "units"]);
    assert!(!csv["refuses"].as_array().unwrap().is_empty());
    let json_spec = &body["adapters"][1];
    assert_eq!(json_spec["files"][0]["name"], "input");
    assert_eq!(json_spec["options"], json!([]));
}

// (f) describe through the catalog carries the adapters, and the schema says so.
#[tokio::test]
async fn catalog_describe_lists_adapters() {
    let envelope = catalog::invoke(&Call {
        function: "describe".into(),
        version: None,
        input: json!({}),
        database: None,
        caller: "test".into(),
    })
    .await;
    assert_eq!(envelope.status, "calculated");
    let result = envelope.result.unwrap();
    assert_eq!(result["adapters"], ingest::describe_adapters());
    assert_eq!(
        profiles(&result["adapters"]),
        ["canonical.csv/1", "canonical.json/1"]
    );
    let schema = catalog::find("describe", None).unwrap().output_schema();
    assert!(schema["required"]
        .as_array()
        .unwrap()
        .contains(&json!("adapters")));
    let entry = &schema["properties"]["adapters"]["items"];
    for adapter in result["adapters"].as_array().unwrap() {
        for key in entry["required"].as_array().unwrap() {
            assert!(adapter.get(key.as_str().unwrap()).is_some(), "{key}");
        }
    }
}

#[test]
fn registry_profiles_are_versioned_unique_and_self_consistent() {
    let mut seen = std::collections::BTreeSet::new();
    for adapter in ingest::registry() {
        let profile = adapter.profile();
        assert!(ingest::is_versioned_profile(profile), "{profile}");
        assert_eq!(adapter.spec().profile, profile);
        assert!(seen.insert(profile), "duplicate {profile}");
        assert!(std::ptr::addr_eq(
            ingest::adapter(profile).unwrap(),
            *adapter
        ));
    }
    for bad in [
        "acme", "acme/", "acme/0", "acme/01", "Acme/1", "/1", "acme/1.0", "a b/1",
    ] {
        assert!(!ingest::is_versioned_profile(bad), "{bad}");
    }
    assert!(ingest::is_versioned_profile("acme.gl/12"));
}
