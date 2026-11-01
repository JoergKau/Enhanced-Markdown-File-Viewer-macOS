//! Markdown → structured document model.
//!
//! # Purpose
//!
//! This module encapsulates **the entire Markdown parser** behind a small,
//! UI-independent data model. The live preview (`ui.rs`) consumes nothing but
//! [`Document`]. The PDF export renders the Markdown source through
//! `html_export.rs`, which reuses the source-level helpers defined here so both
//! paths interpret the text the same way. The parser can be tested without any
//! GUI.
//!
//! # Pipeline
//!
//! ```text
//! Markdown string
//!   └─ preprocess (comments/scripts, math, [TOC], tables, abbreviations, divs, admonitions, <details>)
//!        └─ pulldown-cmark (CommonMark + GFM flags)
//!             └─ event stream (Vec<Event<'static>>)
//!                  └─ Walker (state machine, cursor `pos`)
//!                       └─ post-process (highlight, embeds, autolinks, callouts)
//!                            └─ Document { metadata, title, blocks, footnotes }
//! ```
//!
//! The events are materialized first (`Event::into_static`) so the [`Walker`]
//! can look ahead/behind by index. For typical document sizes (a few MB at
//! most) this is cheap and makes the code much simpler than a pure
//! iterator-based parser.
//!
//! # Extension layers
//!
//! * [`preprocess`] rewrites constructs pulldown-cmark cannot represent
//!   (`$$…$$` with lone `=` lines, `<details>` across blank lines, `[TOC]`,
//!   fenced divs, admonitions, grid tables) and removes unsafe HTML
//!   comments/scripts.
//! * The [`Walker`] builds the structural model (paragraphs, lists, tables,
//!   footnotes, definition lists, Mermaid, details).
//! * [`postprocess_blocks`] adds text-level extensions that have no pulldown
//!   event (highlight, sub/superscript intraword forms, wiki embeds, bare
//!   URLs, Obsidian comments and callout titles).
//!
//! # Invariants (important when extending!)
//!
//! * **Progress guarantee:** every loop in [`Walker`] must either advance
//!   `pos` or leave the loop on each iteration. A cursor that moves backwards
//!   causes an infinite loop with memory explosion (historically: a
//!   macOS-Jetsam OOM). The relevant places are [`Walker::blocks`] (the
//!   stray-inline branch explicitly checks progress) and
//!   [`Walker::parse_inlines`] (block starts are returned *without* moving the
//!   cursor).
//! * **End tags:** `blocks()` does **not** consume the end event of its
//!   surrounding container; the caller (e.g. `container`) does.
//!   `parse_inlines(Some(stop))` consumes the matching end tag.
//! * **Tight lists:** pulldown-cmark emits `Text` without a `Paragraph`
//!   wrapper inside tight list items, so the inline parser must also accept
//!   "bare" text events.
//!
//! # Extension guide
//!
//! To support a new **block**:
//! 1. add a variant to [`Block`],
//! 2. build it from the matching `Tag` in [`Walker::container`],
//! 3. render it in `ui::render_block` and in `html_export::render_markdown`
//!    (print-ready HTML → WeasyPrint PDF).
//!
//! To support a new **inline style**:
//! 1. add a variant to [`Inline`],
//! 2. handle it in [`Walker::parse_inlines`] (and add it to [`is_span_tag`]
//!    so it is recognized as a span),
//! 3. render it in `ui::render_inlines` and in `html_export`'s inline
//!    rendering (print-ready HTML).

use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, Event, Options, Parser, Tag, TagEnd,
};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// Text alignment of a table column (GFM `---`, `:---:`, `---:`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TableAlign {
    #[default]
    None,
    Left,
    Center,
    Right,
}

fn align_from(a: Alignment) -> TableAlign {
    match a {
        Alignment::None => TableAlign::None,
        Alignment::Left => TableAlign::Left,
        Alignment::Center => TableAlign::Center,
        Alignment::Right => TableAlign::Right,
    }
}

/// GFM / Obsidian alert type (`> [!NOTE]`, `> [!WARNING]`, …).
///
/// GitHub defines the first five variants; Obsidian additionally understands
/// `QUESTION`, `SUCCESS`, `FAILURE`, `DANGER`, `BUG`, `EXAMPLE` and `QUOTE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
    Question,
    Success,
    Failure,
    Danger,
    Bug,
    Example,
    Quote,
}

impl AlertKind {
    /// Parses the `[!TYPE]` marker of an Obsidian callout.
    pub fn from_marker(marker: &str) -> Option<Self> {
        Some(match marker.trim().to_ascii_uppercase().as_str() {
            "NOTE" | "INFO" => AlertKind::Note,
            "TIP" | "HINT" => AlertKind::Tip,
            "IMPORTANT" => AlertKind::Important,
            "WARNING" | "WARN" => AlertKind::Warning,
            "CAUTION" => AlertKind::Caution,
            "QUESTION" | "HELP" | "FAQ" => AlertKind::Question,
            "SUCCESS" | "CHECK" | "DONE" => AlertKind::Success,
            "FAILURE" | "FAIL" | "MISSING" => AlertKind::Failure,
            "DANGER" | "ERROR" => AlertKind::Danger,
            "BUG" => AlertKind::Bug,
            "EXAMPLE" => AlertKind::Example,
            "QUOTE" | "CITE" => AlertKind::Quote,
            _ => return None,
        })
    }
}

fn alert_from(kind: BlockQuoteKind) -> AlertKind {
    match kind {
        BlockQuoteKind::Note => AlertKind::Note,
        BlockQuoteKind::Tip => AlertKind::Tip,
        BlockQuoteKind::Important => AlertKind::Important,
        BlockQuoteKind::Warning => AlertKind::Warning,
        BlockQuoteKind::Caution => AlertKind::Caution,
    }
}

/// Inline elements (pieces of running text).
///
/// Inline elements may be nested arbitrarily (`**bold with *italic* **`).
/// Every variant must be rendered by `ui::render_inlines` and by the print
/// HTML renderer in `html_export.rs`; [`Inline::plain_text`] returns the
/// unformatted text (e.g. for alt texts, PDF metadata and table column
/// widths).
#[derive(Clone, Debug, PartialEq)]
pub enum Inline {
    /// Plain text (may contain hard breaks as `"\n"`).
    Text(String),
    /// *italic*
    Emph(Vec<Inline>),
    /// **bold**
    Strong(Vec<Inline>),
    /// ~~strikethrough~~
    Strike(Vec<Inline>),
    /// ==highlight==
    Highlight(Vec<Inline>),
    /// <u>underline</u>
    Underline(Vec<Inline>),
    /// <kbd>key</kbd>
    Kbd(Vec<Inline>),
    /// ``inline code``
    Code(String),
    /// LaTeX formula (`$..$` inline / `$$..$$` display). The LaTeX source is
    /// shown typographically; a full math renderer would be a separate,
    /// very large undertaking.
    Math { tex: String, display: bool },
    /// `[text](url)`
    Link { inlines: Vec<Inline>, url: String },
    /// Obsidian `[[target]]` / `[[target|label]]` wiki-link.
    WikiLink { target: String, label: String },
    /// `![alt](url)` (also produced by `<img>` and `![[image.png]]`).
    Image {
        alt: String,
        url: String,
        title: Option<String>,
        /// Explicit width from HTML (`<img width="320">`), in points.
        width: Option<f32>,
        /// Explicit height from HTML (`<img height="168">`), in points.
        height: Option<f32>,
    },
    /// Obsidian embed of something that is not an image (`![[Note]]`,
    /// `![[file.pdf#page=2]]`). Rendered as a compact embed card.
    WikiEmbed { target: String, label: String },
    /// `[^label]` — footnote reference.
    FootnoteRef { label: String, number: usize },
    /// Obsidian inline tag (`#project`).
    Tag(String),
    /// Pandoc citation key or cluster (`[@doe2024]`).
    Citation(String),
    /// Markdown Extra abbreviation (`*[HTML]: HyperText Markup Language`).
    Abbr { text: String, title: String },
    /// ^superscript^
    Super(Vec<Inline>),
    /// ~subscript~
    Sub(Vec<Inline>),
}

impl Inline {
    /// Nested inline content of a styling element or link (empty otherwise).
    fn children(&self) -> &[Inline] {
        match self {
            Inline::Emph(c)
            | Inline::Strong(c)
            | Inline::Strike(c)
            | Inline::Highlight(c)
            | Inline::Underline(c)
            | Inline::Kbd(c)
            | Inline::Super(c)
            | Inline::Sub(c)
            | Inline::Link { inlines: c, .. } => c.as_slice(),
            _ => &[],
        }
    }

    fn children_mut(&mut self) -> &mut [Inline] {
        match self {
            Inline::Emph(c)
            | Inline::Strong(c)
            | Inline::Strike(c)
            | Inline::Highlight(c)
            | Inline::Underline(c)
            | Inline::Kbd(c)
            | Inline::Super(c)
            | Inline::Sub(c)
            | Inline::Link { inlines: c, .. } => c.as_mut_slice(),
            _ => &mut [],
        }
    }

    /// Unformatted text content.
    ///
    /// Used for alt texts, PDF metadata and column widths. Links prefer the
    /// link text, images use their alt text, and footnote references appear
    /// as their number (or `[label]` before numbering).
    pub fn plain_text(&self) -> String {
        match self {
            Inline::Text(t) | Inline::Code(t) => t.clone(),
            Inline::Emph(c)
            | Inline::Strong(c)
            | Inline::Strike(c)
            | Inline::Highlight(c)
            | Inline::Underline(c)
            | Inline::Kbd(c)
            | Inline::Super(c)
            | Inline::Sub(c) => c.iter().map(|i| i.plain_text()).collect(),
            Inline::Math { tex, .. } => tex.clone(),
            Inline::Tag(tag) => format!("#{tag}"),
            Inline::Citation(cite) => format!("[{cite}]"),
            Inline::Abbr { text, .. } => text.clone(),
            Inline::Link { inlines, url } => {
                let t: String = inlines.iter().map(|i| i.plain_text()).collect();
                if t.trim().is_empty() { url.clone() } else { t }
            }
            Inline::WikiLink { label, target } => {
                if label.trim().is_empty() {
                    target.clone()
                } else {
                    label.clone()
                }
            }
            Inline::Image { alt, .. } => alt.clone(),
            Inline::WikiEmbed { label, target } => {
                if label.trim().is_empty() {
                    target.clone()
                } else {
                    label.clone()
                }
            }
            Inline::FootnoteRef { label, number } => {
                if *number > 0 {
                    format!("[{number}]")
                } else {
                    format!("[{label}]")
                }
            }
        }
    }
}

/// One entry in an (un)ordered list; `task` = GFM task-list checkbox.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ListItem {
    pub blocks: Vec<Block>,
    /// `Some(checked)` for task lists (`- [ ]` / `- [x]`), otherwise `None`.
    pub task: Option<bool>,
}

/// One term/definition pair of a Markdown definition list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DefinitionItem {
    pub term: Vec<Inline>,
    pub definitions: Vec<Vec<Block>>,
}

/// One table cell.
///
/// GFM cells are always 1×1; HTML `<td colspan rowspan>` may span several
/// columns or rows. `col` is the resolved 0-based grid column (filled in by
/// [`place_table_cells`]), so the preview and PDF renderers can place cells
/// without re-running the grid algorithm.
#[derive(Clone, Debug, PartialEq)]
pub struct TableCell {
    pub inlines: Vec<Inline>,
    pub colspan: usize,
    pub rowspan: usize,
    pub col: usize,
}

impl TableCell {
    /// A plain 1×1 cell (GFM tables).
    pub fn new(inlines: Vec<Inline>) -> Self {
        Self {
            inlines,
            colspan: 1,
            rowspan: 1,
            col: 0,
        }
    }

    /// A cell with HTML `colspan`/`rowspan` attributes.
    pub fn spanned(inlines: Vec<Inline>, colspan: usize, rowspan: usize) -> Self {
        Self {
            inlines,
            colspan: colspan.max(1),
            rowspan: rowspan.max(1),
            col: 0,
        }
    }

    /// Plain text of the cell (used for measuring and summaries).
    pub fn plain_text(&self) -> String {
        self.inlines.iter().map(Inline::plain_text).collect()
    }
}

/// Block-level elements.
///
/// This enum is the central interface between the parser and the preview
/// (`ui.rs`) — the PDF export renders the raw Markdown source through the
/// ported print pipeline (`html_export.rs`) and does not consume
/// this model. To support a new Markdown block type, add a variant here and
/// handle it in exactly three places:
///
/// 1. `Walker::container` (parsing from pulldown-cmark events),
/// 2. `render_block` in `ui.rs` (on-screen rendering),
/// 3. `render_markdown` in `html_export.rs` (print-ready HTML → WeasyPrint PDF).
#[allow(clippy::enum_variant_names)] // `CodeBlock`/`BlockQuote` are intentional.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading {
        /// 1..=6
        level: u8,
        inlines: Vec<Inline>,
    },
    /// Paragraph.
    Paragraph(Vec<Inline>),
    /// Standalone image block (Markdown or HTML). Kept separate from
    /// [`Block::Paragraph`] so a centered HTML wrapper
    /// (`<p align="center"><img …></p>`) can be honoured.
    Image { image: Inline, center: bool },
    /// Display math (`$$…$$`) as a standalone block. Kept separate from
    /// [`Block::Paragraph`] so the preview can center and typeset it.
    Math(String),
    /// Code block; `lang` = language tag after ``` in fenced blocks.
    CodeBlock { lang: Option<String>, code: String },
    /// A Mermaid diagram (```mermaid). The source is rendered as a diagram in
    /// the preview and kept as a labelled code block in the PDF.
    Mermaid(String),
    /// `>` quotation, optionally a GFM/Obsidian callout (`[!NOTE]` …).
    BlockQuote {
        kind: Option<AlertKind>,
        /// Custom title after the marker (`> [!WARNING] Careful!`).
        title: Option<String>,
        /// Obsidian fold state: `Some(false)` = folded by default (`-`),
        /// `Some(true)` = expanded (`+`), `None` = not collapsible.
        fold: Option<bool>,
        blocks: Vec<Block>,
    },
    List {
        ordered: bool,
        /// Start number of ordered lists.
        start: u64,
        /// CommonMark tight list: item paragraphs are rendered without block
        /// spacing (no blank lines between items in the source).
        tight: bool,
        items: Vec<ListItem>,
    },
    /// Markdown definition list (`Term` / `: definition`).
    DefinitionList(Vec<DefinitionItem>),
    Table {
        aligns: Vec<TableAlign>,
        /// Header rows. GFM tables have exactly one; HTML `<thead>` may contain
        /// several (the second row of the sample table holds `CPU`/`RAM`).
        head: Vec<Vec<TableCell>>,
        rows: Vec<Vec<TableCell>>,
    },
    /// Collapsible HTML `<details>` element (summary + Markdown body).
    Details {
        summary: String,
        open: bool,
        blocks: Vec<Block>,
    },
    /// `<video>`, `<audio>` or `<iframe>` embedded media placeholder.
    Media { kind: String, url: String },
    /// Generated table of contents (`[TOC]`).
    Toc(Vec<(u8, String)>),
    /// Pandoc/Quarto fenced div (`::: warning`) or MkDocs admonition.
    FencedDiv {
        /// Raw class/info string from the opening fence (e.g. `warning`).
        class: String,
        blocks: Vec<Block>,
    },
    /// A group of blocks produced from one raw HTML container (e.g. a styled
    /// `<div>` banner). Rendered like the surrounding document flow.
    Group(Vec<Block>),
    /// Raw HTML blocks — the source is kept and rendered like a code block.
    Html(String),
    /// `---` horizontal rule.
    Rule,
}

impl Block {
    /// Blocks nested directly inside a container (quote, details, div, group).
    fn nested_blocks(&self) -> Option<&[Block]> {
        match self {
            Block::BlockQuote { blocks, .. }
            | Block::Details { blocks, .. }
            | Block::FencedDiv { blocks, .. }
            | Block::Group(blocks) => Some(blocks.as_slice()),
            _ => None,
        }
    }

    fn nested_blocks_mut(&mut self) -> Option<&mut Vec<Block>> {
        match self {
            Block::BlockQuote { blocks, .. }
            | Block::Details { blocks, .. }
            | Block::FencedDiv { blocks, .. }
            | Block::Group(blocks) => Some(blocks),
            _ => None,
        }
    }

    /// First line of inline content of a block (e.g. for outlines/table of
    /// contents or fallback text in the PDF).
    pub fn summarize(&self) -> String {
        match self {
            Block::Heading { inlines, .. } | Block::Paragraph(inlines) => {
                let mut s = String::new();
                for i in inlines {
                    s.push_str(&i.plain_text());
                }
                s
            }
            Block::Math(tex) | Block::Mermaid(tex) => tex.clone(),
            Block::CodeBlock { lang, .. } => lang.clone().unwrap_or_else(|| "code".into()),
            Block::BlockQuote { blocks, .. } => {
                blocks.first().map(|b| b.summarize()).unwrap_or_default()
            }
            Block::List { items, .. } => items
                .first()
                .and_then(|i| i.blocks.first())
                .map(|b| b.summarize())
                .unwrap_or_default(),
            Block::DefinitionList(items) => items
                .first()
                .map(|i| i.term.iter().map(|x| x.plain_text()).collect())
                .unwrap_or_default(),
            Block::Table { head, .. } => head
                .first()
                .map(|first_cell| {
                    first_cell
                        .iter()
                        .map(TableCell::plain_text)
                        .collect::<Vec<_>>()
                        .join(" | ")
                })
                .unwrap_or_default(),
            Block::Details { summary, .. } => summary.clone(),
            Block::FencedDiv { class, .. } => class.clone(),
            Block::Toc(_) => "Contents".into(),
            Block::Media { kind, .. } => kind.clone(),
            Block::Image { image, .. } => image.plain_text(),
            Block::Group(blocks) => blocks.first().map(Block::summarize).unwrap_or_default(),
            Block::Rule => "———".into(),
            Block::Html(_) => "<html>".into(),
        }
    }
}

/// Fully parsed document — the only interface between the parser, the preview
/// and the PDF export.
///
/// `title` is derived from the first H1 and is used for PDF metadata and
/// suggested file names. Footnotes are stored separately because they only
/// appear as references in the running text and are rendered at the end of
/// the document (or at the end of the PDF).
#[derive(Clone, Debug, Default)]
pub struct Document {
    /// First H1 heading, if present (PDF/title bar).
    pub title: Option<String>,
    pub blocks: Vec<Block>,
    /// Footnote definitions (`label` → content) in document order.
    pub footnotes: BTreeMap<String, Vec<Block>>,
    /// Footnote numbers in first-reference order (`label` → 1-based number).
    pub footnote_numbers: BTreeMap<String, usize>,
    /// YAML/TOML front-matter properties in source order.
    pub metadata: Vec<(String, String)>,
    /// 1-based source line of each entry in [`Document::blocks`]. Used by the
    /// split view to jump from a clicked preview block to the matching source
    /// line. The vector is parallel to `blocks` (missing entries → line 1).
    pub block_lines: Vec<usize>,
}

