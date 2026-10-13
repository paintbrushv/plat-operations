//! Pluggable source adapters for the exact-cent core.
//!
//! An adapter turns a host's local files and options into the vendor-neutral
//! [`Dataset`] plus a `sources` provenance object that is recorded verbatim on
//! the revision. No vendor layout is assumed by the core: every shape,
//! including the canonical CSV and JSON this crate ships, is one adapter in
//! the registry, addressed by a versioned profile id such as
//! `canonical.csv/1`.
//!
//! Profile ids fail closed. A host names the exact profile it means; an
//! unknown id refuses with `UNKNOWN_PROFILE` rather than guessing. A change to
//! what an adapter accepts or how it maps a file into a `Dataset` is a new
//! profile id (`.../2`), never an edit to an existing one, so a stored
//! revision's `sources.profile` always names the mapping that produced it.
//!
//! Paths are a host concern. The calculation catalog never takes a path:
//! the host runs adapters over local files (`boxscore-exact import-with`), and
//! `import_period` runs a registered adapter over file contents sent inline in
//! the call (`AdapterInputs::with_contents`). Either way the adapter reads the
//! same bytes, so the same files produce the same dataset and the same hashes,
//! and the dataset reaches `store::import` the same way.
//!
//! Inline contents are bounded like files: each one at most 2 MiB (the bound
//! [`read_bounded`] enforces) and all of them together at most 2 MiB. Every
//! inline file is listed in the recorded `sources.files` as
//! `{"name", "sha256", "bytes", "inline": true}`; path-backed files are
//! recorded only by what the adapter itself states, as before.
//!
//! `sources.supplied` is reserved for the caller's own `sources` object
//! (`import_period` puts it there); an adapter that states it refuses with
//! `ADAPTER_CONTRACT`, as does one that states a different `profile` or, with
//! inline files, its own `files`.
use super::{digest, error, protocol::MAX_REQUEST_BYTES, store::Dataset, variance::Line, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};

pub const CANONICAL_CSV: &str = "canonical.csv/1";
/// The one `sources` key a caller writes: `import_period` records the caller's
/// own `sources` object under it. Every other key is the server's.
pub const SUPPLIED: &str = "supplied";
pub const CANONICAL_JSON: &str = "canonical.json/1";

/// One named input file an adapter accepts.
#[derive(Debug, Clone)]
pub struct FileSpec {
    pub name: &'static str,
    pub required: bool,
    pub description: &'static str,
    /// Expected header row or document shape.
    pub shape: &'static str,
}

/// One named string option an adapter accepts.
#[derive(Debug, Clone)]
pub struct OptionSpec {
    pub name: &'static str,
    pub required: bool,
    pub description: &'static str,
}

/// What an adapter accepts and what makes it refuse. Published by
/// `boxscore-exact adapters` and the catalog's `describe`.
#[derive(Debug, Clone)]
pub struct AdapterSpec {
    pub profile: &'static str,
    pub summary: &'static str,
    pub files: Vec<FileSpec>,
    pub options: Vec<OptionSpec>,
    pub refuses: Vec<&'static str>,
}

impl AdapterSpec {
    pub fn to_value(&self) -> Value {
        json!({
            "profile": self.profile,
            "summary": self.summary,
            "files": self.files.iter().map(|f| json!({
                "name": f.name,
                "required": f.required,
                "description": f.description,
                "shape": f.shape,
            })).collect::<Vec<_>>(),
            "options": self.options.iter().map(|o| json!({
                "name": o.name,
                "required": o.required,
                "description": o.description,
            })).collect::<Vec<_>>(),
            "refuses": self.refuses,
        })
    }
}

/// Named files and string options supplied by the host. A file is either
/// path-backed (`files`, read from local disk by the host process) or held in
/// memory (`contents`, sent inline to `import_period`); when a name is in
/// both, the in-memory bytes win.
#[derive(Debug, Clone, Default)]
pub struct AdapterInputs {
    pub files: BTreeMap<String, PathBuf>,
    pub contents: BTreeMap<String, Vec<u8>>,
    pub options: BTreeMap<String, String>,
}

