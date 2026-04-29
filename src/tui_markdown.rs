use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};

#[derive(Clone, Debug)]
struct ListContext {
    next_index: Option<u64>,
}

#[derive(Clone, Debug)]
struct ItemContext {
    marker: String,
    indent: String,
    first_line: bool,
}

#[derive(Clone, Debug)]
struct LinkContext {
    destination: String,
}

pub(crate) fn render_markdown_text(input: &str) -> Text<'static> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let parser = Parser::new_ext(input, options);
    let mut renderer = MarkdownRenderer::default();
    for event in parser {
        renderer.handle_event(event);
    }
    renderer.finish()
}

#[derive(Default)]
struct MarkdownRenderer {
    text: Text<'static>,
    current_line: Vec<Span<'static>>,
    inline_styles: Vec<Style>,
    list_stack: Vec<ListContext>,
    item_stack: Vec<ItemContext>,
    link_stack: Vec<LinkContext>,
    blockquote_depth: usize,
    heading_style: Option<Style>,
    in_code_block: bool,
    show_code_block_fence: bool,
    pending_blank_line: bool,
}

impl MarkdownRenderer {
    fn handle_event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) => self.push_text(&text),
            Event::Code(code) => {
                let style = self.current_style().patch(code_style());
                self.push_span(Span::styled(code.into_string(), style));
            }
            Event::SoftBreak | Event::HardBreak => self.push_line_break(),
            Event::Rule => {
                self.ensure_blank_line_between_blocks();
                self.push_spans(vec![Span::styled(
                    "───".to_string(),
                    Style::default().fg(Color::DarkGray),
                )]);
                self.push_line_break();
                self.pending_blank_line = true;
            }
            Event::Html(html) | Event::InlineHtml(html) => self.push_text(&html),
            Event::FootnoteReference(_) | Event::TaskListMarker(_) => {}
            Event::InlineMath(math) | Event::DisplayMath(math) => self.push_text(&math),
        }
    }

    fn start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.ensure_blank_line_between_blocks(),
            Tag::Heading { level, .. } => {
                self.ensure_blank_line_between_blocks();
                let style = heading_style(level as usize);
                self.heading_style = Some(style);
                self.push_span(Span::styled(
                    format!("{} ", "#".repeat(level as usize)),
                    style,
                ));
            }
            Tag::BlockQuote(_) => {
                self.ensure_blank_line_between_blocks();
                self.blockquote_depth += 1;
            }
            Tag::CodeBlock(kind) => {
                self.ensure_blank_line_between_blocks();
                let fence = match kind {
                    CodeBlockKind::Fenced(lang) => {
                        let value = lang.split_whitespace().next().unwrap_or_default();
                        if value.is_empty() {
                            Some("```".to_string())
                        } else {
                            Some(format!("```{value}"))
                        }
                    }
                    CodeBlockKind::Indented => None,
                };
                self.show_code_block_fence = fence.is_some();
                if let Some(fence) = fence {
                    self.push_spans(vec![Span::styled(
                        fence,
                        Style::default().fg(Color::DarkGray),
                    )]);
                    self.push_line_break();
                }
                self.in_code_block = true;
            }
            Tag::List(start) => self.list_stack.push(ListContext { next_index: start }),
            Tag::Item => self.start_item(),
            Tag::Emphasis => self.push_style(Style::default().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(Style::default().add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => {
                self.push_style(Style::default().add_modifier(Modifier::CROSSED_OUT))
            }
            Tag::Link { dest_url, .. } => {
                self.push_style(link_style());
                self.link_stack.push(LinkContext {
                    destination: dest_url.into_string(),
                });
            }
            Tag::Image { .. }
            | Tag::Table(_)
            | Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
            | Tag::FootnoteDefinition(_)
            | Tag::HtmlBlock
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Superscript
            | Tag::Subscript
            | Tag::MetadataBlock(_) => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush_current_line();
                self.pending_blank_line = true;
            }
            TagEnd::Heading(_) => {
                self.heading_style = None;
                self.flush_current_line();
                self.pending_blank_line = true;
            }
            TagEnd::BlockQuote(_) => {
                self.flush_current_line();
                self.blockquote_depth = self.blockquote_depth.saturating_sub(1);
                self.pending_blank_line = true;
            }
            TagEnd::CodeBlock => {
                self.flush_current_line();
                if self.in_code_block {
                    self.in_code_block = false;
                    if self.show_code_block_fence {
                        self.push_spans(vec![Span::styled(
                            "```".to_string(),
                            Style::default().fg(Color::DarkGray),
                        )]);
                        self.flush_current_line();
                    }
                    self.show_code_block_fence = false;
                }
                self.pending_blank_line = true;
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                self.pending_blank_line = true;
            }
            TagEnd::Item => {
                self.flush_current_line();
                self.item_stack.pop();
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.inline_styles.pop();
            }
            TagEnd::Link => {
                self.inline_styles.pop();
                if let Some(link) = self.link_stack.pop() {
                    self.push_span(Span::styled(
                        format!(" ({})", link.destination),
                        link_style(),
                    ));
                }
            }
            TagEnd::Image
            | TagEnd::Table
            | TagEnd::TableHead
            | TagEnd::TableRow
            | TagEnd::TableCell
            | TagEnd::FootnoteDefinition
            | TagEnd::HtmlBlock
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::MetadataBlock(_) => {}
        }
    }

    fn finish(mut self) -> Text<'static> {
        self.flush_current_line();
        self.text
    }

    fn start_item(&mut self) {
        self.flush_current_line();

        let depth = self.list_stack.len();
        let indent = " ".repeat(depth.saturating_sub(1) * 2);
        let marker = match self
            .list_stack
            .last_mut()
            .and_then(|context| context.next_index.as_mut())
        {
            Some(index) => {
                let current = *index;
                *index = index.saturating_add(1);
                format!("{indent}{current}. ")
            }
            None => format!("{indent}- "),
        };
        let item_indent = " ".repeat(marker.chars().count());

        self.item_stack.push(ItemContext {
            marker,
            indent: item_indent,
            first_line: true,
        });
    }

    fn ensure_blank_line_between_blocks(&mut self) {
        if self.pending_blank_line {
            self.flush_current_line();
            self.pending_blank_line = false;
        }
    }

    fn push_text(&mut self, text: &str) {
        let style = self.current_style();
        for (index, segment) in text.split('\n').enumerate() {
            if index > 0 {
                self.push_line_break();
            }
            if segment.is_empty() {
                continue;
            }
            self.push_span(Span::styled(segment.to_string(), style));
        }
    }

    fn push_line_break(&mut self) {
        self.flush_current_line();
    }

    fn push_style(&mut self, style: Style) {
        let next = self.current_style().patch(style);
        self.inline_styles.push(next);
    }

    fn current_style(&self) -> Style {
        let mut style = self.heading_style.unwrap_or_default();
        if self.in_code_block {
            style = style.patch(code_style());
        }
        if let Some(inline) = self.inline_styles.last().copied() {
            style = style.patch(inline);
        }
        style
    }

    fn push_spans(&mut self, spans: Vec<Span<'static>>) {
        for span in spans {
            self.push_span(span);
        }
    }

    fn push_span(&mut self, span: Span<'static>) {
        self.ensure_blank_line_between_blocks();
        if self.current_line.is_empty() {
            self.current_line.extend(self.line_prefix());
        }
        self.current_line.push(span);
        if let Some(item) = self.item_stack.last_mut() {
            item.first_line = false;
        }
    }

    fn flush_current_line(&mut self) {
        if self.current_line.is_empty() {
            return;
        }
        self.text
            .lines
            .push(Line::from(std::mem::take(&mut self.current_line)));
    }

    fn line_prefix(&self) -> Vec<Span<'static>> {
        let mut spans = Vec::new();

        for _ in 0..self.blockquote_depth {
            spans.push(Span::styled("> ", quote_style()));
        }

        for (index, item) in self.item_stack.iter().enumerate() {
            let prefix = if index + 1 == self.item_stack.len() && item.first_line {
                item.marker.clone()
            } else {
                item.indent.clone()
            };
            spans.push(Span::raw(prefix));
        }

        if self.in_code_block {
            spans.push(Span::styled(
                "    ".to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }

        spans
    }
}

fn heading_style(level: usize) -> Style {
    let base = Style::default().add_modifier(Modifier::BOLD);
    if level <= 2 {
        base
    } else {
        base.add_modifier(Modifier::ITALIC)
    }
}

fn code_style() -> Style {
    Style::default().fg(Color::Cyan)
}

fn link_style() -> Style {
    Style::default()
        .fg(Color::Blue)
        .add_modifier(Modifier::UNDERLINED)
}

fn quote_style() -> Style {
    Style::default().fg(Color::Green)
}

#[cfg(test)]
mod tests {
    use super::render_markdown_text;

    fn flatten_lines(input: &str) -> Vec<String> {
        render_markdown_text(input)
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
}