impl Document {
    /// Plain text of the given blocks: one line per heading or paragraph, the
    /// summary of every other block. Used by tests.
    #[cfg(test)]
    pub fn plain_blocks(blocks: &[Block]) -> String {
        let mut out = String::new();
        for b in blocks {
            match b {
                Block::Paragraph(inl) | Block::Heading { inlines: inl, .. } => {
                    for i in inl {
                        out.push_str(&i.plain_text());
                    }
                    out.push('\n');
                }
                _ => out.push_str(&b.summarize()),
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// pulldown-cmark extensions enabled for every document and for inline HTML
/// table cells (which are parsed as small Markdown fragments).
fn parser_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_MATH
        | Options::ENABLE_GFM
        | Options::ENABLE_SUPERSCRIPT
        | Options::ENABLE_SUBSCRIPT
        | Options::ENABLE_WIKILINKS
        | Options::ENABLE_DEFINITION_LIST
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS
}

/// Extracts YAML, TOML (`+++`) or JSON front matter and returns the remaining
/// Markdown. Nested maps are flattened to dotted keys for the Properties card.
fn extract_front_matter(markdown: &str) -> (Vec<(String, String)>, String) {
    let markdown = markdown.strip_prefix('\u{feff}').unwrap_or(markdown);

    if let Some(rest) = markdown.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---\n") {
            return (flatten_yaml(&rest[..end]), rest[end + 5..].to_string());
        }
        if let Some(yaml) = rest.strip_suffix("\n---") {
            return (flatten_yaml(yaml), String::new());
        }
    }

    if let Some(rest) = markdown.strip_prefix("+++\n") {
        if let Some(end) = rest.find("\n+++\n") {
            let metadata = toml::from_str::<toml::Value>(&rest[..end])
                .map(|value| flatten_toml("", &value))
                .unwrap_or_default();
            return (metadata, rest[end + 5..].to_string());
        }
        if let Some(toml_text) = rest.strip_suffix("\n+++") {
            let metadata = toml::from_str::<toml::Value>(toml_text)
                .map(|value| flatten_toml("", &value))
                .unwrap_or_default();
            return (metadata, String::new());
        }
    }

    // JSON front matter (`{ ... }`, single- or multi-line).
    if markdown.starts_with('{') {
        for (index, _) in markdown.char_indices().filter(|(_, c)| *c == '}') {
            let rest = &markdown[index + 1..];
            if !(rest.is_empty() || rest.starts_with('\n')) {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&markdown[..=index]) {
                return (
                    flatten_json("", &value),
                    rest.strip_prefix('\n').unwrap_or(rest).to_string(),
                );
            }
        }
    }

    (Vec::new(), markdown.to_string())
}

fn yaml_scalar(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_string()
}

/// Flattens common nested YAML into `parent.child` keys.
fn flatten_yaml(raw: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut stack: Vec<(usize, String)> = Vec::new();

    for line in raw.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim();
        while stack
            .last()
            .is_some_and(|(parent_indent, _)| *parent_indent >= indent)
        {
            stack.pop();
        }

        if let Some(item) = trimmed.strip_prefix("- ") {
            if !stack.is_empty() {
                let key = stack
                    .iter()
                    .map(|(_, part)| part.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                let item = yaml_scalar(item);
                if let Some((_, value)) = out.iter_mut().find(|(k, _)| k == &key) {
                    if !value.is_empty() {
                        value.push_str(", ");
                    }
                    value.push_str(&item);
                } else {
                    out.push((key, item));
                }
            }
            continue;
        }

        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let full_key = if stack.is_empty() {
            key.to_string()
        } else {
            format!(
                "{}.{}",
                stack
                    .iter()
                    .map(|(_, part)| part.as_str())
                    .collect::<Vec<_>>()
                    .join("."),
                key
            )
        };
        let value = value.trim();
        if value.is_empty() {
            stack.push((indent, key.to_string()));
        } else {
            out.push((full_key, yaml_scalar(value)));
        }
    }
    out
}

fn flatten_toml(prefix: &str, value: &toml::Value) -> Vec<(String, String)> {
    let name = |key: &str| {
        if prefix.is_empty() {
            key.to_string()
        } else {
            format!("{prefix}.{key}")
        }
    };
    match value {
        toml::Value::Table(table) => table
            .iter()
            .flat_map(|(key, value)| flatten_toml(&name(key), value))
            .collect(),
        toml::Value::Array(items) => {
            let text = items.iter().map(toml_scalar).collect::<Vec<_>>().join(", ");
            vec![(prefix.to_string(), text)]
        }
        other => vec![(prefix.to_string(), toml_scalar(other))],
    }
}

fn toml_scalar(value: &toml::Value) -> String {
    match value {
        toml::Value::String(text) => text.clone(),
        toml::Value::Integer(number) => number.to_string(),
        toml::Value::Float(number) => number.to_string(),
        toml::Value::Boolean(boolean) => boolean.to_string(),
        toml::Value::Datetime(datetime) => datetime.to_string(),
        toml::Value::Array(_) | toml::Value::Table(_) => value.to_string(),
    }
}

fn flatten_json(prefix: &str, value: &serde_json::Value) -> Vec<(String, String)> {
    let name = |key: &str| {
        if prefix.is_empty() {
            key.to_string()
        } else {
            format!("{prefix}.{key}")
        }
    };
    match value {
        serde_json::Value::Object(map) => map
            .iter()
            .flat_map(|(key, value)| flatten_json(&name(key), value))
            .collect(),
        serde_json::Value::Array(items) => {
            let text = items.iter().map(json_scalar).collect::<Vec<_>>().join(", ");
            vec![(prefix.to_string(), text)]
        }
        other => vec![(prefix.to_string(), json_scalar(other))],
    }
}

fn json_scalar(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Removes Pandoc/Markdown-Extra heading attributes such as `{#id .class}`
/// from the displayed heading text.
fn strip_heading_attributes(inlines: &mut [Inline]) {
    let Some(Inline::Text(text)) = inlines.last_mut() else {
        return;
    };
    let trimmed = text.trim_end();
    if !trimmed.ends_with('}') {
        return;
    }
    let Some(start) = trimmed.rfind('{') else {
        return;
    };
    let attributes = &trimmed[start + 1..trimmed.len() - 1];
    let has_attribute_token = attributes
        .split_whitespace()
        .any(|token| token.starts_with(['#', '.']) || token.contains('='));
    if !has_attribute_token
        || !attributes.chars().all(|c| {
            c.is_alphanumeric()
                || matches!(
                    c,
                    '#' | '.' | '-' | '_' | '=' | '"' | '\'' | ' ' | ':' | '/'
                )
        })
    {
        return;
    }
    *text = trimmed[..start].trim_end().to_string();
}

/// Expands Pandoc-style inline footnotes (`^[text]`) into standard labelled
/// footnote references plus definitions appended at the end of the document.
pub(crate) fn expand_inline_footnotes(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len() + 64);
    let mut definitions: Vec<String> = Vec::new();
    let mut fence = FenceTracker::default();

    for line in markdown.split_inclusive('\n') {
        if fence.feed(line) {
            out.push_str(line);
            continue;
        }

        let chars: Vec<char> = line.chars().collect();
        let mut i = 0usize;
        let mut in_code = false;
        while i < chars.len() {
            if chars[i] == '`' {
                in_code = !in_code;
                out.push('`');
                i += 1;
                continue;
            }
            if !in_code && chars[i] == '^' && chars.get(i + 1) == Some(&'[') {
                let mut end = i + 2;
                while end < chars.len() && chars[end] != ']' {
                    end += 1;
                }
                if end < chars.len() {
                    let body: String = chars[i + 2..end].iter().collect();
                    if !body.trim().is_empty() {
                        let label = format!("mdviewer-inline-{}", definitions.len() + 1);
                        out.push_str(&format!("[^{label}]"));
                        definitions.push(format!("[^{label}]: {body}"));
                        i = end + 1;
                        continue;
                    }
                }
            }
            out.push(chars[i]);
            i += 1;
        }
    }

    if !definitions.is_empty() {
        out.push_str("\n\n");
        for definition in definitions {
            out.push_str(&definition);
            out.push('\n');
        }
    }
    out
}

/// Replaces `[TOC]` lines with a sentinel block.
fn protect_toc(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len() + 16);
    let mut fence = FenceTracker::default();
    for line in markdown.split_inclusive('\n') {
        if fence.feed(line) || line.trim() != "[TOC]" {
            out.push_str(line);
        } else {
            out.push_str("```");
            out.push_str(TOC_LANG);
            out.push_str("\n```\n");
        }
    }
    out
}

/// Collects headings for a `[TOC]` block.
fn collect_headings(blocks: &[Block]) -> Vec<(u8, String)> {
    let mut out = Vec::new();
    fn visit(blocks: &[Block], out: &mut Vec<(u8, String)>) {
        for block in blocks {
            if let Block::Heading { level, inlines } = block {
                out.push((*level, inlines.iter().map(|i| i.plain_text()).collect()));
            } else if let Some(inner) = block.nested_blocks() {
                visit(inner, out);
            }
        }
    }
    visit(blocks, &mut out);
    out
}

fn fill_toc(blocks: &mut [Block], headings: &[(u8, String)]) {
    for block in blocks {
        if let Block::Toc(entries) = block {
            *entries = headings.to_vec();
        } else if let Some(inner) = block.nested_blocks_mut() {
            fill_toc(inner, headings);
        }
    }
}

/// Parses Markdown into a [`Document`].
///
/// Enabled extensions: GFM tables, strikethrough, task lists, footnotes,
/// LaTeX math (`$…$`, `$$…$$`, `\(…\)`, `\[…\]`), GFM alerts (`> [!NOTE]`),
/// Obsidian callouts/wiki-links/embeds, definition lists, YAML front matter,
/// super-/subscript, `==highlight==` and a small, safe HTML subset.
///
/// CRLF line endings are normalised first. The events are materialized once
/// and walked with a cursor; the remaining stages make a bounded number of
/// passes over the source or the block tree. The parser is deliberately total
/// (no panics on exotic input) and guarantees progress (see the module docs).
pub fn parse(md: &str) -> Document {
    let md = md.replace("\r\n", "\n");
    let opts = parser_options();
    let (front_matter, content) = extract_front_matter(&md);
    let content = expand_inline_footnotes(&content);

    // Some constructs are easier (and safer) to protect before pulldown-cmark
    // sees them:
    //  * `$$…$$` / `\[…\]` display math may contain a lone `=` line, which
    //    CommonMark would turn into a Setext heading,
    //  * `<details>…</details>` spans blank lines, which ends an HTML block,
    //  * HTML comments/scripts must never reach the renderer.
    let cleaned = preprocess(&content);

    // Materialize the event stream once (ownership transfer via `into_static`)
    // and assemble it into blocks with a small state machine.
    let events: Vec<Event<'static>> = Parser::new_ext(&cleaned, opts)
        .map(Event::into_static)
        .collect();
    let mut w = Walker {
        events,
        pos: 0,
        footnotes: BTreeMap::new(),
        metadata: front_matter,
    };
    let mut blocks = w.document();
    let headings = collect_headings(&blocks);
    fill_toc(&mut blocks, &headings);
    postprocess_blocks(&mut blocks);
    for f in w.footnotes.values_mut() {
        postprocess_blocks(f);
    }
    let footnote_numbers = collect_footnote_numbers(&blocks, &w.footnotes);
    assign_footnote_numbers(&mut blocks, &footnote_numbers);
    for f in w.footnotes.values_mut() {
        assign_footnote_numbers(f, &footnote_numbers);
    }
    let block_lines = source_lines(&blocks, &md);
    let title = blocks
        .iter()
        .find_map(|b| {
            if let Block::Heading { level: 1, inlines } = b {
                let t: String = inlines.iter().map(|i| i.plain_text()).collect();
                let t = t.trim().to_string();
                if t.is_empty() { None } else { Some(t) }
            } else {
                None
            }
        })
        .or_else(|| {
            w.metadata
                .iter()
                .find(|(k, v)| k.eq_ignore_ascii_case("title") && !v.trim().is_empty())
                .map(|(_, v)| v.trim().to_string())
        });
    Document {
        title,
        blocks,
        footnotes: w.footnotes,
        footnote_numbers,
        metadata: w.metadata,
        block_lines,
    }
}

/// Finds the 1-based source line of every top-level block.
///
/// Preprocessing (math protection, HTML removal, …) changes byte offsets, so
/// instead of trusting pulldown-cmark offsets we search the original source for
/// the block's first distinctive text. The search is strictly forward, which
/// keeps repeated strings (e.g. several "Check:" paragraphs) in order. This is
/// used only for click-to-source navigation; rendering never depends on it.
fn source_lines(blocks: &[Block], md: &str) -> Vec<usize> {
    let lines: Vec<&str> = md.lines().collect();
    let mut cursor = 0usize;
    // Skip YAML/TOML front matter so a `title:` value cannot be mistaken for
    // the document heading.
    if lines.first().is_some_and(|l| l.trim() == "---")
        && let Some(end) = lines
            .iter()
            .skip(1)
            .position(|l| matches!(l.trim(), "---" | "..."))
    {
        cursor = end + 2;
    }

    let mut out = Vec::with_capacity(blocks.len());
    for block in blocks {
        let found = if matches!(block, Block::Rule) {
            (cursor..lines.len()).find(|&i| matches!(lines[i].trim(), "---" | "***" | "___"))
        } else if matches!(block, Block::Details { .. }) {
            (cursor..lines.len()).find(|&i| lines[i].to_ascii_lowercase().contains("<details"))
        } else {
            let needles = block_needles(block);
            needles.iter().find_map(|needle| {
                (!needle.is_empty())
                    .then(|| (cursor..lines.len()).find(|&i| lines[i].contains(needle.as_str())))
                    .flatten()
            })
        };
        if let Some(i) = found {
            out.push(i + 1);
            cursor = i + 1;
        } else {
            out.push((cursor + 1).min(lines.len().max(1)));
        }
    }
    out
}

/// Candidate search strings for a block, from most to least specific.
fn block_needles(block: &Block) -> Vec<String> {
    let text = match block {
        Block::Heading { inlines, .. } | Block::Paragraph(inlines) => {
            inlines.iter().map(|i| i.plain_text()).collect::<String>()
        }
        Block::Math(tex) | Block::Mermaid(tex) => first_nonempty_line(tex),
        Block::CodeBlock { lang, code } => {
            let line = first_nonempty_line(code);
            if line.is_empty() {
                format!("```{}", lang.as_deref().unwrap_or(""))
            } else {
                line
            }
        }
        Block::BlockQuote { blocks, .. } | Block::FencedDiv { blocks, .. } => {
            blocks.first().map(Block::summarize).unwrap_or_default()
        }
        Block::Details { summary, .. } => summary.clone(),
        Block::Toc(_) => "Contents".into(),
        Block::Media { url, .. } => first_nonempty_line(url),
        Block::List { items, .. } => items
            .first()
            .and_then(|i| i.blocks.first())
            .map(Block::summarize)
            .unwrap_or_default(),
        Block::DefinitionList(items) => items
            .first()
            .map(|i| i.term.iter().map(|x| x.plain_text()).collect::<String>())
            .unwrap_or_default(),
        Block::Table { head, .. } => head
            .first()
            .map(|row| row.iter().map(TableCell::plain_text).collect::<String>())
            .unwrap_or_default(),
        Block::Html(src) => first_nonempty_line(src),
        Block::Group(blocks) => blocks.first().map(Block::summarize).unwrap_or_default(),
        Block::Image { image, .. } => image.plain_text(),
        Block::Rule => return vec![],
    };
    text_needles(&text)
}

fn first_nonempty_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

/// Progressive prefixes of the first text line. Short prefixes tolerate inline
/// markup, entities and escapes in the source (`&copy;`, `\.`, `` `code` ``).
fn text_needles(text: &str) -> Vec<String> {
    let line = text.lines().next().unwrap_or("").trim();
    let mut out: Vec<String> = Vec::new();
    for n in [48usize, 30, 18, 10, 6] {
        let candidate: String = line.chars().take(n).collect();
        let candidate = candidate.trim().to_string();
        if !candidate.is_empty() && !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    if let Some(word) = line.split_whitespace().next() {
        let word = word.trim();
        if !word.is_empty() && !out.iter().any(|c| c == word) {
            out.push(word.to_string());
        }
    }
    out
}

/// Private-use marker for a Markdown soft line break. It is turned into a
/// real newline after callout parsing, where the original line boundary is
/// needed to separate an Obsidian callout title from its body.
///
/// The viewer intentionally uses *strict line breaks* (the behaviour of
/// Obsidian, Typora and GitHub-rendered READMEs): a single newline in the
/// source starts a new visual line. Pre-wrapped documents such as README.md
/// therefore keep their intended line structure instead of being reflowed
/// into one long paragraph.
const SOFT_BREAK: char = '\u{E010}';

/// Placeholder fenced language for protected display math.
const MATH_LANG: &str = "mdviewer-math";
/// Placeholder fenced language for `<details>` blocks.
const DETAILS_LANG: &str = "mdviewer-details";
const DETAILS_OPEN_LANG: &str = "mdviewer-details-open";
/// Placeholder fenced language for Pandoc fenced divs (`::: class`).
const DIV_LANG: &str = "mdviewer-div";
/// Placeholder fenced language for `[TOC]`.
const TOC_LANG: &str = "mdviewer-toc";

/// Tracks fenced code blocks (``` or ~~~) over the lines of a Markdown source.
///
/// A closing fence must be at least as long as the opening one and carry no
/// info string, as in CommonMark.
#[derive(Default)]
pub(crate) struct FenceTracker {
    open: Option<(char, usize)>,
}

impl FenceTracker {
    /// Consumes one line and returns `true` if it belongs to a fenced code
    /// block, fence lines included.
    pub(crate) fn feed(&mut self, line: &str) -> bool {
        let Some((c, n, info)) = fence_marker(line) else {
            return self.open.is_some();
        };
        match self.open {
            Some((fc, fn_)) if fc == c && n >= fn_ && info.is_empty() => self.open = None,
            None => self.open = Some((c, n)),
            Some(_) => {}
        }
        true
    }
}

/// A `~` fence longer than any tilde fence inside `lines`, so a sentinel block
/// wrapping other sentinel blocks (display math inside `<details>`, a fenced
/// code block inside a div) only closes at its own closing fence.
fn sentinel_fence<'a>(lines: impl IntoIterator<Item = &'a str>) -> String {
    let longest = lines
        .into_iter()
        .filter_map(fence_marker)
        .filter(|(c, _, _)| *c == '~')
        .map(|(_, n, _)| n)
        .max()
        .unwrap_or(0);
    "~".repeat(longest.max(3) + 1)
}

