# MD-Viewer — Feature List

A native Markdown editor and viewer for macOS on Apple Silicon, written in
Rust with `egui`/`eframe` (wgpu → Metal), `winit` and `pulldown-cmark`.

Status legend: ✅ implemented · ⚠️ supported with limitations · 🔜 planned

---

## 1. Markdown Rendering

The parser is built on `pulldown-cmark` (CommonMark + GitHub Flavored Markdown
extensions) and produces a UI-independent document model used by the preview.
The PDF export renders the raw Markdown through its own ported pipeline
(`html_export.rs`) rather than this model.

### 1.1 Block elements

| Feature | Status | Description |
| --- | --- | --- |
| Headings `#` … `######` | ✅ | Six levels with descending sizes. Headings are not auto-underlined; only explicit `---` rules draw a line. |
| Paragraphs | ✅ | Strict line breaks: a single newline in the source starts a new visual line (Obsidian/Typora behaviour), so pre-wrapped documents such as README.md keep their intended line structure. Hard breaks also become line breaks. |
| Ordered lists | ✅ | Respects the start number; nested lists supported. |
| Unordered lists | ✅ | Bullet markers, nested lists supported. |
| Tight list items | ✅ | Items without blank lines keep their full inline content (including items that start with a formatting span, `* **Bold**: text`) and are rendered compactly, without paragraph/block spacing between items. Loose lists keep their paragraph spacing. |
| Task lists | ✅ | GFM `- [ ]` / `- [x]` rendered as disabled checkboxes in the preview and as native HTML checkboxes in the PDF. |
| Block quotes | ✅ | Nested quotes, accent bar on the left in the preview. |
| GFM / Obsidian callouts | ✅ | `> [!NOTE]`, `TIP`, `IMPORTANT`, `WARNING`, `CAUTION` plus Obsidian `QUESTION`, `SUCCESS`, `FAILURE`, `DANGER`, `BUG`, `EXAMPLE`, `QUOTE`. Rendered as full-width cards with a colored left bar, a per-type icon (ⓘ, 💡, ⚠, ❓, …), a title-case heading and the body. Custom titles replace the default heading; `+`/`-` fold markers add a collapse toggle (expanded by default so the body is visible). |
| Fenced code blocks | ✅ | Optional language tag displayed; horizontal scrolling in the preview. |
| Indented code blocks | ✅ | Parsed as code without language tag. |
| GFM tables | ✅ | Dedicated table layout: content-sized columns that grow to the reading column and shrink proportionally when the window narrows, cells wrap onto multiple lines with adaptive row heights, header row with its own background, visible one-pixel grid, column alignment (`:---`, `:---:`, `---:`), alternating row fills. Inline styles (bold/italic/strike/highlight/code/kbd/sub/sup) are preserved in cells, bare URLs are linkified, and links stay clickable. HTML `<table>` blocks are converted to the same model with **multiple `<thead>` header rows**, `colspan`/`rowspan` (merged cells, grid lines suppressed inside spans, vertically centred content), `text-align`/`align`, and cell content parsed as inline Markdown/HTML (bold, italic, code, math, links, images). In the PDF, GFM tables render with header shading and grid borders; column alignment and HTML `colspan`/`rowspan` are not preserved (the sanitizer drops those attributes). |
| Horizontal rules `---` | ✅ | Thin separator line in preview and PDF. |
| Raw HTML blocks | ✅ | A safe subset is converted into native blocks: `<details>/<summary>`, `<table>`, `<img>` (including centered wrappers), `<div>`/`<section>`/`<article>`/`<figure>` containers with headings, paragraphs, lists, quotes, code and inline formatting (`<strong>`, `<em>`, `<code>`, `<mark>`, `<kbd>`, `<a>`, …). `<script>`, `<style>` and HTML comments are removed; inline CSS is ignored and unsupported markup stays sanitized source, never executed. |
| Mermaid diagrams | ✅ | ```` ```mermaid ```` blocks are parsed and laid out by the pure-Rust `mermaid-svg` crate and rasterized with `resvg`. Flowchart, sequence, pie, class, state, Gantt and ER diagrams use a dark theme matched to the app (charcoal nodes, light text, muted blue/pink/mint palette) inside the standard card layout. The PDF embeds the diagram as a high-resolution image. |
| Definition lists | ✅ | `Term` followed by `: definition` renders the term and indented definition blocks. |
| Front matter | ✅ | YAML (flat + nested), TOML (`+++`) and JSON front matter are shown in a compact "Properties" card with dotted nested keys; the `title` is used as a fallback window title. The PDF export strips the front matter and uses the source file name in the footer. |
| Footnote definitions | ✅ | Collected separately and rendered at the end of the document, numbered in first-reference order with their full block content (paragraphs, lists, code). Inline Pandoc notes `^[text]` are expanded automatically. |
| Fenced divs / admonitions | ✅ | Pandoc/Quarto `::: warning` divs and MkDocs `!!! note "Title"` + indented bodies render as colored cards (warning/tip/important variants). |
| Grid tables | ⚠️ | Simple single-line Pandoc grid tables (`+---+---+`) are converted to GFM tables. Multi-line cells and row spans are not reconstructed. |
| Table of contents | ✅ | A standalone `[TOC]` expands to a contents list built from the document headings. |
| Media embeds | ⚠️ | `<video>`, `<audio>` and `<iframe>` become labelled cards that open the URL; no in-app playback (would require a webview). |
| Inline SVG | ✅ | Safe SVG blocks are rasterized with `resvg` and shown as images; scripts are never executed. |

### 1.2 Inline elements

| Feature | Status | Description |
| --- | --- | --- |
| Bold `**text**` | ✅ | Bold font in preview and PDF. |
| Italic `*text*` | ✅ | Italic font in preview and PDF. |
| Nested inline styles | ✅ | Enclosing styles combine instead of being flattened: `***bold italic***`, `**bold with *italic* and ~~strike~~**`, code/highlight/links inside strong spans, and nested super/subscript. |
| Strikethrough `~~text~~` | ✅ | Strikethrough in preview and PDF (`<del>`, real PDF strikethrough styling). |
| Highlight `==text==` | ✅ | Bright yellow background with dark text in preview and in table cells. The PDF pipeline follows GFM and prints the literal `==text==` markers. |
| Keyboard `<kbd>` | ✅ | Framed monospace key cap in preview; monospace text in the PDF. |
| Underline `<u>…</u>` | ✅ | Underlined in preview and PDF. |
| Inline code `` `code` `` | ✅ | Rounded monospace pill with a padded background in the preview; monospace with light background in PDF. |
| Links `[text](url)` | ✅ | Clickable in the preview (opens in the default browser); clickable **link annotation** in the PDF. |
| GFM autolinks | ✅ | Bare `https://…` URLs and `name@example.com` addresses become clickable links outside code spans. |
| Images `![alt](url)` | ✅ | Local and remote images (PNG/JPEG/GIF/WebP/BMP/TIFF/ICO/SVG; AVIF via macOS `sips`) with optional title. Relative, parent-relative and root-relative (`/assets/…`) paths resolve against the document folder. Remote SVGs without a `.svg` suffix (e.g. shields.io badges) are detected by content type/payload and rasterized. Remote downloads use the system `curl` on a background thread, so they also work in release builds and behind VPNs/proxies. HTML `<img width height>` dimensions and Obsidian `![[image.png]]` embeds are supported. Failed loads show a framed placeholder with alt text, URL and reason. Click opens the picture viewer. PDF embeds local images as base64 data URIs and keeps remote image URLs (WeasyPrint downloads them). |
| Footnotes `[^label]` | ✅ | Numbered superscript reference in the preview (first-reference order); repeated references reuse the number. A dedicated footnote section shows the full content. In the PDF the reference is a clickable superscript and the definition renders as a `div.footnote-definition` at its source position (no relocation to the end). |
| Citations `[@key]` | ⚠️ | Citation keys (`[@doe2024]`, `[-@key]`) render as a styled marker; there is no bibliography/database resolution. |
| Obsidian tags `#tag` | ✅ | Rendered as an inline tag chip (not a heading); block ids (` ^id`) are hidden from the prose. |
| Abbreviations | ✅ | `*[HTML]: HyperText Markup Language` definitions are removed and occurrences render as `<abbr>` with a tooltip. |
| Superscript `^text^` | ✅ | Smaller accent-coloured text raised above the baseline, including intraword `x^2^` and HTML `<sup>`. In the PDF, GFM `^text^` is printed literally; HTML `<sup>` works. |
| Subscript `~text~` | ✅ | Smaller accent-coloured text lowered below the baseline, including intraword `H~2~O` and HTML `<sub>`. In the PDF, GFM `~text~` is printed literally; HTML `<sub>` works. |
| LaTeX math | ✅ | `$…$`, `$$…$$`, `\(…\)`, `\[…\]` and GitHub `` $`…`$ `` delimiters. Inline and display math are typeset in the preview with `latex-rust` (pure Rust, embedded STIX Two Math font). Matrices, `aligned`, `cases`, sums/products/limits are supported. Display equations are centered. Top-level `\\` row breaks in `$$…$$` (browser/MathJax style) are wrapped in a centred single-column `matrix`, so multi-line formulas render instead of falling back to source. The PDF typesets formulas to high-resolution images (inline and display, multi-line included). |
| Obsidian wiki-links | ✅ | `[[Note]]`, `[[Note|label]]`, `[[Note#Heading]]` render as links; `![[Note]]` transcludes the target document in the preview (cycle-safe), `![[file.pdf#page=2]]` shows an attachment card and `![[image.png]]` renders the image. `#^block-id` references open the target file. In the PDF they become relative `.html` links; image embeds are not resolved. |
| Inline HTML | ⚠️ | Safe subset interpreted: `<mark>`, `<kbd>`, `<b>/<strong>`, `<i>/<em>`, `<s>/<del>`, `<u>`, `<code>`, `<sup>`, `<sub>`, `<a href>`, `<br>`, `<img>`, `<abbr>` and `<svg>` (rasterized). Scripts are removed; media/iframe are block-level cards. Unknown tags keep their text content. |