impl AdapterInputs {
    pub fn with_file(mut self, name: &str, path: impl Into<PathBuf>) -> Self {
        self.files.insert(name.to_string(), path.into());
        self
    }
    /// A named file held in memory instead of on disk.
    pub fn with_contents(mut self, name: &str, bytes: impl Into<Vec<u8>>) -> Self {
        self.contents.insert(name.to_string(), bytes.into());
        self
    }
    pub fn with_option(mut self, name: &str, value: impl Into<String>) -> Self {
        self.options.insert(name.to_string(), value.into());
        self
    }
    /// Whether a file of this name was supplied, in memory or by path.
    pub fn has_file(&self, name: &str) -> bool {
        self.contents.contains_key(name) || self.files.contains_key(name)
    }
    /// Every supplied file name, in memory or by path, sorted and unique.
    pub fn file_names(&self) -> impl Iterator<Item = &str> {
        let mut names: Vec<&str> = self
            .contents
            .keys()
            .chain(self.files.keys())
            .map(String::as_str)
            .collect();
        names.sort_unstable();
        names.dedup();
        names.into_iter()
    }
    /// Refuse with `INPUT_LIMIT` when an inline file exceeds the per-file
    /// bound [`read_bounded`] enforces, or all inline files together exceed
    /// `MAX_REQUEST_BYTES`.
    pub fn check_inline_limits(&self) -> Result<()> {
        let mut total: usize = 0;
        for (name, bytes) in &self.contents {
            if bytes.len() > MAX_REQUEST_BYTES {
                return Err(error(
                    "INPUT_LIMIT",
                    format!("Inline file {name:?} exceeds 2 MiB"),
                ));
            }
            total = total.saturating_add(bytes.len());
        }
        if total > MAX_REQUEST_BYTES {
            return Err(error("INPUT_LIMIT", "Inline files exceed 2 MiB in total"));
        }
        Ok(())
    }
    /// `{"name", "sha256", "bytes", "inline": true}` for each inline file, by
    /// name. The hash is of the bytes, exactly as for a path-backed file.
    pub fn inline_sources(&self) -> Vec<Value> {
        self.contents
            .iter()
            .map(|(name, bytes)| {
                json!({
                    "name": name,
                    "sha256": digest(bytes),
                    "bytes": bytes.len(),
                    "inline": true,
                })
            })
            .collect()
    }
    /// Bytes of the named file (bounded at 2 MiB), or `None` if not supplied.
    /// In-memory contents win over a path-backed file of the same name.
    pub fn read_file(&self, name: &str) -> Result<Option<Vec<u8>>> {
        if let Some(bytes) = self.contents.get(name) {
            if bytes.len() > MAX_REQUEST_BYTES {
                return Err(error(
                    "INPUT_LIMIT",
                    format!("Inline file {name:?} exceeds 2 MiB"),
                ));
            }
            return Ok(Some(bytes.clone()));
        }
        self.files.get(name).map(|p| read_bounded(p)).transpose()
    }
    /// Bytes of the named file (bounded at 2 MiB); refuses with `MISSING_FILE`.
    pub fn require_file(&self, name: &str) -> Result<Vec<u8>> {
        self.read_file(name)?.ok_or_else(|| {
            error(
                "MISSING_FILE",
                format!("Required file {name:?} not supplied"),
            )
        })
    }
    /// The named option; refuses with `MISSING_OPTION`.
    pub fn require_option(&self, name: &str) -> Result<&str> {
        self.options.get(name).map(String::as_str).ok_or_else(|| {
            error(
                "MISSING_OPTION",
                format!("Required option {name:?} not supplied"),
            )
        })
    }
}

/// A source adapter: one versioned way of turning local files into a
/// canonical [`Dataset`]. Implementations must be deterministic (same bytes,
/// same `Dataset`), must not infer signs, currency, or account approval, and
/// must refuse rather than coerce anything they do not understand.
pub trait SourceAdapter: Send + Sync {
    /// Versioned profile id, `name/N`.
    fn profile(&self) -> &'static str;
    fn spec(&self) -> AdapterSpec;
    /// Read the inputs into a dataset and a `sources` provenance object.
    /// Hosts call [`read_with`], which checks inputs against [`Self::spec`]
    /// first and stamps `sources.profile`.
    fn read(&self, inputs: &AdapterInputs) -> Result<(Dataset, Value)>;
}

static REGISTRY: &[&dyn SourceAdapter] = &[&CanonicalCsv, &CanonicalJson];

/// Every adapter this build ships, in a fixed order.
pub fn registry() -> &'static [&'static dyn SourceAdapter] {
    REGISTRY
}

/// Resolve a registered adapter by exact profile id.
pub fn adapter(profile: &str) -> Result<&'static dyn SourceAdapter> {
    REGISTRY
        .iter()
        .copied()
        .find(|a| a.profile() == profile)
        .ok_or_else(|| {
            let known: Vec<&str> = REGISTRY.iter().map(|a| a.profile()).collect();
            error(
                "UNKNOWN_PROFILE",
                format!(
                    "No source adapter for profile {profile:?}; registered profiles: {}",
                    known.join(", ")
                ),
            )
        })
}

