//! Print-ready HTML generation for the PDF export.
//!
//! **Print pipeline based on the reference implementation** (ported and
//! reimplemented in Rust), extended where that output fell short:
//!
//! ```text
//! Markdown
//!   ├─ normalise CRLF, expand footnotes / admonitions / grid tables / abbreviations
//!   ├─ protect fenced code blocks with placeholders
//!   ├─ Pandoc fenced divs, media embeds → HTML
//!   ├─ strip YAML frontmatter, heading attributes, [TOC] → HTML contents list
//!   ├─ protect inline code with placeholders
//!   ├─ preprocess math ($…$ / $$…$$ → typeset PNG images)
//!   ├─ preprocess emoji shortcodes (:rocket:)
//!   ├─ preprocess wikilinks and wiki embeds ([[Note|Label]] → Markdown links)
//!   ├─ preprocess callouts (> [!NOTE] → styled HTML blocks)
//!   ├─ inline tags (#tag) and citations ([@key]), inline <svg> → PNG images
//!   ├─ resolve local images (![]() and <img src>) → base64 data URIs (needs base_dir)
//!   ├─ restore inline code; Mermaid fences → PNG images
//!   ├─ pulldown-cmark push_html (GFM | TABLES | TASKLISTS | FOOTNOTES)
//!   └─ ammonia sanitize (script removed, tables/img/details kept)
//!        └─ wrap in the print template (the reference implementation's +
//!           .markdown-body print CSS) → temporary HTML
//!              └─ weasyprint CLI → PDF  (see export.rs)
//! ```
//!
//! The preprocessors, sanitizer, template and print CSS are ported from
//! the reference implementation, with deliberate improvements: WeasyPrint is mandatory (no
//! fallback writer), callouts keep their body as Markdown, math and Mermaid
//! diagrams are typeset to PNGs, and display math may span lines.

use ammonia::Builder as SanitizerBuilder;
use regex::{Captures, Regex};
use std::path::Path;
use std::sync::LazyLock;

// ---------------------------------------------------------------------------
// Emoji shortcodes (ported from the reference implementation lib.rs)
// ---------------------------------------------------------------------------

fn emoji_map() -> &'static [(&'static str, char)] {
    &[
        ("rocket", '🚀'),
        ("heart", '❤'),
        ("thumbsup", '👍'),
        ("+1", '👍'),
        ("smile", '😊'),
        ("fire", '🔥'),
        ("star", '⭐'),
        ("eye", '👁'),
        ("memo", '📝'),
        ("warning", '⚠'),
        ("sparkles", '✨'),
        ("bulb", '💡'),
        ("lock", '🔒'),
        ("unlock", '🔓'),
        ("check", '✅'),
        ("x", '❌'),
        ("question", '❓'),
        ("lightning", '⚡'),
        ("bell", '🔔'),
        ("gear", '⚙'),
        ("book", '📖'),
        ("link", '🔗'),
        ("clipboard", '📋'),
        ("pencil", '✏'),
        ("zap", '⚡'),
        ("globe", '🌍'),
        ("camera", '📷'),
        ("music", '🎵'),
        ("sun", '☀'),
        ("moon", '🌙'),
        ("cloud", '☁'),
        ("rain", '🌧'),
        ("snow", '❄'),
        ("umbrella", '☂'),
        ("anchor", '⚓'),
        ("hammer", '🔨'),
        ("wrench", '🔧'),
        ("shield", '🛡'),
        ("key", '🔑'),
        ("gift", '🎁'),
        ("trophy", '🏆'),
        ("medal", '🎖'),
        ("flag", '🚩'),
        ("target", '🎯'),
        ("chart", '📊'),
        ("bar", '📈'),
        ("email", '📧'),
        ("phone", '📱'),
        ("computer", '💻'),
        ("mobile", '📲'),
        ("desktop", '🖥'),
        ("printer", '🖨'),
        ("battery", '🔋'),
        ("movie", '🎬'),
        ("game", '🎮'),
        ("sports", '⚽'),
        ("music_note", '🎶'),
        ("art", '🎨'),
        ("microphone", '🎤'),
        ("headphone", '🎧'),
        ("tv", '📺'),
        ("frame", '🖼'),
        ("palette", '🎨'),
    ]
}

/// Preprocess emoji shortcodes (`:emoji:`) into unicode characters.
/// (Ported 1:1 from the reference implementation.)
fn preprocess_emojis(markdown: &str) -> String {
    static EMOJI_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r":([a-z0-9_+-]+):").unwrap());
    EMOJI_RE
        .replace_all(markdown, |caps: &Captures| {
            let key = &caps[1];
            for (k, v) in emoji_map() {
                if *k == key {
                    return v.to_string();
                }
            }
            caps[0].to_string() // no match, keep original
        })
        .into_owned()
}

// ---------------------------------------------------------------------------
// Math preprocessing
// ---------------------------------------------------------------------------

/// Renders one formula into a print-sized PNG data URI.
///
/// WeasyPrint cannot run MathJax/KaTeX, so formulas are typeset up-front with
/// the same pure-Rust engine as the preview (`latex-rust`) and embedded as
/// high-resolution PNGs. The `width` attribute converts the raster at
/// `DPI` into CSS pixels (96 px/inch) so the formula matches the body text
/// size instead of being blown up to its raw pixel size.
fn math_img(tex: &str, display: bool, font: &latex_rust::MathFont) -> Option<(String, u32)> {
    const DPI: i64 = 300;
    let source = if display {
        crate::parser::normalize_display_math(tex)
    } else {
        tex.to_string()
    };
    let mut options = latex_rust::PngOptions::new();
    options.font_size_pt = latex_rust::Dim::from_i64(if display { 14 } else { 8 });
    options.dpi = latex_rust::Dim::from_i64(DPI);
    options.color = latex_rust::Color::rgb(26, 26, 26);
    options.display = display;

    let png = latex_rust::latex_to_png(&source, font, &options).ok()?;
    let (width, _) =
        image::ImageReader::with_format(std::io::Cursor::new(&png), image::ImageFormat::Png)
            .into_dimensions()
            .ok()?;
    let width_css = (width as f32 * 96.0 / DPI as f32).round().max(1.0) as u32;
    Some((
        format!("data:image/png;base64,{}", base64_encode(&png)),
        width_css,
    ))
}