### 1.3 Syntax highlighting

| Feature | Status | Description |
| --- | --- | --- |
| Language-aware code coloring | ✅ | Built-in heuristic tokenizer (no `syntect`): keywords, strings, numbers, line/block comments, CamelCase types. |
| Language label | ✅ | Shown above fenced code blocks. |
| Zero extra dependencies | ✅ | Implemented in `ui::highlight`. |

---

## 2. Editor

| Feature | Status | Description |
| --- | --- | --- |
| Multiline source editor | ✅ | Monospace code editor with vertical scrolling. |
| Live parsing | ✅ | The document is re-parsed on every change; preview stays in sync. |
| Cursor position | ✅ | Line/column shown in the status bar. |
| Statistics | ✅ | Line and word count updated on every change. |
| Unsaved indicator | ✅ | Dot in the toolbar and window title logic. |
| Hint text | ✅ | Placeholder when the buffer is empty. |
| Cut / copy / paste | ✅ | Cmd+X/C/V (plus Ctrl+X/C/V fallback). The right-click menu keeps the selection highlighted, so Cut/Copy stay enabled. |
| Context menu | ✅ | Right-click inside the editor offers Cut, Copy, Paste and Select All. |
| Select all | ✅ | Cmd+A or right-click → Select All; the editor keeps keyboard focus. |

