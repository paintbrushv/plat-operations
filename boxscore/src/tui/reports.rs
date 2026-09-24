//! Weekly-report discovery for the TUI Reports screen.
//!
//! Scans each source-registry lane's `Reports/` directory for RPCOE and BDDRE
//! weekly markdown files, parses the date from the filename, and returns a
//! sorted list ready for the list pane.

use std::path::{Path, PathBuf};

use crate::connectors::standardized::source_registry::load_lanes;

/// A discovered weekly markdown report file.
#[derive(Debug, Clone)]
pub struct WeeklyReport {
    /// The lane's `display_name` (e.g. "Maplewood Commons").
    pub property: String,
    /// "RPCOE" or "BDDRE".
    pub kind: String,
    /// `YYYY-MM-DD` parsed from the filename, or the filename stem if no date found.
    pub date: String,
    /// Absolute path to the `.md` file.
    pub path: PathBuf,
}

impl WeeklyReport {
    /// One-line label for the TUI list, e.g. `"maplewood · RPCOE · 2026-06-09"`.
    pub fn label(&self) -> String {
        format!("{} · {} · {}", self.property, self.kind, self.date)
    }

    /// Read the full markdown content from disk.
    pub fn load(&self) -> anyhow::Result<String> {
        Ok(std::fs::read_to_string(&self.path)?)
    }
}

/// Extract a `YYYY-MM-DD` substring from a filename using char-level scanning.
///
/// Looks for any 10-character window that matches `NNNN-NN-NN` where every
/// non-hyphen character is an ASCII digit.  Returns `None` if no match is found.
fn extract_date(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    if bytes.len() < 10 {
        return None;
    }
    for i in 0..=(bytes.len() - 10) {
        let window = &bytes[i..i + 10];
        // Pattern: NNNN-NN-NN  (indices 4 and 7 are hyphens)
        if window[4] == b'-' && window[7] == b'-' {
            let digit_positions = [0, 1, 2, 3, 5, 6, 8, 9];
            if digit_positions.iter().all(|&j| window[j].is_ascii_digit()) {
                // SAFETY: all bytes are ASCII digits or '-'.
                return Some(String::from_utf8_lossy(window).into_owned());
            }
        }
    }
    None
}

/// Scan a single `Reports/` directory for RPCOE and BDDRE weekly markdown files.
///
/// Silently skips the directory if it does not exist or cannot be read.
pub fn discover_in_dir(property: &str, reports_dir: &Path) -> Vec<WeeklyReport> {
    let read_dir = match std::fs::read_dir(reports_dir) {
        Ok(rd) => rd,
        Err(_) => return Vec::new(),
    };

    let mut reports = Vec::new();
    for entry in read_dir.flatten() {
        let file_name = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };

        // Must end in `.md`.
        if !file_name.ends_with(".md") {
            continue;
        }

        // Determine kind from prefix.
        let kind = if file_name.starts_with("RPCOE_Weekly_Report_") {
            "RPCOE"
        } else if file_name.starts_with("BDDRE_Weekly_Report_") {
            "BDDRE"
        } else {
            continue;
        };

        let date = extract_date(&file_name).unwrap_or_else(|| {
            // Fall back to the stem (strip `.md`).
            file_name
                .strip_suffix(".md")
                .unwrap_or(&file_name)
                .to_string()
        });

        reports.push(WeeklyReport {
            property: property.to_string(),
            kind: kind.to_string(),
            date,
            path: entry.path(),
        });
    }
    reports
}

/// Discover weekly reports across all source-registry lanes.
///
/// Returns results sorted by date descending, then property ascending, then
/// kind ascending.  `YYYY-MM-DD` dates compare correctly under lexical order.
pub fn discover_weekly_reports() -> Vec<WeeklyReport> {
    let mut all: Vec<WeeklyReport> = load_lanes()
        .iter()
        .flat_map(|lane| {
            let reports_dir = lane.root_path.join("Reports");
            discover_in_dir(&lane.display_name, &reports_dir)
        })
        .collect();

    all.sort_by(|a, b| {
        // Date descending (newest first), then property and kind ascending.
        b.date
            .cmp(&a.date)
            .then_with(|| a.property.cmp(&b.property))
            .then_with(|| a.kind.cmp(&b.kind))
    });

    all
}

