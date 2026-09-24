//! In-crate markdown styler for Boxscore reader panes.
//!
//! Converts a markdown string (headings, bold key-values, horizontal rules,
//! bullets, and pipe tables) into styled [`ratatui::text::Line`]s that match
//! the Boxscore TUI theme: ACCENT cyan for headings, bold/dim per convention.
//!
//! This module is rendering-only; no app wiring.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

// ── Theme constants (mirrored from tui/ui.rs) ──────────────────────────────
const ACCENT: Color = Color::Cyan;

// ── Public API ─────────────────────────────────────────────────────────────

/// Render report markdown into styled lines for a reader pane `width` columns wide.
///
/// Supports: headings (#/##/###/####), inline **bold**, horizontal rules
/// (--- / ***), bullet lists (- / *), and pipe tables with column alignment
/// detection (numeric cells are right-aligned, others left-aligned).
pub fn render_markdown(source: &str, width: u16) -> Vec<Line<'static>> {
    let lines: Vec<&str> = source.lines().collect();
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let raw = lines[i];

        // ── Blank line ─────────────────────────────────────────────────────
        if raw.trim().is_empty() {
            out.push(Line::from(""));
            i += 1;
            continue;
        }

        // ── Horizontal rule ────────────────────────────────────────────────
        if is_hr(raw) {
            let rule = "─".repeat(width as usize);
            out.push(Line::from(Span::styled(rule, Style::new().dim())));
            i += 1;
            continue;
        }

        // ── Headings ───────────────────────────────────────────────────────
        if raw.starts_with('#') {
            let level = raw.chars().take_while(|c| *c == '#').count();
            let text = raw[level..].trim().to_string();
            let style = match level {
                1 | 2 => Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                3 => Style::new().add_modifier(Modifier::BOLD),
                _ => Style::new()
                    .add_modifier(Modifier::BOLD)
                    .add_modifier(Modifier::DIM),
            };
            out.push(Line::from(Span::styled(text, style)));
            // Breathing room after every heading
            out.push(Line::from(""));
            i += 1;
            continue;
        }

        // ── Pipe table ─────────────────────────────────────────────────────
        // Collect a run of lines that start with '|'
        if raw.trim_start().starts_with('|') {
            let start = i;
            while i < lines.len() && lines[i].trim_start().starts_with('|') {
                i += 1;
            }
            let table_lines = &lines[start..i];
            let rendered = render_table(table_lines, width);
            out.extend(rendered);
            continue;
        }

        // ── Bullet ─────────────────────────────────────────────────────────
        if raw.starts_with("- ") || raw.starts_with("* ") {
            let text = &raw[2..];
            let mut spans: Vec<Span<'static>> = vec![Span::styled("  • ", Style::new().fg(ACCENT))];
            spans.extend(parse_bold(text));
            out.push(Line::from(spans));
            i += 1;
            continue;
        }

        // ── Plain line (with inline bold) ──────────────────────────────────
        out.push(Line::from(parse_bold(raw)));
        i += 1;
    }

    out
}

// ── Inline bold parser ─────────────────────────────────────────────────────

