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

// (g) import_period@1.1.0: a profile with inline file contents runs the same
// adapter on the same bytes as `import-with` on paths.
fn quickstart(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../docs/quickstart")
        .join(name)
}

fn quickstart_text(name: &str) -> String {
    std::fs::read_to_string(quickstart(name)).unwrap()
}

fn inline_quickstart_input() -> Value {
    json!({
        "profile": "canonical.csv/1",
        "files": {
            "actuals": quickstart_text("actuals.csv"),
            "budgets": quickstart_text("budgets.csv"),
            "snapshot": quickstart_text("snapshot.json"),
        },
        "options": {"property": "Sample Gardens", "period": "2026-05", "units": "24"},
    })
}

async fn call_import(db: &str, input: Value) -> catalog::Envelope {
    catalog::invoke(&Call {
        function: "import_period".into(),
        version: None,
        input,
        database: Some(PathBuf::from(db)),
        caller: "test".into(),
    })
    .await
}

fn refused_with(envelope: &catalog::Envelope) -> (String, String) {
    assert_eq!(envelope.status, "refused");
    let error = envelope.error.as_ref().unwrap();
    (
        error["code"].as_str().unwrap().to_string(),
        error["message"].as_str().unwrap().to_string(),
    )
}

#[tokio::test]
async fn inline_import_matches_import_with_on_the_same_files() {
    let f = Fixture::new();
    let path = |name: &str| quickstart(name).to_str().unwrap().to_string();
    let cli_db = f.database("cli.sqlite");
    let from_paths = imported(run(&[
        "import-with",
        "--database",
        &cli_db,
        "--profile",
        "canonical.csv/1",
        "--file",
        &format!("actuals={}", path("actuals.csv")),
        "--file",
        &format!("budgets={}", path("budgets.csv")),
        "--file",
        &format!("snapshot={}", path("snapshot.json")),
        "--option",
        "property=Sample Gardens",
        "--option",
        "period=2026-05",
        "--option",
        "units=24",
    ]));
    let inline_db = f.database("inline.sqlite");
    let envelope = call_import(&inline_db, inline_quickstart_input()).await;
    assert_eq!(envelope.status, "calculated", "{:?}", envelope.error);
    assert_eq!(envelope.version, Some("1.1.0"));
    let from_inline = envelope.result.unwrap()["revision_id"]
        .as_str()
        .unwrap()
        .to_string();

    let a = review(&cli_db, &from_paths);
    let b = review(&inline_db, &from_inline);
    assert_eq!(a["input_sha256"], b["input_sha256"]);
    assert_eq!(a["variance"], b["variance"]);
    assert_eq!(a["snapshot"], b["snapshot"]);
    for key in [
        "profile",
        "actuals_sha256",
        "budgets_sha256",
        "snapshot_sha256",
    ] {
        assert!(a["sources"][key].is_string(), "{key}");
        assert_eq!(a["sources"][key], b["sources"][key], "{key}");
    }
    // Path-backed files are recorded as before: no per-file list, no path.
    assert!(a["sources"].get("files").is_none(), "{}", a["sources"]);
    // Inline files carry name, hash of the bytes, size and the inline flag.
    let listed: Vec<Value> = ["actuals", "budgets", "snapshot"]
        .iter()
        .map(|name| {
            let file = match *name {
                "snapshot" => "snapshot.json".to_string(),
                other => format!("{other}.csv"),
            };
            let bytes = std::fs::read(quickstart(&file)).unwrap();
            assert_eq!(a["sources"][format!("{name}_sha256")], digest(&bytes));
            json!({"name": name, "sha256": digest(&bytes), "bytes": bytes.len(), "inline": true})
        })
        .collect();
    assert_eq!(b["sources"]["files"], Value::Array(listed));
    assert!(!b["sources"].to_string().contains("quickstart"));

    // The same bytes inline into the database the CLI wrote are the same
    // dataset, so the import is idempotent and returns the CLI's revision.
    let again = call_import(&cli_db, inline_quickstart_input()).await;
    assert_eq!(again.result.unwrap()["revision_id"], json!(from_paths));
}