/// Converts Pandoc/Quarto fenced divs (`::: warning … :::`) into sentinel
/// fenced blocks understood by [`Walker::code_block`]. Divs may nest; the
/// body of each div is converted when it is parsed.
fn protect_fenced_divs(markdown: &str) -> String {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut out = String::with_capacity(markdown.len() + 32);
    let mut i = 0usize;
    let mut fence = FenceTracker::default();

    while i < lines.len() {
        let line = lines[i];
        if fence.feed(line) {
            out.push_str(line);
            out.push('\n');
            i += 1;
            continue;
        }

        let trimmed = line.trim_start();
        if trimmed.starts_with(":::")
            && !trimmed[3..].trim().is_empty()
            && !trimmed.chars().all(|c| c == ':')
        {
            let info = trimmed[3..].trim().to_string();
            let mut body: Vec<&str> = Vec::new();
            let mut body_fence = FenceTracker::default();
            let mut depth = 0usize;
            let mut j = i + 1;
            let mut closed = false;
            while j < lines.len() {
                let candidate = lines[j].trim();
                if !body_fence.feed(lines[j]) {
                    if !candidate.is_empty() && candidate.chars().all(|c| c == ':') {
                        if depth == 0 {
                            closed = true;
                            j += 1;
                            break;
                        }
                        depth -= 1;
                    } else if candidate.starts_with(":::") && !candidate[3..].trim().is_empty() {
                        depth += 1;
                    }
                }
                body.push(lines[j]);
                j += 1;
            }
            if closed {
                let marker = sentinel_fence(body.iter().copied());
                out.push_str(&marker);
                out.push_str(DIV_LANG);
                out.push('\n');
                out.push_str(&info);
                out.push('\n');
                for line in body {
                    out.push_str(line);
                    out.push('\n');
                }
                out.push_str(&marker);
                out.push('\n');
                i = j;
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
        i += 1;
    }
    out
}

/// Converts MkDocs admonitions (`!!! note "Title"` + indented body) into
/// standard callout block quotes.
pub(crate) fn convert_admonitions(markdown: &str) -> String {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut out = String::with_capacity(markdown.len() + 32);
    let mut i = 0usize;
    let mut fence = FenceTracker::default();

    while i < lines.len() {
        let line = lines[i];
        if fence.feed(line) {
            out.push_str(line);
            out.push('\n');
            i += 1;
            continue;
        }

        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("!!! ") {
            let mut parts = rest.trim().splitn(2, char::is_whitespace);
            let kind = parts.next().unwrap_or("note").to_ascii_uppercase();
            let title = parts
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches('"')
                .to_string();
            let marker = match kind.as_str() {
                "INFO" | "ABSTRACT" | "SUMMARY" | "TLDR" => "NOTE",
                "HINT" => "TIP",
                "CHECK" | "DONE" => "SUCCESS",
                "FAIL" | "MISSING" => "FAILURE",
                other if AlertKind::from_marker(other).is_some() => other,
                _ => "NOTE",
            };

            let mut body: Vec<String> = Vec::new();
            let mut j = i + 1;
            while j < lines.len() {
                let candidate = lines[j];
                if candidate.trim().is_empty() {
                    body.push(String::new());
                    j += 1;
                    continue;
                }
                if let Some(stripped) = candidate.strip_prefix("    ") {
                    body.push(stripped.to_string());
                    j += 1;
                } else if let Some(stripped) = candidate.strip_prefix('\t') {
                    body.push(stripped.to_string());
                    j += 1;
                } else {
                    break;
                }
            }

            out.push_str("> [!");
            out.push_str(marker);
            out.push(']');
            if !title.is_empty() {
                out.push(' ');
                out.push_str(&title);
            }
            out.push('\n');
            for line in body {
                if line.is_empty() {
                    out.push_str(">\n");
                } else {
                    out.push_str("> ");
                    out.push_str(&line);
                    out.push('\n');
                }
            }
            out.push('\n');
            i = j;
            continue;
        }

        out.push_str(line);
        out.push('\n');
        i += 1;
    }
    out
}

/// Converts simple Pandoc grid tables (`+---+---+`) into GFM pipe tables.
/// Multi-line cells and row spans are not reconstructed.
pub(crate) fn convert_grid_tables(markdown: &str) -> String {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut out = String::with_capacity(markdown.len());
    let mut i = 0usize;
    let mut fence = FenceTracker::default();
    while i < lines.len() {
        let line = lines[i];
        if fence.feed(line) {
            out.push_str(line);
            out.push('\n');
            i += 1;
            continue;
        }

        let trimmed = line.trim();
        let is_border = trimmed.starts_with('+')
            && trimmed.chars().any(|c| c == '-')
            && trimmed.chars().all(|c| matches!(c, '+' | '-' | '='));
        if is_border {
            let mut rows: Vec<Vec<String>> = Vec::new();
            let mut j = i + 1;
            while j < lines.len() {
                let candidate = lines[j].trim();
                if candidate.starts_with('+')
                    && candidate.chars().all(|c| matches!(c, '+' | '-' | '='))
                {
                    j += 1;
                    continue;
                }
                if candidate.starts_with('|') {
                    rows.push(
                        candidate
                            .trim_matches('|')
                            .split('|')
                            .map(|cell| cell.trim().to_string())
                            .collect(),
                    );
                    j += 1;
                    continue;
                }
                break;
            }
            if !rows.is_empty() {
                let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
                let header = &rows[0];
                out.push('|');
                for column in 0..columns {
                    out.push(' ');
                    out.push_str(header.get(column).map(String::as_str).unwrap_or(""));
                    out.push_str(" |");
                }
                out.push('\n');
                out.push('|');
                for _ in 0..columns {
                    out.push_str(" --- |");
                }
                out.push('\n');
                for row in rows.iter().skip(1) {
                    out.push('|');
                    for column in 0..columns {
                        out.push(' ');
                        out.push_str(row.get(column).map(String::as_str).unwrap_or(""));
                        out.push_str(" |");
                    }
                    out.push('\n');
                }
                i = j;
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
        i += 1;
    }
    out
}

/// Replaces Markdown-Extra abbreviation definitions (`*[HTML]: HyperText …`)
/// with `<abbr>` elements in the prose and removes the definitions. Matches
/// are whole words and are not replaced inside code, HTML tags, `$…$` math or
/// link destinations.
pub(crate) fn preprocess_abbreviations(markdown: &str) -> String {
    let mut definitions: Vec<(String, String)> = Vec::new();
    let mut content = String::with_capacity(markdown.len());
    let mut fence = FenceTracker::default();
    for line in markdown.split_inclusive('\n') {
        if fence.feed(line) {
            content.push_str(line);
            continue;
        }
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("*[")
            && let Some(end) = rest.find("]:")
        {
            let key = rest[..end].trim();
            let title = rest[end + 2..].trim().trim_end_matches('\n');
            if !key.is_empty() && !title.is_empty() {
                definitions.push((key.to_string(), title.to_string()));
            }
            continue;
        }
        content.push_str(line);
    }
    if definitions.is_empty() {
        return content;
    }
    definitions.sort_by_key(|(key, _)| std::cmp::Reverse(key.chars().count()));
    let compiled: Vec<(Vec<char>, &str, &str)> = definitions
        .iter()
        .map(|(key, title)| (key.chars().collect(), key.as_str(), title.as_str()))
        .collect();

    let mut out = String::with_capacity(content.len() + 32);
    let mut fence = FenceTracker::default();
    for line in content.split_inclusive('\n') {
        if fence.feed(line) {
            out.push_str(line);
            continue;
        }

        let chars: Vec<char> = line.chars().collect();
        let mut i = 0usize;
        let mut in_code = false;
        let mut in_tag = false;
        let mut in_math = false;
        let mut in_destination = false;
        while i < chars.len() {
            if chars[i] == '`' {
                in_code = !in_code;
                out.push('`');
                i += 1;
                continue;
            }
            if !in_code && chars[i] == '<' {
                in_tag = true;
            }
            if !in_code && chars[i] == '>' {
                in_tag = false;
            }
            if !in_code && chars[i] == '$' {
                if in_math {
                    in_math = false;
                } else if chars[i + 1..].contains(&'$') {
                    in_math = true;
                }
            }
            if !in_code && chars[i] == ']' && chars.get(i + 1) == Some(&'(') {
                in_destination = true;
            }
            if in_destination && chars[i] == ')' {
                in_destination = false;
            }
            if !in_code && !in_tag && !in_math && !in_destination {
                let mut matched = false;
                for (key_chars, key, title) in &compiled {
                    if chars[i..].starts_with(key_chars) {
                        let previous = i.checked_sub(1).and_then(|p| chars.get(p));
                        let next = chars.get(i + key_chars.len());
                        let boundary =
                            |c: Option<&char>| c.is_none_or(|c| !c.is_alphanumeric() && *c != '_');
                        if boundary(previous) && boundary(next) {
                            out.push_str("<abbr title=\"");
                            out.push_str(&html_escape_attr(title));
                            out.push_str("\">");
                            out.push_str(key);
                            out.push_str("</abbr>");
                            i += key_chars.len();
                            matched = true;
                            break;
                        }
                    }
                }
                if matched {
                    continue;
                }
            }
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn html_escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Source-to-source passes that run before pulldown-cmark: comment and script
/// removal, math and `[TOC]` placeholders, grid tables, abbreviations, fenced
/// divs, admonitions and `<details>`. Fenced code blocks are left untouched by
/// every pass.
fn preprocess(md: &str) -> String {
    let src = strip_unsafe_html(md);
    let src = protect_inline_math(&protect_display_math(&src));
    let src = protect_toc(&src);
    let src = convert_grid_tables(&src);
    let src = preprocess_abbreviations(&src);
    let src = protect_fenced_divs(&src);
    let src = convert_admonitions(&src);
    protect_details(&src)
}

/// Converts `\(…\)` inline math to `$…$` so pulldown-cmark emits a math
/// event. Pulldown would otherwise treat `\(` as an escaped parenthesis and
/// the formula would silently become plain text.
fn protect_inline_math(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let mut fence = FenceTracker::default();
    for line in md.split_inclusive('\n') {
        if fence.feed(line) {
            out.push_str(line);
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0usize;
        let mut in_code = false;
        let mut prev: Option<char> = None;
        while i < chars.len() {
            let c = chars[i];
            if c == '`' {
                in_code = !in_code;
                out.push(c);
                prev = Some(c);
                i += 1;
                continue;
            }
            if !in_code && c == '\\' && chars.get(i + 1) == Some(&'(') && prev != Some('\\') {
                let mut j = i + 2;
                while j + 1 < chars.len() && !(chars[j] == '\\' && chars[j + 1] == ')') {
                    j += 1;
                }
                if j + 1 < chars.len() {
                    let inner: String = chars[i + 2..j].iter().collect();
                    if !inner.trim().is_empty() && !inner.contains('$') {
                        out.push('$');
                        out.push_str(&inner);
                        out.push('$');
                        prev = Some('$');
                        i = j + 2;
                        continue;
                    }
                }
            }
            out.push(c);
            prev = Some(c);
            i += 1;
        }
    }
    out
}

/// True if `line` starts a fenced code block (``` or ~~~) and returns the
/// fence marker and its length.
pub(crate) fn fence_marker_for_export(line: &str) -> Option<(char, usize)> {
    fence_marker(line).map(|(c, n, _)| (c, n))
}

/// Parses a fence line into its character, run length and info string.
fn fence_marker(line: &str) -> Option<(char, usize, &str)> {
    let trimmed = line.trim_start();
    let c = trimmed.chars().next()?;
    if c != '`' && c != '~' {
        return None;
    }
    let len = trimmed.chars().take_while(|&x| x == c).count();
    (len >= 3).then(|| (c, len, trimmed[len..].trim()))
}

/// Removes HTML comments and `<script>`/`<style>` content outside code fences.
///
/// Other HTML is left alone: the parser/renderer only implements a small,
/// safe subset.
fn strip_unsafe_html(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let mut lines = md.split_inclusive('\n');
    let mut fence = FenceTracker::default();

    while let Some(line) = lines.next() {
        if fence.feed(line) {
            out.push_str(line);
            continue;
        }

        let lower = line.trim_start().to_ascii_lowercase();
        if lower.starts_with("<!--") {
            skip_block(line, "-->", &mut lines, &mut out);
            continue;
        }
        if lower.starts_with("<script") {
            skip_block(line, "</script>", &mut lines, &mut out);
            continue;
        }
        if lower.starts_with("<style") {
            skip_block(line, "</style>", &mut lines, &mut out);
            continue;
        }
        out.push_str(line);
    }
    out
}

/// Drops a block that starts on `first` and ends on the first line containing
/// `closing` (ASCII case-insensitive). Text after the closing marker is kept.
///
/// The removed lines are replaced by blank lines so the line count, and with it
/// click-to-source navigation, stays valid.
fn skip_block(
    first: &str,
    closing: &str,
    lines: &mut std::str::SplitInclusive<'_, char>,
    out: &mut String,
) {
    let mut current = first;
    let mut removed_newlines = 0usize;
    loop {
        if let Some(pos) = current.to_ascii_lowercase().find(closing) {
            for _ in 0..removed_newlines {
                out.push('\n');
            }
            out.push_str(&current[pos + closing.len()..]);
            return;
        }
        removed_newlines += current.matches('\n').count();
        match lines.next() {
            Some(next) => current = next,
            None => {
                for _ in 0..removed_newlines {
                    out.push('\n');
                }
                return;
            }
        }
    }
}

/// Replaces `$$…$$` and `\[…\]` display math with a sentinel fenced block.
///
/// `$…$` inline math is handled by pulldown-cmark; `\(…\)` is converted to
/// `$…$` by [`protect_inline_math`] during text post-processing.
fn protect_display_math(md: &str) -> String {
    let mut out = String::with_capacity(md.len() + 32);
    let lines: Vec<&str> = md.split_inclusive('\n').collect();
    let mut i = 0usize;
    let mut fence = FenceTracker::default();

    while i < lines.len() {
        let line = lines[i];
        if fence.feed(line) {
            out.push_str(line);
            i += 1;
            continue;
        }

        let trimmed = line.trim_start();
        let is_dollar = trimmed.starts_with("$$");
        let is_bracket = trimmed.starts_with("\\[");
        if is_dollar || is_bracket {
            let close = if is_dollar { "$$" } else { "\\]" };
            let after_open = &trimmed[2..];

            // A single-line formula does not need the sentinel block. `$$…$$`
            // is already understood by pulldown-cmark; `\[…\]` is rewritten
            // to the equivalent dollar form on the same line.
            if let Some(pos) = after_open.rfind(close) {
                if is_bracket {
                    let prefix = &line[..line.len() - trimmed.len()];
                    let tex = &after_open[..pos];
                    let rest = &after_open[pos + close.len()..];
                    out.push_str(&format!("{prefix}$${tex}$${rest}"));
                } else {
                    out.push_str(line);
                }
                i += 1;
                continue;
            }

            // Collect lines until the closing delimiter. Text after the
            // delimiter on that line is kept as ordinary source.
            let mut chunk = String::from(after_open);
            let mut closing = None;
            for (j, l) in lines.iter().enumerate().skip(i + 1) {
                if let Some(pos) = l.rfind(close) {
                    chunk.push_str(&l[..pos]);
                    closing = Some((j, &l[pos + close.len()..]));
                    break;
                }
                chunk.push_str(l);
            }
            let Some((end, rest)) = closing else {
                // Unclosed delimiter: leave the source untouched.
                out.push_str(line);
                i += 1;
                continue;
            };
            let tex = chunk.trim();
            if tex.is_empty() {
                // An empty formula produces no block; keep the line count.
                for _ in i..=end {
                    out.push('\n');
                }
            } else {
                out.push_str(&format!("~~~~{MATH_LANG}\n{tex}\n~~~~\n"));
            }
            if !rest.trim().is_empty() {
                out.push_str(rest);
            }
            i = end + 1;
            continue;
        }
        out.push_str(line);
        i += 1;
    }
    out
}

/// Replaces `<details>…</details>` with a sentinel fenced block so that its
/// blank lines and Markdown body survive pulldown-cmark's HTML block rules.
fn protect_details(md: &str) -> String {
    let lower = md.to_ascii_lowercase();
    let mut out = String::with_capacity(md.len());
    let mut i = 0usize;
    let mut fence = FenceTracker::default();
    while i < md.len() {
        let line_end = md[i..].find('\n').map_or(md.len(), |p| i + p + 1);
        let line = &md[i..line_end];
        if fence.feed(line) {
            out.push_str(line);
            i = line_end;
            continue;
        }
        if let Some(rel) = lower[i..line_end].find("<details") {
            let abs = i + rel;
            if let Some(cend) = lower[abs..].find("</details>") {
                let tag_end = cend + "</details>".len();
                let block = &md[abs..abs + tag_end];
                // The opening tag ends at the first `>` before the closing tag.
                let open_end = block[..cend].find('>').map_or(cend, |p| p + 1);
                let open = block[..open_end].to_ascii_lowercase().contains("open");
                let (summary, body) = split_summary(&block[open_end..cend]);
                let lang = if open {
                    DETAILS_OPEN_LANG
                } else {
                    DETAILS_LANG
                };
                let content = format!("{}\n{}", summary.trim(), body.trim());
                let marker = sentinel_fence(content.lines());
                let replacement = format!("{marker}{lang}\n{content}\n{marker}\n");
                // Pad with blank lines so the replacement occupies exactly as
                // many source lines as the original element. The newline after
                // `</details>` is copied separately by the outer loop, hence
                // the `+ 1`. This keeps click-to-source line numbers correct.
                let before = &md[i..abs];
                out.push_str(before);
                let mut newlines = replacement.matches('\n').count();
                if !before.trim().is_empty() {
                    // The sentinel fence must start its own line.
                    out.push('\n');
                    newlines += 1;
                }
                out.push_str(&replacement);
                let missing = block.matches('\n').count().saturating_sub(newlines) + 1;
                for _ in 0..missing {
                    out.push('\n');
                }
                i = abs + tag_end;
                continue;
            }
        }
        out.push_str(line);
        i = line_end;
    }
    out
}

/// Splits the inside of a `<details>` element into its summary and body.
fn split_summary(inner: &str) -> (String, String) {
    let lower = inner.to_ascii_lowercase();
    if let Some(start) = lower.find("<summary")
        && let Some(gt) = inner[start..].find('>')
    {
        let content_start = start + gt + 1;
        if let Some(end_rel) = inner[content_start..]
            .to_ascii_lowercase()
            .find("</summary>")
        {
            let summary = strip_tags(&inner[content_start..content_start + end_rel]);
            let mut body = String::from(&inner[..start]);
            body.push_str(&inner[content_start + end_rel + "</summary>".len()..]);
            return (summary, body);
        }
    }
    (String::new(), inner.to_string())
}

/// Removes simple HTML tags from a string.
fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// State machine over the materialized event stream.
///
/// `pos` points at the next event that has not been consumed yet. All methods
/// uphold the invariant that they either advance `pos` or return cleanly (see
/// the module docs, section "Invariants").
struct Walker {
    events: Vec<Event<'static>>,
    pos: usize,
    footnotes: BTreeMap<String, Vec<Block>>,
    metadata: Vec<(String, String)>,
}

/// Span-level tags (may appear inside inline runs).
///
/// Everything that returns `true` here is consumed by
/// [`Walker::parse_inlines`]; all other `Tag`s count as block boundaries and
/// are left for the block parser. New inline styles must be added here.
fn is_span_tag(tag: &Tag<'_>) -> bool {
    matches!(
        tag,
        Tag::Emphasis
            | Tag::Strong
            | Tag::Strikethrough
            | Tag::Superscript
            | Tag::Subscript
            | Tag::Link { .. }
            | Tag::Image { .. }
    )
}

/// What terminates an inline run (used by [`Walker::parse_inlines_until`]).
#[derive(Clone)]
enum InlineStop {
    /// A pulldown-cmark end tag (consumed when matched).
    Events(Option<TagEnd>),
    /// A closing HTML tag name such as `"mark"` (consumed when matched).
    Html(String),
}

impl Walker {
    fn peek(&self) -> Option<&Event<'static>> {
        self.events.get(self.pos)
    }

    /// Collects blocks at the top level of the current container.
    /// Stops (WITHOUT consuming the end event) when the container ends.
    ///
    /// Invariant (critical against infinite loops/OOM): every loop iteration
    /// must either advance `pos` or leave the loop. The defensive stray-inline
    /// branch below verifies this explicitly.
    #[allow(clippy::while_let_loop)] // deliberately `loop` + `match`, see invariant above
    fn blocks(&mut self) -> Vec<Block> {
        let mut out = Vec::new();
        loop {
            let Some(ev) = self.peek().cloned() else {
                break;
            };
            match ev {
                // End tag of the surrounding container → return
                Event::End(_) => break,
                Event::Rule => {
                    self.pos += 1;
                    out.push(Block::Rule);
                }
                Event::Start(tag) => {
                    // A tight list item may start directly with an inline span
                    // (`*   **Bold**: text`) instead of a Paragraph. Consume
                    // the whole inline run; consuming only the span start used
                    // to drop the span and everything after it.
                    if is_span_tag(&tag) {
                        let before = self.pos;
                        let inlines = self.parse_inlines(None);
                        if self.pos == before {
                            self.pos += 1;
                        }
                        if !inlines.is_empty() {
                            out.push(Block::Paragraph(inlines));
                        }
                        continue;
                    }
                    self.pos += 1;
                    if let Some(b) = self.container(tag) {
                        out.push(b);
                    }
                }
                // "Stray" inline events (defensive) → small paragraph.
                // IMPORTANT: the inline parser *must* consume at least one
                // event, otherwise we loop forever — enforce progress here.
                _ => {
                    let before = self.pos;
                    let inlines = self.parse_inlines(None);
                    if self.pos == before {
                        // Nothing consumed (e.g. a raw HTML event) →
                        // consume it hard so we cannot get stuck.
                        self.pos += 1;
                    }
                    if !inlines.is_empty() {
                        out.push(Block::Paragraph(inlines));
                    }
                }
            }
        }
        out
    }

    /// Top-level blocks up to the end of the event stream. A stray end tag
    /// (unbalanced HTML) is skipped instead of hiding the rest of the document.
    fn document(&mut self) -> Vec<Block> {
        let mut out = self.blocks();
        while self.peek().is_some() {
            self.pos += 1;
            out.extend(self.blocks());
        }
        out
    }

    /// Handles a container start (the start event is already consumed).
    ///
    /// Returning `None` means: the tag does not produce a block of its own
    /// (e.g. a footnote definition, which is stored in `self.footnotes`).
    fn container(&mut self, tag: Tag<'static>) -> Option<Block> {
        match tag {
            Tag::Paragraph => {
                let inlines = self.parse_inlines(Some(TagEnd::Paragraph));
                // A paragraph that consists only of display math becomes a
                // dedicated block so the preview can center/typeset it.
                let mut significant = inlines
                    .iter()
                    .filter(|i| !matches!(i, Inline::Text(t) if t.trim().is_empty()));
                if let Some(Inline::Math { tex, display: true }) = significant.next()
                    && significant.next().is_none()
                {
                    return Some(Block::Math(tex.trim().to_string()));
                }
                Some(Block::Paragraph(inlines))
            }
            Tag::Heading { level, .. } => {
                let mut inlines = self.parse_inlines(Some(TagEnd::Heading(level)));
                strip_heading_attributes(&mut inlines);
                Some(Block::Heading {
                    level: level as u8,
                    inlines,
                })
            }
            Tag::CodeBlock(kind) => Some(self.code_block(kind)),
            Tag::HtmlBlock => self.html_block(),
            Tag::BlockQuote(kind) => {
                let blocks = self.blocks();
                self.pos += 1; // End(BlockQuote)
                let mut quote = Block::BlockQuote {
                    kind: kind.map(alert_from),
                    title: None,
                    fold: None,
                    blocks,
                };
                apply_callout(&mut quote);
                Some(quote)
            }
            Tag::List(start) => Some(self.list(start)),
            Tag::DefinitionList => Some(self.definition_list()),
            Tag::Table(aligns) => Some(self.table(aligns)),
            Tag::FootnoteDefinition(name) => {
                let inner = self.blocks();
                self.pos += 1; // End(FootnoteDefinition)
                self.footnotes
                    .entry(name.to_string())
                    .or_default()
                    .extend(inner);
                None
            }
            Tag::MetadataBlock(_) => {
                self.metadata_block();
                None
            }
            // Span tags should not appear here; skip defensively.
            _ => None,
        }
    }

    /// Reads a YAML/TOML metadata block into `self.metadata`.
    ///
    /// Fallback for metadata blocks that [`extract_front_matter`] did not
    /// consume. Only `key: value` pairs and `- item` lines are read, and nested
    /// keys are not prefixed, so no YAML parser is needed.
    fn metadata_block(&mut self) {
        let mut raw = String::new();
        loop {
            match self.peek().cloned() {
                Some(Event::Text(t)) | Some(Event::Html(t)) => {
                    self.pos += 1;
                    raw.push_str(&t);
                }
                Some(Event::End(TagEnd::MetadataBlock(_))) => {
                    self.pos += 1;
                    break;
                }
                None => break,
                Some(_) => {
                    self.pos += 1;
                }
            }
        }
        for line in raw.lines() {
            let line = line.trim_end();
            if line.trim().is_empty() {
                continue;
            }
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim();
                if key.is_empty() {
                    continue;
                }
                let value = value
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string();
                self.metadata.push((key.to_string(), value));
            } else if line.trim_start().starts_with('-') {
                // Array item of the previous key.
                if let Some((_, value)) = self.metadata.last_mut() {
                    let item = line
                        .trim_start()
                        .trim_start_matches('-')
                        .trim()
                        .trim_matches('"');
                    if !value.is_empty() {
                        value.push_str(", ");
                    }
                    value.push_str(item);
                }
            }
        }
    }

    /// Reads a fenced/indented code block up to `End(CodeBlock)`.
    ///
    /// The language tag is the first word of the info string (e.g. ```rust).
    /// A trailing newline is removed because the renderers work line by line.
    ///
    /// Sentinel languages injected by [`preprocess`] are translated here:
    /// `mdviewer-toc` → [`Block::Toc`], `mdviewer-div` → [`Block::FencedDiv`],
    /// `mdviewer-math` → [`Block::Math`], `mdviewer-details` and
    /// `mdviewer-details-open` → [`Block::Details`].
    fn code_block(&mut self, kind: CodeBlockKind<'static>) -> Block {
        let info = match &kind {
            CodeBlockKind::Fenced(info) => info.trim().to_string(),
            CodeBlockKind::Indented => String::new(),
        };
        let lang = info
            .split([' ', '\t'])
            .next()
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let mut code = String::new();
        loop {
            match self.peek().cloned() {
                Some(Event::Text(t)) => {
                    self.pos += 1;
                    code.push_str(&t);
                }
                Some(Event::End(TagEnd::CodeBlock)) => {
                    // Consume the end tag so `blocks()` can continue with the
                    // next block (otherwise the rest of the document is lost).
                    self.pos += 1;
                    break;
                }
                None => break,
                Some(_) => {
                    self.pos += 1; // skip unexpected events
                }
            }
        }
        // Remove trailing newline (the renderer adds its own).
        let code = code.trim_end_matches('\n').to_string();
        match lang.as_deref() {
            Some(TOC_LANG) => Block::Toc(Vec::new()),
            Some(DIV_LANG) => {
                let (info, body) = code.split_once('\n').unwrap_or((&code, ""));
                Block::FencedDiv {
                    class: info.trim().to_string(),
                    blocks: self.parse_nested(body),
                }
            }
            Some(MATH_LANG) => Block::Math(code),
            Some(DETAILS_LANG) | Some(DETAILS_OPEN_LANG) => {
                let mut lines = code.splitn(2, '\n');
                let summary = lines.next().unwrap_or_default().trim().to_string();
                let body = lines.next().unwrap_or_default();
                Block::Details {
                    summary: if summary.is_empty() {
                        "Details".into()
                    } else {
                        summary
                    },
                    open: lang.as_deref() == Some(DETAILS_OPEN_LANG),
                    blocks: self.parse_nested(body),
                }
            }
            Some("mermaid") => Block::Mermaid(code),
            Some("math") | Some("latex") | Some("tex") => Block::Math(code),
            _ => Block::CodeBlock { lang, code },
        }
    }

    /// Parses the body of a sentinel block as its own document. Footnote
    /// definitions inside the body belong to the enclosing document.
    fn parse_nested(&mut self, body: &str) -> Vec<Block> {
        let nested = parse(body);
        for (label, blocks) in nested.footnotes {
            self.footnotes.entry(label).or_default().extend(blocks);
        }
        nested.blocks
    }

    /// Reads a raw HTML block.
    ///
    /// A small, safe subset is interpreted:
    /// * `<img …>` → an image block with explicit dimensions,
    /// * `<table>…</table>` → a GFM table,
    /// * comments and `<script>`/`<style>` are dropped (also handled earlier
    ///   by [`preprocess`] so they never reach this point),
    /// * everything else remains [`Block::Html`] and is shown as sanitized
    ///   source (never executed).
    fn html_block(&mut self) -> Option<Block> {
        let mut html = String::new();
        loop {
            match self.peek().cloned() {
                Some(Event::Html(t)) => {
                    self.pos += 1;
                    html.push_str(&t);
                }
                Some(Event::End(TagEnd::HtmlBlock)) => {
                    self.pos += 1;
                    break;
                }
                None => break,
                Some(_) => {
                    self.pos += 1;
                }
            }
        }
        let html = html.trim().to_string();
        let lower = html.to_ascii_lowercase();
        if lower.starts_with("<!--") || lower.starts_with("<script") || lower.starts_with("<style")
        {
            return None;
        }
        if lower.starts_with("<svg")
            && let Some(uri) = svg_to_png_data_uri(&html)
        {
            return Some(Block::Image {
                image: Inline::Image {
                    alt: "SVG image".into(),
                    url: uri,
                    title: None,
                    width: None,
                    height: None,
                },
                center: false,
            });
        }
        if lower.starts_with("<img")
            && let Some(img) = parse_html_img(&html)
        {
            return Some(Block::Image {
                image: img,
                center: false,
            });
        }
        // GitHub-style centered image: `<p align="center"><img …></p>`.
        if (lower.starts_with("<p") || lower.starts_with("<div"))
            && let Some((img, center)) = parse_html_image_wrapper(&html)
        {
            return Some(Block::Image { image: img, center });
        }
        if lower.starts_with("<table")
            && let Some((aligns, head, rows)) = parse_html_table(&html)
        {
            return Some(Block::Table { aligns, head, rows });
        }
        for (tag, kind) in [("video", "Video"), ("audio", "Audio"), ("iframe", "Embed")] {
            if lower.starts_with(&format!("<{tag}"))
                && let Some(url) = html_attr(&html, "src")
                && !url.trim().is_empty()
            {
                return Some(Block::Media {
                    kind: kind.to_string(),
                    url,
                });
            }
        }
        if html.is_empty() {
            return None;
        }
        // Styled `<div>`/`<section>` banners and other safe block HTML are
        // converted into regular blocks instead of being shown as source.
        if let Some(blocks) = parse_html_block_content(&html) {
            return Some(Block::Group(blocks));
        }
        Some(Block::Html(html))
    }

    /// Reads a definition list (`term` followed by `: definition`).
    fn definition_list(&mut self) -> Block {
        let mut items: Vec<DefinitionItem> = Vec::new();
        loop {
            match self.peek().cloned() {
                Some(Event::Start(Tag::DefinitionListTitle)) => {
                    self.pos += 1;
                    let term = self.parse_inlines(Some(TagEnd::DefinitionListTitle));
                    items.push(DefinitionItem {
                        term,
                        definitions: Vec::new(),
                    });
                }
                Some(Event::Start(Tag::DefinitionListDefinition)) => {
                    self.pos += 1;
                    let blocks = self.blocks();
                    self.pos += 1; // End(DefinitionListDefinition)
                    if let Some(last) = items.last_mut() {
                        last.definitions.push(blocks);
                    }
                }
                Some(Event::End(TagEnd::DefinitionList)) | None => {
                    self.pos += 1;
                    break;
                }
                Some(_) => {
                    self.pos += 1;
                }
            }
        }
        Block::DefinitionList(items)
    }

    /// Reads an (un)ordered list including nested lists.
    ///
    /// `start` is `Some(n)` for ordered lists (`1.`, `3)` …), otherwise
    /// `None`. Task markers (`- [ ]` / `- [x]`) appear right after
    /// `Start(Item)` and are consumed before the item blocks.
    ///
    /// Note on tight lists: an item's content may arrive directly as `Text`
    /// events (without a `Paragraph` wrapper); `blocks()` groups such stray
    /// inline events into a paragraph.
    fn list(&mut self, start: Option<u64>) -> Block {
        let ordered = start.is_some();
        let mut tight = true;
        let mut items = Vec::new();
        loop {
            match self.peek().cloned() {
                Some(Event::Start(Tag::Item)) => {
                    self.pos += 1;
                    let mut item = ListItem::default();
                    // Task-list marker sits at the start of the item.
                    while matches!(self.peek(), Some(Event::TaskListMarker(_))) {
                        if let Some(Event::TaskListMarker(b)) = self.peek().cloned() {
                            self.pos += 1;
                            item.task = Some(b);
                        }
                    }
                    tight &= !self.item_has_paragraph();
                    item.blocks = self.blocks();
                    self.pos += 1; // End(Item)
                    items.push(item);
                }
                Some(Event::End(TagEnd::List(_))) | None => {
                    self.pos += 1;
                    break;
                }
                Some(_) => {
                    self.pos += 1;
                }
            }
        }
        Block::List {
            ordered,
            start: start.unwrap_or(1),
            tight,
            items,
        }
    }

    /// True if the current list item contains a `Paragraph` event.
    ///
    /// pulldown-cmark wraps item content in paragraphs only for *loose* lists
    /// (items separated by blank lines); tight items arrive as bare inline
    /// events. Scanning ahead lets the renderer keep tight lists compact
    /// without threading a flag through `blocks()`.
    fn item_has_paragraph(&self) -> bool {
        let mut depth = 0usize;
        for ev in &self.events[self.pos..] {
            match ev {
                Event::Start(Tag::Item) => depth += 1,
                Event::End(TagEnd::Item) => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                Event::Start(Tag::Paragraph) if depth == 0 => return true,
                _ => {}
            }
        }
        false
    }

    /// Reads a GFM table: optional alignments, header row, body.
    ///
    /// The alignments (`:---`, `:---:`, `---:`) arrive as `Vec<Alignment>`
    /// from pulldown-cmark and are translated into [`TableAlign`].
    fn table(&mut self, aligns: Vec<Alignment>) -> Block {
        let align_v: Vec<TableAlign> = aligns.iter().map(|a| align_from(*a)).collect();
        let mut head: Vec<Vec<TableCell>> = Vec::new();
        let mut rows: Vec<Vec<TableCell>> = Vec::new();
        loop {
            match self.peek().cloned() {
                Some(Event::Start(Tag::TableHead)) => {
                    self.pos += 1;
                    head = vec![self.cells()];
                    self.pos += 1; // End(TableHead)
                    // The body follows directly after the head.
                }
                Some(Event::Start(Tag::TableRow)) => {
                    self.pos += 1;
                    rows.push(self.cells());
                    self.pos += 1; // End(TableRow)
                }
                Some(Event::End(TagEnd::Table)) | None => {
                    self.pos += 1;
                    break;
                }
                Some(_) => {
                    self.pos += 1;
                }
            }
        }

        // GFM: the header row defines the column count. Body rows with more
        // cells have their excess ignored; rows with fewer cells are padded so
        // the renderers always see a rectangular grid.
        let ncols = head
            .first()
            .map(|row| row.iter().map(|c| c.colspan.max(1)).sum::<usize>())
            .unwrap_or(0)
            .max(1);
        for row in head.iter_mut().chain(rows.iter_mut()) {
            row.truncate(ncols);
            let used: usize = row.iter().map(|c| c.colspan.max(1)).sum();
            for _ in used..ncols {
                row.push(TableCell::new(Vec::new()));
            }
        }
        place_table_cells(&mut head, &mut rows);

        let mut aligns = align_v;
        aligns.resize(ncols, TableAlign::None);
        Block::Table { aligns, head, rows }
    }

    /// Reads all cells of the current table row.
    fn cells(&mut self) -> Vec<TableCell> {
        let mut out = Vec::new();
        while matches!(self.peek(), Some(Event::Start(Tag::TableCell))) {
            self.pos += 1;
            out.push(TableCell::new(self.parse_inlines(Some(TagEnd::TableCell))));
        }
        out
    }

    /// Collects inline events up to the consuming `stop` end tag.
    ///
    /// * With `Some(stop)`: the matching `End` event is consumed.
    /// * With `None`: stops **exclusively** before the next block event
    ///   (e.g. `Start(List)` in tight lists) or `End` event.
    ///
    /// Block starts are returned *without* moving the cursor — this is
    /// crucial: a `pos -= 1` here would make the cursor run backwards and
    /// cause an infinite loop (memory explosion) in `blocks()`.
    fn parse_inlines(&mut self, stop: Option<TagEnd>) -> Vec<Inline> {
        self.parse_inlines_until(InlineStop::Events(stop))
    }

    /// Collects inline events until the matching `</tag>` HTML event.
    fn parse_inlines_html(&mut self, tag: &str) -> Vec<Inline> {
        self.parse_inlines_until(InlineStop::Html(tag.to_ascii_lowercase()))
    }

    fn parse_inlines_until(&mut self, stop: InlineStop) -> Vec<Inline> {
        let mut out: Vec<Inline> = Vec::new();
        loop {
            match self.peek().cloned() {
                None => return out,
                Some(Event::End(t)) => {
                    if matches!(&stop, InlineStop::Events(Some(s)) if *s == t) {
                        self.pos += 1;
                    }
                    return out;
                }
                Some(Event::Start(tag)) => {
                    // Only handle real span-level tags; leave block starts
                    // (e.g. nested lists in tight lists) untouched for the
                    // block parser.
                    if !is_span_tag(&tag) {
                        return out;
                    }
                    self.pos += 1;
                    match tag {
                        Tag::Emphasis => {
                            out.push(Inline::Emph(self.parse_inlines(Some(TagEnd::Emphasis))))
                        }
                        Tag::Strong => {
                            out.push(Inline::Strong(self.parse_inlines(Some(TagEnd::Strong))))
                        }
                        Tag::Strikethrough => out.push(Inline::Strike(
                            self.parse_inlines(Some(TagEnd::Strikethrough)),
                        )),
                        Tag::Superscript => {
                            out.push(Inline::Super(self.parse_inlines(Some(TagEnd::Superscript))))
                        }
                        Tag::Subscript => {
                            out.push(Inline::Sub(self.parse_inlines(Some(TagEnd::Subscript))))
                        }
                        Tag::Link {
                            dest_url,
                            link_type,
                            ..
                        } => {
                            let inlines = self.parse_inlines(Some(TagEnd::Link));
                            if matches!(link_type, pulldown_cmark::LinkType::WikiLink { .. }) {
                                let label: String =
                                    inlines.iter().map(|i| i.plain_text()).collect();
                                out.push(Inline::WikiLink {
                                    target: dest_url.to_string(),
                                    label,
                                });
                            } else {
                                out.push(Inline::Link {
                                    inlines,
                                    url: dest_url.to_string(),
                                });
                            }
                        }
                        Tag::Image {
                            dest_url,
                            title,
                            link_type,
                            ..
                        } => {
                            let inner = self.parse_inlines(Some(TagEnd::Image));
                            let alt = inner.iter().map(|i| i.plain_text()).collect::<String>();
                            let url = dest_url.to_string();
                            if matches!(link_type, pulldown_cmark::LinkType::WikiLink { .. }) {
                                if is_image_target(&url) {
                                    out.push(Inline::Image {
                                        alt,
                                        url,
                                        title: None,
                                        width: None,
                                        height: None,
                                    });
                                } else {
                                    out.push(Inline::WikiEmbed {
                                        target: url,
                                        label: alt,
                                    });
                                }
                            } else {
                                out.push(Inline::Image {
                                    alt,
                                    url,
                                    title: (!title.is_empty()).then(|| title.to_string()),
                                    width: None,
                                    height: None,
                                });
                            }
                        }
                        // `is_span_tag` admits no other tags.
                        _ => {}
                    }
                }
                Some(Event::Text(t)) => {
                    self.pos += 1;
                    // Merge consecutive text fragments.
                    if let Some(Inline::Text(prev)) = out.last_mut() {
                        prev.push_str(&t);
                    } else {
                        out.push(Inline::Text(t.to_string()));
                    }
                }
                Some(Event::Code(c)) => {
                    self.pos += 1;
                    out.push(Inline::Code(c.to_string()));
                }
                Some(Event::InlineMath(c)) => {
                    self.pos += 1;
                    out.push(Inline::Math {
                        tex: clean_math(&c),
                        display: false,
                    });
                }
                Some(Event::DisplayMath(c)) => {
                    self.pos += 1;
                    out.push(Inline::Math {
                        tex: clean_math(&c),
                        display: true,
                    });
                }
                Some(Event::SoftBreak) => {
                    self.pos += 1;
                    out.push(Inline::Text(SOFT_BREAK.to_string()));
                }
                Some(Event::HardBreak) => {
                    self.pos += 1;
                    out.push(Inline::Text("\n".into()));
                }
                Some(Event::FootnoteReference(name)) => {
                    self.pos += 1;
                    out.push(Inline::FootnoteRef {
                        label: name.to_string(),
                        number: 0,
                    });
                }
                Some(Event::InlineHtml(h)) => {
                    self.pos += 1;
                    let trimmed = h.trim();
                    if let InlineStop::Html(tag) = &stop
                        && html_is_close(trimmed, tag)
                    {
                        return out;
                    }
                    self.inline_html(trimmed, &mut out);
                }
                // Anything else (block start) → stop.
                Some(_) => return out,
            }
        }
    }

    /// Interprets one raw inline-HTML token.
    ///
    /// Only a safe subset is understood. Unknown tags are transparent (their
    /// content is kept), while scripting/embedding elements are dropped
    /// together with their content.
    fn inline_html(&mut self, html: &str, out: &mut Vec<Inline>) {
        let lower = html.to_ascii_lowercase();
        if lower.starts_with("<!--") {
            // Multi-token comments: consume until the closing marker.
            if !lower.contains("-->") {
                self.skip_html_until("-->");
            }
            return;
        }
        if let Some((name, closing)) = html_tag_name(html) {
            if closing {
                return; // stray closing tag
            }
            // Elements that must never be interpreted (and whose content is
            // not shown either).
            if matches!(
                name.as_str(),
                "script" | "style" | "iframe" | "object" | "embed" | "svg" | "math"
            ) {
                self.skip_html_until(&format!("</{name}>"));
                return;
            }
            match name.as_str() {
                "br" | "hr" | "wbr" => {
                    if name != "wbr" {
                        out.push(Inline::Text("\n".into()));
                    }
                }
                "img" => {
                    if let Some(img) = parse_html_img(html) {
                        out.push(img);
                    }
                }
                // Void and self-closing tags never have content to consume.
                _ if lower.ends_with("/>")
                    || matches!(
                        name.as_str(),
                        "area"
                            | "base"
                            | "col"
                            | "input"
                            | "link"
                            | "meta"
                            | "param"
                            | "source"
                            | "track"
                    ) => {}
                _ => {
                    let inner = self.parse_inlines_html(&name);
                    match name.as_str() {
                        "mark" => out.push(Inline::Highlight(inner)),
                        "kbd" => out.push(Inline::Kbd(inner)),
                        "b" | "strong" => out.push(Inline::Strong(inner)),
                        "i" | "em" | "cite" | "dfn" => out.push(Inline::Emph(inner)),
                        "s" | "del" | "strike" => out.push(Inline::Strike(inner)),
                        "u" | "ins" => out.push(Inline::Underline(inner)),
                        "abbr" => {
                            let title = html_attr(html, "title").unwrap_or_default();
                            let text: String = inner.iter().map(|i| i.plain_text()).collect();
                            if !text.is_empty() && !title.is_empty() {
                                out.push(Inline::Abbr { text, title });
                            } else {
                                out.extend(inner);
                            }
                        }
                        "sup" => out.push(Inline::Super(inner)),
                        "sub" => out.push(Inline::Sub(inner)),
                        "code" => {
                            let text: String = inner.iter().map(|i| i.plain_text()).collect();
                            out.push(Inline::Code(text));
                        }
                        "a" => {
                            let href = html_attr(html, "href").unwrap_or_default();
                            if href.is_empty() {
                                out.extend(inner);
                            } else {
                                out.push(Inline::Link {
                                    inlines: inner,
                                    url: href,
                                });
                            }
                        }
                        // Unknown/structural tags are transparent.
                        _ => out.extend(inner),
                    }
                }
            }
        }
    }

    /// Consumes inline events up to and including the HTML token that contains
    /// `marker`. Stops before any start or end tag without consuming it, so the
    /// enclosing parser still sees its own boundaries.
    fn skip_html_until(&mut self, marker: &str) {
        let marker = marker.to_ascii_lowercase();
        while let Some(ev) = self.peek().cloned() {
            match ev {
                Event::Start(_) | Event::End(_) => break,
                Event::InlineHtml(h) => {
                    self.pos += 1;
                    if h.to_ascii_lowercase().contains(&marker) {
                        break;
                    }
                }
                _ => self.pos += 1,
            }
        }
    }
}

/// True if an HTML token is a closing tag for `tag`.
fn html_is_close(html: &str, tag: &str) -> bool {
    let Some((name, closing)) = html_tag_name(html) else {
        return false;
    };
    closing && name == tag
}

/// Extracts `(tag_name_lowercase, is_closing)` from an HTML tag token.
fn html_tag_name(html: &str) -> Option<(String, bool)> {
    let s = html.trim();
    if !s.starts_with('<') || s.starts_with("<!") || s.starts_with("<?") {
        return None;
    }
    let mut rest = &s[1..];
    let closing = rest.starts_with('/');
    if closing {
        rest = &rest[1..];
    }
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect::<String>()
        .to_ascii_lowercase();
    (!name.is_empty()).then_some((name, closing))
}

/// Lowercases a tag and blanks out the content of quoted attribute values, so
/// attribute names are only found outside values. Byte offsets are preserved.
fn mask_quoted_values(tag: &str) -> Option<String> {
    let mut bytes = tag.to_ascii_lowercase().into_bytes();
    let mut quote: Option<u8> = None;
    let mut after_eq = false;
    for b in bytes.iter_mut() {
        if let Some(q) = quote {
            if *b == q {
                quote = None;
            } else {
                *b = b' ';
            }
        } else if after_eq && matches!(*b, b'"' | b'\'') {
            quote = Some(*b);
        }
        after_eq = match *b {
            b'=' => true,
            c if c.is_ascii_whitespace() => after_eq,
            _ => false,
        };
    }
    String::from_utf8(bytes).ok()
}

/// Reads an attribute value from a raw HTML tag (case-insensitive). Names that
/// occur inside quoted values are ignored.
fn html_attr(tag: &str, attr: &str) -> Option<String> {
    // The masked copy keeps byte offsets, so attribute names are located in it
    // and values are sliced from the original tag verbatim.
    let lower = mask_quoted_values(tag)?;
    let mut start = 0usize;
    while let Some(pos) = lower[start..].find(attr) {
        let abs = start + pos;
        let before_ok = abs == 0
            || lower[..abs]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_whitespace());
        let after = tag[abs + attr.len()..].trim_start();
        if before_ok && after.starts_with('=') {
            let v = after[1..].trim_start();
            let quote = v.chars().next()?;
            if quote == '"' || quote == '\'' {
                let end = v[1..].find(quote)?;
                return Some(v[1..1 + end].to_string());
            }
            let end = v
                .find(|c: char| c.is_whitespace() || c == '>')
                .unwrap_or(v.len());
            return Some(v[..end].to_string());
        }
        start = abs + attr.len();
    }
    None
}

// ---------------------------------------------------------------------------
// Safe block-level HTML → document model
// ---------------------------------------------------------------------------

/// Tags that start a new block when they appear in raw HTML.
fn is_html_block_tag(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "section"
            | "article"
            | "main"
            | "header"
            | "footer"
            | "aside"
            | "figure"
            | "figcaption"
            | "center"
            | "ul"
            | "ol"
            | "li"
            | "blockquote"
            | "pre"
            | "hr"
            | "table"
            | "details"
    ) || heading_level(name).is_some()
}

fn heading_level(name: &str) -> Option<u8> {
    match name {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

/// Accumulates inline runs between block elements, collapsing HTML
/// whitespace (source indentation/newlines) the way a browser would.
#[derive(Default)]
struct HtmlInlineRun {
    items: Vec<Inline>,
    pending_space: bool,
}

impl HtmlInlineRun {
    fn text(&mut self, raw: &str) {
        let decoded = decode_html_entities(raw);
        let leading_space = decoded.starts_with(char::is_whitespace);
        let trailing_space = decoded.ends_with(char::is_whitespace);
        let text = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            if !self.items.is_empty() {
                self.pending_space = true;
            }
            return;
        }
        if (self.pending_space || leading_space) && !self.items.is_empty() {
            self.items.push(Inline::Text(" ".into()));
        }
        self.items.push(Inline::Text(text));
        self.pending_space = trailing_space;
    }

    fn extend(&mut self, mut more: Vec<Inline>) {
        if more.is_empty() {
            return;
        }
        if self.pending_space && !self.items.is_empty() {
            self.items.push(Inline::Text(" ".into()));
        }
        self.items.append(&mut more);
        self.pending_space = false;
    }

    fn flush(&mut self, out: &mut Vec<Block>) {
        self.pending_space = false;
        if self.items.is_empty() {
            return;
        }
        let mut items = std::mem::take(&mut self.items);
        if let Some(Inline::Text(t)) = items.first_mut() {
            *t = t.trim_start().to_string();
        }
        if let Some(Inline::Text(t)) = items.last_mut() {
            *t = t.trim_end().to_string();
        }
        items.retain(|i| !matches!(i, Inline::Text(t) if t.is_empty()));
        if !items.is_empty() {
            out.push(Block::Paragraph(items));
        }
    }
}

/// Converts the children of an HTML element into blocks.
fn convert_html_children(
    parser: &tl::Parser<'_>,
    handles: &[tl::NodeHandle],
    out: &mut Vec<Block>,
) {
    let mut run = HtmlInlineRun::default();
    for &handle in handles {
        let Some(node) = handle.get(parser) else {
            continue;
        };
        match node {
            tl::Node::Raw(text) => run.text(text.as_utf8_str().as_ref()),
            tl::Node::Comment(_) => {}
            tl::Node::Tag(tag) => {
                let name = tag.name().as_utf8_str().to_ascii_lowercase();
                if is_html_block_tag(&name) {
                    run.flush(out);
                    convert_html_block_tag(parser, tag, &name, out);
                } else if name == "img" {
                    run.flush(out);
                    if let Some(image) = parse_html_img(&tag.outer_html(parser)) {
                        out.push(Block::Image {
                            image,
                            center: false,
                        });
                    }
                } else {
                    // Inline tags (`strong`, `em`, `span`, `mark`, `a`, …):
                    // reuse the existing Markdown/inline-HTML parser.
                    run.extend(parse_inline_fragment(&tag.outer_html(parser)));
                }
            }
        }
    }
    run.flush(out);
}

/// Converts one block-level HTML tag into document blocks.
fn convert_html_block_tag(
    parser: &tl::Parser<'_>,
    tag: &tl::HTMLTag<'_>,
    name: &str,
    out: &mut Vec<Block>,
) {
    if let Some(level) = heading_level(name) {
        let inlines = parse_inline_fragment(&tag.inner_html(parser));
        if !inlines.is_empty() {
            out.push(Block::Heading { level, inlines });
        }
        return;
    }
    match name {
        "p" | "figcaption" => {
            let inlines = parse_inline_fragment(&tag.inner_html(parser));
            if !inlines.is_empty() {
                out.push(Block::Paragraph(inlines));
            }
        }
        "ul" | "ol" => {
            if let Some(list) = html_list(parser, tag, name == "ol") {
                out.push(list);
            }
        }
        "blockquote" => {
            let mut blocks = Vec::new();
            convert_html_children(parser, tag.children().top().as_slice(), &mut blocks);
            if !blocks.is_empty() {
                let mut quote = Block::BlockQuote {
                    kind: None,
                    title: None,
                    fold: None,
                    blocks,
                };
                apply_callout(&mut quote);
                out.push(quote);
            }
        }
        "pre" => out.push(html_pre(parser, tag)),
        "hr" => out.push(Block::Rule),
        "table" => {
            if let Some((aligns, head, rows)) = parse_html_table(&tag.outer_html(parser)) {
                out.push(Block::Table { aligns, head, rows });
            }
        }
        "details" => out.push(html_details(parser, tag)),
        // Containers (`div`, `section`, `figure`, `li`, unknown block tags)
        // are flattened: their children inherit the surrounding flow.
        _ => convert_html_children(parser, tag.children().top().as_slice(), out),
    }
}

/// Converts an HTML `<ul>`/`<ol>` into a list block. `<ol start="n">` sets the
/// first number.
fn html_list(parser: &tl::Parser<'_>, tag: &tl::HTMLTag<'_>, ordered: bool) -> Option<Block> {
    let start = tag
        .attributes()
        .get("start")
        .flatten()
        .and_then(|value| value.as_utf8_str().trim().parse::<u64>().ok())
        .unwrap_or(1);
    let mut items = Vec::new();
    for &handle in tag.children().top().as_slice() {
        let Some(node) = handle.get(parser) else {
            continue;
        };
        let Some(child) = node.as_tag() else {
            continue;
        };
        if !child.name().as_utf8_str().eq_ignore_ascii_case("li") {
            continue;
        }
        let mut blocks = Vec::new();
        convert_html_children(parser, child.children().top().as_slice(), &mut blocks);
        items.push(ListItem { blocks, task: None });
    }
    if items.is_empty() {
        return None;
    }
    Some(Block::List {
        ordered,
        start,
        tight: true,
        items,
    })
}

/// Converts `<pre>`/`<pre><code class="language-…">` into a code block.
fn html_pre(parser: &tl::Parser<'_>, tag: &tl::HTMLTag<'_>) -> Block {
    let mut lang = None;
    let mut code = String::new();
    if let Some(child) = tag
        .children()
        .top()
        .iter()
        .filter_map(|h| h.get(parser))
        .filter_map(|n| n.as_tag())
        .find(|t| t.name().as_utf8_str().eq_ignore_ascii_case("code"))
    {
        lang = child
            .attributes()
            .get("class")
            .flatten()
            .and_then(|classes| {
                classes
                    .as_utf8_str()
                    .split_whitespace()
                    .find_map(|c| c.strip_prefix("language-").map(str::to_string))
            });
        code = child.inner_text(parser).into_owned();
    }
    if code.trim().is_empty() {
        code = tag.inner_text(parser).into_owned();
    }
    let code = decode_html_entities(&code);
    let code = code
        .strip_prefix('\n')
        .unwrap_or(&code)
        .trim_end_matches('\n')
        .to_string();
    if lang.as_deref() == Some("mermaid") {
        Block::Mermaid(code)
    } else {
        Block::CodeBlock { lang, code }
    }
}

/// Converts an HTML `<details>` element (summary + body blocks).
fn html_details(parser: &tl::Parser<'_>, tag: &tl::HTMLTag<'_>) -> Block {
    let mut summary = "Details".to_string();
    let mut body: Vec<tl::NodeHandle> = Vec::new();
    for &handle in tag.children().top().as_slice() {
        let is_summary = handle
            .get(parser)
            .and_then(|n| n.as_tag())
            .is_some_and(|t| t.name().as_utf8_str().eq_ignore_ascii_case("summary"));
        if is_summary {
            if let Some(summary_tag) = handle.get(parser).and_then(|n| n.as_tag()) {
                let text = decode_html_entities(&summary_tag.inner_text(parser));
                let text = text.trim();
                if !text.is_empty() {
                    summary = text.to_string();
                }
            }
        } else {
            body.push(handle);
        }
    }
    let mut blocks = Vec::new();
    convert_html_children(parser, &body, &mut blocks);
    Block::Details {
        summary,
        open: tag.attributes().contains("open"),
        blocks,
    }
}

/// Converts a raw HTML block into regular blocks when only supported markup
/// is used. Returns `None` when nothing convertible is found (the caller then
/// keeps the sanitized source).
fn parse_html_block_content(html: &str) -> Option<Vec<Block>> {
    let dom = tl::parse(html, tl::ParserOptions::default()).ok()?;
    let mut out = Vec::new();
    convert_html_children(dom.parser(), dom.children(), &mut out);
    (!out.is_empty()).then_some(out)
}

/// Decodes the HTML entities that commonly appear in Markdown files.
fn decode_html_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        // Only the first 13 bytes can hold a short entity; scanning further
        // would make every `&` in a long text quadratic.
        let Some(end) = tail.as_bytes().iter().take(13).position(|&b| b == b';') else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let entity = &tail[1..end];
        let named = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            "mdash" => Some('\u{2014}'),
            "ndash" => Some('\u{2013}'),
            "hellip" => Some('\u{2026}'),
            "lsquo" => Some('\u{2018}'),
            "rsquo" => Some('\u{2019}'),
            "ldquo" => Some('\u{201c}'),
            "rdquo" => Some('\u{201d}'),
            "bull" => Some('\u{2022}'),
            "middot" => Some('\u{b7}'),
            "euro" => Some('\u{20ac}'),
            "larr" => Some('\u{2190}'),
            "rarr" => Some('\u{2192}'),
            "copy" => Some('\u{a9}'),
            "reg" => Some('\u{ae}'),
            "trade" => Some('\u{2122}'),
            "laquo" => Some('\u{ab}'),
            "raquo" => Some('\u{bb}'),
            "times" => Some('\u{d7}'),
            _ => None,
        };
        let decoded = named.or_else(|| {
            let number = entity.strip_prefix('#')?;
            let code = if let Some(hex) = number.strip_prefix(['x', 'X']) {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                number.parse::<u32>().ok()?
            };
            char::from_u32(code)
        });
        if let Some(c) = decoded {
            out.push(c);
            rest = &tail[end + 1..];
        } else {
            out.push('&');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

/// Rasterizes a safe SVG block to a PNG data URI (scripts are never executed
/// by resvg). Returns `None` for unsupported SVG.
pub(crate) fn svg_to_png_data_uri(svg: &str) -> Option<String> {
    let raster = crate::svg::rasterize(svg, 2.0, 2000.0)?;
    let png = raster.pixmap.encode_png().ok()?;
    Some(format!(
        "data:image/png;base64,{}",
        crate::html_export::base64_encode(&png)
    ))
}

/// Builds an image inline from a raw `<img …>` tag.
fn parse_html_img(tag: &str) -> Option<Inline> {
    let url = html_attr(tag, "src")?.trim().to_string();
    if url.is_empty() {
        return None;
    }
    let alt = html_attr(tag, "alt").unwrap_or_default();
    let title = html_attr(tag, "title");
    let dimension = |name: &str| {
        html_attr(tag, name)
            .and_then(|v| v.trim().trim_end_matches("px").trim().parse::<f32>().ok())
            .filter(|v| v.is_finite())
    };
    let width = dimension("width");
    let height = dimension("height");
    Some(Inline::Image {
        alt,
        url,
        title,
        width,
        height,
    })
}

/// Parses a block-level image wrapped in a simple `<p>`/`<div>` container,
/// e.g. GitHub's centered README icon:
///
/// ```html
/// <p align="center"><img src="assets/icon.png" width="180"></p>
/// ```
///
/// Returns `None` when the wrapper contains anything besides the image, so
/// arbitrary HTML is still shown as sanitized source.
fn parse_html_image_wrapper(html: &str) -> Option<(Inline, bool)> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<img")?;
    let gt = lower[start..].find('>')?;
    let img_end = start + gt + 1;
    let before = html[..start].trim();
    let after = html[img_end..].trim();

    // Only container tags may surround the image: no text, no other elements.
    let is_wrapper = |s: &str| {
        strip_tags(s).trim().is_empty()
            && s.split('<').skip(1).all(|tag| {
                let name: String = tag
                    .trim_start_matches('/')
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .collect();
                matches!(name.to_ascii_lowercase().as_str(), "p" | "div" | "picture")
            })
    };
    if !is_wrapper(before) || !is_wrapper(after) {
        return None;
    }

    let center = lower.contains("align=\"center\"")
        || lower.contains("align='center'")
        || lower.contains("text-align:center")
        || lower.contains("text-align: center");
    let image = parse_html_img(&html[start..img_end])?;
    Some((image, center))
}

/// Alignments, header rows and body rows of a converted HTML table.
type HtmlTable = (Vec<TableAlign>, Vec<Vec<TableCell>>, Vec<Vec<TableCell>>);
/// Converts an HTML table into the shared table model.
///
/// Supports `<thead>`/`<tbody>`, multiple header rows, `colspan`, `rowspan`,
/// inline formatting (`<b>`, `<i>`, `<code>`, `<mark>`, …) and nested Markdown
/// inside cells. Colours and other CSS are ignored (the app uses its own dark
/// palette), but the structure and text styling are preserved.
fn parse_html_table(html: &str) -> Option<HtmlTable> {
    let lower = html.to_ascii_lowercase();
    let thead = lower.find("<thead").and_then(|start| {
        lower[start..]
            .find("</thead>")
            .map(|end| (start, start + end))
    });

    let mut head: Vec<Vec<TableCell>> = Vec::new();
    let mut rows: Vec<Vec<TableCell>> = Vec::new();
    let mut pos = 0usize;
    while let Some(tr_rel) = lower[pos..].find("<tr") {
        let tr_start = pos + tr_rel;
        let Some(gt) = lower[tr_start..].find('>') else {
            break;
        };
        let content_start = tr_start + gt + 1;
        let Some(end_rel) = lower[content_start..].find("</tr>") else {
            break;
        };
        let content = &html[content_start..content_start + end_rel];
        let mut row: Vec<TableCell> = Vec::new();
        let mut cell_pos = 0usize;
        let lc = content.to_ascii_lowercase();
        while let Some(cell_rel) = lc[cell_pos..].find("<t") {
            let cs = cell_pos + cell_rel;
            let is_header = lc[cs..].starts_with("<th");
            let is_cell = is_header || lc[cs..].starts_with("<td");
            if !is_cell {
                break;
            }
            let Some(cgt) = lc[cs..].find('>') else { break };
            let open_tag = &content[cs..cs + cgt];
            let cstart = cs + cgt + 1;
            let close = if is_header { "</th>" } else { "</td>" };
            let Some(cend_rel) = lc[cstart..].find(close) else {
                break;
            };
            let raw = &content[cstart..cstart + cend_rel];
            let colspan = html_attr(open_tag, "colspan")
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(1)
                .clamp(1, 64);
            let rowspan = html_attr(open_tag, "rowspan")
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(1)
                .clamp(1, 512);
            row.push(TableCell::spanned(
                parse_inline_fragment(raw),
                colspan,
                rowspan,
            ));
            cell_pos = cstart + cend_rel + close.len();
        }
        if !row.is_empty() {
            let in_head = thead.is_some_and(|(s, e)| tr_start >= s && tr_start < e);
            if in_head {
                head.push(row);
            } else {
                rows.push(row);
            }
        }
        pos = content_start + end_rel + "</tr>".len();
    }
    if head.is_empty() && rows.is_empty() {
        return None;
    }
    // A table without <thead> uses its first row as the header (GFM-style).
    if head.is_empty() {
        head.push(rows.remove(0));
    }

    let ncols = place_table_cells(&mut head, &mut rows);
    let align = table_text_align(html);
    Some((vec![align.unwrap_or(TableAlign::None); ncols], head, rows))
}

/// Parses an inline Markdown/HTML fragment: HTML table cells, headings,
/// paragraphs and inline tags inside HTML blocks.
///
/// Reuses the normal inline event walker, so nested Markdown, `<b>`/`<i>`
/// styling, code, math, links and images all keep working. Text-level
/// extensions are not applied here; the enclosing document's
/// [`postprocess_blocks`] pass covers the returned inlines.
fn parse_inline_fragment(src: &str) -> Vec<Inline> {
    let src = strip_unsafe_html(src);
    let raw: Vec<Event<'static>> = Parser::new_ext(&src, parser_options())
        .map(Event::into_static)
        .collect();
    // A standalone fragment is parsed as one or more paragraphs. The inline
    // walker expects a bare inline run, so unwrap the paragraph events.
    let mut events: Vec<Event<'static>> = Vec::with_capacity(raw.len());
    for ev in raw {
        match ev {
            Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph) => {}
            other => events.push(other),
        }
    }
    let mut w = Walker {
        events,
        pos: 0,
        footnotes: BTreeMap::new(),
        metadata: Vec::new(),
    };
    let mut inlines = w.parse_inlines(None);
    if inlines.is_empty() {
        let text = strip_tags(&src).trim().to_string();
        if !text.is_empty() {
            inlines.push(Inline::Text(text));
        }
    }
    // Cells should not carry the surrounding indentation/newlines.
    if let Some(Inline::Text(t)) = inlines.first_mut() {
        *t = t.trim_start().to_string();
    }
    if let Some(Inline::Text(t)) = inlines.last_mut() {
        *t = t.trim_end().to_string();
    }
    inlines.retain(|i| !matches!(i, Inline::Text(t) if t.is_empty()));
    inlines
}

/// Resolves the 0-based grid column of every cell and returns the column
/// count. `rowspan`/`colspan` cells occupy their full rectangle, so later
/// cells in the same row are pushed to the next free column.
fn place_table_cells(head: &mut [Vec<TableCell>], rows: &mut [Vec<TableCell>]) -> usize {
    let total_rows = head.len() + rows.len();
    let mut occupied: Vec<Vec<bool>> = Vec::new();
    let mut ncols = 0usize;
    for (ri, row) in head.iter_mut().chain(rows.iter_mut()).enumerate() {
        if occupied.len() <= ri {
            occupied.resize(ri + 1, Vec::new());
        }
        let mut col = 0usize;
        for cell in row.iter_mut() {
            cell.colspan = cell.colspan.max(1);
            cell.rowspan = cell
                .rowspan
                .max(1)
                .min(total_rows.saturating_sub(ri).max(1));
            while occupied[ri].get(col).copied().unwrap_or(false) {
                col += 1;
            }
            for rr in ri..ri + cell.rowspan {
                if occupied.len() <= rr {
                    occupied.resize(rr + 1, Vec::new());
                }
                if occupied[rr].len() < col + cell.colspan {
                    occupied[rr].resize(col + cell.colspan, false);
                }
                for slot in occupied[rr].iter_mut().skip(col).take(cell.colspan) {
                    *slot = true;
                }
            }
            cell.col = col;
            ncols = ncols.max(col + cell.colspan);
            col += cell.colspan;
        }
    }
    ncols.max(1)
}

/// Reads `text-align` from a table's inline `style`/`align` attribute.
fn table_text_align(html: &str) -> Option<TableAlign> {
    let open_end = html.find('>')?;
    let tag = &html[..=open_end];
    if let Some(style) = html_attr(tag, "style") {
        let s = style
            .to_ascii_lowercase()
            .replace([' ', '\t', '\r', '\n'], "");
        if s.contains("text-align:center") {
            return Some(TableAlign::Center);
        }
        if s.contains("text-align:right") {
            return Some(TableAlign::Right);
        }
        if s.contains("text-align:left") {
            return Some(TableAlign::Left);
        }
    }
    match html_attr(tag, "align")
        .map(|v| v.to_ascii_lowercase())
        .as_deref()
    {
        Some("center") => Some(TableAlign::Center),
        Some("right") => Some(TableAlign::Right),
        Some("left") => Some(TableAlign::Left),
        _ => None,
    }
}

/// Drops the backtick wrapper GitHub uses for `$`…`$` inline math.
fn clean_math(tex: &str) -> String {
    let t = tex.trim();
    if t.len() >= 2 && t.starts_with('`') && t.ends_with('`') {
        return t[1..t.len() - 1].trim().to_string();
    }
    t.to_string()
}

/// Makes display math with top-level `\\` row breaks renderable.
///
/// MathJax/KaTeX (and therefore GitHub, Obsidian, …) accept `\\` directly in
/// `$$…$$` and centre each resulting row. `latex-rust` instead rejects a row
/// break that is not inside an environment with `RowBreakOutside`, so formulas
/// like
///
/// ```text
/// $$ a = b \\ \Rightarrow c = d $$
/// ```
///
/// used to fall back to raw source. Wrapping the formula in a single-column
/// `matrix` reproduces the browser layout: centred rows, no equation numbers,
/// display-style fractions. Row breaks inside nested environments (e.g.
/// `cases`, `aligned`, a nested `matrix`) are left untouched, and an already
/// wrapped formula is returned unchanged, so the function is idempotent.
///
/// This is applied at render time (preview and PDF) rather than while parsing,
/// so the document model keeps the original source for click-to-source mapping
/// and for the plain-source fallback.
pub fn normalize_display_math(tex: &str) -> String {
    let t = tex.trim();
    if !has_top_level_row_break(t) {
        return t.to_string();
    }
    let body = trim_trailing_row_breaks(t);
    format!("\\begin{{matrix}}\n{body}\n\\end{{matrix}}")
}

/// True if `tex` contains a `\\` row break outside braces and environments.
fn has_top_level_row_break(tex: &str) -> bool {
    let bytes = tex.as_bytes();
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                let rest = &tex[i..];
                if rest.starts_with("\\\\") {
                    if depth == 0 {
                        return true;
                    }
                    i += 2;
                } else if let Some(after) = rest.strip_prefix("\\begin{") {
                    let len = after.find('}').map_or(after.len(), |p| p + 1);
                    depth += 1;
                    i += "\\begin{".len() + len;
                } else if let Some(after) = rest.strip_prefix("\\end{") {
                    let len = after.find('}').map_or(after.len(), |p| p + 1);
                    depth = depth.saturating_sub(1);
                    i += "\\end{".len() + len;
                } else {
                    // Skip the escaped character (e.g. `\{`, `\%`) so its brace
                    // does not change the nesting depth.
                    i += 2;
                }
            }
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            _ => i += 1,
        }
    }
    false
}