/// Builds the HTML for one math expression, or falls back to styled TeX
/// source when the formula cannot be typeset.
fn math_html(tex: &str, display: bool, font: Option<&latex_rust::MathFont>) -> String {
    let alt = encode_safe(tex.trim());
    let img = font.and_then(|f| math_img(tex, display, f));
    match (display, img) {
        (true, Some((uri, width))) => format!(
            "\n\n<div class=\"math-display\"><img src=\"{uri}\" alt=\"{alt}\" width=\"{width}\"></div>\n\n"
        ),
        (false, Some((uri, width))) => {
            format!("<img class=\"math-inline\" src=\"{uri}\" alt=\"{alt}\" width=\"{width}\">")
        }
        (true, None) => format!(
            "\n\n<div class=\"math-display\">{}</div>\n\n",
            encode_safe(tex.trim())
        ),
        (false, None) => format!(
            "<span class=\"math-inline\">{}</span>",
            encode_safe(tex.trim())
        ),
    }
}

/// Preprocess LaTeX math expressions (`$inline$` / `$$display$$`) into
/// embedded, typeset images.
///
/// Display math is matched across newlines (DOTALL), so multi-line formulas
/// and top-level `\\` row breaks work like in the preview.
fn preprocess_math(markdown: &str) -> String {
    // Load the math font once per document instead of once per formula.
    let font = latex_rust::MathFont::stix_two_math().ok();
    static BLOCK_MATH_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)\$\$(.+?)\$\$").unwrap());
    static INLINE_MATH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\$([^\$]+)\$").unwrap());

    let mut result = markdown.to_string();

    // Process display math first ($$...$$), replace with placeholders.
    let mut block_placeholders: Vec<(usize, String)> = Vec::new();
    let mut idx = 0usize;
    let temp = BLOCK_MATH_RE.replace_all(&result, |caps: &Captures| {
        let placeholder = format!("\x00BLOCK_MATH_{}\x00", idx);
        block_placeholders.push((idx, math_html(&caps[1], true, font.as_ref())));
        idx += 1;
        placeholder
    });
    result = temp.into_owned();

    // Process inline math ($...$) on the result with placeholders.
    result = INLINE_MATH_RE
        .replace_all(&result, |caps: &Captures| {
            math_html(&caps[1], false, font.as_ref())
        })
        .into_owned();

    // Restore display math blocks.
    for (i, html) in block_placeholders.iter() {
        let placeholder = format!("\x00BLOCK_MATH_{}\x00", i);
        result = result.replace(&placeholder, html);
    }

    result
}

// ---------------------------------------------------------------------------
// Callout preprocessing (ported)
// ---------------------------------------------------------------------------

/// Preprocess GitHub/Obsidian-style callouts (`[!TYPE]`, `+`/`-` fold markers)
/// into styled HTML blocks. The body stays Markdown, separated from the
/// block-level HTML by blank lines (as in [`preprocess_fenced_divs_html`]).
/// (Based on the reference implementation.)
fn preprocess_callouts(markdown: &str) -> String {
    static CALLOUT_HEADER_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^> \[!(\w+)]([+-])?(?:\s+(.*))?$").unwrap());

    let lines: Vec<&str> = markdown.lines().collect();
    let mut result = String::new();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        if let Some(cap) = CALLOUT_HEADER_RE.captures(line) {
            let kind = cap[1].to_lowercase();
            let fold = cap.get(2).map(|m| m.as_str());
            let title = cap
                .get(3)
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default();
            let mut chars = kind.chars();
            let mut summary: String = chars
                .next()
                .into_iter()
                .flat_map(char::to_uppercase)
                .collect();
            summary.push_str(chars.as_str());
            if !title.is_empty() {
                summary = encode_safe(&title);
            }

            // Collect content lines (`>` blank lines keep paragraphs apart).
            i += 1;
            let mut content_lines: Vec<String> = Vec::new();
            while i < lines.len() {
                if let Some(content) = lines[i].strip_prefix("> ") {
                    content_lines.push(content.to_string());
                    i += 1;
                } else if lines[i].trim() == ">" {
                    content_lines.push(String::new());
                    i += 1;
                } else {
                    break;
                }
            }
            let body = content_lines.join("\n");

            let (open, close) = match fold {
                Some("+") => (
                    format!(
                        "<details class=\"callout {kind}\" open=\"open\"><summary>{summary}</summary>"
                    ),
                    "</details>",
                ),
                Some(_) => (
                    format!("<details class=\"callout {kind}\"><summary>{summary}</summary>"),
                    "</details>",
                ),
                None => (
                    format!("<div class=\"callout {kind}\">\n\n<p><strong>{summary}</strong></p>"),
                    "</div>",
                ),
            };
            // Blank lines around the body let it parse as Markdown. The block
            // also needs a trailing blank line, otherwise CommonMark keeps
            // consuming the following block (e.g. a fenced code block) as raw
            // HTML and it is rendered as literal text.
            result.push_str(&format!("{open}\n\n{body}\n\n{close}\n\n"));
            continue;
        }
        result.push_str(line);
        result.push('\n');
        i += 1;
    }

    result
}

// ---------------------------------------------------------------------------
// Wikilink preprocessing (ported)
// ---------------------------------------------------------------------------

/// Preprocess wikilinks (`[[link]]`) into standard Markdown links.
/// Supports: `[[Page]]`, `[[#Heading]]`, `[[Page#Heading]]`, `[[Page|Display]]`.
/// Embeds (`![[…]]`) become images when they name an image file, and labelled
/// spans otherwise: the PDF cannot show an embedded note.
/// (Ported from the reference implementation.)
fn preprocess_wikilinks(markdown: &str) -> String {
    static WIKILINK_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(!?)\[\[(.*?)\]\]").unwrap());

    WIKILINK_RE
        .replace_all(markdown, |caps: &Captures| {
            let target = &caps[2];
            // Split on | for display text: [[Page|Display]]
            let (link_target, display_text) = if let Some(pipe_pos) = target.find('|') {
                (&target[..pipe_pos], target[pipe_pos + 1..].to_string())
            } else {
                (target, target.to_string())
            };
            if !caps[1].is_empty() {
                return if image_mime(Path::new(link_target)).is_some() {
                    let src = encode_safe(link_target);
                    format!("<img src=\"{src}\" alt=\"{src}\">")
                } else {
                    format!(
                        "<span class=\"embed\">Embedded note: {}</span>",
                        encode_safe(&display_text)
                    )
                };
            }
            if let Some(rest) = link_target.strip_prefix('#') {
                // In-page anchor: [[#Heading]] or [[#Heading|Display]]
                let heading = rest.to_lowercase().replace(' ', "-");
                // Strip leading # from display text for clean rendering
                let display = display_text.strip_prefix('#').unwrap_or(&display_text);
                let anchor = format!("#{heading}");
                format!("[{display}]({anchor})")
            } else if link_target.contains('#') {
                // Page with anchor: [[Page#Heading]] or [[Page#Heading|Display]]
                let parts: Vec<&str> = link_target.splitn(2, '#').collect();
                let page = parts[0].to_lowercase().replace(' ', "-") + ".html";
                let anchor = parts[1].to_lowercase().replace(' ', "-");
                format!("[{display_text}]({page}#{anchor})")
            } else {
                // Simple page link: [[Page]] or [[Page|Display]]
                let link = link_target.to_lowercase().replace(' ', "-") + ".html";
                format!("[{display_text}]({link})")
            }
        })
        .into_owned()
}