#[tokio::test]
async fn inline_import_refuses_with_typed_errors() {
    let f = Fixture::new();
    let db = f.database("inline-refusals.sqlite");
    let dataset = serde_json::to_value(expected_dataset()).unwrap();

    let mut both = inline_quickstart_input();
    both["dataset"] = dataset.clone();
    let (code, message) = refused_with(&call_import(&db, both).await);
    assert_eq!(code, "INVALID_INPUT");
    assert!(message.contains("exactly one of dataset"), "{message}");

    let (code, _) = refused_with(&call_import(&db, json!({"options": {}})).await);
    assert_eq!(code, "INVALID_INPUT");

    let (code, message) = refused_with(
        &call_import(&db, json!({"dataset": dataset, "files": {"actuals": "x"}})).await,
    );
    assert_eq!(code, "INVALID_INPUT");
    assert!(message.contains("only with profile"), "{message}");

    let mut unknown = inline_quickstart_input();
    unknown["profile"] = json!("yardi.gl/1");
    let (code, message) = refused_with(&call_import(&db, unknown).await);
    assert_eq!(code, "UNKNOWN_PROFILE");
    assert!(
        message.contains("canonical.csv/1") && message.contains("canonical.json/1"),
        "{message}"
    );

    let mut missing = inline_quickstart_input();
    missing["files"].as_object_mut().unwrap().remove("budgets");
    let (code, message) = refused_with(&call_import(&db, missing).await);
    assert_eq!(code, "MISSING_FILE");
    assert!(message.contains("budgets"), "{message}");

    let mut extra = inline_quickstart_input();
    extra["files"]["rentroll"] = json!("unit,rent\n");
    let (code, _) = refused_with(&call_import(&db, extra).await);
    assert_eq!(code, "UNKNOWN_FILE");

    let mut no_period = inline_quickstart_input();
    no_period["options"]
        .as_object_mut()
        .unwrap()
        .remove("period");
    let (code, _) = refused_with(&call_import(&db, no_period).await);
    assert_eq!(code, "MISSING_OPTION");

    // An explicit null is not "absent": the schema refuses it, and so does
    // the engine, naming the key.
    for key in ["dataset", "profile", "files", "options"] {
        let mut nulled = if key == "dataset" {
            inline_quickstart_input()
        } else {
            json!({"dataset": dataset.clone()})
        };
        nulled[key] = Value::Null;
        let (code, message) = refused_with(&call_import(&db, nulled).await);
        assert_eq!(code, "INVALID_INPUT", "{key}");
        assert!(
            message.contains(key) && message.contains("null"),
            "{message}"
        );
    }

    let mut not_text = inline_quickstart_input();
    not_text["files"]["actuals"] = json!(["not", "a", "string"]);
    let (code, _) = refused_with(&call_import(&db, not_text).await);
    assert_eq!(code, "INVALID_INPUT");

    // Nothing was stored by any refusal.
    let (code, listed) = run(&["call", "--database", &db, "--function", "list_revisions"]);
    assert_eq!(code, 0);
    assert_eq!(listed["result"]["revisions"], json!([]));
}

#[tokio::test]
async fn inline_import_records_caller_sources_under_supplied() {
    let f = Fixture::new();
    let db = f.database("supplied.sqlite");
    let mut input = inline_quickstart_input();
    input["sources"] = json!({"uploaded_as": "may-gl.csv"});
    let envelope = call_import(&db, input).await;
    let revision = envelope.result.unwrap()["revision_id"]
        .as_str()
        .unwrap()
        .to_string();
    let reviewed = review(&db, &revision);
    assert_eq!(
        reviewed["sources"]["supplied"],
        json!({"uploaded_as": "may-gl.csv"})
    );
    assert_eq!(reviewed["sources"]["profile"], "canonical.csv/1");
}