/// Removes trailing `\\` (and `\\[2pt]` spacing) so the generated `matrix`
/// does not end with an empty row.
fn trim_trailing_row_breaks(mut s: &str) -> &str {
    loop {
        let trimmed = s.trim_end();
        if let Some(head) = trimmed.strip_suffix("\\\\") {
            s = head;
            continue;
        }
        if trimmed.ends_with(']')
            && let Some(pos) = trimmed.rfind("\\\\[")
            && !trimmed[pos + 3..trimmed.len() - 1].contains(['[', ']'])
        {
            s = &trimmed[..pos];
            continue;
        }
        return trimmed;
    }
}

/// True if `url` looks like an image path (used for Obsidian embeds).
fn is_image_target(url: &str) -> bool {
    let path = url.split(['#', '?']).next().unwrap_or(url);
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "avif" | "ico"
    )
}

// ---------------------------------------------------------------------------
// Post-processing (text-level extensions)
// ---------------------------------------------------------------------------

/// Applies text-level extensions to every inline run of a block tree
/// (`==highlight==`, Obsidian comments/embeds, `\(…\)` math, bare URLs). It
/// then drops paragraphs that became empty and finalizes soft line breaks.
fn postprocess_blocks(blocks: &mut Vec<Block>) {
    for block in blocks.iter_mut() {
        if let Some(inner) = block.nested_blocks_mut() {
            postprocess_blocks(inner);
        }
        match block {
            Block::Heading { inlines, .. } | Block::Paragraph(inlines) => {
                postprocess_inlines(inlines, true)
            }
            Block::List { items, .. } => {
                for item in items {
                    postprocess_blocks(&mut item.blocks);
                }
            }
            Block::DefinitionList(items) => {
                for item in items {
                    postprocess_inlines(&mut item.term, true);
                    for def in &mut item.definitions {
                        postprocess_blocks(def);
                    }
                }
            }
            Block::Table { head, rows, .. } => {
                for row in head.iter_mut().chain(rows.iter_mut()) {
                    for cell in row {
                        postprocess_inlines(&mut cell.inlines, true);
                    }
                }
            }
            Block::BlockQuote { .. }
            | Block::Details { .. }
            | Block::FencedDiv { .. }
            | Block::Group(_)
            | Block::Toc(_)
            | Block::Media { .. }
            | Block::Image { .. }
            | Block::Math(_)
            | Block::Mermaid(_)
            | Block::CodeBlock { .. }
            | Block::Html(_)
            | Block::Rule => {}
        }
    }
    // Text-level extensions may have removed the only text (HTML comments,
    // Obsidian comments); drop the resulting empty paragraphs.
    blocks.retain(|b| {
        !matches!(b, Block::Paragraph(inl)
            if inl.iter().all(|i| matches!(i, Inline::Text(t) if t.trim().is_empty())))
    });
    normalize_soft_breaks(blocks);
}