/// Specs of every registered adapter, as a JSON array.
pub fn describe_adapters() -> Value {
    Value::Array(REGISTRY.iter().map(|a| a.spec().to_value()).collect())
}

/// `name/N`: a lowercase name of `[a-z0-9._-]` and a positive integer version.
pub fn is_versioned_profile(id: &str) -> bool {
    let Some((name, version)) = id.split_once('/') else {
        return false;
    };
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        && !version.is_empty()
        && !version.starts_with('0')
        && version.bytes().all(|b| b.is_ascii_digit())
}

/// Run any adapter, registered or not: check the inputs against its spec,
/// read, and record `sources.profile`. This is the only path hosts use.
pub fn read_with(adapter: &dyn SourceAdapter, inputs: &AdapterInputs) -> Result<(Dataset, Value)> {
    let profile = adapter.profile();
    let spec = adapter.spec();
    if !is_versioned_profile(profile) || spec.profile != profile {
        return Err(error(
            "ADAPTER_CONTRACT",
            format!("Adapter profile {profile:?} is not a consistent versioned id (name/N)"),
        ));
    }
    let expected_files = || {
        spec.files
            .iter()
            .map(|f| f.name)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let expected_options = || {
        spec.options
            .iter()
            .map(|o| o.name)
            .collect::<Vec<_>>()
            .join(", ")
    };
    inputs.check_inline_limits()?;
    for name in inputs.file_names() {
        if !spec.files.iter().any(|f| f.name == name) {
            return Err(error(
                "UNKNOWN_FILE",
                format!(
                    "{profile} has no file named {name:?}; it accepts: {}",
                    expected_files()
                ),
            ));
        }
    }
    for name in inputs.options.keys() {
        if !spec.options.iter().any(|o| o.name == name) {
            return Err(error(
                "UNKNOWN_OPTION",
                format!(
                    "{profile} has no option named {name:?}; it accepts: {}",
                    expected_options()
                ),
            ));
        }
    }
    if let Some(missing) = spec
        .files
        .iter()
        .find(|f| f.required && !inputs.has_file(f.name))
    {
        return Err(error(
            "MISSING_FILE",
            format!("{profile} requires file {:?}", missing.name),
        ));
    }
    if let Some(missing) = spec
        .options
        .iter()
        .find(|o| o.required && !inputs.options.contains_key(o.name))
    {
        return Err(error(
            "MISSING_OPTION",
            format!("{profile} requires option {:?}", missing.name),
        ));
    }
    let (data, mut sources) = adapter.read(inputs)?;
    let Some(object) = sources.as_object_mut() else {
        return Err(error(
            "ADAPTER_CONTRACT",
            format!("{profile} must return sources as a JSON object"),
        ));
    };
    if object.contains_key(SUPPLIED) {
        return Err(error(
            "ADAPTER_CONTRACT",
            format!("{profile} stated sources.supplied, which records the caller's sources"),
        ));
    }
    match object.get("profile") {
        None => {
            object.insert("profile".into(), json!(profile));
        }
        Some(Value::String(stated)) if stated == profile => {}
        Some(_) => {
            return Err(error(
                "ADAPTER_CONTRACT",
                format!("{profile} stated a different sources.profile"),
            ))
        }
    }
    if !inputs.contents.is_empty() {
        if object.contains_key("files") {
            return Err(error(
                "ADAPTER_CONTRACT",
                format!("{profile} stated sources.files, which records inline files"),
            ));
        }
        object.insert("files".into(), Value::Array(inputs.inline_sources()));
    }
    Ok((data, sources))
}

/// Read a local file, refusing anything over the 2 MiB input cap.
pub fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((MAX_REQUEST_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(error("INPUT_LIMIT", "Input file exceeds 2 MiB"));
    }
    Ok(bytes)
}

/// Canonical GL rows: `account_code,account_name,category,amount` with
/// decimal-string money.
pub fn csv_lines(bytes: &[u8]) -> Result<Vec<Line>> {
    csv::Reader::from_reader(bytes)
        .deserialize()
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| {
            error(
                "INVALID_INPUT",
                "CSV requires account_code, account_name, category, and decimal amount",
            )
        })
}

const GL_HEADER: &str = "account_code,account_name,category,amount";
const REFUSES_ALWAYS: [&str; 7] = [
    "UNKNOWN_FILE, UNKNOWN_OPTION, MISSING_FILE, MISSING_OPTION: a file or option this spec does not name, or a required one absent",
    "INPUT_LIMIT: any file over 2 MiB, inline files over 2 MiB in total, or more than 100000 GL rows",
    "INVALID_INPUT: money that is not a decimal string with at most two places, unknown fields, or malformed JSON",
    "INVALID_INPUT (at import): not USD, not positive_costs, a property outside 1-120 characters or with leading or trailing whitespace, a period that is not YYYY-MM, a unit count outside 1..=10000000, or an empty account code or category",
    "SNAPSHOT_MISMATCH (at import): a snapshot dated outside the period, or unit counts that do not sum to the unit count",
    "ACCOUNT_MAPPING_CONFLICT (at import): conflicting categories for one account code",
    "MONEY_OVERFLOW (at import): any total outside the checked signed-cent range",
];