/// Keep only reports whose filename month (YYYY-MM) matches `period`.
///
/// A report belongs to period P iff `report.date[..7] == P`.  Reports whose
/// `date` field is shorter than 7 characters (malformed fallback stems) are
/// excluded silently.
pub fn filter_by_period(reports: Vec<WeeklyReport>, period: &str) -> Vec<WeeklyReport> {
    reports
        .into_iter()
        .filter(|r| r.date.len() >= 7 && r.date[..7] == *period)
        .collect()
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write_file(dir: &std::path::Path, name: &str, contents: &str) {
        fs::write(dir.join(name), contents).unwrap();
    }

    #[test]
    fn discover_in_dir_finds_three_reports_and_excludes_decoys() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        write_file(dir, "RPCOE_Weekly_Report_2026-06-09.md", "# RPCOE June 9");
        write_file(dir, "BDDRE_Weekly_Report_2026-05-18.md", "# BDDRE May 18");
        write_file(dir, "RPCOE_Weekly_Report_2026-06-12.md", "# RPCOE June 12");
        // Decoys — should NOT be picked up.
        write_file(dir, "notes.txt", "ignore me");
        write_file(dir, "rpcoe_recommendations.csv", "also ignore");

        let mut reports = discover_in_dir("Prop", dir);
        assert_eq!(
            reports.len(),
            3,
            "expected 3 reports, got {}",
            reports.len()
        );

        // Sort for deterministic assertions.
        reports.sort_by(|a, b| a.date.cmp(&b.date).then(a.kind.cmp(&b.kind)));

        assert_eq!(reports[0].kind, "BDDRE");
        assert_eq!(reports[0].date, "2026-05-18");
        assert_eq!(reports[1].kind, "RPCOE");
        assert_eq!(reports[1].date, "2026-06-09");
        assert_eq!(reports[2].kind, "RPCOE");
        assert_eq!(reports[2].date, "2026-06-12");
    }

    #[test]
    fn newest_rpcoe_date_is_june_12() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        write_file(dir, "RPCOE_Weekly_Report_2026-06-09.md", "");
        write_file(dir, "BDDRE_Weekly_Report_2026-05-18.md", "");
        write_file(dir, "RPCOE_Weekly_Report_2026-06-12.md", "");

        let reports = discover_in_dir("Prop", dir);
        let newest_rpcoe = reports
            .iter()
            .filter(|r| r.kind == "RPCOE")
            .max_by(|a, b| a.date.cmp(&b.date))
            .expect("should have at least one RPCOE");

        assert_eq!(newest_rpcoe.date, "2026-06-12");
    }

    #[test]
    fn label_formats_correctly() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_file(dir, "RPCOE_Weekly_Report_2026-06-12.md", "content");

        let reports = discover_in_dir("Prop", dir);
        let rpcoe = reports.iter().find(|r| r.date == "2026-06-12").unwrap();
        assert_eq!(rpcoe.label(), "Prop · RPCOE · 2026-06-12");
    }

    #[test]
    fn load_returns_file_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_file(dir, "RPCOE_Weekly_Report_2026-06-09.md", "# Hello Report");

        let reports = discover_in_dir("Prop", dir);
        assert_eq!(reports.len(), 1);
        let contents = reports[0].load().unwrap();
        assert_eq!(contents, "# Hello Report");
    }

    #[test]
    fn missing_dir_returns_empty() {
        let reports = discover_in_dir("Ghost", Path::new("/tmp/nonexistent-dir-xyz-abc"));
        assert!(reports.is_empty());
    }

    #[test]
    fn extract_date_finds_date_in_filename() {
        assert_eq!(
            super::extract_date("RPCOE_Weekly_Report_2026-06-09.md"),
            Some("2026-06-09".to_string())
        );
        assert_eq!(super::extract_date("notes.txt"), None);
    }

    #[test]
    fn filter_by_period_matches_filename_month() {
        fn make(date: &str) -> WeeklyReport {
            WeeklyReport {
                property: "Prop".to_string(),
                kind: "RPCOE".to_string(),
                date: date.to_string(),
                path: std::path::PathBuf::from("x"),
            }
        }

        let reports = vec![
            make("2026-06-09"),
            make("2026-05-18"),
            make("2026-06-12"),
            make("bad"), // malformed — shorter than 7 chars
        ];

        // June matches exactly the two June reports.
        let june = super::filter_by_period(reports.clone(), "2026-06");
        assert_eq!(june.len(), 2, "expected 2 June reports, got {}", june.len());
        assert!(june.iter().all(|r| r.date.starts_with("2026-06")));

        // May returns exactly the one May report.
        let may = super::filter_by_period(reports.clone(), "2026-05");
        assert_eq!(may.len(), 1);
        assert_eq!(may[0].date, "2026-05-18");

        // July returns none.
        let july = super::filter_by_period(reports.clone(), "2026-07");
        assert!(july.is_empty(), "expected no July reports");

        // Malformed "bad" (len < 7) is never included in any period.
        let any = super::filter_by_period(vec![make("bad")], "bad");
        assert!(any.is_empty(), "malformed date must never match");
    }
}