/// Finalizes soft line breaks: every remaining line boundary becomes a real
/// newline (strict line breaks). Callout titles have already been extracted,
/// so the marker has done its job.
fn normalize_soft_breaks(blocks: &mut [Block]) {
    fn inlines(v: &mut [Inline]) {
        for i in v.iter_mut() {
            match i {
                Inline::Text(t) if t.contains(SOFT_BREAK) => {
                    *t = t.replace(SOFT_BREAK, "\n");
                }
                _ => inlines(i.children_mut()),
            }
        }
    }
    for b in blocks.iter_mut() {
        if let Some(inner) = b.nested_blocks_mut() {
            normalize_soft_breaks(inner);
        }
        match b {
            Block::Heading { inlines: v, .. } | Block::Paragraph(v) => inlines(v),
            Block::List { items, .. } => {
                for i in items {
                    normalize_soft_breaks(&mut i.blocks);
                }
            }
            Block::DefinitionList(items) => {
                for i in items {
                    inlines(&mut i.term);
                    for d in &mut i.definitions {
                        normalize_soft_breaks(d);
                    }
                }
            }
            Block::Table { head, rows, .. } => {
                for row in head.iter_mut().chain(rows.iter_mut()) {
                    for cell in row {
                        inlines(&mut cell.inlines);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Recursively rewrites inline elements and expands text-level syntax.
fn postprocess_inlines(inlines: &mut Vec<Inline>, linkify: bool) {
    for inl in inlines.iter_mut() {
        match inl {
            Inline::Emph(v)
            | Inline::Strong(v)
            | Inline::Strike(v)
            | Inline::Highlight(v)
            | Inline::Underline(v)
            | Inline::Kbd(v)
            | Inline::Super(v)
            | Inline::Sub(v) => postprocess_inlines(v, linkify),
            Inline::Link { inlines, .. } => postprocess_inlines(inlines, false),
            _ => {}
        }
    }
    let old = std::mem::take(inlines);
    for inl in old {
        match inl {
            Inline::Text(t) => inlines.extend(expand_text(&t, linkify)),
            other => inlines.push(other),
        }
    }
    // Obsidian block ids (` ^abc123`) are metadata, not visible text. Only an
    // id at the very end of the text counts.
    if let Some(Inline::Text(text)) = inlines.last_mut() {
        let trimmed = text.trim_end();
        if let Some(start) = trimmed.rfind('^') {
            let id = &trimmed[start + 1..];
            if !id.is_empty()
                && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
                && trimmed[..start].ends_with(char::is_whitespace)
            {
                *text = trimmed[..start].trim_end().to_string();
            }
        }
    }
}

/// Expands one text run into styled/linked inlines.
fn expand_text(text: &str, linkify: bool) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();
    let mut buf = String::new();
    let mut i = 0usize;

    macro_rules! flush {
        () => {{
            if !buf.is_empty() {
                out.push(Inline::Text(std::mem::take(&mut buf)));
            }
        }};
    }

    while i < text.len() {
        let rest = &text[i..];

        // Pandoc citation: [@key], [-@key], [@key, p. 2]
        if (rest.starts_with("[@") || rest.starts_with("[-@"))
            && let Some(end) = rest.find(']')
        {
            let after = rest[end + 1..].chars().next();
            if !matches!(after, Some('(') | Some('[')) {
                flush!();
                out.push(Inline::Citation(rest[1..end].to_string()));
                i += end + 1;
                continue;
            }
        }

        // Obsidian inline tag: #project (not a URL fragment or a word suffix).
        if let Some(after_hash) = rest.strip_prefix('#') {
            let previous_word_char = buf
                .chars()
                .last()
                .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '/');
            let tag: String = after_hash
                .chars()
                .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '/'))
                .collect();
            if !previous_word_char && !tag.is_empty() && !tag.chars().all(|c| c.is_ascii_digit()) {
                flush!();
                out.push(Inline::Tag(tag.clone()));
                i += 1 + tag.len();
                continue;
            }
        }

        // Obsidian comment: %% … %%
        if rest.starts_with("%%")
            && let Some(end) = rest[2..].find("%%")
        {
            flush!();
            i += 2 + end + 2;
            continue;
        }

        // Inline math: \( … \)
        if rest.starts_with("\\(")
            && let Some(end) = rest[2..].find("\\)")
        {
            flush!();
            out.push(Inline::Math {
                tex: rest[2..2 + end].trim().to_string(),
                display: false,
            });
            i += 2 + end + 2;
            continue;
        }

        // Display math in an inline run: \[ … \]
        if rest.starts_with("\\[")
            && let Some(end) = rest[2..].find("\\]")
        {
            flush!();
            out.push(Inline::Math {
                tex: rest[2..2 + end].trim().to_string(),
                display: true,
            });
            i += 2 + end + 2;
            continue;
        }

        // Obsidian embed: ![[target]] / ![[target|label]]
        if rest.starts_with("![[")
            && let Some(end) = rest[3..].find("]]")
        {
            let inner = &rest[3..3 + end];
            let (target, label) = match inner.split_once('|') {
                Some((t, l)) => (t.trim().to_string(), l.trim().to_string()),
                None => (inner.trim().to_string(), String::new()),
            };
            if !target.is_empty() {
                flush!();
                if is_image_target(&target) {
                    out.push(Inline::Image {
                        alt: if label.is_empty() {
                            target.clone()
                        } else {
                            label
                        },
                        url: target,
                        title: None,
                        width: None,
                        height: None,
                    });
                } else {
                    out.push(Inline::WikiEmbed { target, label });
                }
                i += 3 + end + 2;
                continue;
            }
        }

        // ==highlight==
        if rest.starts_with("==")
            && let Some(end) = rest[2..].find("==")
        {
            let inner = &rest[2..2 + end];
            if !inner.is_empty() && !inner.starts_with('=') {
                flush!();
                out.push(Inline::Highlight(expand_text(inner, false)));
                i += 2 + end + 2;
                continue;
            }
        }

        // Subscript: ~text~ (single tildes, not ~~strikethrough~~)
        if rest.starts_with('~')
            && !rest.starts_with("~~")
            && let Some(end) = rest[1..].find('~')
        {
            let inner = &rest[1..1 + end];
            if !inner.is_empty()
                && !inner.chars().any(char::is_whitespace)
                && inner.chars().any(|c| c.is_alphanumeric())
            {
                flush!();
                out.push(Inline::Sub(expand_text(inner, false)));
                i += 1 + end + 1;
                continue;
            }
        }

        // Superscript: ^text^ (single carets)
        if rest.starts_with('^')
            && let Some(end) = rest[1..].find('^')
        {
            let inner = &rest[1..1 + end];
            if !inner.is_empty()
                && !inner.chars().any(char::is_whitespace)
                && inner.chars().any(|c| c.is_alphanumeric())
            {
                flush!();
                out.push(Inline::Super(expand_text(inner, false)));
                i += 1 + end + 1;
                continue;
            }
        }

        if linkify {
            // Bare web URL (GFM autolink extension).
            if rest.starts_with("http://") || rest.starts_with("https://") {
                let end = rest
                    .find(|c: char| {
                        c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | ']' | ')')
                    })
                    .unwrap_or(rest.len());
                let url = rest[..end].trim_end_matches(['.', ',', ';', ':', '!', '?']);
                if url.len() > 8 {
                    flush!();
                    out.push(Inline::Link {
                        inlines: vec![Inline::Text(url.to_string())],
                        url: url.to_string(),
                    });
                    i += url.len();
                    continue;
                }
            }
            // Bare e-mail address. Only attempted at the start of a word, so
            // long tokens are not rescanned from every position.
            let word_start = !text[..i]
                .chars()
                .next_back()
                .is_some_and(is_email_local_char);
            if word_start && let Some((email, len)) = match_email(rest) {
                flush!();
                out.push(Inline::Link {
                    inlines: vec![Inline::Text(email.clone())],
                    url: format!("mailto:{email}"),
                });
                i += len;
                continue;
            }
        }

        // Copy one character.
        let ch = rest.chars().next().unwrap();
        buf.push(ch);
        i += ch.len_utf8();
    }
    flush!();
    out
}

fn is_email_local_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-')
}

