//! START_HERE.md and the `start_here` MCP prompt must offer the same three
//! questions. The doc marks them with a `start-here-questions` HTML comment
//! followed by a fenced block holding one question per line; this test reads
//! the file at test time and compares that block with
//! `mcp::START_HERE_QUESTIONS`, which the prompt text is built from.

use boxscore::exact::mcp::{start_here_text, START_HERE_QUESTIONS};
use std::path::Path;

const MARKER: &str = "<!-- start-here-questions";

/// The non-blank lines of the first fenced block after the marker.
fn questions_in(doc: &str) -> Vec<String> {
    let mut lines = doc.lines().skip_while(|l| !l.starts_with(MARKER));
    assert!(lines.next().is_some(), "no {MARKER} marker");
    let opening = lines
        .by_ref()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default();
    assert!(
        opening.starts_with("```"),
        "the marker must be followed by a fenced block, found {opening:?}"
    );
    lines
        .take_while(|l| !l.starts_with("```"))
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

fn start_here_doc() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../START_HERE.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn start_here_doc_and_prompt_offer_the_same_three_questions() {
    let doc = start_here_doc();
    assert_eq!(
        doc.matches(MARKER).count(),
        1,
        "exactly one {MARKER} marker"
    );
    assert_eq!(questions_in(&doc), START_HERE_QUESTIONS);
    let prompt = start_here_text();
    for question in START_HERE_QUESTIONS {
        assert!(prompt.contains(question), "prompt is missing {question:?}");
    }
    // The doc tells the reader the prompt exists.
    assert!(doc.contains("`start_here`"));
}

#[test]
fn the_parser_notices_a_changed_question() {
    let doc = format!(
        "intro\n{MARKER}: note -->\n```text\nWhat is loaded?\nSomething else?\n{}\n```\n",
        START_HERE_QUESTIONS[2]
    );
    assert_ne!(questions_in(&doc), START_HERE_QUESTIONS);
}