/// `canonical.csv/1`: canonical GL CSVs for actuals and budgets, an optional
/// snapshot JSON object, and the property, period, and unit count as options.
pub struct CanonicalCsv;

impl SourceAdapter for CanonicalCsv {
    fn profile(&self) -> &'static str {
        CANONICAL_CSV
    }
    fn spec(&self) -> AdapterSpec {
        AdapterSpec {
            profile: CANONICAL_CSV,
            summary: "Canonical GL CSVs (decimal-string money) plus an optional snapshot JSON object; vendor exports must already be mapped to this layout.",
            files: vec![
                FileSpec {
                    name: "actuals",
                    required: true,
                    description: "Actual GL rows for the period",
                    shape: GL_HEADER,
                },
                FileSpec {
                    name: "budgets",
                    required: true,
                    description: "Budget GL rows for the period",
                    shape: GL_HEADER,
                },
                FileSpec {
                    name: "snapshot",
                    required: false,
                    description: "Occupancy and rent snapshot inside the period",
                    shape: "JSON object: as_of_date, occupied_units, vacant_units, down_units, market_rent_total, in_place_rent_total, delinquent_amount, prepaid_amount, concessions_amount",
                },
            ],
            options: vec![
                OptionSpec {
                    name: "property",
                    required: true,
                    description: "Property name, 1-120 characters, no leading or trailing whitespace",
                },
                OptionSpec {
                    name: "period",
                    required: true,
                    description: "Period as YYYY-MM",
                },
                OptionSpec {
                    name: "units",
                    required: true,
                    description: "Property unit count, a whole number",
                },
            ],
            refuses: [
                "INVALID_INPUT: a CSV without account_code, account_name, category, and decimal amount columns",
                "INVALID_INPUT: units that is not a whole number",
            ]
            .into_iter()
            .chain(REFUSES_ALWAYS)
            .collect(),
        }
    }
    fn read(&self, inputs: &AdapterInputs) -> Result<(Dataset, Value)> {
        let a = inputs.require_file("actuals")?;
        let b = inputs.require_file("budgets")?;
        let snap = inputs.read_file("snapshot")?;
        let property = inputs.require_option("property")?.to_string();
        let period = inputs.require_option("period")?.to_string();
        let unit_count: i64 = inputs
            .require_option("units")?
            .parse()
            .map_err(|_| error("INVALID_INPUT", "Option units must be a whole number"))?;
        let data = Dataset {
            property,
            period,
            currency: "USD".into(),
            expense_convention: "positive_costs".into(),
            unit_count,
            actuals: csv_lines(&a)?,
            budgets: csv_lines(&b)?,
            snapshot: snap
                .as_ref()
                .map(|v| serde_json::from_slice(v))
                .transpose()?,
        };
        let sources = json!({
            "profile": CANONICAL_CSV,
            "actuals_sha256": digest(&a),
            "budgets_sha256": digest(&b),
            "snapshot_sha256": snap.as_ref().map(|v| digest(v)),
        });
        Ok((data, sources))
    }
}

/// `canonical.json/1`: one canonical period dataset as JSON.
pub struct CanonicalJson;

impl SourceAdapter for CanonicalJson {
    fn profile(&self) -> &'static str {
        CANONICAL_JSON
    }
    fn spec(&self) -> AdapterSpec {
        AdapterSpec {
            profile: CANONICAL_JSON,
            summary: "One canonical period dataset as JSON, the same shape as import_period's dataset.",
            files: vec![FileSpec {
                name: "input",
                required: true,
                description: "Canonical period dataset",
                shape: "JSON object: property, period, currency, expense_convention, unit_count, actuals, budgets, snapshot",
            }],
            options: Vec::new(),
            refuses: REFUSES_ALWAYS.to_vec(),
        }
    }
    fn read(&self, inputs: &AdapterInputs) -> Result<(Dataset, Value)> {
        let bytes = inputs.require_file("input")?;
        let data: Dataset = serde_json::from_slice(&bytes)?;
        let sources = json!({
            "profile": CANONICAL_JSON,
            "canonical_json_sha256": digest(&bytes),
        });
        Ok((data, sources))
    }
}