---

## 3. Preview & Layout

| Feature | Status | Description |
| --- | --- | --- |
| Rendered Markdown view (default) | ✅ | Opens directly in the formatted document, no split screen. |
| Centered reading column | ✅ | Max ~1000 px line length in full preview (split panes use their available width). |
| Source editor / split mode | ✅ | Toolbar **Edit** / ⌘P shows raw Markdown left and the live rendered preview right (resizable). **Preview** returns to the full-window view. |
| Zoom | ✅ | Font size for preview and editor via toolbar **A−/A+** or ⌘+/⌘−. A percentage readout between the buttons shows the current magnification (100% = 16 px default); clicking it, or pressing ⌘0, resets to 100%. The readout, A− and A+ share the same control size. |
| Picture viewer | ✅ | Click any image in the rendered preview to open a modal viewer with a dimmed backdrop that blocks input to the document underneath. |
| Pan, zoom & close | ✅ | Scroll/trackpad zooms (5 %–800 %), drag pans, **+ / − / Reset** buttons control the view. **Esc**, a backdrop click or **Close (Esc)** exits. |
| Viewer information | ✅ | Title from the alt text (fallback: file name), live zoom percentage, and the source URL as a tooltip. |
| Scrollable preview | ✅ | Vertical scrolling; code blocks scroll horizontally. |
| Click preview → jump to source | ✅ | In split mode, clicking any rendered block moves the editor caret to the matching source line and centers that line in the editor pane. Top-level blocks (headings, paragraphs, lists, tables, code, quotes, math, Mermaid, details) are mapped by content, so the mapping survives preprocessing. In full preview the target line is remembered for the next switch to Edit. |
| Editor → preview scroll sync | ✅ | Switching from **Edit** back to **Preview** scrolls the rendered view to the block containing the editor caret, so you return to the section you were editing instead of the preview's old scroll position. Works for the toolbar buttons and ⌘P. |
| Preview → split sync | ✅ | The first visible preview block is tracked every frame. Switching from **Preview** to **Edit** scrolls both the editor and the split preview to that section, so reading at chapter 8 continues at chapter 8 on both sides. |
| Drag & drop | ✅ | Drop `.md`/`.markdown`/`.mdown`/`.txt` files onto the window; each opens in its own tab. |
| Large documents | ✅ | Above 250 top-level blocks the preview renders only the visible window plus overscan, using measured/estimated block heights; normal documents keep the exact all-block layout. |
| Mixed LTR/RTL text | ⚠️ | Pure RTL text and common mixed-direction lines render correctly (bidi run reordering); inline runs that themselves span hard line breaks and deeply nested mixed inline runs are not reordered. |