/// Split a string on `**` markers and return a vec of Spans, toggling bold.
/// All returned Spans own their strings (`'static` via `.to_string()`).
fn parse_bold(input: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = input;
    let mut bold = false;

    while let Some(pos) = rest.find("**") {
        let before = &rest[..pos];
        if !before.is_empty() {
            let style = if bold {
                Style::new().add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            };
            spans.push(Span::styled(before.to_string(), style));
        }
        bold = !bold;
        rest = &rest[pos + 2..];
    }

    // Remainder after the last `**` (or the whole string if no `**`)
    if !rest.is_empty() {
        let style = if bold {
            Style::new().add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        spans.push(Span::styled(rest.to_string(), style));
    }

    if spans.is_empty() {
        spans.push(Span::raw(String::new()));
    }

    spans
}

// ── Horizontal rule detection ──────────────────────────────────────────────

fn is_hr(line: &str) -> bool {
    let t = line.trim();
    if t.len() < 3 {
        return false;
    }
    let all_dash = t.chars().all(|c| c == '-');
    let all_star = t.chars().all(|c| c == '*');
    all_dash || all_star
}

// ── Table separator detection ──────────────────────────────────────────────

fn is_table_separator(line: &str) -> bool {
    // A line like |---|---| or |:---:|---| — only |, -, :, spaces after trim
    let t = line.trim();
    if !t.starts_with('|') {
        return false;
    }
    t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

// ── Table renderer ─────────────────────────────────────────────────────────

/// Split a pipe-table row into trimmed cell strings (drops empty leading/trailing).
fn split_row(line: &str) -> Vec<String> {
    let t = line.trim();
    // Strip leading/trailing '|' then split
    let inner = t.strip_prefix('|').unwrap_or(t);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    inner.split('|').map(|c| c.trim().to_string()).collect()
}

/// Decide if a column should be right-aligned by checking the majority of its
/// data cells: a cell is "numeric" if, after stripping `$`, `,`, `%`, `+`,
/// `-`, `.`, `(`, `)`, and spaces, it consists solely of ASCII digits.
fn is_numeric_cell(cell: &str) -> bool {
    let stripped: String = cell
        .chars()
        .filter(|c| !matches!(c, '$' | ',' | '%' | '+' | '-' | '.' | '(' | ')' | ' '))
        .collect();
    !stripped.is_empty() && stripped.chars().all(|c| c.is_ascii_digit())
}

fn render_table(raw_lines: &[&str], width: u16) -> Vec<Line<'static>> {
    // Separate header, separator, and data rows (gracefully)
    let mut header_cells: Vec<String> = Vec::new();
    let mut data_rows: Vec<Vec<String>> = Vec::new();
    let mut found_header = false;
    let mut found_sep = false;

    for &line in raw_lines {
        if is_table_separator(line) {
            found_sep = true;
            continue;
        }
        let cells = split_row(line);
        if !found_header {
            header_cells = cells;
            found_header = true;
        } else {
            // Only accept as data if separator was already seen (or we treat
            // line 2+ as data if there is no separator — degrade gracefully).
            let _ = found_sep; // we skip separator regardless
            data_rows.push(cells);
        }
    }

    if !found_header {
        // Nothing to render
        return raw_lines
            .iter()
            .map(|l| Line::from(l.to_string()))
            .collect();
    }

    let ncols = header_cells.len();
    if ncols == 0 {
        return raw_lines
            .iter()
            .map(|l| Line::from(l.to_string()))
            .collect();
    }

    // Normalize data rows to ncols (pad/truncate)
    let data_rows: Vec<Vec<String>> = data_rows
        .into_iter()
        .map(|mut row| {
            row.resize(ncols, String::new());
            row.truncate(ncols);
            row
        })
        .collect();

    // ── Compute natural column widths ──────────────────────────────────────
    let mut col_widths: Vec<usize> = header_cells.iter().map(|c| c.len()).collect();
    for row in &data_rows {
        for (j, cell) in row.iter().enumerate() {
            if j < ncols {
                col_widths[j] = col_widths[j].max(cell.len());
            }
        }
    }

    // ── Constrain to available width ───────────────────────────────────────
    // 2-col indent + (ncols-1) * 2 separator spaces
    let indent: usize = 2;
    let separators: usize = if ncols > 1 { (ncols - 1) * 2 } else { 0 };
    let available = (width as usize).saturating_sub(indent + separators);

    // Proportionally shrink if needed; minimum col width = 3
    let total_natural: usize = col_widths.iter().sum();
    if total_natural > available {
        for w in col_widths.iter_mut() {
            let shrunk = (*w * available / total_natural.max(1)).max(3);
            *w = shrunk;
        }
    }

    // ── Determine per-column alignment from majority of data cells ─────────
    let right_align: Vec<bool> = (0..ncols)
        .map(|j| {
            let numeric_count = data_rows
                .iter()
                .filter(|row| is_numeric_cell(&row[j]))
                .count();
            let total = data_rows.len();
            total > 0 && numeric_count * 2 >= total
        })
        .collect();

    // ── Helper: pad/align/ellipsize a cell to col_width ────────────────────
    let fit_cell = |cell: &str, col_w: usize, right: bool| -> String {
        let display = if cell.len() > col_w {
            // Ellipsize: keep col_w-1 chars + ellipsis
            let truncated: String = cell.chars().take(col_w.saturating_sub(1)).collect();
            format!("{truncated}…")
        } else {
            cell.to_string()
        };
        if right {
            format!("{:>width$}", display, width = col_w)
        } else {
            format!("{:<width$}", display, width = col_w)
        }
    };

    let mut out: Vec<Line<'static>> = Vec::new();
    let prefix = "  "; // 2-col indent

    // ── Header row (cells are bold) ────────────────────────────────────────
    {
        let mut spans: Vec<Span<'static>> = vec![Span::raw(prefix.to_string())];
        for (j, cell) in header_cells.iter().enumerate() {
            let fitted = fit_cell(cell, col_widths[j], right_align[j]);
            spans.push(Span::styled(
                fitted,
                Style::new().add_modifier(Modifier::BOLD),
            ));
            if j + 1 < ncols {
                spans.push(Span::raw("  ".to_string()));
            }
        }
        out.push(Line::from(spans));
    }

    // ── Dim rule under header ──────────────────────────────────────────────
    {
        let table_w: usize = col_widths.iter().sum::<usize>() + separators;
        let rule = format!("{}{}", prefix, "─".repeat(table_w));
        out.push(Line::from(Span::styled(rule, Style::new().dim())));
    }

    // ── Data rows ──────────────────────────────────────────────────────────
    for row in &data_rows {
        let mut spans: Vec<Span<'static>> = vec![Span::raw(prefix.to_string())];
        for (j, cell) in row.iter().enumerate() {
            // For cells with inline bold markers: parse bold, then pad around
            // the parsed spans. For plain cells: fit (align + ellipsize) directly.
            if cell.contains("**") {
                let bold_spans = parse_bold(cell);
                let text_len: usize = cell.replace("**", "").len();
                let col_w = col_widths[j];
                let padding = col_w.saturating_sub(text_len);
                let padded_spans: Vec<Span<'static>> = if right_align[j] {
                    let mut s = vec![Span::raw(" ".repeat(padding))];
                    s.extend(bold_spans);
                    s
                } else {
                    let mut s = bold_spans;
                    s.push(Span::raw(" ".repeat(padding)));
                    s
                };
                spans.extend(padded_spans);
            } else {
                let fitted = fit_cell(cell, col_widths[j], right_align[j]);
                spans.push(Span::raw(fitted));
            }
            if j + 1 < ncols {
                spans.push(Span::raw("  ".to_string()));
            }
        }
        out.push(Line::from(spans));
    }

    out
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Heading ────────────────────────────────────────────────────────────
    #[test]
    fn h1_heading_bold_accent() {
        let lines = render_markdown("# Title", 80);
        // First line is the heading, second is blank breathing room
        assert!(!lines.is_empty());
        let first = &lines[0];
        assert!(!first.spans.is_empty());
        let span = &first.spans[0];
        assert_eq!(span.content, "Title");
        // Must be BOLD
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
        // Must be ACCENT (Cyan)
        assert_eq!(span.style.fg, Some(ACCENT));
    }

    // ── Inline bold parsing ────────────────────────────────────────────────
    #[test]
    fn inline_bold_key_value() {
        let lines = render_markdown("**Property:** maplewood", 80);
        assert!(!lines.is_empty());
        let first = &lines[0];
        // Expect at least two spans: bold "Property:" and normal " maplewood"
        assert!(
            first.spans.len() >= 2,
            "Expected multiple spans, got {:?}",
            first.spans
        );
        // First span should be bold
        let bold_span = &first.spans[0];
        assert!(bold_span.style.add_modifier.contains(Modifier::BOLD));
        assert!(
            bold_span.content.contains("Property:"),
            "Bold span should contain 'Property:', got '{}'",
            bold_span.content
        );
        // Second span should NOT be bold
        let normal_span = &first.spans[1];
        assert!(!normal_span.style.add_modifier.contains(Modifier::BOLD));
        assert!(
            normal_span.content.contains("maplewood"),
            "Normal span should contain 'maplewood', got '{}'",
            normal_span.content
        );
    }

    // ── Pipe table ─────────────────────────────────────────────────────────
    #[test]
    fn pipe_table_rendering() {
        let md = "| Metric | Value |\n|--------|-------|\n| Leases expiring | 106 |\n| Underpriced | 65 |";
        let lines = render_markdown(md, 80);

        // Should NOT have a line containing |---|
        for line in &lines {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                !text.contains("|---"),
                "Separator row should not appear in output: '{text}'"
            );
        }

        // Should have at least 4 lines: header + rule + 2 data rows
        assert!(
            lines.len() >= 4,
            "Expected ≥4 lines for table, got {}",
            lines.len()
        );

        // Header row: cells must be bold
        let header_line = &lines[0];
        let header_bold = header_line
            .spans
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::BOLD));
        assert!(header_bold, "Header row must contain bold spans");

        // Data rows for "106" and "65" (right-aligned) — check they are present
        // and "106" appears in the output text (right-aligned within its column)
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("|");
        assert!(
            all_text.contains("106"),
            "Output must contain data cell '106'"
        );
        assert!(
            all_text.contains("65"),
            "Output must contain data cell '65'"
        );

        // Right-alignment check: in the Value column (col index 1), numeric cells
        // should be right-padded — i.e. the cell string for "106" should have
        // leading spaces before digits.
        // Find the data row containing "106"
        let row_106 = lines.iter().find(|l| {
            let t: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            t.contains("106")
        });
        assert!(row_106.is_some(), "A line with '106' must exist");
        let row_text: String = row_106
            .unwrap()
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        // The value column cell for "106" is right-aligned, so "106" should NOT
        // be immediately preceded by non-space (i.e. within the cell the number
        // has leading spaces).  We check that there is at least one space before
        // the "106" substring within the cell portion of the row.
        // After the indent + Metric column + separator, the value cell is right-padded.
        let idx_106 = row_text.find("106").expect("'106' must be in row");
        if idx_106 > 0 {
            let preceding = row_text.as_bytes()[idx_106 - 1];
            assert_eq!(
                preceding, b' ',
                "Right-aligned '106' should have leading space"
            );
        }
    }

    // ── Horizontal rule ────────────────────────────────────────────────────
    #[test]
    fn horizontal_rule() {
        let lines = render_markdown("---", 40);
        assert!(!lines.is_empty());
        let first = &lines[0];
        let text: String = first.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.chars().all(|c| c == '─'),
            "HR should be all ─, got '{text}'"
        );
        assert_eq!(text.chars().count(), 40, "HR should span full width");
        // Should be dim
        let is_dim = first
            .spans
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::DIM));
        assert!(is_dim, "HR should be dim-styled");
    }

    // ── No panic on malformed table ────────────────────────────────────────
    #[test]
    fn malformed_table_no_panic() {
        let md = "| only one cell no close";
        let lines = render_markdown(md, 60);
        // Should not panic and should produce at least one line
        assert!(!lines.is_empty());
    }

    // ── Bullet ─────────────────────────────────────────────────────────────
    #[test]
    fn bullet_renders_accent_dot() {
        let lines = render_markdown("- Hello world", 80);
        assert!(!lines.is_empty());
        let first = &lines[0];
        let first_span = &first.spans[0];
        assert!(
            first_span.content.contains('•'),
            "Bullet should contain •, got '{}'",
            first_span.content
        );
        assert_eq!(
            first_span.style.fg,
            Some(ACCENT),
            "Bullet • should be ACCENT colored"
        );
    }

    // ── Reasonable line count for multi-element document ──────────────────
    #[test]
    fn reasonable_line_count() {
        let md = "# Report\n\n**Key:** value\n\n---\n\n- Item 1\n- Item 2";
        let lines = render_markdown(md, 80);
        // heading + blank + blank + kv + blank + hr + blank + bullet + bullet = 9
        assert!(lines.len() >= 7, "Expected ≥7 lines, got {}", lines.len());
    }
}