// ---------------------------------------------------------------------------
// Image path resolution (ported)
// ---------------------------------------------------------------------------

/// Simple base64 encoder for inline image data URIs (ported).
pub(crate) fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    let mut i = 0;
    let len = data.len();
    while i < len {
        let remaining = len - i;
        let b0 = data[i] as u32;
        if remaining > 1 {
            let b1 = data[i + 1] as u32;
            if remaining > 2 {
                let b2 = data[i + 2] as u32;
                let buf = (b0 << 16) | (b1 << 8) | b2;
                result.push(CHARS[(buf >> 18) as usize] as char);
                result.push(CHARS[((buf >> 12) & 0x3F) as usize] as char);
                result.push(CHARS[((buf >> 6) & 0x3F) as usize] as char);
                result.push(CHARS[(buf & 0x3F) as usize] as char);
                i += 3;
            } else {
                let buf = (b0 << 16) | (b1 << 8);
                result.push(CHARS[(buf >> 18) as usize] as char);
                result.push(CHARS[((buf >> 12) & 0x3F) as usize] as char);
                result.push(CHARS[((buf >> 6) & 0x3F) as usize] as char);
                result.push('=');
                i += 2;
            }
        } else {
            let buf = b0 << 16;
            result.push(CHARS[(buf >> 18) as usize] as char);
            result.push(CHARS[((buf >> 12) & 0x3F) as usize] as char);
            result.push('=');
            result.push('=');
            i += 1;
        }
    }
    result
}

/// MIME type of an image file, judged by its extension.
fn image_mime(path: &Path) -> Option<&'static str> {
    Some(
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "svg" => "image/svg+xml",
            "webp" => "image/webp",
            "bmp" => "image/bmp",
            "ico" => "image/x-icon",
            "tiff" | "tif" => "image/tiff",
            "avif" => "image/avif",
            _ => return None,
        },
    )
}

/// Read an image file and return a base64 data URI (ported).
fn read_image_as_data_uri(path: &Path) -> Option<String> {
    let mime = image_mime(path)?;
    let data = std::fs::read(path).ok()?;
    Some(format!("data:{mime};base64,{}", base64_encode(&data)))
}

/// Renders a ```mermaid fence to a print-ready image block.
///
/// WeasyPrint has no JavaScript runtime, so Mermaid diagrams are laid out and
/// rasterized up-front with the same pure-Rust engine as the preview. Returns
/// `None` for non-Mermaid fences and rendering failures (the fence is then
/// kept as a normal code block).
fn mermaid_html(fence: &str) -> Option<String> {
    let rest = fence.strip_prefix("```")?;
    let newline = rest.find('\n')?;
    if !rest[..newline].trim().eq_ignore_ascii_case("mermaid") {
        return None;
    }
    let code = rest[newline + 1..].strip_suffix("```")?;

    let svg = mermaid_svg::render(code).ok()?;
    // Render at 2x for crisp print output, capped to a sane pixmap size.
    let raster = crate::svg::rasterize(&svg, 2.0, 2400.0)?;
    let png = raster.pixmap.encode_png().ok()?;
    let width_css = raster.width.round().max(1.0) as u32;
    Some(format!(
        "\n\n<div class=\"mermaid\"><img src=\"data:image/png;base64,{}\" \
         alt=\"Mermaid diagram\" width=\"{width_css}\"></div>\n\n",
        base64_encode(&png)
    ))
}

/// True when the `#` at `i` may start a tag: it follows whitespace or `(`, but
/// a `(` that opens a link destination (`](#heading)`) does not count.
fn tag_can_start_at(chars: &[char], i: usize) -> bool {
    match chars[i - 1] {
        c if c.is_whitespace() => true,
        '(' => !(i >= 2 && chars[i - 2] == ']'),
        _ => false,
    }
}

/// Wraps Obsidian tags and Pandoc citations in styled spans. Code fences and
/// inline code are already protected by placeholders at this point.
fn preprocess_tags_and_citations(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    let chars: Vec<char> = markdown.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        // Citation: [@key] / [-@key], but not a Markdown link label.
        let citation_start = chars[i] == '['
            && (chars.get(i + 1) == Some(&'@')
                || (chars.get(i + 1) == Some(&'-') && chars.get(i + 2) == Some(&'@')));
        if citation_start && let Some(end_rel) = chars[i..].iter().position(|&c| c == ']') {
            {
                let end = i + end_rel;
                let after = chars.get(end + 1).copied();
                if !matches!(after, Some('(') | Some('[')) {
                    let value: String = chars[i + 1..end].iter().collect();
                    out.push_str("<span class=\"citation\">[");
                    out.push_str(&encode_safe(&value));
                    out.push_str("]</span>");
                    i = end + 1;
                    continue;
                }
            }
        }

        if chars[i] == '#' && i > 0 && tag_can_start_at(&chars, i) {
            let mut end = i + 1;
            while end < chars.len()
                && (chars[end].is_alphanumeric() || matches!(chars[end], '_' | '-' | '/'))
            {
                end += 1;
            }
            if end > i + 1 {
                let tag: String = chars[i + 1..end].iter().collect();
                out.push_str("<span class=\"tag\">#");
                out.push_str(&encode_safe(&tag));
                out.push_str("</span>");
                i = end;
                continue;
            }
        }

        out.push(chars[i]);
        i += 1;
    }
    out
}

/// URLs that are left for WeasyPrint as they are: remote, inline and fragments.
fn is_remote_or_inline(url: &str) -> bool {
    url.starts_with("http://")
        || url.starts_with("https://")
        || url.starts_with("data:")
        || url.starts_with('#')
}

/// Data URI for a local image. Relative and root-relative (`/a.png`) paths
/// are taken from the document folder, as in the preview.
fn local_image_uri(url: &str, base_dir: Option<&Path>) -> Option<String> {
    read_image_as_data_uri(&base_dir?.join(url.trim_start_matches('/')))
}

/// Quoted `src` / `alt` attribute of an HTML tag: group 1 is the whitespace
/// before it, group 2 the name, groups 3 and 4 the double- or single-quoted value.
static HTML_ATTR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)(\s)(src|alt)\s*=\s*(?:"([^"]*)"|'([^']*)')"#).unwrap());

