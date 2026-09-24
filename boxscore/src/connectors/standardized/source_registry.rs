use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PropertyLane {
    pub property_key: String,
    pub display_name: String,
    pub root_path: PathBuf,
    pub standardized_path: PathBuf,
    pub raw_data_path: PathBuf,
    pub primary_property_ids: Vec<String>,
    pub unit_count_hint: Option<u32>,
}

pub fn built_in_lanes() -> Vec<PropertyLane> {
    vec![
        lane(
            "maplewood",
            "Maplewood Commons",
            "data/maplewood",
            &["p101"],
            Some(494),
        ),
        lane(
            "juniper_fund",
            "Juniper Fund",
            "data/juniper_fund",
            &[
                "juniper_fund",
                "juniper-fund",
                "200101",
                "200102",
                "200103",
                "200104",
            ],
            Some(172),
        ),
        lane(
            "willow-brook",
            "Willow Brook",
            "data/willow_brook",
            &["p102"],
            Some(304),
        ),
    ]
}

/// Resolve the repo root (the parent of the `boxscore/` crate directory).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("Boxscore crate should live under repo root")
        .to_path_buf()
}

/// Locate the lanes config file, honoring `$BOXSCORE_LANES`, then
/// `{repo_root}/boxscore/lanes.json`, then `{repo_root}/lanes.json`.
///
/// Returns the first path that exists, or `None` when no config is present
/// (in which case `load_lanes()` falls back to the built-in lanes).
fn lanes_config_path() -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("BOXSCORE_LANES") {
        if !env_path.trim().is_empty() {
            let path = PathBuf::from(env_path);
            return path.exists().then_some(path);
        }
    }
    let root = repo_root();
    let candidates = [
        root.join("boxscore").join("lanes.json"),
        root.join("lanes.json"),
    ];
    candidates.into_iter().find(|path| path.exists())
}

/// Make a relative `root_path` absolute against the repo root (built-in lanes
/// already resolve their paths absolutely, so only configured relative paths
/// need this). Also re-derive the standardized/raw subpaths when they were not
/// supplied explicitly in the config.
fn resolve_configured_lane(mut lane: PropertyLane) -> PropertyLane {
    if lane.root_path.is_relative() {
        lane.root_path = repo_root().join(&lane.root_path);
    }
    if lane.standardized_path.as_os_str().is_empty() {
        lane.standardized_path = lane.root_path.join("Standardized");
    } else if lane.standardized_path.is_relative() {
        lane.standardized_path = repo_root().join(&lane.standardized_path);
    }
    if lane.raw_data_path.as_os_str().is_empty() {
        lane.raw_data_path = lane.root_path.join("Data");
    } else if lane.raw_data_path.is_relative() {
        lane.raw_data_path = repo_root().join(&lane.raw_data_path);
    }
    lane
}

/// Parse a `lanes.json` file into resolved `PropertyLane`s. The JSON shape is a
/// top-level array of `PropertyLane` objects (the same struct that already
/// derives `Serialize`/`Deserialize`). Relative paths resolve against the repo
/// root; omitted `standardized_path`/`raw_data_path` default to
/// `<root>/Standardized` and `<root>/Data`.
fn parse_lanes_config(path: &Path) -> anyhow::Result<Vec<PropertyLane>> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read lanes config {}: {e}", path.display()))?;
    let lanes: Vec<PropertyLane> = serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("failed to parse lanes config {}: {e}", path.display()))?;
    Ok(lanes.into_iter().map(resolve_configured_lane).collect())
}

/// The active set of property lanes.
///
/// Backward-compatible by design: when no `lanes.json` config is present, this
/// returns exactly `built_in_lanes()`. When a config IS present, configured
/// lanes are merged onto the built-ins — a configured lane sharing a
/// `property_key` with a built-in overrides it, and new keys extend the set.
/// Built-in ordering is preserved; newly-added configured lanes append in
/// config order.
///
/// A malformed config never crashes the harness: it logs to stderr and falls
/// back to the built-ins.
pub fn load_lanes() -> Vec<PropertyLane> {
    let Some(config_path) = lanes_config_path() else {
        return built_in_lanes();
    };
    match parse_lanes_config(&config_path) {
        Ok(configured) => merge_lanes(built_in_lanes(), configured),
        Err(err) => {
            eprintln!(
                "boxscore: ignoring lanes config {} ({err}); using built-in lanes",
                config_path.display()
            );
            built_in_lanes()
        }
    }
}