---

## 4. File Handling

| Feature | Status | Description |
| --- | --- | --- |
| Open | ✅ | Native macOS file dialog (⌘O), filters for Markdown/text; opens in a new tab. |
| Multiple tabs | ✅ | Every document has its own tab (file name or “Untitled”, unsaved dot); the strip only appears with 2+ tabs. Click to switch, re-open a file to jump to its tab, close with ×/middle-click/⌘W. Edits, scroll position and view state are preserved per tab; closing a dirty tab prompts to save. |
| Finder "Open With" / double-click | ✅ | macOS open-document Apple Events are handled via a runtime `NSApplicationDelegate` hook (see `src/macos_open.rs`). Works on cold start and for a running instance. |
| Save | ✅ | Writes to the current path (⌘S); new documents open "Save as…". |
| Save as | ✅ | Native save dialog (⌘⇧S) with a title-derived default name. |
| Drag & drop open | ✅ | See above. |
| Encoding | ✅ | Reads UTF-8 (BOM stripped), UTF-16LE/BE (BOM) and Windows-1252 fallback; rejects binary data with a clear error. Writes UTF-8. |
| Internal links | ✅ | Relative `.md` links, extension-less links, wiki-links and wiki embeds resolve against the document folder and open in a tab. `#fragment` targets open the file but do not scroll to the heading yet. |
| Window title | ✅ | Shows the file name and an unsaved marker. |
| Close confirmation | ✅ | Closing the window with unsaved changes cancels the close and shows a macOS-style dialog: **Save**, **Don't Save**, **Cancel**. Enter = Save, Esc = Cancel. A cancelled Save As or a write error keeps the dialog open. |

---

## 5. PDF Export

