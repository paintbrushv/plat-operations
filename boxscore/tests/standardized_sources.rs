use std::path::Path;
use std::sync::Mutex;

use boxscore::connectors::standardized::{
    profiler::profile_csv,
    source_registry::{built_in_lanes, expected_standardized_files, lane_by_key, load_lanes},
};

/// Serializes the tests that read or mutate the process-global `$BOXSCORE_LANES`
/// env var so they never race each other under the parallel test runner.
static LANES_ENV_GUARD: Mutex<()> = Mutex::new(());

#[test]
fn built_in_lanes_include_three_properties() {
    let lanes = built_in_lanes();
    let keys = lanes
        .iter()
        .map(|lane| lane.property_key.as_str())
        .collect::<Vec<_>>();

    assert_eq!(keys, vec!["maplewood", "juniper_fund", "willow-brook"]);
    assert_eq!(lanes[0].display_name, "Maplewood Commons");
    assert_eq!(lanes[1].display_name, "Juniper Fund");
    assert_eq!(lanes[2].display_name, "Willow Brook");
}

#[test]
fn expected_standardized_files_are_named() {
    let files = expected_standardized_files();

    assert!(files.contains(&"budget_comparison.csv"));
    assert!(files.contains(&"chart_of_accounts.csv"));
    assert!(files.contains(&"rent_roll.csv"));
    assert!(files.contains(&"aged_receivables.csv"));
    assert!(files.contains(&"leasing_funnel.csv"));
    assert!(files.contains(&"collections_unified.csv"));
}

#[test]
fn lane_lookup_returns_standardized_paths_for_existing_lanes() {
    let lane = lane_by_key("maplewood").expect("maplewood lane should exist");

    assert_eq!(lane.property_key, "maplewood");
    assert!(lane
        .standardized_path
        .ends_with("data/maplewood/Standardized"));
    assert!(lane.raw_data_path.ends_with("data/maplewood/Data"));
    assert!(lane.primary_property_ids.contains(&"p101".to_string()));
}

#[test]
fn profile_csv_counts_rows_without_exposing_records() {
    let temp_dir = tempfile::tempdir().expect("temp dir should be created");
    let file = temp_dir.path().join("sample.csv");
    std::fs::write(&file, "name,balance\nResident A,100\nResident B,200\n")
        .expect("fixture should be written");

    let profile = profile_csv(&file, "sample");

    assert!(profile.exists);
    assert_eq!(profile.row_count, 2);
    assert_eq!(profile.headers, vec!["name", "balance"]);
    assert_eq!(profile.privacy_level, "sample");
}

#[test]
fn profile_csv_marks_missing_files_without_panicking() {
    let profile = profile_csv(Path::new("missing/not-here.csv"), "medium");

    assert!(!profile.exists);
    assert_eq!(profile.row_count, 0);
    assert!(profile.headers.is_empty());
    assert_eq!(profile.privacy_level, "medium");
}

#[test]
fn load_lanes_falls_back_to_built_ins_when_no_config_present() {
    // With no `$BOXSCORE_LANES` and no on-disk config, `load_lanes()` must be
    // byte-for-byte identical to the built-in set — the backward-compatibility
    // guarantee the existing 3-lane harness depends on.
    //
    // (This relies on the repo not shipping a checked-in lanes.json; the
    // onboarding flow writes to a gitignored path.)
    let _guard = LANES_ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    assert!(std::env::var("BOXSCORE_LANES").is_err());
    assert_eq!(load_lanes(), built_in_lanes());
}

#[test]
fn load_lanes_merges_a_configured_fourth_lane_and_keeps_built_ins() {
    // Point BOXSCORE_LANES at a temp config that adds a 4th lane. The built-ins
    // must remain (and keep their order) while the configured lane appends.
    let _guard = LANES_ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("temp dir");
    let config_path = dir.path().join("lanes.json");
    std::fs::write(
        &config_path,
        r#"[
            {
                "property_key": "bagholder-flats",
                "display_name": "Bagholder Flats",
                "root_path": "data/Bagholder_Flats",
                "standardized_path": "",
                "raw_data_path": "",
                "primary_property_ids": ["s999"],
                "unit_count_hint": 88
            }
        ]"#,
    )
    .expect("write temp config");

    // SAFETY: this is the only test that mutates BOXSCORE_LANES; it sets and
    // unsets within the same test body, and no other test reads this var.
    std::env::set_var("BOXSCORE_LANES", &config_path);
    let lanes = load_lanes();
    std::env::remove_var("BOXSCORE_LANES");

    let keys: Vec<&str> = lanes.iter().map(|l| l.property_key.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            "maplewood",
            "juniper_fund",
            "willow-brook",
            "bagholder-flats"
        ],
        "built-ins preserved in order, configured lane appended"
    );

    let extra = lanes
        .iter()
        .find(|l| l.property_key == "bagholder-flats")
        .expect("configured lane present");
    assert_eq!(extra.display_name, "Bagholder Flats");
    assert_eq!(extra.unit_count_hint, Some(88));
    // Relative root resolved against repo root; empty subpaths derived.
    assert!(extra.root_path.is_absolute());
    assert!(extra
        .standardized_path
        .ends_with("data/Bagholder_Flats/Standardized"));
    assert!(extra.raw_data_path.ends_with("data/Bagholder_Flats/Data"));
}

#[test]
fn load_lanes_lets_config_override_a_built_in_by_key() {
    let _guard = LANES_ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("temp dir");
    let config_path = dir.path().join("lanes.json");
    std::fs::write(
        &config_path,
        r#"[
            {
                "property_key": "maplewood",
                "display_name": "maplewood (overridden)",
                "root_path": "data/maplewood",
                "standardized_path": "",
                "raw_data_path": "",
                "primary_property_ids": ["p101"],
                "unit_count_hint": 500
            }
        ]"#,
    )
    .expect("write temp config");

    std::env::set_var("BOXSCORE_LANES", &config_path);
    let lanes = load_lanes();
    std::env::remove_var("BOXSCORE_LANES");

    // Still exactly 3 lanes (override-in-place, not append) and maplewood reflects
    // the configured display name / unit hint.
    let keys: Vec<&str> = lanes.iter().map(|l| l.property_key.as_str()).collect();
    assert_eq!(keys, vec!["maplewood", "juniper_fund", "willow-brook"]);
    let maplewood = &lanes[0];
    assert_eq!(maplewood.display_name, "maplewood (overridden)");
    assert_eq!(maplewood.unit_count_hint, Some(500));
}