/// A caller's `sources` can never take a key the server owns: in the dataset
/// form and the profile form alike, `profile` and `files` sent by the caller
/// end up only under `supplied`.
#[tokio::test]
async fn caller_sources_cannot_impersonate_server_keys() {
    let f = Fixture::new();
    let db = f.database("impersonate.sqlite");
    let forged = json!({
        "profile": "canonical.csv/1",
        "files": [{"name": "actuals", "sha256": "0".repeat(64), "bytes": 1, "inline": true}],
        "canonical_json_sha256": "f".repeat(64),
    });
    let envelope = call_import(
        &db,
        json!({"dataset": serde_json::to_value(expected_dataset()).unwrap(), "sources": forged}),
    )
    .await;
    assert_eq!(envelope.status, "calculated", "{:?}", envelope.error);
    let revision = envelope.result.unwrap()["revision_id"]
        .as_str()
        .unwrap()
        .to_string();
    let recorded = json!({"canonical_json_sha256": expected_hash(), "supplied": forged});
    assert_eq!(review(&db, &revision)["sources"], recorded);
    let (code, listed) = run(&["call", "--database", &db, "--function", "list_revisions"]);
    assert_eq!(code, 0);
    assert_eq!(listed["result"]["revisions"][0]["sources"], recorded);
    let provenance = catalog::invoke(&Call {
        function: "get_provenance".into(),
        version: None,
        input: json!({"trace_id": envelope.trace_id}),
        database: Some(PathBuf::from(&db)),
        caller: "test".into(),
    })
    .await;
    assert_eq!(provenance.result.unwrap()["revision"]["sources"], recorded);

    // Without caller sources the dataset form records only the server's hash.
    let bare_db = f.database("impersonate-bare.sqlite");
    let bare = call_import(
        &bare_db,
        json!({"dataset": serde_json::to_value(expected_dataset()).unwrap()}),
    )
    .await;
    let bare_revision = bare.result.unwrap()["revision_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        review(&bare_db, &bare_revision)["sources"],
        json!({"canonical_json_sha256": expected_hash()})
    );

    // The profile form: the adapter's profile and files stand, the forged
    // ones sit under supplied.
    let inline_db = f.database("impersonate-inline.sqlite");
    let mut input = inline_quickstart_input();
    input["sources"] = json!({"profile": "acme.gl/1", "files": []});
    let envelope = call_import(&inline_db, input).await;
    let revision = envelope.result.unwrap()["revision_id"]
        .as_str()
        .unwrap()
        .to_string();
    let sources = review(&inline_db, &revision)["sources"].clone();
    assert_eq!(sources["profile"], "canonical.csv/1");
    assert_eq!(sources["files"].as_array().unwrap().len(), 3);
    assert_eq!(
        sources["supplied"],
        json!({"profile": "acme.gl/1", "files": []})
    );
}

#[tokio::test]
async fn padded_property_refuses_instead_of_starting_a_second_series() {
    let f = Fixture::new();
    let db = f.database("padded.sqlite");
    for padded in ["Synthetic Oak ", " Synthetic Oak", "Synthetic Oak\t"] {
        let mut dataset = expected_dataset();
        dataset.property = padded.to_string();
        let (code, message) = refused_with(
            &call_import(
                &db,
                json!({"dataset": serde_json::to_value(&dataset).unwrap()}),
            )
            .await,
        );
        assert_eq!(code, "INVALID_INPUT", "{padded:?}");
        assert!(message.contains("whitespace"), "{message}");
    }
    // The same rule through an adapter option.
    let mut input = inline_quickstart_input();
    input["options"]["property"] = json!("Sample Gardens ");
    let (code, _) = refused_with(&call_import(&db, input).await);
    assert_eq!(code, "INVALID_INPUT");
    // Inner spaces are part of the name.
    let envelope = call_import(&db, inline_quickstart_input()).await;
    assert_eq!(envelope.status, "calculated", "{:?}", envelope.error);
}

/// An adapter that exists only in this test and returns whatever `sources`
/// it is built with, to drive `read_with`'s contract checks.
struct Stated(Value);

impl SourceAdapter for Stated {
    fn profile(&self) -> &'static str {
        "stated.test/1"
    }
    fn spec(&self) -> AdapterSpec {
        AdapterSpec {
            profile: "stated.test/1",
            summary: "Synthetic: returns the sources it was built with",
            files: vec![FileSpec {
                name: "ledger",
                required: false,
                description: "Any bytes",
                shape: "anything",
            }],
            options: vec![],
            refuses: vec![],
        }
    }
    fn read(&self, inputs: &AdapterInputs) -> Result<(Dataset, Value)> {
        inputs.read_file("ledger")?;
        Ok((expected_dataset(), self.0.clone()))
    }
}

#[test]
fn adapter_contract_refuses_sources_the_adapter_must_not_state() {
    let inline = AdapterInputs::default().with_contents("ledger", "x");
    let contract = |adapter: &Stated, inputs: &AdapterInputs| {
        let e = ingest::read_with(adapter, inputs).err().unwrap();
        assert_eq!(e.code, "ADAPTER_CONTRACT", "{}", e.message);
        e.message
    };

    // sources.files is read_with's record of inline files.
    let message = contract(&Stated(json!({"files": []})), &inline);
    assert!(message.contains("sources.files"), "{message}");

    // sources.profile, when stated, must be the adapter's own.
    let message = contract(
        &Stated(json!({"profile": "canonical.csv/1"})),
        &AdapterInputs::default(),
    );
    assert!(message.contains("sources.profile"), "{message}");

    // sources.supplied is the caller's, with or without inline files.
    for inputs in [&inline, &AdapterInputs::default()] {
        let message = contract(&Stated(json!({"supplied": {}})), inputs);
        assert!(message.contains("sources.supplied"), "{message}");
    }

    // sources must be an object.
    let message = contract(&Stated(json!(["not", "an", "object"])), &inline);
    assert!(message.contains("JSON object"), "{message}");

    // The same adapter with nothing reserved stated passes, and gets its
    // profile and the inline file list from read_with.
    let (_, sources) = ingest::read_with(&Stated(json!({"ledger_sha256": "h"})), &inline).unwrap();
    assert_eq!(sources["profile"], "stated.test/1");
    assert_eq!(
        sources["files"],
        json!([{"name": "ledger", "sha256": digest(b"x"), "bytes": 1, "inline": true}])
    );
    let (_, sources) =
        ingest::read_with(&Stated(json!({"profile": "stated.test/1"})), &inline).unwrap();
    assert_eq!(sources["profile"], "stated.test/1");
}