/// Matches a bare e-mail address at the start of `s`.
fn match_email(s: &str) -> Option<(String, usize)> {
    let first = s.chars().next()?;
    if !first.is_ascii_alphanumeric() {
        return None;
    }
    let local_end = s.find(|c: char| !is_email_local_char(c)).unwrap_or(s.len());
    if s.as_bytes().get(local_end) != Some(&b'@') {
        return None;
    }
    let domain_start = local_end + 1;
    let domain_len = s[domain_start..]
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-')))
        .unwrap_or(s.len() - domain_start);
    // A trailing sentence period is not part of the address, so it must not
    // count as the dot of the domain either (`a@b. Thanks` is not an address).
    let domain = s[domain_start..domain_start + domain_len].trim_end_matches('.');
    if !domain.contains('.') {
        return None;
    }
    let len = domain_start + domain.len();
    Some((s[..len].to_string(), len))
}

/// Detects an Obsidian callout in the first paragraph of a block quote and
/// moves its marker (`[!TYPE]`, optional `+`/`-` and custom title) into the
/// quote metadata.
fn apply_callout(block: &mut Block) {
    let Block::BlockQuote {
        kind,
        title,
        fold,
        blocks,
    } = block
    else {
        return;
    };
    let Some(Block::Paragraph(inlines)) = blocks.first_mut() else {
        return;
    };
    let Some(Inline::Text(text)) = inlines.first_mut() else {
        return;
    };
    let trimmed = text.trim_start();
    if !trimmed.starts_with("[!") {
        return;
    }
    let Some(close) = trimmed.find(']') else {
        return;
    };
    let Some(marker_kind) = AlertKind::from_marker(&trimmed[2..close]) else {
        return;
    };
    let mut rest = trimmed[close + 1..].trim_start();
    let mut folded = None;
    if let Some(stripped) = rest.strip_prefix('-') {
        rest = stripped.trim_start();
        folded = Some(false);
    } else if let Some(stripped) = rest.strip_prefix('+') {
        rest = stripped.trim_start();
        folded = Some(true);
    }

    // The custom title is the remainder of the marker line. The soft-break
    // sentinel preserves that boundary after pulldown-cmark joined the lines.
    let (custom, body) = match rest.find(SOFT_BREAK) {
        Some(pos) => (
            rest[..pos].trim().to_string(),
            rest[pos + SOFT_BREAK.len_utf8()..].trim_start().to_string(),
        ),
        None => (rest.trim().to_string(), String::new()),
    };

    // What remains of the first text run is the body that followed the marker
    // line; an empty remainder removes the run.
    if body.trim().is_empty() {
        inlines.remove(0);
    } else {
        *text = body;
    }
    if inlines.is_empty() {
        blocks.remove(0);
    }
    *kind = Some(marker_kind);
    if !custom.is_empty() {
        *title = Some(custom);
    }
    *fold = folded;
}