/// Merge configured lanes onto the built-ins by `property_key`: a configured
/// entry overrides a built-in with the same key in place; unmatched configured
/// entries append in order.
fn merge_lanes(built_in: Vec<PropertyLane>, configured: Vec<PropertyLane>) -> Vec<PropertyLane> {
    let mut merged = built_in;
    for lane in configured {
        if let Some(slot) = merged
            .iter_mut()
            .find(|existing| existing.property_key == lane.property_key)
        {
            *slot = lane;
        } else {
            merged.push(lane);
        }
    }
    merged
}

/// Look up a lane by key across the active (config-aware) lane set, falling back
/// to the built-ins when no config is present.
pub fn lane_by_key(key: &str) -> Option<PropertyLane> {
    load_lanes()
        .into_iter()
        .find(|lane| lane.property_key == key)
}

/// Append (or override) a lane in the `lanes.json` config, creating the file if
/// it does not yet exist. Returns the path written. Used by `boxscore onboard`.
///
/// The config written is always at `{repo_root}/boxscore/lanes.json` (the
/// primary location) unless `$BOXSCORE_LANES` points elsewhere, in which case
/// that path is used. Only *configured* lanes are persisted — the built-ins
/// stay implicit via the fallback, so the file never duplicates them.
pub fn upsert_lane_config(lane: &PropertyLane) -> anyhow::Result<PathBuf> {
    let target = match std::env::var("BOXSCORE_LANES") {
        Ok(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => repo_root().join("boxscore").join("lanes.json"),
    };

    // Load existing *configured* lanes (not the merged built-ins) so we persist
    // only the explicit config set.
    let mut configured: Vec<PropertyLane> = if target.exists() {
        let raw = std::fs::read_to_string(&target)?;
        serde_json::from_str(&raw).map_err(|e| {
            anyhow::anyhow!("existing {} is not valid lanes JSON: {e}", target.display())
        })?
    } else {
        Vec::new()
    };

    if let Some(slot) = configured
        .iter_mut()
        .find(|existing| existing.property_key == lane.property_key)
    {
        *slot = lane.clone();
    } else {
        configured.push(lane.clone());
    }

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&target, serde_json::to_string_pretty(&configured)?)?;
    Ok(target)
}

pub fn expected_standardized_files() -> Vec<&'static str> {
    vec![
        "budget_comparison.csv",
        "chart_of_accounts.csv",
        "rent_roll.csv",
        "aged_receivables.csv",
        "leasing_funnel.csv",
        "collections_unified.csv",
        "lease_expirations.csv",
        "concessions.csv",
        "weekly_activity.csv",
        "turn_costs_summary.csv",
        "unit_pnl_annual.csv",
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceFileStatus {
    pub file_name: String,
    pub path: PathBuf,
    pub exists: bool,
}

pub fn inspect_lane(lane: &PropertyLane) -> Vec<SourceFileStatus> {
    expected_standardized_files()
        .into_iter()
        .map(|file_name| {
            let path = lane.standardized_path.join(file_name);
            SourceFileStatus {
                file_name: file_name.to_string(),
                exists: path.exists(),
                path,
            }
        })
        .collect()
}

fn lane(
    property_key: &str,
    display_name: &str,
    root_path: &str,
    primary_property_ids: &[&str],
    unit_count_hint: Option<u32>,
) -> PropertyLane {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("Boxscore crate should live under repo root")
        .to_path_buf();
    let root_path = repo_root.join(root_path);
    PropertyLane {
        property_key: property_key.to_string(),
        display_name: display_name.to_string(),
        standardized_path: root_path.join("Standardized"),
        raw_data_path: root_path.join("Data"),
        root_path,
        primary_property_ids: primary_property_ids
            .iter()
            .map(|id| (*id).to_string())
            .collect(),
        unit_count_hint,
    }
}