The PDF export is based on the reference implementation's print pipeline (extended with
our own print fixes): the **raw
Markdown source** is preprocessed and rendered to sanitized print HTML
(`src/html_export.rs`), then converted with the external **WeasyPrint** CLI
(`src/export.rs`). WeasyPrint is **mandatory** — there is no fallback writer.
If the executable is missing or exits non-zero, the status bar shows the
error (including WeasyPrint's stderr) and no PDF is written. On top of the
ported pipeline we fixed callout/fence ordering, allow long tables to break
across pages with repeating headers, and typeset math and Mermaid diagrams as
embedded images.

| Feature | Status | Description |
| --- | --- | --- |
| One-click export | ✅ | Toolbar button or ⌘E opens a native save dialog. A missing `.pdf` extension is added automatically. |
| Print pipeline | ✅ | Fenced/inline code is protected, YAML front matter stripped, math/emoji/wiki-link/callout preprocessors run, pulldown-cmark renders GFM (tables, task lists, footnotes), and ammonia sanitizes the HTML with the same tag/attribute policy as the reference implementation. |
| Mandatory WeasyPrint | ✅ | Single export path via the `weasyprint` CLI, looked up in `PATH` and `/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin` (works when launched from Finder). Missing/failing binary = error, never a degraded or broken PDF. |
| WeasyPrint onboarding | ✅ | WeasyPrint is not bundled. When the CLI is missing, a dialog at launch explains that only PDF export needs it, shows the copyable `brew install weasyprint` command, links to weasyprint.org and offers **Check again** (no restart required). The **Export PDF** toolbar button is greyed out and reopens this dialog instead of a save dialog; the editor and preview remain fully usable. |
| A4 pagination | ✅ | `@page` A4 with 2 cm side/top margins and a 2.5 cm bottom margin; page breaks are automatic. |
| Page footer | ✅ | Running footer `"<file name> - Page n of m"` using CSS page counters. |
| Text & typography | ✅ | Selectable text, 10 pt body, system font stack with Unicode fallback, GitHub-style `.markdown-body` spacing, `page-break-after: avoid` on headings and `page-break-inside: avoid` for code blocks, block quotes and callouts. Tables may break between rows and repeat their header on each page, so long tables do not leave empty pages. |
| Headings & paragraphs | ✅ | Typographic hierarchy and normal line wrapping. |
| Lists & task lists | ✅ | Bullets, ordered numbering and native checkbox inputs for GFM task lists. |
| Code blocks | ✅ | Monospace on a light background with the `language-*` class preserved and line breaks intact. |
| Block quotes & callouts | ✅ | Indented quotes and colored callout cards (`.callout.note/.tip/.warning/.caution/.important`) with a border and tinted background. |
| Tables | ⚠️ | GFM tables with borders, padded cells, shaded header row and zebra striping. Column alignment and HTML `colspan`/`rowspan` are dropped by the sanitizer. |
| Links | ✅ | Real clickable PDF link annotations (URLs, footnote references). |
| Images | ⚠️ | Existing local images (`![alt](path)`) are embedded as base64 data URIs; remote `http(s)` images keep their URL and are fetched by WeasyPrint. Missing files stay as broken references. |
| Footnotes | ⚠️ | Clickable superscript reference; the definition renders as a `div.footnote-definition` where it appears in the source. |
| Math | ✅ | Typeset with `latex-rust` to high-resolution images (inline and display, multi-line and row breaks). |
| Mermaid | ✅ | Rendered with `mermaid-svg` + `resvg` and embedded as a high-resolution image. |
| Emoji shortcodes | ✅ | `:rocket:`, `:heart:`, … are replaced with the actual Unicode emoji. |
| Unicode | ✅ | Full Unicode text and system-font fallback via WeasyPrint (CJK, emoji, accented characters). The preview uses macOS system fallback fonts for CJK/Arabic/Hebrew and run-level bidi reordering for mixed-direction lines. |
| Document title | ✅ | The `<title>` (and PDF metadata) uses the source file name, matching the footer. |
| YAML front matter | ✅ | Stripped from the PDF body (the preview still shows the Properties card). |

---

## 6. User Interface

| Feature | Status | Description |
| --- | --- | --- |
| Dark & Paper themes | ✅ | Slate/charcoal **dark** is the default; a toolbar toggle (sun icon while dark, moon while Paper) switches to a warm off-white **Paper** light theme (window `#f6f2e9`, panels `#f0eadf`, text `#2d2a24`) — deliberately not bright white. The choice applies instantly to the editor, preview, code highlighting, math and Mermaid diagrams and resets to dark on launch. |
| Typography | ✅ | One fixed Apple typeface, no font picker: **SF Pro** (San Francisco, the macOS system font) for UI and Markdown, with real weight 700 bold and true italic cuts selected through the variable font's `wght`/`opsz` axes; **SF Mono** for code. Falls back to Helvetica Neue if SF Pro is unavailable. |
| Tab strip | ✅ | macOS-style strip below the toolbar, hidden while only one document is open. With 2+ documents all tabs share the full row width equally; active tab blends into the content, dirty tabs show a dot, each tab has a close button. ⌘W closes the active tab and closing down to one hides the strip again. |
| IDE-like layout | ✅ | Compact toolbar, tab strip, editor, preview, status bar. |
| Native window | ✅ | 1280×860 default, 680×440 minimum. |
| Native app icon | ✅ | Apple-style squircle icon in the app's slate/blue palette: a Markdown page with a folded corner and the `M↓` mark. Ships as `assets/AppIcon.icns` and is embedded by `build_app.sh`. The same 1024 px master (`assets/AppIcon-1024.png`) is installed at runtime via `ViewportBuilder::with_icon`, so Dock and minimized-window tiles match the bundle icon (and `cargo run` looks correct too). |
| High-performance GPU | ✅ | wgpu `PowerPreference::HighPerformance`. |
| Status toasts | ✅ | Success/error messages auto-hide after ~4 seconds. |
| Keyboard shortcuts | ✅ | See table below. |
| About panel | ✅ | **MD-Viewer → About MD-Viewer** shows the standard panel, grown to fit a two-line credits block at the bottom: *Enhanced Markdown File Viewer for macOS* and a **clickable** *Project Page: https://github.com/JoergKau* (the URL carries an `NSLink` attribute and opens in the default browser). The panel explicitly supplies the name, runtime icon and version, and adds the MIT copyright when no `.app` bundle is present, so it looks correct both in the bundle and under `cargo run`. Implemented by swizzling `orderFrontStandardAboutPanel:`. |

### Keyboard shortcuts

| Shortcut | Action |
| --- | --- |
| ⌘O | Open file |
| ⌘S | Save |
| ⌘⇧S | Save as… |
| ⌘E | Export PDF |
| ⌘W | Close the active tab (prompts when dirty) |
| ⌘P | Toggle rendered view / editor |
| ⌘+ / ⌘− | Increase/decrease font size |
| ⌘0 | Reset magnification to 100% |
| ⌘X / ⌘C / ⌘V | Cut / copy / paste in the editor (Ctrl works too) |
| ⌘A | Select all in the editor |

---

## 7. Technical Characteristics

| Aspect | Description |
| --- | --- |
| Language | Rust 2024 edition |
| UI / rendering | `eframe` 0.37 + `egui` 0.37, wgpu backend (Metal on macOS) |
| Windowing | `winit` 0.30 |
| Markdown parser | `pulldown-cmark` 0.13 (CommonMark + GFM flags) |
| File dialogs | `rfd` 0.16 (native macOS dialogs) |
| Images | `egui_extras` 0.37 + `image` 0.25 (PNG/JPEG/GIF/WebP/BMP/TIFF/ICO) + `resvg` (SVG) + macOS `sips` (AVIF); file loader + background `curl` downloader |
| Math | `latex-rust` 3.0.1 (pure Rust; embedded STIX Two Math; PNG raster → egui texture and PDF images) |
| HTML parsing | `tl` 0.7 (fast, dependency-light HTML parser for safe raw-HTML block conversion) |
| Text decoding / bidi | `encoding_rs` 0.8 (UTF-16/legacy), `unicode-bidi` 0.3 (mixed LTR/RTL run ordering) |
| Front matter | `toml` 0.8 + `serde_json` 1 (TOML/JSON; YAML handled natively) |
| Typography | Fixed **SF Pro** (San Francisco system font, variable `wght`/`opsz` axes) for UI and Markdown, **SF Mono** for code; no font picker (Helvetica Neue fallback) |
| Diagrams | `mermaid-svg` 0.7 (pure-Rust Mermaid parser/layout) + `resvg` 0.45 (SVG → texture) |
| macOS integration | `objc2` runtime hook for Finder open events (macOS only) |
| PDF | Mandatory WeasyPrint CLI (detected in PATH + Homebrew locations); raw Markdown → sanitized print HTML (`html_export.rs`, the reference implementation port) → WeasyPrint (`export.rs`). No fallback writer. A startup onboarding dialog shows the install command when the CLI is missing. |
| External runtime dependencies | WeasyPrint (`brew install weasyprint`) is required for PDF export; the app shows an onboarding dialog with the command when it is missing. The editor and preview need no external runtime (remote images use the system `curl`). |
| Target platform | macOS 11+ on Apple Silicon (arm64) only; Intel is not supported |
| Distribution | `build_app.sh` creates the `.app` bundle, signs it (ad-hoc locally, Developer ID + hardened runtime when configured), optionally notarizes and staples it, and writes a zipped arm64 release archive with a SHA-256 checksum. |
| Test coverage | 107 unit/integration tests + 1 ignored deep test: parser (CommonMark/GFM, tight/loose lists, nested inline styles, HTML `<sub>`/`<sup>`, HTML block conversion, the extended compatibility corpus, tables with spans, math incl. row breaks, Mermaid, wiki-links, footnotes), print HTML (preprocessors, sanitizer, template, base64, escaping, callout/fence ordering, typeset math and Mermaid images), WeasyPrint runner (missing/failing binary, real end-to-end export when installed), dark/Paper palettes, tabs (labels, switching, edit preservation, closing), encoding detection (BOM/UTF-16/legacy), bidi run ordering, link resolution, preview virtualization, Pandoc/MkDocs/Obsidian extensions, front matter formats, media/SVG handling, compiled-in test fixtures, picture-viewer logic, magnification readout, LaTeX texture rendering |
| Warnings | Zero `cargo build` warnings, zero `cargo clippy` warnings |

---

## 8. Known Limitations

1. **PDF math and Mermaid are raster images.** Formulas are typeset with
   `latex-rust` and diagrams rendered with `mermaid-svg`/`resvg`, then embedded
   as high-resolution images. They look correct but are not selectable or
   searchable text in the PDF.
2. **HTML is converted, not a browser.** A safe subset of block and inline
   tags is mapped to native blocks (`<div>`/`<section>` containers, headings,
   paragraphs, lists, quotes, code, tables, details, inline formatting).
   Arbitrary CSS is ignored, `<video>`/`<audio>`/`<iframe>` are placeholder
   cards (no playback), and constructs outside the subset remain sanitized
   source. Scripts/styles/comments never execute; full HTML/CSS playback would
   require a webview.
3. **PDF export requires WeasyPrint.** There is deliberately no fallback
   writer, so a missing or failing `weasyprint` binary means export fails with
   an error message instead of producing a diminished PDF. Install it with
   `brew install weasyprint`; a startup dialog offers the same command and a
   **Check again** button. The editor and preview work without it.
4. **Extension coverage is pragmatic.** Grid tables support simple cells only;
   citations have no bibliography lookup; AVIF preview requires macOS `sips`;
   transclusion is preview-only. Full HTML/CSS/media playback remains out of
   scope (see item 2).
5. **GFM-only PDF syntax.** Things the preview adds on top of GFM —
   `==highlight==`, `^sup^`, `~sub~`, definition lists, Obsidian image embeds —
   are printed literally in the PDF.
6. **Remote images require network access.** On the WeasyPrint path remote
   `http(s)` images are downloaded by the CLI itself.
7. **No incremental parsing.** The whole document is re-parsed on each change;
   preview layout is virtualized above 250 top-level blocks, but parsing and a
   single pathologically large nested block (or huge footnote section) are
   still processed in full.
8. **Tabs live in one window.** Multiple documents are supported via tabs, but
   there is no detached window or side-by-side document view yet.
9. **RTL support is partial.** Glyphs and common mixed-direction lines are
   reordered, but inline runs that themselves span hard line breaks and deeply
   nested mixed inline runs keep logical order. Internal `#fragment`
   links open the target file but do not scroll to the heading.
10. **No syntax-highlighted editor.** Highlighting is applied to rendered code
   blocks, not inside the Markdown source editor.

---

## 9. Roadmap Ideas

- 🔜 Outline / table-of-contents sidebar
- 🔜 Search & replace
- 🔜 Localization (i18n) of the UI
- 🔜 Export to HTML
- 🔜 Auto-save and session restore