/// Numbers footnotes in the order their first reference appears in the block
/// tree. Definitions that are never referenced are appended afterwards, in
/// label order, so their content is still visible.
fn collect_footnote_numbers(
    blocks: &[Block],
    footnotes: &BTreeMap<String, Vec<Block>>,
) -> BTreeMap<String, usize> {
    let mut map: BTreeMap<String, usize> = BTreeMap::new();
    fn walk(blocks: &[Block], map: &mut BTreeMap<String, usize>) {
        for b in blocks {
            if let Some(inner) = b.nested_blocks() {
                walk(inner, map);
            }
            match b {
                Block::Heading { inlines, .. } | Block::Paragraph(inlines) => {
                    walk_inlines(inlines, map);
                }
                Block::List { items, .. } => {
                    for i in items {
                        walk(&i.blocks, map);
                    }
                }
                Block::DefinitionList(items) => {
                    for i in items {
                        walk_inlines(&i.term, map);
                        for d in &i.definitions {
                            walk(d, map);
                        }
                    }
                }
                Block::Table { head, rows, .. } => {
                    for row in head.iter().chain(rows.iter()) {
                        for c in row {
                            walk_inlines(&c.inlines, map);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    fn walk_inlines(inlines: &[Inline], map: &mut BTreeMap<String, usize>) {
        for i in inlines {
            if let Inline::FootnoteRef { label, .. } = i {
                let next = map.len() + 1;
                map.entry(label.clone()).or_insert(next);
            }
            walk_inlines(i.children(), map);
        }
    }
    walk(blocks, &mut map);
    for label in footnotes.keys() {
        let next = map.len() + 1;
        map.entry(label.clone()).or_insert(next);
    }
    map
}

/// Writes the freshly computed footnote numbers back into all references.
fn assign_footnote_numbers(blocks: &mut [Block], numbers: &BTreeMap<String, usize>) {
    for b in blocks.iter_mut() {
        if let Some(inner) = b.nested_blocks_mut() {
            assign_footnote_numbers(inner, numbers);
        }
        match b {
            Block::Heading { inlines, .. } | Block::Paragraph(inlines) => {
                assign_in(inlines, numbers)
            }
            Block::List { items, .. } => {
                for i in items {
                    assign_footnote_numbers(&mut i.blocks, numbers);
                }
            }
            Block::DefinitionList(items) => {
                for i in items {
                    assign_in(&mut i.term, numbers);
                    for d in &mut i.definitions {
                        assign_footnote_numbers(d, numbers);
                    }
                }
            }
            Block::Table { head, rows, .. } => {
                for row in head.iter_mut().chain(rows.iter_mut()) {
                    for c in row {
                        assign_in(&mut c.inlines, numbers);
                    }
                }
            }
            _ => {}
        }
    }
    fn assign_in(inlines: &mut [Inline], numbers: &BTreeMap<String, usize>) {
        for i in inlines.iter_mut() {
            if let Inline::FootnoteRef { label, number } = i {
                *number = numbers.get(label).copied().unwrap_or(0);
            }
            assign_in(i.children_mut(), numbers);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readme_keeps_content_after_leading_images() {
        let doc = parse(include_str!("../README.md"));
        let headings: Vec<String> = doc
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Heading { inlines, .. } => {
                    Some(inlines.iter().map(|i| i.plain_text()).collect())
                }
                _ => None,
            })
            .collect();
        assert!(headings.iter().any(|h| h == "Highlights"), "{headings:?}");
        assert!(headings.iter().any(|h| h == "Usage"), "{headings:?}");
        assert!(headings.iter().any(|h| h == "License"), "{headings:?}");
    }

    #[test]
    fn soft_line_breaks_are_preserved() {
        let doc = parse("first line\nsecond line\n\nthird paragraph");
        let Block::Paragraph(first) = &doc.blocks[0] else {
            panic!("expected paragraph");
        };
        let text: String = first.iter().map(Inline::plain_text).collect();
        assert_eq!(text, "first line\nsecond line");
        assert_eq!(doc.blocks.len(), 2);
    }

    #[test]
    fn html_centered_image_becomes_an_image_block() {
        let doc = parse(
            "<p align=\"center\"><img src=\"assets/AppIcon-1024.png\" width=\"180\" alt=\"icon\"></p>",
        );
        let Block::Image { image, center } = &doc.blocks[0] else {
            panic!("expected image block, got {:?}", doc.blocks);
        };
        assert!(*center);
        let Inline::Image {
            url, width, alt, ..
        } = image
        else {
            panic!("expected image inline");
        };
        assert_eq!(url, "assets/AppIcon-1024.png");
        assert_eq!(*width, Some(180.0));
        assert_eq!(alt, "icon");
    }

    #[test]
    fn html_div_banner_becomes_blocks() {
        let doc = parse(
            "<div style=\"background-color: #e3f2fd; border-left: 5px solid #2196f3;\">\n\
             <strong>Information Alert:</strong> parsed safely!\n\
             </div>\n",
        );
        let Block::Group(blocks) = &doc.blocks[0] else {
            panic!("expected converted HTML blocks, got {:?}", doc.blocks);
        };
        assert!(matches!(blocks[0], Block::Paragraph(_)));
        let Block::Paragraph(inlines) = &blocks[0] else {
            unreachable!()
        };
        assert!(matches!(inlines.first(), Some(Inline::Strong(_))));
        assert_eq!(blocks[0].summarize(), "Information Alert: parsed safely!");
    }

    #[test]
    fn html_section_converts_headings_lists_and_entities() {
        let doc = parse(
            "<section>\n\
             <h2>Title &amp; more</h2>\n\
             <p><em>Body</em></p>\n\
             <ul><li>One</li><li>Two</li></ul>\n\
             </section>\n",
        );
        let Block::Group(blocks) = &doc.blocks[0] else {
            panic!("expected converted HTML blocks, got {:?}", doc.blocks);
        };
        assert!(matches!(
            &blocks[0],
            Block::Heading { level: 2, inlines } if matches!(inlines.first(), Some(Inline::Text(t)) if t == "Title & more")
        ));
        assert!(matches!(
            &blocks[1],
            Block::Paragraph(inl) if matches!(inl.first(), Some(Inline::Emph(_)))
        ));
        let Block::List { ordered, items, .. } = &blocks[2] else {
            panic!("expected list, got {:?}", blocks[2]);
        };
        assert!(!ordered);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn html_table_inside_a_container_is_converted() {
        let doc = parse(
            "<div>\n\
             <table><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>2</td></tr></table>\n\
             </div>\n",
        );
        let Block::Group(blocks) = &doc.blocks[0] else {
            panic!("expected converted HTML blocks, got {:?}", doc.blocks);
        };
        let Block::Table { head, rows, .. } = &blocks[0] else {
            panic!("expected table, got {:?}", blocks[0]);
        };
        assert_eq!(head.len(), 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][1].plain_text(), "2");
    }

    #[test]
    fn heading_and_inline_styles() {
        let doc = parse("# Title\n\n**bold** *italic* ~~strike~~ `code`");
        assert_eq!(doc.title.as_deref(), Some("Title"));
        match &doc.blocks[1] {
            Block::Paragraph(inl) => {
                assert_eq!(
                    inl,
                    &vec![
                        Inline::Strong(vec![Inline::Text("bold".into())]),
                        Inline::Text(" ".into()),
                        Inline::Emph(vec![Inline::Text("italic".into())]),
                        Inline::Text(" ".into()),
                        Inline::Strike(vec![Inline::Text("strike".into())]),
                        Inline::Text(" ".into()),
                        Inline::Code("code".into()),
                    ]
                );
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn nested_lists_and_tasks() {
        let md = "- [ ] open\n- [x] done\n  - nested\n1. first\n2. second\n";
        let doc = parse(md);
        let mut lists = Vec::new();
        fn collect<'a>(b: &'a Block, lists: &mut Vec<&'a Block>) {
            if let Block::List { .. } = b {
                lists.push(b);
            }
            match b {
                Block::BlockQuote { blocks, .. } => blocks.iter().for_each(|x| collect(x, lists)),
                Block::List { items, .. } => {
                    for i in items {
                        i.blocks.iter().for_each(|x| collect(x, lists));
                    }
                }
                _ => {}
            }
        }
        doc.blocks.iter().for_each(|b| collect(b, &mut lists));
        assert_eq!(lists.len(), 3);
        if let Block::List { ordered, items, .. } = lists[0] {
            assert!(!ordered);
            // According to pulldown-cmark's tight-list events, "nested" is a
            // *nested* list inside the "done" item — not a third item.
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].task, Some(false));
            assert_eq!(items[1].task, Some(true));
            assert!(matches!(items[1].blocks[0], Block::Paragraph(_)));
            assert!(matches!(
                items[1]
                    .blocks
                    .iter()
                    .find(|b| matches!(b, Block::List { ordered: false, .. })),
                Some(Block::List { .. })
            ));
        } else {
            panic!()
        }
        if let Block::List { ordered, items, .. } = lists[1] {
            // The nested list (inside "done").
            assert!(!ordered);
            assert_eq!(items.len(), 1);
        } else {
            panic!()
        }
        if let Block::List { ordered, items, .. } = lists[2] {
            // Ordered list: "1. first / 2. second".
            assert!(ordered);
            assert_eq!(items.len(), 2);
        } else {
            panic!()
        }
    }

    #[test]
    fn gfm_table() {
        let md = "| a | b |\n| :-: | --: |\n| 1 | 2 |\n";
        let doc = parse(md);
        match &doc.blocks[0] {
            Block::Table { aligns, head, rows } => {
                assert_eq!(aligns, &[TableAlign::Center, TableAlign::Right]);
                assert_eq!(head.len(), 1);
                assert_eq!(head[0].len(), 2);
                assert_eq!(rows.len(), 1);
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    #[test]
    fn footnotes_and_math() {
        let md = "Text[^1] and $x^2$.\n\n[^1]: A footnote\n";
        let doc = parse(md);
        assert!(doc.footnotes.contains_key("1"));
        match &doc.blocks[0] {
            Block::Paragraph(inl) => {
                assert!(matches!(
                    inl.iter().find(|i| matches!(i, Inline::FootnoteRef { .. })),
                    Some(Inline::FootnoteRef { .. })
                ));
                assert!(matches!(
                    inl.iter()
                        .find(|i| matches!(i, Inline::Math { display: false, .. })),
                    Some(Inline::Math { .. })
                ));
            }
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    #[test]
    fn display_math_becomes_block() {
        let doc = parse("$$\n\\frac{1}{2}\n$$\n");
        match &doc.blocks[0] {
            Block::Math(tex) => assert!(tex.contains("frac")),
            other => panic!("expected display math block, got {other:?}"),
        }
    }

    #[test]
    fn top_level_math_row_breaks_get_a_matrix_wrapper() {
        let tex = r"H \mathrm{e}^{-\alpha L/2} = B \cos(kL/2)
\text{ and }
-\alpha H \mathrm{e}^{-\alpha L/2} = - k B \sin(kL/2)
\\
\Rightarrow
\alpha = k \tan(kL/2)";
        let wrapped = normalize_display_math(tex);
        assert!(wrapped.starts_with("\\begin{matrix}\n"));
        assert!(wrapped.ends_with("\n\\end{matrix}"));
        assert!(wrapped.contains(r"\alpha = k \tan(kL/2)"));
        // Idempotent: the row break now lives inside the matrix.
        assert_eq!(normalize_display_math(&wrapped), wrapped);

        // A trailing row break must not create an empty final row.
        let trailing = normalize_display_math("a = b \\\\\n");
        assert!(trailing.ends_with("a = b\n\\end{matrix}"));
    }

    #[test]
    fn math_row_breaks_inside_environments_are_untouched() {
        for tex in [
            r"\begin{aligned} a &= b \\ c &= d \end{aligned}",
            r"\begin{cases} 1 & x > 0 \\ 0 & \text{else} \end{cases}",
            r"\text{first \\ second}",
            r"\begin{matrix} a \\ b \end{matrix}",
        ] {
            assert_eq!(normalize_display_math(tex), tex.trim());
        }
    }

    #[test]
    fn display_math_with_row_breaks_survives_parsing() {
        let md = "$$\na = b\n\\\\\n\\Rightarrow c = d\n$$\n";
        let doc = parse(md);
        match &doc.blocks[0] {
            // The model keeps the original source for click-to-source mapping…
            Block::Math(tex) => assert!(tex.contains("\\\\")),
            other => panic!("expected display math block, got {other:?}"),
        }
        // …while the renderer receives a wrapped, renderable formula.
        let Block::Math(tex) = &doc.blocks[0] else {
            unreachable!()
        };
        let wrapped = normalize_display_math(tex);
        let font = latex_rust::MathFont::stix_two_math().unwrap();
        let mut options = latex_rust::PngOptions::new();
        options.font_size_pt = latex_rust::Dim::from_i64(16);
        options.dpi = latex_rust::Dim::from_i64(144);
        options.display = true;
        assert!(
            latex_rust::latex_to_png(&wrapped, &font, &options).is_ok(),
            "row-broken display math must typeset"
        );
    }

    #[test]
    fn obsidian_callout_titles_and_folds() {
        let doc =
            parse("> [!TIP]- Folded by default\n> This callout requests a collapsed state.\n");
        let Block::BlockQuote {
            kind,
            title,
            fold,
            blocks,
        } = &doc.blocks[0]
        else {
            panic!("expected callout, got {:?}", doc.blocks[0]);
        };
        assert_eq!(*kind, Some(AlertKind::Tip));
        assert_eq!(title.as_deref(), Some("Folded by default"));
        assert_eq!(*fold, Some(false));
        let Block::Paragraph(body) = &blocks[0] else {
            panic!("expected body paragraph");
        };
        let text: String = body.iter().map(Inline::plain_text).collect();
        assert_eq!(text.trim(), "This callout requests a collapsed state.");
    }

    #[test]
    fn html_sub_and_sup_become_script_inlines() {
        let doc = parse("H<sub>2</sub>O and x<sup>2</sup>\n");
        let Block::Paragraph(inl) = &doc.blocks[0] else {
            panic!("expected paragraph");
        };
        assert!(
            inl.iter()
                .any(|i| matches!(i, Inline::Sub(v) if v.iter().map(Inline::plain_text).collect::<String>() == "2"))
        );
        assert!(
            inl.iter()
                .any(|i| matches!(i, Inline::Super(v) if v.iter().map(Inline::plain_text).collect::<String>() == "2"))
        );
    }

    #[test]
    fn nested_inline_styles_are_preserved_in_the_model() {
        let doc = parse("***both*** and **bold *italic* ~~strike~~**\n");
        let Block::Paragraph(inl) = &doc.blocks[0] else {
            panic!("expected paragraph");
        };
        // `***both***` → emphasis containing strong (or vice versa).
        let both = match &inl[0] {
            Inline::Emph(v) => v.iter().any(|i| matches!(i, Inline::Strong(_))),
            Inline::Strong(v) => v.iter().any(|i| matches!(i, Inline::Emph(_))),
            _ => false,
        };
        assert!(both, "expected nested emphasis + strong, got {:?}", inl[0]);
        // The outer strong span keeps its nested emphasis and strike.
        let strong = inl
            .iter()
            .find_map(|i| match i {
                Inline::Strong(v) => Some(v),
                _ => None,
            })
            .expect("strong span");
        assert!(strong.iter().any(|i| matches!(i, Inline::Emph(_))));
        assert!(strong.iter().any(|i| matches!(i, Inline::Strike(_))));
    }

    #[test]
    fn tight_and_loose_lists_are_distinguished() {
        let tight = parse("* one\n* two\n");
        let Block::List {
            tight: is_tight, ..
        } = &tight.blocks[0]
        else {
            panic!("expected list");
        };
        assert!(*is_tight);

        let loose = parse("* one\n\n* two\n");
        let Block::List {
            tight: is_tight, ..
        } = &loose.blocks[0]
        else {
            panic!("expected list");
        };
        assert!(!*is_tight);
    }

    #[test]
    fn tight_list_items_keep_inline_spans() {
        // Tight list items arrive without a Paragraph wrapper. They used to
        // lose everything after the first inline span (`*  **Bold**: text`).
        let doc = parse("*   **Bold Text**: **This is bold text**\n");
        let Block::List { items, .. } = &doc.blocks[0] else {
            panic!("expected list, got {:?}", doc.blocks[0]);
        };
        let Block::Paragraph(inl) = &items[0].blocks[0] else {
            panic!("expected paragraph");
        };
        assert!(matches!(inl.first(), Some(Inline::Strong(_))));
        let text: String = inl.iter().map(Inline::plain_text).collect();
        assert_eq!(text, "Bold Text: This is bold text");

        // Plain trailing text after a leading span is kept too.
        let doc = parse("*   **Bold** rest\n");
        let Block::List { items, .. } = &doc.blocks[0] else {
            panic!("expected list");
        };
        let Block::Paragraph(inl) = &items[0].blocks[0] else {
            panic!("expected paragraph");
        };
        let text: String = inl.iter().map(Inline::plain_text).collect();
        assert_eq!(text, "Bold rest");
    }

    #[test]
    fn html_table_supports_spans_and_inline_markup() {
        let md = r#"<table style="text-align: center;">
<thead>
<tr><th rowspan="2">Host</th><th colspan="2">Cluster A</th></tr>
<tr><th>CPU</th><th>RAM</th></tr>
</thead>
<tbody>
<tr><td><b>Mainframe 01</b></td><td>80%</td><td>16 GB</td></tr>
<tr><td>Mainframe 02</td><td colspan="2">OFFLINE</td></tr>
</tbody>
</table>
"#;
        let doc = parse(md);
        let Block::Table { aligns, head, rows } = &doc.blocks[0] else {
            panic!("expected HTML table, got {:?}", doc.blocks[0]);
        };
        // Both <thead> rows are header rows.
        assert_eq!(head.len(), 2);
        // Row 1: rowspan cell + colspan cell → 3 columns.
        assert_eq!(head[0].len(), 2);
        assert_eq!((head[0][0].col, head[0][0].rowspan), (0, 2));
        assert_eq!((head[0][1].col, head[0][1].colspan), (1, 2));
        // Row 2: CPU/RAM start after the rowspan cell.
        assert_eq!(head[1][0].col, 1);
        assert_eq!(head[1][1].col, 2);
        // Body: colspan cell occupies columns 1..3.
        assert_eq!(rows[1][1].colspan, 2);
        assert_eq!(rows[1][1].col, 1);
        assert_eq!(rows[1][1].plain_text(), "OFFLINE");
        // Inline HTML inside cells keeps its styling.
        assert!(matches!(
            rows[0][0].inlines.first(),
            Some(Inline::Strong(_))
        ));
        // `text-align: center` applies to every column.
        assert!(aligns.iter().all(|a| *a == TableAlign::Center));
    }

    #[test]
    fn fenced_code_with_language() {
        let md = "```rust\nfn main() {}\n```\n";
        let doc = parse(md);
        match &doc.blocks[0] {
            Block::CodeBlock { lang, code } => {
                assert_eq!(lang.as_deref(), Some("rust"));
                assert_eq!(code, "fn main() {}");
            }
            other => panic!("expected code, got {other:?}"),
        }
    }

    #[test]
    fn gfm_alert_quote() {
        let doc = parse("> [!NOTE]\n> note\n");
        match &doc.blocks[0] {
            Block::BlockQuote { kind, blocks, .. } => {
                assert_eq!(*kind, Some(AlertKind::Note));
                assert_eq!(blocks.len(), 1);
            }
            other => panic!("expected quote, got {other:?}"),
        }
    }

    /// Regression: a code block must consume its own end tag, otherwise every
    /// following block is silently dropped.
    #[test]
    fn content_after_code_block_is_kept() {
        let md = "```rust\nfn main() {}\n```\n\n## After\n\n| a | b |\n| - | - |\n| 1 | 2 |\n";
        let doc = parse(md);
        assert!(
            doc.blocks
                .iter()
                .any(|b| matches!(b, Block::Heading { level: 2, .. }))
        );
        assert!(doc.blocks.iter().any(|b| matches!(b, Block::Table { .. })));
    }

    /// The bundled README (the only Markdown file that ships with the
    /// sources) must parse end-to-end into a non-trivial document.
    #[test]
    fn bundled_readme_parses() {
        let doc = parse(include_str!("../README.md"));
        assert!(!doc.blocks.is_empty());
        assert!(doc.blocks.len() > 10, "suspiciously few blocks");
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Name of a block variant. Exhaustive on purpose: adding a `Block`
    /// variant makes this fail to compile until it is added to the coverage
    /// test and a fixture exercises it.
    fn block_kind(block: &Block) -> &'static str {
        match block {
            Block::Heading { .. } => "heading",
            Block::Paragraph(_) => "paragraph",
            Block::Image { .. } => "image",
            Block::Math(_) => "math",
            Block::CodeBlock { .. } => "code",
            Block::Mermaid(_) => "mermaid",
            Block::BlockQuote { .. } => "quote",
            Block::List { .. } => "list",
            Block::DefinitionList(_) => "definition_list",
            Block::Table { .. } => "table",
            Block::Details { .. } => "details",
            Block::FencedDiv { .. } => "fenced_div",
            Block::Toc(_) => "toc",
            Block::Media { .. } => "media",
            Block::Group(_) => "html_group",
            Block::Html(_) => "html_source",
            Block::Rule => "rule",
        }
    }

    fn inline_kind(inline: &Inline) -> &'static str {
        match inline {
            Inline::Text(_) => "text",
            Inline::Emph(_) => "emph",
            Inline::Strong(_) => "strong",
            Inline::Strike(_) => "strike",
            Inline::Highlight(_) => "highlight",
            Inline::Underline(_) => "underline",
            Inline::Kbd(_) => "kbd",
            Inline::Code(_) => "code",
            Inline::Math { .. } => "math",
            Inline::Link { .. } => "link",
            Inline::WikiLink { .. } => "wikilink",
            Inline::Image { .. } => "image",
            Inline::WikiEmbed { .. } => "wiki_embed",
            Inline::FootnoteRef { .. } => "footnote_ref",
            Inline::Tag(_) => "tag",
            Inline::Citation(_) => "citation",
            Inline::Abbr { .. } => "abbr",
            Inline::Super(_) => "super",
            Inline::Sub(_) => "sub",
        }
    }

    fn walk_inlines(inlines: &[Inline], seen: &mut BTreeSet<&'static str>) {
        for inline in inlines {
            seen.insert(inline_kind(inline));
            walk_inlines(inline.children(), seen);
        }
    }

    fn walk_block(
        block: &Block,
        blocks: &mut BTreeSet<&'static str>,
        inlines: &mut BTreeSet<&'static str>,
    ) {
        blocks.insert(block_kind(block));
        match block {
            Block::Heading { inlines: i, .. } | Block::Paragraph(i) => walk_inlines(i, inlines),
            Block::Image { image, .. } => walk_inlines(std::slice::from_ref(image), inlines),
            Block::BlockQuote { blocks: b, .. }
            | Block::Details { blocks: b, .. }
            | Block::Group(b) => {
                for block in b {
                    walk_block(block, blocks, inlines);
                }
            }
            Block::List { items, .. } => {
                for item in items {
                    for block in &item.blocks {
                        walk_block(block, blocks, inlines);
                    }
                }
            }
            Block::DefinitionList(items) => {
                for item in items {
                    walk_inlines(&item.term, inlines);
                    for definition in &item.definitions {
                        for block in definition {
                            walk_block(block, blocks, inlines);
                        }
                    }
                }
            }
            Block::Table { head, rows, .. } => {
                for cell in head.iter().chain(rows.iter()).flat_map(|row| row.iter()) {
                    walk_inlines(&cell.inlines, inlines);
                }
            }
            _ => {}
        }
    }

    /// The bundled feature documents must exercise every construct the parser
    /// can produce. If a new feature is added without a fixture, this test
    /// fails and points at what is missing — manual checklists can then never
    /// silently fall behind.
    #[test]
    fn feature_corpus_exercises_every_variant() {
        let corpus = [
            crate::test_fixtures::FEATURE_GALLERY,
            crate::test_fixtures::COMPATIBILITY_SAMPLE,
            // An empty container keeps the sanitized-source path covered.
            "<source srcset=\"x\">\n\nText with <u>underline</u>.\n",
            "::: note\nFenced div body.\n:::\n",
            "Tag #project and citation [@doe2024].\n",
            "[TOC]\n\n# Heading\n",
            "<video src=\"clip.mp4\">\n</video>\n",
            "*[HTML]: HyperText Markup Language\n\nHTML text.\n",
        ];
        let mut blocks = BTreeSet::new();
        let mut inlines = BTreeSet::new();
        for markdown in corpus {
            let doc = parse(markdown);
            for block in &doc.blocks {
                walk_block(block, &mut blocks, &mut inlines);
            }
        }

        let expected_blocks = [
            "heading",
            "paragraph",
            "image",
            "math",
            "code",
            "mermaid",
            "quote",
            "list",
            "definition_list",
            "table",
            "details",
            "fenced_div",
            "toc",
            "media",
            "html_group",
            "html_source",
            "rule",
        ];
        let expected_inlines = [
            "text",
            "emph",
            "strong",
            "strike",
            "highlight",
            "underline",
            "kbd",
            "code",
            "math",
            "link",
            "wikilink",
            "image",
            "wiki_embed",
            "footnote_ref",
            "tag",
            "citation",
            "abbr",
            "super",
            "sub",
        ];
        let missing_blocks: Vec<_> = expected_blocks
            .iter()
            .filter(|kind| !blocks.contains(*kind))
            .collect();
        let missing_inlines: Vec<_> = expected_inlines
            .iter()
            .filter(|kind| !inlines.contains(*kind))
            .collect();
        assert!(
            missing_blocks.is_empty(),
            "feature corpus lacks block constructs: {missing_blocks:?}"
        );
        assert!(
            missing_inlines.is_empty(),
            "feature corpus lacks inline constructs: {missing_inlines:?}"
        );
    }
}

#[cfg(test)]
mod extended_tests {
    use super::*;

    fn compat_doc() -> Document {
        parse(crate::test_fixtures::COMPATIBILITY_SAMPLE)
    }

    fn collect_inlines<'a>(blocks: &'a [Block], out: &mut Vec<&'a Inline>) {
        for b in blocks {
            match b {
                Block::Heading { inlines, .. } | Block::Paragraph(inlines) => {
                    out.extend(inlines.iter())
                }
                Block::Image { image, .. } => out.push(image),
                Block::BlockQuote { blocks, .. } | Block::Details { blocks, .. } => {
                    collect_inlines(blocks, out)
                }
                Block::List { items, .. } => {
                    for i in items {
                        collect_inlines(&i.blocks, out);
                    }
                }
                Block::DefinitionList(items) => {
                    for i in items {
                        out.extend(i.term.iter());
                        for d in &i.definitions {
                            collect_inlines(d, out);
                        }
                    }
                }
                Block::Table { head, rows, .. } => {
                    for row in head.iter().chain(rows.iter()) {
                        for c in row {
                            out.extend(c.inlines.iter());
                        }
                    }
                }
                _ => {}
            }
        }
    }

    #[test]
    fn grid_tables_become_gfm_tables() {
        let doc = parse(
            "+--------+--------+\n| Left   | Right  |\n+========+========+\n| one    | two    |\n+--------+--------+\n",
        );
        let Block::Table { head, rows, .. } = &doc.blocks[0] else {
            panic!("expected grid table conversion, got {:?}", doc.blocks[0]);
        };
        assert_eq!(head[0][0].plain_text(), "Left");
        assert_eq!(rows[0][1].plain_text(), "two");
    }

    #[test]
    fn svg_blocks_rasterize_safely() {
        let uri = svg_to_png_data_uri(
            r#"<svg width="10" height="10" xmlns="http://www.w3.org/2000/svg"><rect width="10" height="10" fill="red"/></svg>"#,
        )
        .expect("simple SVG should rasterize");
        assert!(uri.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn media_blocks_are_recognized() {
        let doc = parse("<video src=\"clip.mp4\">\n</video>\n");
        let Block::Media { kind, url } = &doc.blocks[0] else {
            panic!("expected media block, got {:?}", doc.blocks[0]);
        };
        assert_eq!(kind, "Video");
        assert_eq!(url, "clip.mp4");
    }

    #[test]
    fn pandoc_fenced_divs_and_mkdocs_admonitions() {
        let doc = parse("::: warning\n**Careful** here.\n:::\n");
        let Block::FencedDiv { class, blocks } = &doc.blocks[0] else {
            panic!("expected fenced div, got {:?}", doc.blocks[0]);
        };
        assert_eq!(class, "warning");
        assert!(matches!(blocks[0], Block::Paragraph(_)));

        let doc = parse("!!! note \"Custom title\"\n    Body text.\n");
        let Block::BlockQuote {
            kind,
            title,
            blocks,
            ..
        } = &doc.blocks[0]
        else {
            panic!("expected admonition callout, got {:?}", doc.blocks[0]);
        };
        assert_eq!(*kind, Some(AlertKind::Note));
        assert_eq!(title.as_deref(), Some("Custom title"));
        assert!(matches!(blocks[0], Block::Paragraph(_)));
    }

    #[test]
    fn abbreviations_become_inline_abbr() {
        let doc = parse("*[HTML]: HyperText Markup Language\n\nHTML text.\n");
        let Block::Paragraph(inlines) = &doc.blocks[0] else {
            panic!("expected paragraph");
        };
        assert!(matches!(
            inlines.first(),
            Some(Inline::Abbr { text, title }) if text == "HTML" && title == "HyperText Markup Language"
        ));
    }

    #[test]
    fn toc_collects_headings() {
        let doc = parse("[TOC]\n\n# One\n\n## Two\n");
        let Block::Toc(entries) = &doc.blocks[0] else {
            panic!("expected TOC, got {:?}", doc.blocks[0]);
        };
        assert_eq!(
            entries,
            &vec![(1, "One".to_string()), (2, "Two".to_string())]
        );
    }

    #[test]
    fn heading_attributes_are_not_displayed() {
        let doc = parse("# Title {#intro .lead}\n");
        let Block::Heading { inlines, .. } = &doc.blocks[0] else {
            panic!("expected heading");
        };
        assert_eq!(
            inlines.iter().map(|i| i.plain_text()).collect::<String>(),
            "Title"
        );
    }

    #[test]
    fn inline_footnotes_are_expanded() {
        let doc = parse("Text ^[inline note] here.\n");
        assert!(doc.footnotes.contains_key("mdviewer-inline-1"));
    }

    #[test]
    fn front_matter_supports_nested_yaml_toml_and_json() {
        let yaml = parse(
            "---\ntitle: Doc\nmeta:\n  author: Ada\n  tags:\n    - one\n    - two\n---\n# Body\n",
        );
        assert_eq!(
            yaml.metadata
                .iter()
                .find(|(k, _)| k == "meta.author")
                .map(|(_, v)| v.as_str()),
            Some("Ada")
        );
        assert_eq!(
            yaml.metadata
                .iter()
                .find(|(k, _)| k == "meta.tags")
                .map(|(_, v)| v.as_str()),
            Some("one, two")
        );

        let toml = parse("+++\ntitle = \"Doc\"\n[meta]\nauthor = \"Ada\"\n+++\n# Body\n");
        assert_eq!(
            toml.metadata
                .iter()
                .find(|(k, _)| k == "meta.author")
                .map(|(_, v)| v.as_str()),
            Some("Ada")
        );

        let json = parse("{\"title\": \"Doc\", \"meta\": {\"author\": \"Ada\"}}\n# Body\n");
        assert_eq!(
            json.metadata
                .iter()
                .find(|(k, _)| k == "meta.author")
                .map(|(_, v)| v.as_str()),
            Some("Ada")
        );
    }

    #[test]
    fn front_matter_extends_and_callouts() {
        let doc = compat_doc();
        assert_eq!(doc.title.as_deref(), Some("Compatibility Sample"));
        assert!(
            doc.metadata
                .iter()
                .any(|(k, v)| k == "title" && v.contains("Compatibility Sample"))
        );
        assert!(
            doc.metadata
                .iter()
                .any(|(k, v)| k == "extensions" && v.contains("Mermaid"))
        );

        let mut quotes = Vec::new();
        fn walk<'a>(blocks: &'a [Block], quotes: &mut Vec<&'a Block>) {
            for b in blocks {
                if let Block::BlockQuote { .. } = b {
                    quotes.push(b);
                }
                if let Block::BlockQuote { blocks, .. } = b {
                    walk(blocks, quotes);
                }
            }
        }
        walk(&doc.blocks, &mut quotes);
        assert!(quotes.iter().any(|b| matches!(
            b,
            Block::BlockQuote { kind: Some(AlertKind::Warning), title: Some(t), .. }
                if t == "Custom callout title"
        )));
        assert!(quotes.iter().any(|b| matches!(
            b,
            Block::BlockQuote {
                kind: Some(AlertKind::Tip),
                fold: Some(false),
                ..
            }
        )));
        assert!(quotes.iter().any(|b| matches!(
            b,
            Block::BlockQuote {
                kind: Some(AlertKind::Question),
                fold: Some(true),
                ..
            }
        )));
    }

    #[test]
    fn highlight_sub_super_images_and_html() {
        let doc = compat_doc();
        let mut inl = Vec::new();
        collect_inlines(&doc.blocks, &mut inl);
        assert!(inl.iter().any(|i| matches!(i, Inline::Highlight(v) if v[0].plain_text() == "Common highlight extension")));
        assert!(
            inl.iter()
                .any(|i| matches!(i, Inline::Sub(v) if v[0].plain_text() == "2"))
        );
        assert!(
            inl.iter()
                .any(|i| matches!(i, Inline::Super(v) if v[0].plain_text() == "2"))
        );

        // Remote image keeps its title.
        assert!(inl.iter().any(|i| matches!(
            i,
            Inline::Image { alt, title: Some(t), .. }
                if alt == "Remote placeholder image" && t == "Remote image title"
        )));
        // Obsidian embed becomes an image; HTML <img> keeps dimensions.
        assert!(inl.iter().any(|i| matches!(
            i,
            Inline::Image { url, .. } if url.contains("Pasted image")
        )));
        assert!(inl.iter().any(|i| matches!(
            i,
            Inline::Image { alt, width: Some(320.0), height: Some(168.0), .. }
                if alt == "HTML image test"
        )));
        // Inline HTML is interpreted.
        assert!(
            inl.iter()
                .any(|i| matches!(i, Inline::Kbd(v) if v[0].plain_text() == "Ctrl"))
        );
        assert!(inl.iter().any(
            |i| matches!(i, Inline::Highlight(v) if v[0].plain_text() == "HTML mark element")
        ));

        // <details>, the HTML table and the script/comment handling.
        assert!(doc.blocks.iter().any(|b| matches!(
            b,
            Block::Details { summary, .. } if summary == "Expandable details test"
        )));
        assert!(doc.blocks.iter().any(|b| matches!(
            b,
            Block::Table { head, .. } if head[0][0].plain_text() == "HTML"
        )));
        assert!(!doc.blocks.iter().any(|b| matches!(
            b,
            Block::Html(src) if src.to_ascii_lowercase().contains("<script")
                || src.contains("hidden html comment")
        )));
    }

    #[test]
    fn tables_footnotes_math_mermaid_wiki_and_definitions() {
        let doc = compat_doc();
        // Bare URL in a table is linkified.
        let mut links = Vec::new();
        fn walk_links<'a>(blocks: &'a [Block], out: &mut Vec<&'a Inline>) {
            for b in blocks {
                match b {
                    Block::Table { head, rows, .. } => {
                        for c in head.iter().chain(rows.iter()).flatten() {
                            out.extend(c.inlines.iter());
                        }
                    }
                    Block::BlockQuote { blocks, .. } | Block::Details { blocks, .. } => {
                        walk_links(blocks, out)
                    }
                    Block::List { items, .. } => {
                        for i in items {
                            walk_links(&i.blocks, out)
                        }
                    }
                    _ => {}
                }
            }
        }
        walk_links(&doc.blocks, &mut links);
        assert!(links.iter().any(|i| matches!(
            i,
            Inline::Link { url, .. }
                if url == "https://example.com/a/very/long/path/that/tests/wrapping/behaviour"
        )));

        // Footnotes are numbered in reference order.
        assert_eq!(doc.footnotes.len(), 2);
        assert_eq!(doc.footnote_numbers.get("standard-note"), Some(&1));
        assert_eq!(doc.footnote_numbers.get("long-note"), Some(&2));

        // Display math survives the matrix's lone `=` line.
        assert!(doc.blocks.iter().any(|b| matches!(
            b,
            Block::Math(tex) if tex.contains("bmatrix")
        )));
        assert!(doc.blocks.iter().any(|b| matches!(
            b,
            Block::Math(tex) if tex.contains(r"\mathbf{A}")
        )));

        assert_eq!(
            doc.blocks
                .iter()
                .filter(|b| matches!(b, Block::Mermaid(_)))
                .count(),
            7
        );
        assert!(doc.blocks.iter().any(|b| matches!(
            b,
            Block::DefinitionList(items) if items.len() == 3
        )));

        let mut inl = Vec::new();
        collect_inlines(&doc.blocks, &mut inl);
        assert!(
            inl.iter()
                .any(|i| matches!(i, Inline::WikiLink { target, .. } if target == "Project Alpha"))
        );
        assert!(
            inl.iter().any(
                |i| matches!(i, Inline::WikiEmbed { target, .. } if target == "Embedded Note")
            )
        );
    }

    #[test]
    fn top_level_blocks_map_to_source_lines() {
        let md = "# Alpha\n\nBody text.\n\n```\ncode\n```\n\n## Beta\n\n- item\n\n<div>\n<p>HTML</p>\n</div>\n\n## Gamma\n\nEnd.\n";
        let doc = parse(md);
        assert_eq!(doc.block_lines.len(), doc.blocks.len());
        let line = |needle: &str| {
            doc.blocks
                .iter()
                .position(|b| b.summarize().starts_with(needle))
                .map(|i| doc.block_lines[i])
        };
        assert_eq!(line("Alpha"), Some(1));
        assert_eq!(line("Body text."), Some(3));
        assert_eq!(line("code"), Some(6));
        assert_eq!(line("Beta"), Some(9));
        assert_eq!(line("item"), Some(11));
        assert_eq!(line("HTML"), Some(14));
        assert_eq!(line("Gamma"), Some(17));
        assert_eq!(line("End."), Some(19));
        // Mapping is monotonic: later blocks never point at earlier lines.
        assert!(doc.block_lines.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn paren_math_and_comment_cleanup() {
        let doc = parse("Inline \\(a^2+b^2=c^2\\) math.\n\n%%hidden%%\n\nVisible.\n");
        let text = Document::plain_blocks(&doc.blocks);
        assert!(text.contains("Visible."));
        assert!(!text.contains("%%hidden%%"));
        assert!(doc.blocks.iter().any(|b| matches!(
            b,
            Block::Paragraph(inl) if inl.iter().any(|i| matches!(
                i,
                Inline::Math { tex, display: false } if tex.contains("a^2")
            ))
        )));
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;

    fn summaries(doc: &Document) -> Vec<String> {
        doc.blocks.iter().map(Block::summarize).collect()
    }

    #[test]
    fn text_after_a_comment_close_is_kept() {
        assert_eq!(strip_unsafe_html("<!-- note -->Visible\n"), "Visible\n");
        assert_eq!(
            strip_unsafe_html("<!--\nhidden\n--> after\nnext\n"),
            "\n\n after\nnext\n"
        );
    }

    #[test]
    fn unclosed_comment_keeps_the_line_count() {
        let md = "a\n<!-- open\nhidden\nstill hidden";
        assert_eq!(strip_unsafe_html(md), "a\n\n\n");
    }

    #[test]
    fn script_and_style_blocks_are_removed_with_trailing_text() {
        assert_eq!(
            strip_unsafe_html("<script>\nalert(1)\n</script>kept\n"),
            "\n\nkept\n"
        );
        assert_eq!(strip_unsafe_html("<STYLE>p{}</STYLE>\nx\n"), "\nx\n");
    }

    fn links(text: &str) -> Vec<String> {
        expand_text(text, true)
            .into_iter()
            .filter_map(|i| match i {
                Inline::Link { url, .. } => Some(url),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn empty_display_math_keeps_following_content() {
        let doc = parse("$$\n$$\n\nAfter the formula.\n\n\\[\n\\]\n\nAfter again.\n");
        let texts = summaries(&doc);
        assert!(
            texts.contains(&"After the formula.".to_string()),
            "{texts:?}"
        );
        assert!(texts.contains(&"After again.".to_string()), "{texts:?}");
        assert!(!texts.iter().any(|t| t.contains("$$") || t.contains("\\[")));
    }

    #[test]
    fn text_after_closing_math_delimiter_is_kept() {
        let doc = parse("$$\nx = 1\n$$ more text\n\n\\[\ny\n\\] tail\n");
        let texts = summaries(&doc);
        assert!(texts.iter().any(|t| t.trim() == "more text"), "{texts:?}");
        assert!(texts.iter().any(|t| t.trim() == "tail"), "{texts:?}");
    }

    #[test]
    fn text_before_details_on_the_same_line_is_kept() {
        let doc = parse("Intro <details><summary>S</summary>Body</details>\n");
        assert!(
            summaries(&doc).iter().any(|t| t.trim() == "Intro"),
            "{:?}",
            summaries(&doc)
        );
        assert!(
            doc.blocks
                .iter()
                .any(|b| matches!(b, Block::Details { summary, .. } if summary == "S"))
        );
    }

    #[test]
    fn malformed_details_tag_does_not_panic() {
        let doc = parse("<details</details>\n\nAfter\n");
        assert!(summaries(&doc).contains(&"After".to_string()));
    }

    #[test]
    fn void_and_self_closing_inline_tags_are_kept() {
        let doc = parse("a <br/> b <br /> c <img src=\"x.png\" /> d\n");
        let Block::Paragraph(inl) = &doc.blocks[0] else {
            panic!("expected paragraph, got {:?}", doc.blocks);
        };
        let breaks: usize = inl
            .iter()
            .filter_map(|i| match i {
                Inline::Text(t) => Some(t.matches('\n').count()),
                _ => None,
            })
            .sum();
        assert_eq!(breaks, 2);
        assert!(
            inl.iter()
                .any(|i| matches!(i, Inline::Image { url, .. } if url == "x.png"))
        );
    }

    #[test]
    fn fence_inside_fenced_div_does_not_end_the_div() {
        let doc = parse("::: note\n```rust\nfn x() {}\n```\n:::\n\nAfter\n");
        let Block::FencedDiv { class, blocks } = &doc.blocks[0] else {
            panic!("expected fenced div, got {:?}", doc.blocks);
        };
        assert_eq!(class, "note");
        assert!(matches!(blocks[0], Block::CodeBlock { .. }));
        assert!(summaries(&doc).contains(&"After".to_string()));
    }

    #[test]
    fn nested_fenced_divs_are_converted() {
        let doc = parse("::: note\nOuter\n::: tip\nInner\n:::\nAfter inner\n:::\n\nAfter\n");
        let Block::FencedDiv { blocks, .. } = &doc.blocks[0] else {
            panic!("expected fenced div, got {:?}", doc.blocks);
        };
        assert!(
            blocks
                .iter()
                .any(|b| matches!(b, Block::FencedDiv { class, .. } if class == "tip"))
        );
        assert!(summaries(&doc).contains(&"After".to_string()));
    }

    #[test]
    fn fence_tracker_requires_a_bare_closing_fence() {
        let mut fence = FenceTracker::default();
        assert!(fence.feed("```\n"));
        assert!(fence.feed("[TOC]\n"));
        assert!(fence.feed("```rust\n"));
        assert!(fence.feed("```\n"));
        assert!(!fence.feed("[TOC]\n"));
    }

    #[test]
    fn heading_with_bare_braces_keeps_them() {
        let doc = parse("## Using {config}\n");
        assert_eq!(summaries(&doc), vec!["Using {config}".to_string()]);
    }

    #[test]
    fn footnotes_inside_details_are_kept_and_numbered() {
        let doc = parse(
            "<details>\n<summary>More</summary>\n\nBody[^n].\n\n[^n]: Note text\n</details>\n",
        );
        assert!(doc.footnotes.contains_key("n"));
        let Block::Details { blocks, .. } = &doc.blocks[0] else {
            panic!("expected details, got {:?}", doc.blocks);
        };
        let Block::Paragraph(inl) = &blocks[0] else {
            panic!("expected paragraph");
        };
        assert!(
            inl.iter()
                .any(|i| matches!(i, Inline::FootnoteRef { number: 1, .. }))
        );
    }

    #[test]
    fn footnote_references_inside_divs_are_numbered() {
        let doc = parse("::: note\nText[^a].\n\n[^a]: Definition.\n:::\n");
        assert!(doc.footnotes.contains_key("a"));
        let Block::FencedDiv { blocks, .. } = &doc.blocks[0] else {
            panic!("expected fenced div, got {:?}", doc.blocks);
        };
        let Block::Paragraph(inl) = &blocks[0] else {
            panic!("expected paragraph");
        };
        assert!(
            inl.iter()
                .any(|i| matches!(i, Inline::FootnoteRef { number: 1, .. }))
        );
    }

    #[test]
    fn details_with_display_math_keeps_the_rest_of_the_document() {
        let doc =
            parse("<details>\n<summary>Math</summary>\n\n$$\nx = 1\n$$\n\n</details>\n\n# Next\n");
        assert!(summaries(&doc).contains(&"Next".to_string()));
        let Block::Details { blocks, .. } = &doc.blocks[0] else {
            panic!("expected details, got {:?}", doc.blocks);
        };
        assert!(blocks.iter().any(|b| matches!(b, Block::Math(_))));
    }

    #[test]
    fn stray_end_tag_does_not_truncate_the_document() {
        let doc = parse("- x <object>y\n- z\n\n# After\n");
        assert!(summaries(&doc).contains(&"After".to_string()));
    }

    #[test]
    fn html_image_wrapper_with_prose_is_not_an_image() {
        let doc = parse("<p>Caption <img src=\"a.png\"></p>\n");
        assert!(!matches!(doc.blocks[0], Block::Image { .. }));
        assert!(summaries(&doc).iter().any(|t| t.contains("Caption")));
    }

    #[test]
    fn bare_emails_end_before_a_sentence_period() {
        assert!(links("a@b. Thanks").is_empty());
        assert_eq!(links("(jane@example.com)"), vec!["mailto:jane@example.com"]);
        assert_eq!(
            links("write jane@example.com."),
            vec!["mailto:jane@example.com"]
        );
    }

    #[test]
    fn html_attr_ignores_quoted_values() {
        assert_eq!(
            html_attr("<img alt=\"see src=x\" src=\"real.png\">", "src").as_deref(),
            Some("real.png")
        );
    }

    #[test]
    fn common_named_entities_are_decoded() {
        assert_eq!(
            decode_html_entities("&ldquo;Hi&rdquo; &euro;5 &rsquo;&amp;"),
            "\u{201c}Hi\u{201d} \u{20ac}5 \u{2019}&"
        );
    }

    #[test]
    fn html_image_dimensions_must_be_finite() {
        let Some(Inline::Image {
            url, width, height, ..
        }) = parse_html_img("<img src=\" a.png \" width=\"NaN\" height=\"inf\">")
        else {
            panic!("expected image");
        };
        assert_eq!(url, "a.png");
        assert_eq!((width, height), (None, None));
    }

    #[test]
    fn ordered_html_list_honours_start() {
        let doc = parse("<ol start=\"5\">\n<li>a</li>\n</ol>\n");
        let Block::Group(blocks) = &doc.blocks[0] else {
            panic!("expected converted HTML, got {:?}", doc.blocks);
        };
        assert!(matches!(
            blocks[0],
            Block::List {
                ordered: true,
                start: 5,
                ..
            }
        ));
    }

    #[test]
    fn crlf_documents_parse_like_lf_documents() {
        let doc = parse("---\r\ntitle: Doc\r\n---\r\n# Body\r\n\r\nText\r\n");
        assert!(doc.metadata.iter().any(|(k, v)| k == "title" && v == "Doc"));
        assert_eq!(
            summaries(&doc),
            vec!["Body".to_string(), "Text".to_string()]
        );
    }

    #[test]
    fn block_ids_are_stripped_only_at_the_end() {
        let doc = parse("A note ^block-1\n");
        assert_eq!(summaries(&doc), vec!["A note".to_string()]);
    }

    #[test]
    fn abbreviations_skip_link_destinations_and_math() {
        let doc =
            parse("*[HTML]: HyperText\n\nSee [HTML](https://x.test/HTML), $HTML$ and HTML.\n");
        let Block::Paragraph(inl) = &doc.blocks[0] else {
            panic!("expected paragraph");
        };
        assert!(inl.iter().any(|i| matches!(
            i,
            Inline::Link { url, .. } if url == "https://x.test/HTML"
        )));
        assert!(inl.iter().any(|i| matches!(
            i,
            Inline::Math { tex, .. } if tex == "HTML"
        )));
        assert!(inl.iter().any(|i| matches!(i, Inline::Abbr { .. })));
    }

    #[test]
    fn error_admonition_matches_the_error_alert() {
        let admonition = parse("!!! error\n    Body\n");
        let alert = parse("> [!ERROR]\n> Body\n");
        let kind = |doc: &Document| match &doc.blocks[0] {
            Block::BlockQuote { kind, .. } => *kind,
            other => panic!("expected callout, got {other:?}"),
        };
        assert_eq!(kind(&admonition), Some(AlertKind::Danger));
        assert_eq!(kind(&admonition), kind(&alert));
    }
}
