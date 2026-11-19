use super::{render_markdown_text, render_markdown_text_spaced};

fn flatten_lines(input: &str) -> Vec<String> {
    render_markdown_text(input)
        .lines
        .into_iter()
        .map(|line| line.spans.into_iter().map(|span| span.content).collect())
        .collect()
}

fn flatten_spaced_lines(input: &str) -> Vec<String> {
    render_markdown_text_spaced(input)
        .lines
        .into_iter()
        .map(|line| line.spans.into_iter().map(|span| span.content).collect())
        .collect()
}

#[test]
fn renders_inline_styles_and_links() {
    let lines = flatten_lines("**Bold** and `code` with [docs](https://example.com)");
    assert_eq!(lines, vec!["Bold and code with docs (https://example.com)"]);
}

#[test]
fn renders_blockquote_and_lists() {
    let lines = flatten_lines("> quoted\n>\n- one\n- two");
    assert_eq!(lines, vec!["> quoted", "- one", "- two"]);
}

#[test]
fn renders_fenced_code_block() {
    let lines = flatten_lines("```rust\nfn main() {}\n```");
    assert_eq!(lines, vec!["```rust", "    fn main() {}", "```"]);
}

#[test]
fn renders_balanced_fenced_code_block_without_language() {
    let lines = flatten_lines("```\nplain\n```");
    assert_eq!(lines, vec!["```", "    plain", "```"]);
}

#[test]
fn renders_indented_code_block() {
    let lines = flatten_lines("    one\n    two");
    assert_eq!(lines, vec!["    one", "    two"]);
}

#[test]
fn spaced_renderer_keeps_one_blank_line_between_top_level_blocks() {
    let lines = flatten_spaced_lines("First\n\nSecond\n\n- one\n- two");
    assert_eq!(lines, vec!["First", "", "Second", "", "- one", "- two"]);
}

#[test]
fn spaced_renderer_keeps_one_blank_line_between_heading_and_body() {
    let lines = flatten_spaced_lines("### Title\n\nBody");
    assert_eq!(lines, vec!["### Title", "", "Body"]);
}