/// Value of a quoted `src` or `alt` attribute in an HTML tag.
fn html_attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    HTML_ATTR_RE
        .captures_iter(tag)
        .find(|caps| caps[2].eq_ignore_ascii_case(name))
        .and_then(|caps| caps.get(3).or(caps.get(4)))
        .map(|m| m.as_str())
}

/// Resolves the `src` of one raw `<img>` tag like a Markdown image. A missing
/// local image is replaced by its alt text.
fn resolve_html_img(tag: &str, base_dir: Option<&Path>) -> String {
    let Some(src) = html_attr(tag, "src") else {
        return tag.to_string();
    };
    if is_remote_or_inline(src) {
        return tag.to_string();
    }
    match local_image_uri(src, base_dir) {
        Some(uri) => HTML_ATTR_RE
            .replace_all(tag, |caps: &Captures| {
                if caps[2].eq_ignore_ascii_case("src") {
                    format!("{}src=\"{uri}\"", &caps[1])
                } else {
                    caps[0].to_string()
                }
            })
            .into_owned(),
        None => html_attr(tag, "alt").unwrap_or_default().to_string(),
    }
}

/// Resolves local images in Markdown (`![alt](path)`) and raw HTML
/// (`<img src="path">`) to base64 data URIs, so WeasyPrint never reads files
/// relative to the temporary HTML file. Remote and `data:` URLs are kept. A
/// local image that cannot be loaded (or has no document folder to resolve
/// against) is replaced by its alt text. Runs before markdown parsing so the
/// parser sees data URIs natively.
fn resolve_image_paths(text: &str, base_dir: Option<&Path>) -> String {
    static MD_IMAGE_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"!\[([^\]]*)\]\(([^)]+)\)").unwrap());
    static HTML_IMG_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?is)<img\b[^>]*>").unwrap());

    let text = MD_IMAGE_RE.replace_all(text, |caps: &Captures| {
        // The destination ends at the first space; an optional title follows.
        let (url, title) = caps[2].split_at(caps[2].find(' ').unwrap_or(caps[2].len()));
        if is_remote_or_inline(url) {
            return caps[0].to_string();
        }
        match local_image_uri(url, base_dir) {
            Some(uri) => format!("![{}]({uri}{title})", &caps[1]),
            None => caps[1].to_string(),
        }
    });
    HTML_IMG_RE
        .replace_all(&text, |caps: &Captures| {
            resolve_html_img(&caps[0], base_dir)
        })
        .into_owned()
}

// ---------------------------------------------------------------------------
// Main rendering pipeline (based on the reference implementation)
// ---------------------------------------------------------------------------

/// Rasterizes inline/block `<svg>` elements to PNG images for WeasyPrint
/// (resvg ignores scripts).
fn preprocess_svg(markdown: &str) -> String {
    static SVG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<svg\b.*?</svg>").unwrap());
    SVG_RE
        .replace_all(
            markdown,
            |caps: &Captures| match crate::parser::svg_to_png_data_uri(&caps[0]) {
                Some(uri) => format!("<img src=\"{uri}\" alt=\"SVG image\">"),
                None => caps[0].to_string(),
            },
        )
        .into_owned()
}

/// Replaces `<video>`, `<audio>` and `<iframe>` embeds with labelled links.
fn preprocess_media_html(markdown: &str) -> String {
    static MEDIA_RES: LazyLock<Vec<(Regex, &'static str, &'static str)>> = LazyLock::new(|| {
        [
            ("video", "Video", "🎬"),
            ("audio", "Audio", "🔊"),
            ("iframe", "Embed", "🌐"),
        ]
        .into_iter()
        .map(|(tag, kind, icon)| {
            let pattern = format!(
                r#"(?is)<{tag}\b[^>]*?src\s*=\s*["']([^"']+)["'][^>]*?(?:/>|>.*?</{tag}>)"#
            );
            (Regex::new(&pattern).unwrap(), kind, icon)
        })
        .collect()
    });

    let mut out = markdown.to_string();
    for (re, kind, icon) in MEDIA_RES.iter() {
        out = re
            .replace_all(&out, |caps: &Captures| {
                format!(
                    "<p class=\"media\">{icon} <a href=\"{}\" rel=\"noopener noreferrer\">{kind}</a></p>",
                    encode_safe(&caps[1])
                )
            })
            .into_owned();
    }
    out
}

