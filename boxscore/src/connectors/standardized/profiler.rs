use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::source_registry::{expected_standardized_files, PropertyLane};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CsvProfile {
    pub path: PathBuf,
    pub exists: bool,
    pub headers: Vec<String>,
    pub row_count: usize,
    pub privacy_level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaneProfile {
    pub lane_key: String,
    pub display_name: String,
    pub csv_profiles: Vec<CsvProfile>,
}

pub fn profile_csv(path: &Path, privacy_level: &str) -> CsvProfile {
    if !path.exists() {
        return CsvProfile {
            path: path.to_path_buf(),
            exists: false,
            headers: Vec::new(),
            row_count: 0,
            privacy_level: privacy_level.to_string(),
        };
    }

    match csv::Reader::from_path(path) {
        Ok(mut reader) => {
            let headers = reader
                .headers()
                .map(|headers| headers.iter().map(str::to_string).collect())
                .unwrap_or_default();
            let row_count = reader.records().filter(Result::is_ok).count();
            CsvProfile {
                path: path.to_path_buf(),
                exists: true,
                headers,
                row_count,
                privacy_level: privacy_level.to_string(),
            }
        }
        Err(_) => CsvProfile {
            path: path.to_path_buf(),
            exists: true,
            headers: Vec::new(),
            row_count: 0,
            privacy_level: privacy_level.to_string(),
        },
    }
}

pub fn profile_lane(lane: &PropertyLane) -> LaneProfile {
    let csv_profiles = expected_standardized_files()
        .into_iter()
        .map(|file_name| profile_csv(&lane.standardized_path.join(file_name), "medium"))
        .collect();

    LaneProfile {
        lane_key: lane.property_key.clone(),
        display_name: lane.display_name.clone(),
        csv_profiles,
    }
}
