//! Dev preview: render a real discovered RPCOE/BDDRE report through the TUI markdown
//! styler and print the resulting layout as plain text (verifies table alignment,
//! heading spacing, etc. without a TTY). Run: `cargo run --example preview_report`.

fn dump(label: &str, md: &str, width: u16) {
    println!("\n========== {label} (rendered at width {width}) ==========\n");
    for line in boxscore::tui::markdown::render_markdown(md, width) {
        let s: String = line.spans.iter().map(|sp| sp.content.as_ref()).collect();
        println!("{s}");
    }
}

fn main() {
    let reports = boxscore::tui::reports::discover_weekly_reports();
    println!("discovered {} weekly reports", reports.len());
    for kind in ["RPCOE", "BDDRE"] {
        if let Some(r) = reports.iter().find(|r| r.kind == kind) {
            match r.load() {
                Ok(md) => dump(&r.label(), &md, 100),
                Err(e) => println!("failed to load {}: {e}", r.label()),
            }
        } else {
            println!("no {kind} report found");
        }
    }
}