/// canonical.json/1 inline gives the same revision as import-with on the
/// same file.
#[tokio::test]
async fn inline_canonical_json_matches_import_with_on_the_same_file() {
    let f = Fixture::new();
    let cli_db = f.database("json-cli.sqlite");
    let from_path = imported(run(&[
        "import-with",
        "--database",
        &cli_db,
        "--profile",
        "canonical.json/1",
        "--file",
        &format!("input={}", f.path("period.json")),
    ]));
    let text = std::fs::read_to_string(f.path("period.json")).unwrap();
    let input = json!({"profile": "canonical.json/1", "files": {"input": text}});
    let inline_db = f.database("json-inline.sqlite");
    let envelope = call_import(&inline_db, input.clone()).await;
    assert_eq!(envelope.status, "calculated", "{:?}", envelope.error);
    let from_inline = envelope.result.unwrap()["revision_id"]
        .as_str()
        .unwrap()
        .to_string();

    let a = review(&cli_db, &from_path);
    let b = review(&inline_db, &from_inline);
    assert_eq!(a["input_sha256"], expected_hash());
    assert_eq!(a["input_sha256"], b["input_sha256"]);
    assert_eq!(a["variance"], b["variance"]);
    assert_eq!(a["snapshot"], b["snapshot"]);
    let bytes = text.as_bytes();
    assert_eq!(
        a["sources"],
        json!({"profile": "canonical.json/1", "canonical_json_sha256": digest(bytes)})
    );
    assert_eq!(
        b["sources"],
        json!({"profile": "canonical.json/1", "canonical_json_sha256": digest(bytes),
               "files": [{"name": "input", "sha256": digest(bytes),
                          "bytes": bytes.len(), "inline": true}]})
    );

    // Inline into the CLI's database is idempotent.
    let again = call_import(&cli_db, input).await;
    assert_eq!(again.result.unwrap()["revision_id"], json!(from_path));
}

#[test]
fn inline_contents_win_over_paths_and_are_bounded() {
    let f = Fixture::new();
    let inputs = f.csv_inputs().with_contents("actuals", BUDGETS);
    assert_eq!(
        inputs.read_file("actuals").unwrap().unwrap(),
        BUDGETS.as_bytes()
    );
    assert_eq!(
        inputs.read_file("budgets").unwrap().unwrap(),
        BUDGETS.as_bytes()
    );
    assert!(inputs.has_file("actuals") && !inputs.has_file("rentroll"));
    assert_eq!(
        inputs.file_names().collect::<Vec<_>>(),
        ["actuals", "budgets", "snapshot"]
    );
    let (_, sources) = ingest::read_with(ingest::adapter(CANONICAL_CSV).unwrap(), &inputs).unwrap();
    assert_eq!(sources["actuals_sha256"], digest(BUDGETS.as_bytes()));
    assert_eq!(
        sources["files"],
        json!([{"name": "actuals", "sha256": digest(BUDGETS.as_bytes()),
                "bytes": BUDGETS.len(), "inline": true}])
    );

    // One inline file over the per-file bound.
    let mib = 1024 * 1024;
    let huge = f
        .csv_inputs()
        .with_contents("actuals", vec![b'a'; 2 * mib + 1]);
    let (code, message) = refusal(&huge, CANONICAL_CSV);
    assert_eq!(code, "INPUT_LIMIT");
    assert!(message.contains("actuals"), "{message}");
    assert_eq!(huge.read_file("actuals").err().unwrap().code, "INPUT_LIMIT");

    // Each file within the bound, the total over it.
    let total = AdapterInputs::default()
        .with_contents("actuals", vec![b'a'; mib + 1])
        .with_contents("budgets", vec![b'b'; mib]);
    let (code, message) = refusal(&total, CANONICAL_CSV);
    assert_eq!(code, "INPUT_LIMIT");
    assert!(message.contains("total"), "{message}");
}