/// Replaces a standalone `[TOC]` line with an HTML contents list built from
/// the document's ATX headings. Lines inside code fences are ignored.
fn insert_toc_html(markdown: &str) -> String {
    let mut fence: Option<(char, usize)> = None;
    // Each line with a flag telling whether it is code (fence markers included).
    let lines: Vec<(&str, bool)> = markdown
        .split_inclusive('\n')
        .map(|line| {
            let marker = crate::parser::fence_marker_for_export(line);
            let in_code = fence.is_some() || marker.is_some();
            if let Some((c, n)) = marker {
                match fence {
                    Some((fc, open)) if fc == c && n >= open => fence = None,
                    None => fence = Some((c, n)),
                    _ => {}
                }
            }
            (line, in_code)
        })
        .collect();

    let mut headings: Vec<(u8, &str)> = Vec::new();
    for &(line, in_code) in &lines {
        if in_code {
            continue;
        }
        let trimmed = line.trim_start();
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && trimmed.chars().nth(hashes) == Some(' ') {
            let mut text = trimmed[hashes..].trim();
            if let Some(start) = text.rfind('{')
                && text.ends_with('}')
            {
                text = text[..start].trim_end();
            }
            headings.push((hashes as u8, text));
        }
    }
    if headings.is_empty() {
        return markdown.to_string();
    }

    let mut out = String::with_capacity(markdown.len() + 64);
    for (line, in_code) in lines {
        if !in_code && line.trim() == "[TOC]" {
            out.push_str("<div class=\"toc\"><strong>Contents</strong><ul>");
            for (level, text) in &headings {
                out.push_str(&format!(
                    "<li class=\"toc-level-{level}\">{}</li>",
                    encode_safe(text)
                ));
            }
            out.push_str("</ul></div>\n");
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Converts Pandoc/Quarto fenced divs to styled HTML containers. Blank lines
/// keep the inner Markdown parseable by pulldown-cmark.
fn preprocess_fenced_divs_html(markdown: &str) -> String {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut out = String::with_capacity(markdown.len() + 32);
    let mut i = 0usize;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if trimmed.starts_with(":::")
            && !trimmed[3..].trim().is_empty()
            && !trimmed.chars().all(|c| c == ':')
        {
            let info = trimmed[3..].trim();
            // `::: warning` or `::: {#id .warning}`: the first `.class` wins.
            let class = match info.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                Some(attrs) => attrs.split_whitespace().find_map(|t| t.strip_prefix('.')),
                None => info.split_whitespace().next(),
            }
            .unwrap_or("div");
            let class = encode_safe(class);
            let mut body: Vec<&str> = Vec::new();
            let mut j = i + 1;
            let mut closed = false;
            while j < lines.len() {
                let candidate = lines[j].trim();
                if !candidate.is_empty() && candidate.chars().all(|c| c == ':') {
                    closed = true;
                    j += 1;
                    break;
                }
                body.push(lines[j]);
                j += 1;
            }
            if closed {
                out.push_str(&format!("<div class=\"fenced-div {class}\">\n\n"));
                for line in body {
                    out.push_str(line);
                    out.push('\n');
                }
                out.push_str("\n</div>\n");
                i = j;
                continue;
            }
        }
        out.push_str(lines[i]);
        out.push('\n');
        i += 1;
    }
    out
}

/// Renders raw Markdown into sanitized HTML body content for the print template.
///
/// Fenced and inline code are protected by placeholders while the text
/// preprocessors run, so they never alter code, and are restored just before
/// pulldown-cmark. Frontmatter is stripped, and line endings are normalised to
/// LF first. Local images are resolved to data URIs relative to `base_dir`
/// (the document folder). The final pulldown-cmark HTML is sanitized with
/// ammonia (same tag/attribute policy as the reference implementation).
fn render_markdown(markdown: &str, base_dir: Option<&Path>) -> String {
    let markdown = markdown.replace("\r\n", "\n");
    let markdown = &crate::parser::expand_inline_footnotes(&markdown);
    let markdown = &crate::parser::convert_admonitions(markdown);
    let markdown = &crate::parser::convert_grid_tables(markdown);
    let markdown = &crate::parser::preprocess_abbreviations(markdown);

    // Phase 0 — Extract fenced blocks so regex preprocessors don't alter code.
    static FENCE_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)```[^\n`]*\n.*?```").unwrap());
    let mut fence_blocks: Vec<String> = Vec::new();
    let without_fences = FENCE_RE
        .replace_all(markdown, |caps: &Captures| {
            let idx = fence_blocks.len();
            fence_blocks.push(caps[0].to_string());
            format!("\x00FENCED_BLOCK_{}\x00", idx)
        })
        .into_owned();

    // Pandoc/Quarto fenced divs (`::: warning … :::`).
    let without_fences = preprocess_fenced_divs_html(&without_fences);
    // Media embeds become labelled links (WeasyPrint cannot play them).
    let without_fences = preprocess_media_html(&without_fences);

    // Phase 0a — Strip YAML frontmatter so delimiters don't render as rules.
    let without_frontmatter = if let Some(rest) = without_fences.strip_prefix("---\n") {
        if let Some(end_pos) = rest.find("\n---\n") {
            rest[end_pos + 5..].to_string()
        } else {
            without_fences.clone()
        }
    } else {
        without_fences.clone()
    };

    // Strip Pandoc/Markdown-Extra heading attributes from the displayed text.
    static HEADING_ATTRS_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?m)^(#{1,6}[ \t]+.*?)[ \t]*\{[#.][^}\n]*\}[ \t]*$").unwrap()
    });
    let without_frontmatter = HEADING_ATTRS_RE
        .replace_all(&without_frontmatter, "$1")
        .into_owned();

    // `[TOC]` → generated HTML contents list.
    let without_frontmatter = insert_toc_html(&without_frontmatter);

    // Phase 0b — Extract inline code (single backticks) for the same reason.
    static INLINE_CODE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`[^`]+`").unwrap());
    let mut inline_blocks: Vec<String> = Vec::new();
    let without_inline = INLINE_CODE_RE
        .replace_all(&without_frontmatter, |caps: &Captures| {
            let idx = inline_blocks.len();
            inline_blocks.push(caps[0].to_string());
            format!("\x00INLINE_CODE_{}\x00", idx)
        })
        .into_owned();

    // 1. Preprocess math
    let with_math = preprocess_math(&without_inline);
    // 2. Preprocess emojis
    let with_emojis = preprocess_emojis(&with_math);
    // 3. Preprocess wikilinks
    let with_wikilinks = preprocess_wikilinks(&with_emojis);
    // 4. Preprocess callouts
    let with_callouts = preprocess_callouts(&with_wikilinks);
    // 5. Inline Obsidian tags and Pandoc citation markers.
    let with_tags = preprocess_tags_and_citations(&with_callouts);
    // 6. Inline SVG blocks are rasterized safely (resvg ignores scripts).
    let with_svg = preprocess_svg(&with_tags);
    // 7. Resolve local images to data URIs (see resolve_image_paths).
    let mut restored = resolve_image_paths(&with_svg, base_dir);

    // Restore code only now: the passes above must not see code content.
    for (idx, block) in inline_blocks.iter().enumerate() {
        let placeholder = format!("\x00INLINE_CODE_{}\x00", idx);
        restored = restored.replace(&placeholder, block);
    }
    for (idx, block) in fence_blocks.iter().enumerate() {
        let placeholder = format!("\x00FENCED_BLOCK_{}\x00", idx);
        let replacement = mermaid_html(block).unwrap_or_else(|| block.clone());
        restored = restored.replace(&placeholder, &replacement);
    }

    // 8. Parse markdown (same option set as the reference implementation)
    use pulldown_cmark::{Options, Parser, html::push_html};
    let mut options = Options::empty();
    options.insert(Options::ENABLE_GFM);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);
    let parser = Parser::new_ext(&restored, options);
    let mut unsafe_html = String::new();
    push_html(&mut unsafe_html, parser);

    // 9. Sanitize HTML (same policy as the reference implementation)
    SANITIZER.clean(&unsafe_html).to_string()
}

/// Sanitizer policy for the printed HTML: the reference implementation's tags
/// and attributes plus the elements this pipeline emits.
static SANITIZER: LazyLock<SanitizerBuilder<'static>> = LazyLock::new(|| {
    let mut builder = SanitizerBuilder::new();
    builder
        .add_tags(&[
            "table", "thead", "tbody", "tr", "th", "td", "input", "details", "summary", "img",
            "abbr",
        ])
        .add_tag_attributes("abbr", &["title"])
        .add_tag_attributes("input", &["type", "checked"])
        .add_tag_attributes("code", &["class"])
        .add_tag_attributes("span", &["class"])
        .add_tag_attributes("div", &["class"])
        .add_tag_attributes("details", &["class", "open"])
        .add_tag_attributes("summary", &["class"])
        .add_tag_attributes("img", &["src", "alt", "width", "height", "class"])
        .add_url_schemes(&["data"]);
    builder
});

// ---------------------------------------------------------------------------
// Print template (ported from the reference implementation)
// ---------------------------------------------------------------------------

