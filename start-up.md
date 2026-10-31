# 🚀 MD-Viewer

---
> A native Markdown editor and viewer for **macOS on Apple Silicon**, written in Rust with **egui/eframe** (**wgpu** → Metal), **winit** and **pulldown-cmark**.

## Markdown

| Feature | Details |
| --- | --- |
| **CommonMark + GFM** | headings, emphasis, strikethrough, inline code, ordered/unordered/task lists, tables, block quotes, fenced and indented code, links, images, footnotes and horizontal rules. |
| **Extended dialects** | Obsidian callouts, wiki-links and `![[…]]` embeds, tags, block ids, comments and properties; Pandoc/Quarto fenced divs, inline notes, citations, heading attributes, grid tables and `[TOC]`; MkDocs admonitions and abbreviations; YAML, TOML and JSON front matter. |
| **Math** | inline and display LaTeX (`$…$`, `$$…$$`, `\(…\)`, `\[…\]`), including matrices, `aligned`, `cases`, sums/products and multi-line formulas. |
| **Diagrams** | Mermaid flowcharts, sequence, pie, class, state, Gantt and ER diagrams; inline and block SVG rasterized safely. |
| **Safe HTML** | `<details>`, HTML tables with `colspan`/`rowspan`, images, `<div>`/`<section>` containers and inline formatting. Scripts, styles and comments never execute; video/audio/iframe become labelled cards. |
| **Code blocks** | language-aware syntax highlighting and a language label. |

> [!NOTE] Editor & preview
> - Rendered preview by default; **Edit** (⌘⇧P) opens a resizable split view with
>   the source editor and live preview.
> - Click any preview block to jump to the matching source line; switching views
>   keeps the section you were reading or editing.
> - Multiple tabs with per-tab edits, scroll position and unsaved markers.
> - Zoom (⌘+/⌘−, ⌘0), picture viewer with pan/zoom, drag & drop.
> - Large documents are virtualized (visible window plus overscan) above 250
>   top-level blocks.
> - UTF-8/UTF-16/BOM/Windows-1252 input; CJK, Arabic and Hebrew via system fonts,
>   with run-level bidi ordering for mixed-direction text.
> - Relative `.md` links, wiki-links and wiki embeds open in tabs; `![[Note]]`
>   transclusion is cycle-safe.

> [!TIP] Files
> - Native open, save and save-as dialogs; drag & drop; Finder double-click and
>   **Open With** (cold start and running instance).
> - Close confirmation with **Save / Don't Save / Cancel**; closing the window
>   saves all dirty tabs.
> - `start-up.md` is loaded at launch as an untitled document; its relative
>   images resolve against its folder. Deleting it starts an empty
>   document.

> [!IMPORTANT] PDF export
> - One-click **Export PDF** (⌘E) with a native save dialog.
> - A4 pagination, running footer, selectable text, clickable links, embedded
>   images, styled code/tables/callouts, typeset math and Mermaid diagrams.
> - Uses the mandatory **WeasyPrint** CLI — no fallback writer. When it is
>   missing, the button is greyed out and an onboarding dialog offers the install
>   command and a re-check.
> - PDF output follows GFM; preview-only extensions are printed as source.

> [!WARNING] User interface
> - Permanent **dark** slate/charcoal theme plus a warm **Paper** light theme.
> - SF Pro for UI and Markdown, SF Mono for code; no font picker.
> - Native app icon, About panel with a clickable project link, status toasts.
> - Keyboard shortcuts: <kbd>⌘O</kbd>, <kbd>⌘S</kbd>, <kbd>⌘⇧S</kbd>, <kbd>⌘E</kbd>, <kbd>⌘W</kbd>, <kbd>⌘P</kbd>, <kbd>⌘+/−/0</kbd>, <kbd>⌘X/C/V</kbd>, <kbd>⌘A</kbd>.

> [!NOTE] Technical
> - Rust 2024 · eframe/egui 0.37 · wgpu (Metal) · winit 0.30 ·
>   pulldown-cmark 0.13 · arm64 macOS 11+.
> - 115 unit/integration tests plus an ignored deep render pass; zero build,
>   clippy and rustfmt warnings.
> - `.app` packaging with ad-hoc or Developer ID signing, optional notarization,
>   and a zipped arm64 release archive with SHA-256 checksum.