/// Escapes `&`, `<`, `>`, `"` and `'` for HTML text and attribute values.
fn encode_safe(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            c => out.push(c),
        }
    }
    out
}

/// The `.markdown-body` content styles from the reference implementation
/// (light theme values inlined, verbatim classes) plus its `@media print`
/// block (sized headings, break rules, black links).
const MARKDOWN_BODY_CSS: &str = r##"
  .markdown-body { color: #1a1a1a; }
  .markdown-body h1 { font-size: 2em; font-weight: 600; margin: 1em 0 0.5em; padding-bottom: 0.3em; border-bottom: 1px solid #e0e0e0; }
  .markdown-body h2 { font-size: 1.5em; font-weight: 600; margin: 1em 0 0.5em; padding-bottom: 0.3em; border-bottom: 1px solid #e0e0e0; }
  .markdown-body h3 { font-size: 1.25em; font-weight: 600; margin: 1em 0 0.5em; }
  .markdown-body h4 { font-size: 1em; font-weight: 600; margin: 1em 0 0.5em; }
  .markdown-body h5, .markdown-body h6 { font-size: 0.875em; font-weight: 600; margin: 1em 0 0.5em; }
  .markdown-body p { margin: 0 0 1em; }
  .markdown-body a { color: #0366d6; text-decoration: none; }
  .markdown-body strong { font-weight: 600; }
  .markdown-body em { font-style: italic; }
  .markdown-body blockquote { margin: 0 0 1em; padding: 0.5em 1em; color: #1a1a1a; opacity: 0.8; border-left: 0.25em solid #e0e0e0; background: #f1f5f9; }
  .markdown-body code { background: #f6f8fa; padding: 0.2em 0.4em; border-radius: 3px; font-size: 0.875em; font-family: 'SFMono-Regular', Consolas, 'Liberation Mono', Menlo, monospace; }
  .markdown-body pre { background: #f6f8fa; padding: 16px; border-radius: 6px; margin: 0 0 1em; }
  .markdown-body pre code { background: none; padding: 0; font-size: 0.85em; line-height: 1.6; }
  .markdown-body ul, .markdown-body ol { margin: 0 0 1em; padding-left: 2em; }
  .markdown-body li { margin: 0.25em 0; }
  .markdown-body li > ul, .markdown-body li > ol { margin: 0.25em 0 0; }
  .markdown-body input[type="checkbox"] { margin-right: 0.5em; }
  .markdown-body table { border-collapse: collapse; width: 100%; margin: 0 0 1em; }
  .markdown-body th, .markdown-body td { border: 1px solid #e0e0e0; padding: 6px 13px; text-align: left; }
  .markdown-body th { font-weight: 600; background: #f6f8fa; }
  .markdown-body tr:nth-child(2n) { background: #f6f8fa; }
  .markdown-body img { max-width: 100%; height: auto; border-radius: 4px; }
  .markdown-body hr { border: none; border-top: 1px solid #e0e0e0; margin: 2em 0; }
  .markdown-body .footnote-definition { font-size: 0.85em; opacity: 0.8; margin-top: 0.5em; }
  .markdown-body .footnote-definition sup { font-weight: 600; }
  .markdown-body .footnote-definition a[href^="#fnref"] { text-decoration: none; font-weight: 600; }
  .callout { margin: 1em 0; padding: 1em; border-radius: 6px; border-left: 4px solid; }
  .callout.note { background: #dbeafe; border-color: #3b82f6; }
  .callout.tip { background: #d1fae5; border-color: #10b981; }
  .callout.warning { background: #fef3c7; border-color: #f59e0b; }
  .callout.caution { background: #fee2e2; border-color: #ef4444; }
  .callout.important { background: #ede9fe; border-color: #8b5cf6; }
  .callout summary { cursor: pointer; font-weight: 600; margin-bottom: 0.5em; outline: none; }
  .fenced-div { margin: 1em 0; padding: 0.75em 1em; border-radius: 6px; border-left: 4px solid #94a3b8; background: #f8fafc; }
  .fenced-div.warning, .fenced-div.caution, .fenced-div.danger { border-color: #f59e0b; background: #fffbeb; }
  .fenced-div.tip, .fenced-div.success { border-color: #10b981; background: #ecfdf5; }
  .fenced-div.important { border-color: #8b5cf6; background: #f5f3ff; }
  .tag { color: #0366d6; background: #eef6ff; border-radius: 3px; padding: 0 3px; }
  .toc { margin: 1em 0; padding: 0.75em 1em; background: #f8fafc; border: 1px solid #e2e8f0; border-radius: 6px; }
  .toc ul { margin: 0.5em 0 0; padding-left: 1.5em; }
  .toc .toc-level-2 { margin-left: 1em; }
  .toc .toc-level-3 { margin-left: 2em; }
  .media { margin: 1em 0; padding: 0.6em 0.8em; background: #f6f8fa; border-radius: 6px; }
  .citation { color: #57606a; font-style: italic; }
  .embed { color: #57606a; background: #f6f8fa; border: 1px solid #e0e0e0; border-radius: 3px; padding: 0 4px; }
  abbr { text-decoration: none; border-bottom: 1px dotted #94a3b8; }
  .math-inline { vertical-align: middle; color: #0366d6; }
  .math-display { margin: 1em 0; text-align: center; padding: 0.5em 0; }
  .math-display img, .mermaid img { max-width: 100%; height: auto; }
  .mermaid { margin: 1em 0; text-align: center; }
"##;

/// The print media block from the reference implementation, with one
/// deliberate pagination fix: tables may break across pages (`table` is not
/// `page-break-inside: avoid`) and the header repeats via
/// `table-header-group`; rows themselves stay intact. Keeping whole tables
/// together produced nearly empty pages in front of long tables.
const PRINT_CSS: &str = r#"
  @media print {
    body { background: white !important; color: black !important; }
    .markdown-body { font-size: 10pt; line-height: 1.25; }
    h1 { font-size: 16pt !important; }
    h2 { font-size: 13pt !important; }
    h3 { font-size: 11pt !important; }
    img, svg { max-width: 100% !important; page-break-inside: avoid; height: auto !important; }
    a { color: black !important; text-decoration: underline; }
    /* Long tables must be allowed to split across pages: forcing the whole
       table to stay on one page leaves nearly empty pages in front of it.
       Rows stay intact and the header repeats via table-header-group. */
    table { page-break-inside: auto; }
    thead { display: table-header-group; }
    tr { page-break-inside: avoid; }
    pre, blockquote, .callout { page-break-inside: avoid; }
    h1, h2, h3 { page-break-after: avoid; }
  }
"#;

/// Escapes `s` for a double-quoted CSS string inside `<style>`: backslashes,
/// quotes and control characters are escaped, and `<` so the value cannot
/// close the `<style>` element. Unlike [`encode_safe`], HTML entities would
/// print literally here.
fn css_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '<' => out.push_str("\\3C "),
            c if c.is_control() => out.push_str(&format!("\\{:X} ", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Builds the complete print document — the ported union of the reference implementation's
/// `pdf_export.rs` template (A4 page setup, footer with
/// `filename - Page n of m` counters) and its print `.markdown-body` styles.
///
/// `markdown_html` is sanitized body HTML (see [`render_markdown`]).
fn prepare_print_html(markdown_html: &str, filename: &str) -> String {
    let escaped_filename = encode_safe(filename);
    let css_filename = css_string(filename);
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>{escaped_filename}</title>
<style>
@page {{
  size: A4;
  margin: 2cm 2cm 2.5cm 2cm;
  @bottom-center {{
    content: "{css_filename} - Page " counter(page) " of " counter(pages);
    font-size: 7pt;
    color: #666;
  }}
}}
body {{
  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif;
  line-height: 1.5;
  font-size: 10pt;
  color: #1a1a1a;
}}
{MARKDOWN_BODY_CSS}
{PRINT_CSS}
</style>
</head>
<body>
<div class="markdown-body">
{markdown_html}
</div>
</body>
</html>"#,
        escaped_filename = escaped_filename,
        css_filename = css_filename,
        MARKDOWN_BODY_CSS = MARKDOWN_BODY_CSS,
        PRINT_CSS = PRINT_CSS,
        markdown_html = markdown_html
    )
}

// ---------------------------------------------------------------------------
// Public API used by export.rs
// ---------------------------------------------------------------------------

/// Renders the raw Markdown source into the complete print HTML document.
///
/// `base_dir` is the folder containing the source file, used for relative
/// image resolution (data URIs); without it local images become their alt
/// text. `filename` labels the running footer.
pub fn print_html_for(markdown: &str, base_dir: Option<&Path>, filename: &str) -> String {
    let body = render_markdown(markdown, base_dir);
    prepare_print_html(&body, filename)
}

// ---------------------------------------------------------------------------
// Tests (ported assertions from the reference implementation's render_markdown tests plus
// template coverage)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_renders_header() {
        let input = "# Header";
        assert_eq!(render_markdown(input, None), "<h1>Header</h1>\n");
    }

    /// Mermaid fences are laid out and rasterized for the PDF instead of
    /// being exported as source code.
    #[test]
    fn it_typesets_mermaid_fences() {
        let input = "```mermaid\ngraph TD;\n    A-->B;\n```";
        let output = render_markdown(input, None);
        assert!(
            output.contains("class=\"mermaid\""),
            "expected a mermaid image block: {output}"
        );
        assert!(output.contains("data:image/png;base64,"));
        assert!(!output.contains("language-mermaid"));
    }

    #[test]
    fn it_sanitizes_xss() {
        let input = "<script>alert('xss')</script>";
        assert!(!render_markdown(input, None).contains("<script>"));
    }

    #[test]
    fn it_renders_tables() {
        let input = " | Header |\n | --- |\n | Cell |";
        assert_eq!(
            render_markdown(input, None),
            "<table><thead><tr><th>Header</th></tr></thead><tbody>\n<tr><td>Cell</td></tr>\n</tbody></table>\n"
        );
    }

    #[test]
    fn it_renders_task_lists() {
        let input = "- [x] Done\n- [ ] Pending";
        assert_eq!(
            render_markdown(input, None),
            "<ul>\n<li><input type=\"checkbox\" checked=\"\">\nDone</li>\n<li><input type=\"checkbox\">\nPending</li>\n</ul>\n"
        );
    }

    #[test]
    fn it_resolves_wikilinks() {
        let input = "[[My Document]]";
        assert_eq!(
            render_markdown(input, None),
            "<p><a href=\"my-document.html\" rel=\"noopener noreferrer\">My Document</a></p>\n"
        );
    }

    #[test]
    fn it_renders_fenced_divs_and_admonitions() {
        let output = render_markdown("::: warning\n**Careful** here.\n:::\n", None);
        assert!(output.contains("fenced-div warning"));
        assert!(output.contains("<strong>Careful</strong>"));

        let output = render_markdown("!!! note \"Custom\"\n    Body text.\n", None);
        assert!(output.contains("class=\"callout note\""));
        assert!(output.contains("Custom"));
    }

    #[test]
    fn it_renders_toc() {
        let output = render_markdown("[TOC]\n\n# One\n\n## Two\n", None);
        assert!(output.contains("class=\"toc\""));
        assert!(output.contains(">One</li>"));
        assert!(output.contains(">Two</li>"));
    }

    #[test]
    fn it_renders_tags_and_citations() {
        let output = render_markdown("Tag #project and cite [@doe2024].", None);
        assert!(output.contains("class=\"tag\""));
        assert!(output.contains("class=\"citation\""));
    }

    #[test]
    fn it_renders_emoji_shortcodes() {
        let output = render_markdown(":rocket: :heart: :thumbsup:", None);
        assert!(output.contains("🚀"));
        assert!(output.contains("❤"));
        assert!(output.contains("👍"));
    }

    /// Callouts become styled divs with a lowercase class.
    #[test]
    fn it_renders_callouts() {
        let output = render_markdown("> [!NOTE]\n> careful", None);
        assert!(output.contains("div class=\"callout note\""));
        assert!(output.contains("careful"));
    }

    /// A block directly after a callout must not be swallowed into the
    /// callout's HTML block (the reference implementation bug fixed here).
    #[test]
    fn it_keeps_fence_after_callout() {
        let output = render_markdown("> [!NOTE]\n> careful\n\n```rust\nfn main() {}\n```\n", None);
        assert!(
            output.contains("<pre><code class=\"language-rust\">"),
            "fence after callout was not rendered as code: {output}"
        );
    }

    /// Inline and display math are typeset to embedded PNGs (WeasyPrint has
    /// no JavaScript runtime).
    #[test]
    fn it_typesets_math_as_images() {
        let output = render_markdown("Value $E=mc^2$ inline.\n\n$$a + b$$\n", None);
        assert!(output.contains("class=\"math-inline\""));
        assert!(output.contains("class=\"math-display\""));
        assert_eq!(output.matches("data:image/png;base64,").count(), 2);
    }

    /// Display math may span multiple lines and use top-level row breaks.
    #[test]
    fn it_typesets_multiline_display_math() {
        let output = render_markdown("$$\na = b \\\\\n\\Rightarrow c = d\n$$\n", None);
        assert!(output.contains("class=\"math-display\""));
        assert!(output.contains("<img src=\"data:image/png;base64,"));
    }

    /// Scratch folder holding a stub `dot.png`, unique per test.
    fn image_test_dir(tag: &str) -> std::path::PathBuf {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("mdv-imgtest-{tag}-{}-{ts}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("dot.png"), b"\x89PNG-stub").unwrap();
        dir
    }

    /// Local image files are embedded as data URIs.
    #[test]
    fn it_resolves_local_images_as_data_uris() {
        let dir = image_test_dir("md");
        let output = render_markdown("![Alt](dot.png)", Some(&dir));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(output.contains("data:image/png;base64,"));
        assert!(output.contains("Alt"));
    }

    /// Raw `<img>` tags and root-relative paths resolve against the document
    /// folder, like Markdown images.
    #[test]
    fn html_and_root_relative_images_are_resolved() {
        let dir = image_test_dir("html");
        let output = render_markdown(
            "<img src=\"dot.png\" alt=\"A\">\n\n![B](/dot.png)",
            Some(&dir),
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(output.matches("data:image/png;base64,").count(), 2);
        assert!(!output.contains("src=\"dot.png\""));
    }

    /// Missing or unresolvable local images show their alt text; remote
    /// images are left for WeasyPrint.
    #[test]
    fn missing_local_images_degrade_to_alt_text() {
        let dir = image_test_dir("missing");
        let output = render_markdown(
            "![Logo](missing.png)\n\n<img src=\"gone.png\" alt=\"Gone\">",
            Some(&dir),
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert!(output.contains("Logo") && output.contains("Gone"));
        assert!(!output.contains("missing.png") && !output.contains("gone.png"));
        assert!(!output.contains("<img"));

        let without_folder = render_markdown("![Logo](dot.png)", None);
        assert_eq!(without_folder, "<p>Logo</p>\n");

        let remote = render_markdown("![R](https://example.com/a.png)", None);
        assert!(remote.contains("src=\"https://example.com/a.png\""));
    }

    /// `![[image.png]]` embeds an image; other embeds become a labelled span
    /// instead of a broken image.
    #[test]
    fn wiki_embeds_are_images_or_labelled_spans() {
        let dir = image_test_dir("wiki");
        let image = render_markdown("![[dot.png]]", Some(&dir));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(image.contains("data:image/png;base64,"));

        let note = render_markdown("![[Some Note]]", None);
        assert!(note.contains("<span class=\"embed\">Embedded note: Some Note</span>"));
        assert!(!note.contains("<img"));
    }

    /// Code (fenced and inline) is restored after the text passes, so tags,
    /// emoji and image syntax inside code stay literal.
    #[test]
    fn code_content_is_left_intact() {
        let dir = image_test_dir("code");
        let input = "Use `x = 1  # note` here.\n\n```\n![](dot.png) #tag :rocket:\n```\n";
        let output = render_markdown(input, Some(&dir));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(output.contains("<code>x = 1  # note</code>"));
        assert!(output.contains("![](dot.png) #tag :rocket:"));
        assert!(!output.contains("class=\"tag\""));
        assert!(!output.contains("data:image"));
    }

    /// `#` in a link destination is not a tag; a real `(#tag)` still is.
    #[test]
    fn link_destinations_are_not_tags() {
        let output = render_markdown("[text](#heading) and [[#Heading]] but (#tag) too", None);
        assert!(output.contains("href=\"#heading\""));
        assert_eq!(output.matches("class=\"tag\"").count(), 1);
    }

    /// Non-ASCII callout types must not panic and keep their full name.
    #[test]
    fn callout_with_non_ascii_type() {
        let output = render_markdown("> [!Éclair]\n> Body", None);
        assert!(output.contains("callout éclair"), "{output}");
        assert!(output.contains("Éclair"));
    }

    /// Callout bodies are Markdown: emphasis and several paragraphs render.
    #[test]
    fn callout_bodies_render_as_markdown() {
        let output = render_markdown("> [!NOTE]\n> **bold** text\n>\n> Second paragraph.", None);
        assert!(output.contains("<strong>bold</strong> text"), "{output}");
        assert!(output.contains("<p>Second paragraph.</p>"));
    }

    /// `-` folds a callout closed and `+` opens it, as in the preview.
    #[test]
    fn callout_fold_markers() {
        let collapsed = render_markdown("> [!tip]-\n> Hidden", None);
        assert!(collapsed.contains("<details class=\"callout tip\"><summary>Tip</summary>"));
        let expanded = render_markdown("> [!tip]+\n> Shown", None);
        assert!(expanded.contains("<details class=\"callout tip\" open=\"open\">"));
    }

    /// Pandoc attribute blocks give the class from their `.class` token.
    #[test]
    fn fenced_div_attributes_yield_the_class() {
        let output = render_markdown("::: {#intro .warning}\nBody\n:::\n", None);
        assert!(output.contains("class=\"fenced-div warning\""), "{output}");
    }

    /// CRLF line endings must not hide the YAML frontmatter.
    #[test]
    fn crlf_input_strips_frontmatter() {
        let output = render_markdown("---\r\ntitle: X\r\n---\r\n# Hi\r\n", None);
        assert_eq!(output, "<h1>Hi</h1>\n");
    }

    /// The print template carries the A4 page rule, the footer counters and
    /// the markdown-body styles.
    #[test]
    fn print_template_structure() {
        let html = prepare_print_html("<p>content</p>", "test.md");
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("@page"));
        assert!(html.contains("counter(pages)"));
        assert!(html.contains("\"test.md - Page \" counter(page)"));
        assert!(html.contains("class=\"markdown-body\""));
        assert!(html.contains("<p>content</p>"));
    }

    /// The footer label is CSS-escaped inside the `content` string (no HTML
    /// entities), and the `<title>` is HTML-escaped.
    #[test]
    fn print_template_escapes_filename() {
        let html = prepare_print_html("x", "a&\"\\b.md");
        assert!(html.contains(r#"<title>a&amp;&quot;\b.md</title>"#));
        assert!(html.contains(r#"content: "a&\"\\b.md - Page " counter(page)"#));
    }

    #[test]
    fn css_string_escapes_quotes_backslashes_and_newlines() {
        assert_eq!(css_string("a\"b\\c\nd<"), "a\\\"b\\\\c\\A d\\3C ");
    }

    /// End-to-end: raw markdown → print document.
    #[test]
    fn print_html_for_end_to_end() {
        let html = print_html_for("# T\n\n| A | B |\n| - | - |\n| 1 | 2 |\n", None, "doc.md");
        assert!(html.contains("<h1>T</h1>"));
        assert!(html.contains("<td>1</td>"));
        assert!(html.contains("doc.md"));
    }
}
