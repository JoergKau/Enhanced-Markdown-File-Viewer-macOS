//! egui user interface: theme, tabs, app state, toolbar, rendered Markdown view,
//! source editor and status bar — dark by default, with a Paper light theme.
//!
//! # Layout
//!
//! The app opens in the **rendered Markdown view** (no split screen). Every
//! document has a tab in the strip below the toolbar. The toolbar can switch to
//! **Edit**, which shows a split screen: raw Markdown on the left and the live
//! rendered preview on the right. The split is laid out by hand inside the
//! central panel (see `App::edit_split`), not with an egui side panel.
//!
//! ```text
//! ┌──────────────────────────────────────────────────────┐
//! │ toolbar (top panel)                                  │
//! ├──────────────────────────────────────────────────────┤
//! │ tab strip (top panel, two or more tabs)              │
//! ├───────────────────────────┬──────────────────────────┤
//! │ editor (left, Edit mode)  │ live preview (right,     │
//! │                           │ Edit mode)               │
//! ├───────────────────────────┴──────────────────────────┤
//! │ status bar (bottom panel)                            │
//! └──────────────────────────────────────────────────────┘
//! ```
//!
//! # State flow
//!
//! For the active tab, `App::text` is the single source of truth. Whenever the
//! editor reports a change, [`App::reparse`] rebuilds `App::doc`, which the
//! preview consumes. The PDF export re-renders `App::text` through the ported
//! `html_export` pipeline. Inactive tabs live in [`TabState`] snapshots and are
//! swapped in by [`App::switch_tab`]. There is no incremental parsing — for
//! typical Markdown files this is fast enough and keeps the code simple.
//!
//! # Extension points
//!
//! * Toolbar buttons: [`App::toolbar`].
//! * Keyboard shortcuts: [`App::handle_shortcuts`].
//! * New block/inline rendering: [`render_block`] / [`render_inlines`].
//! * Theme colors: the `theme` module and [`apply_theme`].
//! * Syntax highlighting: [`highlight`].
//! * System fonts: [`install_system_fonts`].
//! * Picture viewer: [`ImageViewer`] and [`App::show_image_viewer`].
//! * LaTeX math: [`MathCache`] and [`render_math`].
//! * Mermaid diagrams: [`MermaidCache`], [`mermaid_texture`], [`render_mermaid`].
//! * Preview → source navigation: [`preview_ui`] records [`SourceHit`]s,
//!   [`source_line_at`] resolves the click, `App::jump_line` is consumed by
//!   [`editor_ui`] (caret + centered scroll).
//! * HTML subset from the parser: [`Block::Details`], [`Block::Group`],
//!   [`Block::Html`] and the `Inline` highlight/kbd/underline variants in
//!   [`render_inlines`].
//! * Tabs: [`TabState`], `App::tab_bar`, `App::switch_tab` and
//!   `App::close_tab`.

use crate::parser::{self, AlertKind, Block, Document, Inline, TableAlign, TableCell};
use eframe::egui::emath::GuiRounding as _;
use eframe::egui::{
    self, Align, Checkbox, Color32, CornerRadius, FontId, Frame, Label, Layout, Margin, Modifiers,
    Rect, RichText, ScrollArea, Stroke, TextFormat, TextStyle, Ui, Vec2,
};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::load::{
    Bytes, BytesLoadResult, BytesLoader, BytesPoll, ImageLoadResult, ImageLoader, ImagePoll,
    LoadError, SizeHint,
};
use egui_extras::install_image_loaders;

// ---------------------------------------------------------------------------
// Color palettes: the default slate/charcoal dark theme and a warm
// off-white "paper" theme. All rendering code reads tokens from `palette()`.
mod theme {
    use super::*;
    use std::sync::atomic::{AtomicU8, Ordering};

    /// Available color themes. The app starts in [`Mode::Dark`].
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub enum Mode {
        #[default]
        Dark,
        /// Warm off-white reading theme (the reference implementation's light "paper" look).
        Paper,
    }

    impl Mode {
        /// The theme a click on the toolbar toggle switches to.
        pub fn toggled(self) -> Self {
            match self {
                Mode::Dark => Mode::Paper,
                Mode::Paper => Mode::Dark,
            }
        }

        /// Toolbar icon: sun while dark (click for light), moon while light.
        pub fn icon(self) -> &'static str {
            match self {
                Mode::Dark => "\u{2600}",
                Mode::Paper => "\u{1F319}",
            }
        }

        /// Hover text for the toolbar toggle.
        pub fn hint(self) -> &'static str {
            match self {
                Mode::Dark => "Switch to light mode (Paper)",
                Mode::Paper => "Switch to dark mode",
            }
        }
    }

    /// Runtime color palette. Rendering code reads tokens through
    /// [`palette()`], so switching the theme needs no rebuild of the UI code.
    pub struct Palette {
        pub bg0: Color32,
        pub bg1: Color32,
        pub bg2: Color32,
        pub bg3: Color32,
        pub bg_active: Color32,
        pub stroke: Color32,
        pub text: Color32,
        pub muted: Color32,
        pub faint: Color32,
        pub accent: Color32,
        pub green: Color32,
        pub red: Color32,
        pub orange: Color32,
        pub purple: Color32,
        pub highlight_bg: Color32,
        pub highlight_text: Color32,
        // Syntax highlighting.
        pub sy_plain: Color32,
        pub sy_kw: Color32,
        pub sy_str: Color32,
        pub sy_num: Color32,
        pub sy_com: Color32,
        pub sy_type: Color32,
    }

    /// Slate/charcoal theme (default).
    static DARK: Palette = Palette {
        bg0: Color32::from_rgb(20, 21, 24), // window/preview base
        bg1: Color32::from_rgb(28, 29, 33), // panels
        bg2: Color32::from_rgb(36, 38, 43), // elevated (code, quotes)
        bg3: Color32::from_rgb(48, 51, 58), // hover/inline code
        bg_active: Color32::from_rgb(60, 64, 74),
        stroke: Color32::from_rgb(52, 55, 63),
        text: Color32::from_rgb(228, 230, 235),
        muted: Color32::from_rgb(154, 160, 171),
        faint: Color32::from_rgb(100, 106, 118),
        accent: Color32::from_rgb(96, 165, 250), // soft blue
        green: Color32::from_rgb(74, 222, 128),
        red: Color32::from_rgb(248, 113, 113),
        orange: Color32::from_rgb(251, 191, 36),
        purple: Color32::from_rgb(192, 132, 252),
        highlight_bg: Color32::from_rgb(250, 204, 21),
        highlight_text: Color32::from_rgb(24, 24, 27),
        sy_plain: Color32::from_rgb(201, 209, 221),
        sy_kw: Color32::from_rgb(192, 132, 252),
        sy_str: Color32::from_rgb(165, 215, 143),
        sy_num: Color32::from_rgb(251, 191, 36),
        sy_com: Color32::from_rgb(107, 116, 132),
        sy_type: Color32::from_rgb(100, 210, 255),
    };

    /// Warm "paper" theme: off-white with low glare, not bright white.
    static PAPER: Palette = Palette {
        bg0: Color32::from_rgb(246, 242, 233), // warm paper base
        bg1: Color32::from_rgb(240, 234, 223), // panels
        bg2: Color32::from_rgb(251, 248, 241), // elevated (code, quotes)
        bg3: Color32::from_rgb(233, 226, 212), // hover/inline code
        bg_active: Color32::from_rgb(223, 214, 197),
        stroke: Color32::from_rgb(210, 200, 181),
        text: Color32::from_rgb(45, 42, 36),
        muted: Color32::from_rgb(106, 99, 88),
        faint: Color32::from_rgb(145, 136, 121),
        accent: Color32::from_rgb(25, 101, 179), // deeper blue for contrast
        green: Color32::from_rgb(33, 122, 62),
        red: Color32::from_rgb(176, 48, 48),
        orange: Color32::from_rgb(158, 102, 8),
        purple: Color32::from_rgb(112, 76, 174),
        highlight_bg: Color32::from_rgb(255, 224, 130),
        highlight_text: Color32::from_rgb(45, 42, 36),
        sy_plain: Color32::from_rgb(52, 49, 43),
        sy_kw: Color32::from_rgb(112, 76, 174),
        sy_str: Color32::from_rgb(34, 119, 66),
        sy_num: Color32::from_rgb(158, 102, 8),
        sy_com: Color32::from_rgb(139, 131, 117),
        sy_type: Color32::from_rgb(20, 102, 150),
    };

    /// Current mode as a single atomic; the palette lookup is a cheap match.
    static MODE: AtomicU8 = AtomicU8::new(0);

    pub fn mode() -> Mode {
        match MODE.load(Ordering::Relaxed) {
            0 => Mode::Dark,
            _ => Mode::Paper,
        }
    }

    #[cfg(test)]
    pub(super) fn set_mode(mode: Mode) {
        MODE.store(mode as u8, Ordering::Relaxed);
    }

    /// Switches to the other theme and returns the new mode.
    pub fn toggle() -> Mode {
        let next = mode().toggled();
        MODE.store(next as u8, Ordering::Relaxed);
        next
    }

    pub fn palette() -> &'static Palette {
        match mode() {
            Mode::Dark => &DARK,
            Mode::Paper => &PAPER,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn toggle_switches_between_dark_and_paper() {
            assert_eq!(Mode::Dark.toggled(), Mode::Paper);
            assert_eq!(Mode::Paper.toggled(), Mode::Dark);
        }

        #[test]
        fn toolbar_icons_follow_the_mode() {
            // Default (dark) shows a sun; after switching, a moon.
            assert_eq!(Mode::Dark.icon(), "\u{2600}");
            assert_eq!(Mode::Paper.icon(), "\u{1F319}");
            assert!(Mode::Dark.hint().contains("light"));
            assert!(Mode::Paper.hint().contains("dark"));
        }

        #[test]
        fn paper_is_light_but_not_white() {
            fn luminance(c: Color32) -> u32 {
                299 * c.r() as u32 + 587 * c.g() as u32 + 114 * c.b() as u32
            }
            assert!(luminance(PAPER.bg0) > luminance(PAPER.text));
            assert!(luminance(DARK.bg0) < luminance(DARK.text));
            // Off-white paper, not #fff.
            assert!(luminance(PAPER.bg0) < luminance(Color32::WHITE));
        }
    }
}

use theme::*;

/// Font size that represents 100% magnification in the toolbar.
/// `A−`/`A+` change it in 1 px steps; the readout is relative to this value.
const DEFAULT_FONT_SIZE: f32 = 16.0;
/// Smallest/largest content font size (`A−`/`A+`, ⌘−/⌘+).
const MIN_FONT_SIZE: f32 = 9.0;
const MAX_FONT_SIZE: f32 = 36.0;
/// Width/height shared by the `A−`, magnification readout and `A+` controls.
const ZOOM_CONTROL_SIZE: Vec2 = Vec2::new(52.0, 24.0);
/// How long a status message stays visible in the status bar.
const STATUS_DURATION: Duration = Duration::from_secs(4);
/// File extensions treated as Markdown by dialogs, drops and wiki links.
const MARKDOWN_EXTENSIONS: &[&str] = &["md", "markdown", "mdown", "txt"];
/// Zoom limits of the picture viewer (the same range as [`egui::Scene`]).
const IMAGE_ZOOM_MIN: f32 = 0.05;
const IMAGE_ZOOM_MAX: f32 = 8.0;

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

/// File name of the startup document.
const STARTUP_FILE: &str = "start-up.md";

/// Loads the startup document and the folder it came from.
///
/// Candidates are checked in order:
/// 1. `Contents/Resources/start-up.md` inside a `.app` bundle,
/// 2. `start-up.md` in the current working directory (for `cargo run`).
///
/// The folder is used to resolve relative images and links in the startup
/// document (for example its logo). A missing or unreadable file is **not** an
/// error: the app opens an empty document instead, so deleting `start-up.md`
/// is always safe.
fn startup_document() -> (String, Option<PathBuf>) {
    startup_document_from(&startup_candidates())
}

/// Reads the first readable candidate and returns its folder.
fn startup_document_from(candidates: &[PathBuf]) -> (String, Option<PathBuf>) {
    for path in candidates {
        if let Ok(text) = std::fs::read_to_string(path) {
            return (text, path.parent().map(Path::to_path_buf));
        }
    }
    (String::new(), None)
}

/// Candidate locations for [`STARTUP_FILE`], most specific first.
fn startup_candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(contents) = exe.parent().and_then(Path::parent)
    {
        // .app/Contents/MacOS/MD-Viewer → .app/Contents/Resources/start-up.md
        paths.push(contents.join("Resources").join(STARTUP_FILE));
    }
    if let Ok(cwd) = std::env::current_dir() {
        paths.push(cwd.join(STARTUP_FILE));
    }
    paths
}

/// How the document is displayed.
///
/// The app opens in the rendered Markdown view by default; Edit mode shows the
/// source editor next to the live preview.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewMode {
    /// Rendered Markdown (default).
    Preview,
    /// Raw Markdown source editor next to the live preview.
    Editor,
}

/// A PDF export running on a worker thread, see [`App::start_export`].
struct ExportJob {
    result: std::sync::mpsc::Receiver<std::io::Result<PathBuf>>,
    /// The PDF is opened in the print dialog instead of being reported as saved.
    print: bool,
}

/// A pending caret move for the source editor, see [`App::jump_line`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EditorJump {
    line: usize,
    /// `false` moves the caret and scroll without taking keyboard focus, so a
    /// find that is driven from the find bar keeps typing there.
    focus: bool,
}

impl ViewMode {
    /// The other view mode (⌘⇧P).
    fn toggled(self) -> Self {
        match self {
            ViewMode::Preview => ViewMode::Editor,
            ViewMode::Editor => ViewMode::Preview,
        }
    }
}

/// Button chosen in the unsaved-changes sheet.
#[derive(Clone, Copy, PartialEq, Eq)]
enum UnsavedChoice {
    Save,
    Discard,
    Cancel,
}

/// An image that was clicked in the preview and should open in the viewer.
///
/// Rendering is stateless (`&Document`), so a click is collected in this small
/// value and turned into an [`ImageViewer`] after the preview has rendered.
struct ImageRequest {
    /// Alt text from `![alt](url)` (may be empty).
    alt: String,
    /// Original Markdown URL (for tooltips and the title fallback).
    url: String,
    /// Resolved URI handed to egui's image loader (absolute file URI or HTTP).
    uri: String,
}

/// State of the modal picture viewer.
///
/// The visible part of the image is described by [`ImageViewer::scene_rect`],
/// which [`egui::Scene`] updates while the user pans (drag) or zooms (scroll).
/// `fitted` makes sure the first fit happens once the texture (and therefore
/// its real size) is available, not while the loader still reports a
/// placeholder size.
struct ImageViewer {
    request: ImageRequest,
    /// Visible rectangle in image (scene) coordinates; `Rect::ZERO` = refit.
    scene_rect: Rect,
    /// Whether the initial fit-to-window has been applied.
    fitted: bool,
}

impl ImageViewer {
    fn new(request: ImageRequest) -> Self {
        Self {
            request,
            scene_rect: Rect::ZERO,
            fitted: false,
        }
    }

    /// Human-readable title: alt text, or the file name from the URL.
    fn title(&self) -> String {
        if !self.request.alt.trim().is_empty() {
            return self.request.alt.clone();
        }
        self.request
            .url
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or("Image")
            .to_string()
    }

    /// Zooms by `factor` (> 1 = in) around the center of the visible area.
    ///
    /// `viewport` is the on-screen size of the scene. The resulting scale is
    /// clamped to the same range as [`egui::Scene`], so the buttons and the
    /// scroll wheel behave identically.
    fn zoom(&mut self, factor: f32, viewport: Vec2) {
        if !self.scene_rect.is_positive() || viewport.x <= 0.0 {
            return;
        }
        let current = viewport.x / self.scene_rect.width();
        let target = (current * factor).clamp(IMAGE_ZOOM_MIN, IMAGE_ZOOM_MAX);
        let size = self.scene_rect.size() * (current / target);
        self.scene_rect = Rect::from_center_size(self.scene_rect.center(), size);
    }
}

// ---------------------------------------------------------------------------
// LaTeX math rendering
// ---------------------------------------------------------------------------

/// Cache key for a rendered formula.
///
/// Rendering LaTeX to a texture is relatively expensive, so results are cached
/// for the lifetime of the window. `size_cent` uses hundredths of a point to
/// keep the key hashable without relying on `f32` hashing.
#[derive(Clone, PartialEq, Eq, Hash)]
struct MathKey {
    tex: String,
    size_cent: u32,
    display: bool,
    /// Palette the raster was rendered with (0 = dark, 1 = Paper), so a
    /// theme switch does not reuse light-on-light or dark-on-dark textures.
    theme: u8,
}

/// A rendered formula ready to be drawn with `egui::Image`.
struct MathTexture {
    handle: egui::TextureHandle,
    /// Logical size in points (texture pixels divided by pixels-per-point).
    size: Vec2,
}

/// Lazy LaTeX renderer and texture cache.
///
/// Uses `latex-rust` (pure Rust, embedded STIX Two Math font) with its PNG
/// backend. The raster is converted to an egui texture once per formula/size;
/// the cache is cleared once it holds [`MATH_CACHE_LIMIT`] entries.
/// If a formula contains an unsupported construct, the entry stores `None` and
/// the preview falls back to the LaTeX source.
#[derive(Default)]
struct MathCache {
    entries: HashMap<MathKey, Option<MathTexture>>,
    /// `None` until the first formula is rendered; `Some(None)` if the font
    /// failed to load, so the failure is not retried for every formula.
    font: Option<Option<latex_rust::MathFont>>,
}

/// Upper bound for cached formula textures. Inline and display formulas use
/// different sizes at the same time, so entries are dropped in bulk instead of
/// on every size change.
const MATH_CACHE_LIMIT: usize = 512;

impl MathCache {
    /// Returns a texture for `tex`, rendering it on first use.
    fn texture(
        &mut self,
        ctx: &egui::Context,
        tex: &str,
        size: f32,
        display: bool,
    ) -> Option<&MathTexture> {
        let key = MathKey {
            tex: tex.to_string(),
            size_cent: (size * 100.0).round() as u32,
            display,
            theme: theme::mode() as u8,
        };
        if !self.entries.contains_key(&key) {
            if self.entries.len() >= MATH_CACHE_LIMIT {
                self.entries.clear();
            }
            let rendered = self.render(ctx, &key);
            self.entries.insert(key.clone(), rendered);
        }
        self.entries.get(&key).and_then(|entry| entry.as_ref())
    }

    /// Parses/lays out/rasterizes one formula and uploads it as a texture.
    fn render(&mut self, ctx: &egui::Context, key: &MathKey) -> Option<MathTexture> {
        let font = self
            .font
            .get_or_insert_with(|| latex_rust::MathFont::stix_two_math().ok())
            .as_ref()?;

        let ppp = ctx.pixels_per_point();
        let mut options = latex_rust::PngOptions::new();
        options.font_size_pt =
            latex_rust::Dim::from_i64((key.size_cent as f32 / 100.0).round() as i64);
        options.dpi = latex_rust::Dim::from_i64((72.0 * ppp).round().clamp(1.0, 2400.0) as i64);
        let c = palette().text;
        options.color = latex_rust::Color::rgb(c.r(), c.g(), c.b());
        options.display = key.display;

        let png = latex_rust::latex_to_png(&key.tex, font, &options).ok()?;
        let image = image::load_from_memory_with_format(&png, image::ImageFormat::Png).ok()?;
        let rgba = image.to_rgba8();
        let width = rgba.width() as usize;
        let height = rgba.height() as usize;
        let color_image = egui::ColorImage::from_rgba_unmultiplied([width, height], rgba.as_raw());
        let handle = ctx.load_texture(
            format!("latex:{}:{}:{}", key.size_cent, key.display, key.tex),
            color_image,
            egui::TextureOptions::LINEAR,
        );
        Some(MathTexture {
            handle,
            size: Vec2::new(width as f32, height as f32) / ppp,
        })
    }
}

/// Draws a typeset formula, falling back to styled LaTeX source on failure.
fn render_math(ui: &mut Ui, tex: &str, size: f32, display: bool, cache: &mut MathCache) {
    // Browser-style `\\` row breaks are only valid inside an environment for
    // `latex-rust`; normalise display math before rendering (see
    // [`crate::parser::normalize_display_math`]).
    let source = tex;
    let normalized;
    let tex = if display {
        normalized = crate::parser::normalize_display_math(tex);
        normalized.as_str()
    } else {
        tex
    };
    if let Some(math) = cache.texture(ui.ctx(), tex, size, display) {
        let texture = egui::load::SizedTexture::new(math.handle.id(), math.size);
        ui.add(egui::Image::new(texture).max_width(ui.available_width()));
    } else {
        // Show the author's original source, not the internal wrapper.
        ui.label(
            RichText::new(source)
                .size(size * 0.95)
                .italics()
                .color(palette().purple),
        );
    }
}

/// Snapshot of one open document (tab).
///
/// The active tab lives in [`App`]'s direct fields (editor code touches them on
/// every frame); inactive tabs are stored here and swapped in by
/// [`App::switch_tab`]. This keeps the hot rendering path unchanged while
/// still supporting multiple documents.
#[derive(Clone, Default)]
struct TabState {
    text: String,
    doc: parser::Document,
    path: Option<PathBuf>,
    encoding: crate::encoding::TextEncoding,
    dirty: bool,
    line_count: usize,
    word_count: usize,
    last_cursor: (usize, usize),
    preview_line: Option<usize>,
    preview_top_line: usize,
}

impl TabState {
    /// A tab for `text` that is not dirty and starts at the top of the preview.
    fn from_text(text: String, path: Option<PathBuf>) -> Self {
        let (line_count, word_count) = text_stats(&text);
        Self {
            doc: parser::parse(&text),
            text,
            path,
            encoding: crate::encoding::TextEncoding::Utf8,
            dirty: false,
            line_count,
            word_count,
            last_cursor: (1, 1),
            preview_line: None,
            preview_top_line: 1,
        }
    }

    fn blank() -> Self {
        Self::from_text(String::new(), None)
    }
}

/// Line and word counts shown in the status bar.
fn text_stats(text: &str) -> (usize, usize) {
    (text.lines().count(), text.split_whitespace().count())
}

/// Label shown on a tab: the file name, or "Untitled".
fn tab_label(path: Option<&Path>) -> String {
    path.and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("Untitled")
        .to_string()
}

fn media_icon(kind: &str) -> &'static str {
    match kind {
        "Video" => "🎬",
        "Audio" => "🔊",
        _ => "🌐",
    }
}

/// Display label for a Pandoc fenced-div info string (`{.warning}` → warning).
fn fenced_div_label(class: &str) -> String {
    class
        .trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .split_whitespace()
        .next()
        .unwrap_or("div")
        .trim_start_matches('.')
        .trim_start_matches('#')
        .to_string()
}

/// Accent color of a fenced div, chosen by the whole words in its label
/// (`multiple` is not `tip`).
fn fenced_div_color(label: &str) -> Color32 {
    let lower = label.to_ascii_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_ascii_alphanumeric()).collect();
    let has = |names: &[&str]| words.iter().any(|word| names.contains(word));
    if has(&["warning", "caution", "danger"]) {
        palette().red
    } else if has(&["tip", "success"]) {
        palette().green
    } else if has(&["important"]) {
        palette().purple
    } else {
        palette().accent
    }
}

/// Cached embedded documents (`![[Note]]` transclusion).
#[derive(Clone, Default)]
struct TransclusionCache {
    entries: HashMap<PathBuf, (Option<std::time::SystemTime>, Arc<parser::Document>)>,
}

/// Context key for the current transclusion nesting depth.
fn transclusion_depth_id() -> egui::Id {
    egui::Id::unique("mdviewer_transclusion_depth")
}

/// If a paragraph consists only of a wiki embed (`![[Note]]`), resolve it so the
/// transclusion can be rendered as a full-width block instead of inline.
fn standalone_transclusion(inlines: &[Inline], base_dir: Option<&Path>) -> Option<PathBuf> {
    let mut target = None;
    for inline in inlines {
        match inline {
            Inline::Text(text) if text.trim().is_empty() => {}
            Inline::WikiEmbed { target: name, .. } if target.is_none() => {
                target = Some(name.as_str());
            }
            _ => return None,
        }
    }
    resolve_link_target(target?, base_dir, true)
}

/// Renders an Obsidian transclusion (`![[Note]]`) as a full-width block, with
/// cycle protection and a small parsed-document cache.
fn render_transclusion(
    ui: &mut Ui,
    path: &Path,
    f: f32,
    image_click: &mut Option<ImageRequest>,
    math_cache: &mut MathCache,
) {
    let depth = ui
        .data(|data| data.get_temp::<usize>(transclusion_depth_id()))
        .unwrap_or(0);
    if depth >= 2 {
        return;
    }
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let cache_id = egui::Id::unique("mdviewer_transclusion_cache");
    let cached = ui.data_mut(|data| {
        let cache = data.get_temp_mut_or_default::<TransclusionCache>(cache_id);
        cache
            .entries
            .get(path)
            .filter(|(cached_modified, _)| *cached_modified == modified)
            .map(|(_, doc)| Arc::clone(doc))
    });
    let doc = match cached {
        Some(doc) => doc,
        None => {
            let Ok((text, _)) = crate::encoding::read_text(path) else {
                return;
            };
            let doc = Arc::new(parser::parse(&text));
            ui.data_mut(|data| {
                data.get_temp_mut_or_default::<TransclusionCache>(cache_id)
                    .entries
                    .insert(path.to_path_buf(), (modified, Arc::clone(&doc)));
            });
            doc
        }
    };
    if doc.blocks.is_empty() {
        return;
    }

    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("embedded");
    ui.data_mut(|data| data.insert_temp(transclusion_depth_id(), depth + 1));
    Frame::new()
        .fill(palette().bg1)
        .stroke(Stroke::new(1.0, palette().stroke))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin {
            left: 14,
            right: 14,
            top: 8,
            bottom: 10,
        })
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(
                RichText::new(format!("⧉ embedded: {name}"))
                    .size(f * 0.75)
                    .color(palette().faint),
            );
            ui.add_space(4.0);
            render_blocks(
                ui,
                &doc.blocks,
                f * 0.95,
                path.parent(),
                image_click,
                math_cache,
            );
        });
    ui.data_mut(|data| data.insert_temp(transclusion_depth_id(), depth));
}

/// Truncates a tab label to `max_chars` characters, adding an ellipsis.
fn truncate_label(label: &str, max_chars: usize) -> String {
    let mut chars = label.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// Key for cached preview block heights. Heights depend on the document,
/// available width, font size and theme.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct PreviewKey {
    revision: u64,
    width: u32,
    font: u32,
    theme: u8,
}

/// Measured/estimated heights of top-level preview blocks, used to render only
/// the visible window of very large documents.
#[derive(Default)]
struct PreviewMetrics {
    key: PreviewKey,
    heights: Vec<f32>,
}

/// Above this many top-level blocks the preview renders only the visible
/// window (plus overscan) instead of laying out the whole document each frame.
const VIRTUALIZE_THRESHOLD: usize = 250;
/// Extra content rendered above/below the viewport for smooth scrolling.
const PREVIEW_OVERSCAN: f32 = 800.0;

/// Application state.
///
/// The struct owns the active document's text/model, all open tabs and the
/// view preferences. It is created once per window in [`App::new`].
pub struct App {
    /// Editor buffer (single source of truth for the active tab).
    text: String,
    /// Parsed model for the preview (the PDF export renders `text` directly).
    doc: parser::Document,
    /// Currently open file.
    path: Option<PathBuf>,
    /// Encoding the open file was read with; saves write it back.
    encoding: crate::encoding::TextEncoding,
    /// Folder of the startup document, used to resolve its relative images
    /// and links. `None` when no startup document was found.
    startup_base: Option<PathBuf>,
    /// Unsaved changes.
    dirty: bool,
    /// All open tabs. The active slot is a snapshot from the last switch; its
    /// live state is in the fields above.
    tabs: Vec<TabState>,
    /// Index of the active tab in [`App::tabs`].
    active_tab: usize,
    /// Tab whose close button was pressed and that has unsaved changes.
    pending_tab_close: Option<usize>,
    /// Current view mode (rendered preview by default).
    mode: ViewMode,
    /// Whether the outline sidebar is visible.
    outline_open: bool,
    /// Headings for the outline sidebar, rebuilt when the preview revision changes.
    outline: Arc<Vec<(u8, usize, String)>>,
    outline_revision: u64,
    /// Find/replace bar state.
    find_open: bool,
    find_query: String,
    replace_text: String,
    find_matches: Vec<(usize, usize)>,
    find_index: usize,
    find_cache: (String, u64),
    find_focus: bool,
    /// Split position in Edit mode (`0.5` = 50/50). Persists while dragging.
    split_fraction: f32,
    /// Font size (editor & preview; ⌘+/⌘− zoom).
    font_size: f32,
    /// Status toast: (text, is_error, timestamp).
    status: Option<(String, bool, Instant)>,
    /// PDF export or print running on a worker thread.
    export_job: Option<ExportJob>,
    /// Cached statistics.
    line_count: usize,
    word_count: usize,
    /// Last cursor position (line, column).
    last_cursor: (usize, usize),
    /// Character index and preview revision `last_cursor` was computed for.
    cursor_key: Option<(usize, u64)>,
    /// Cached window title to avoid sending a viewport command every frame.
    last_title: String,
    /// Open modal picture viewer, if any.
    image_viewer: Option<ImageViewer>,
    /// Cached typeset formulas (LaTeX → texture).
    math_cache: MathCache,
    /// Monotonic revision used to invalidate preview layout caches.
    preview_revision: u64,
    /// Cached heights for the virtualized preview.
    preview_metrics: PreviewMetrics,
    /// Pending caret move for the source editor (Edit mode only), set by
    /// preview clicks, outline clicks and find reveals. Consumed by
    /// [`editor_ui`] on the next frame.
    jump_line: Option<EditorJump>,
    /// Pending preview scroll target (source line) when switching from the
    /// editor back to the rendered preview.
    preview_line: Option<usize>,
    /// Source line of the first visible preview block (updated every frame).
    /// Used when switching from Preview to Edit so both panes show the same
    /// section the user was reading.
    preview_top_line: usize,
    /// Open "save changes before closing?" confirmation dialog.
    close_dialog: bool,
    /// WeasyPrint onboarding dialog. Shown once per launch when the CLI is
    /// missing, and again when PDF export is attempted without it.
    weasyprint_dialog: bool,
    /// Whether the WeasyPrint CLI was found. Controls the Export PDF button
    /// (greyed out while PDF export is unavailable).
    weasyprint_ready: bool,
    /// Whether the macOS open-document delegate hooks are installed.
    open_glue_installed: bool,
}

impl App {
    /// Creates the app, applies the theme and installs image loaders.
    ///
    /// `initial` optionally opens a file passed on the command line.
    pub fn new(cc: &eframe::CreationContext, initial: Option<PathBuf>) -> Self {
        apply_theme(&cc.egui_ctx);
        install_image_loaders(&cc.egui_ctx);
        // Remote images are downloaded with the system `curl` (present on
        // every macOS install). This is deliberately not the `egui_extras`
        // HTTP loader: `ureq`'s non-blocking connect can fail with EBADF in
        // release builds of the GUI app on macOS. curl uses the system network
        // stack (including VPN/proxy settings) and is rock solid.
        cc.egui_ctx
            .add_bytes_loader(Arc::new(CurlBytesLoader::default()));
        // egui_extras' SvgLoader only recognizes URIs that end in `.svg`.
        // Shields.io badges (and many other services) return `image/svg+xml`
        // without an extension, so register a content-sniffing loader.
        cc.egui_ctx
            .add_image_loader(Arc::new(SvgSniffLoader::default()));
        install_system_fonts(&cc.egui_ctx);
        crate::macos_open::set_context(cc.egui_ctx.clone());

        let (text, startup_base) = startup_document();
        let doc = parser::parse(&text);
        let (line_count, word_count) = text_stats(&text);
        let weasyprint_ready = crate::export::weasyprint_path().is_some();
        let mut app = Self {
            text,
            doc,
            path: None,
            encoding: crate::encoding::TextEncoding::Utf8,
            startup_base,
            dirty: false,
            tabs: Vec::new(),
            active_tab: 0,
            pending_tab_close: None,
            mode: ViewMode::Preview,
            outline_open: false,
            outline: Arc::new(Vec::new()),
            outline_revision: 0,
            find_open: false,
            find_query: String::new(),
            replace_text: String::new(),
            find_matches: Vec::new(),
            find_index: 0,
            find_cache: (String::new(), 0),
            find_focus: false,
            split_fraction: 0.5,
            font_size: DEFAULT_FONT_SIZE,
            status: None,
            export_job: None,
            line_count,
            word_count,
            last_cursor: (1, 1),
            cursor_key: None,
            last_title: String::new(),
            image_viewer: None,
            math_cache: MathCache::default(),
            preview_revision: 1,
            preview_metrics: PreviewMetrics::default(),
            jump_line: None,
            preview_line: None,
            preview_top_line: 1,
            close_dialog: false,
            weasyprint_dialog: !weasyprint_ready,
            weasyprint_ready,
            open_glue_installed: false,
        };
        app.store_active();
        if let Some(path) = initial {
            app.open_path_in_current_tab(path);
        }
        app
    }

    /// Folder used to resolve relative images and links for the active
    /// document. While the startup document is untitled, its own folder is
    /// used so the logo and other relative assets still resolve.
    fn base_dir(&self) -> Option<PathBuf> {
        self.path
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .or_else(|| self.startup_base.clone())
    }

    // ---------------- Tabs ----------------

    /// Captures the active document into a [`TabState`].
    fn snapshot(&self) -> TabState {
        TabState {
            text: self.text.clone(),
            doc: self.doc.clone(),
            path: self.path.clone(),
            encoding: self.encoding,
            dirty: self.dirty,
            line_count: self.line_count,
            word_count: self.word_count,
            last_cursor: self.last_cursor,
            preview_line: self.preview_line,
            preview_top_line: self.preview_top_line,
        }
    }

    /// Writes the live fields back into the active tab slot.
    fn store_active(&mut self) {
        let snapshot = self.snapshot();
        if self.tabs.is_empty() {
            self.tabs.push(snapshot);
            self.active_tab = 0;
        } else if let Some(slot) = self.tabs.get_mut(self.active_tab) {
            *slot = snapshot;
        }
    }

    /// Replaces the live fields with `tab` (no snapshot of the old state).
    fn load_tab(&mut self, tab: TabState) {
        self.text = tab.text;
        self.doc = tab.doc;
        self.path = tab.path;
        self.encoding = tab.encoding;
        self.dirty = tab.dirty;
        self.line_count = tab.line_count;
        self.word_count = tab.word_count;
        self.last_cursor = tab.last_cursor;
        self.cursor_key = None;
        // A caret move is only meant for the tab that requested it.
        self.jump_line = None;
        // Restore the reading position: remember where the tab was left and
        // scroll the preview there on the next frame.
        self.preview_line = tab
            .preview_line
            .or_else(|| Some(tab.preview_top_line.max(1)));
        self.preview_top_line = tab.preview_top_line;
        self.preview_revision = self.preview_revision.wrapping_add(1);
        self.image_viewer = None;
    }

    /// Activates tab `index`, saving the current document first.
    fn switch_tab(&mut self, index: usize) {
        if index >= self.tabs.len() || index == self.active_tab {
            return;
        }
        self.store_active();
        let tab = std::mem::take(&mut self.tabs[index]);
        self.load_tab(tab);
        self.active_tab = index;
    }

    /// Path of tab `index` (live data for the active tab).
    fn tab_path(&self, index: usize) -> Option<&Path> {
        if index == self.active_tab {
            self.path.as_deref()
        } else {
            self.tabs.get(index).and_then(|t| t.path.as_deref())
        }
    }

    /// Dirty state of tab `index` (live data for the active tab).
    fn tab_dirty(&self, index: usize) -> bool {
        if index == self.active_tab {
            self.dirty
        } else {
            self.tabs.get(index).is_some_and(|t| t.dirty)
        }
    }

    /// Number of documents with unsaved changes.
    fn dirty_count(&self) -> usize {
        (0..self.tabs.len()).filter(|i| self.tab_dirty(*i)).count()
    }

    /// Index of the tab already showing `path`, if any.
    fn find_tab(&self, path: &Path) -> Option<usize> {
        let canonical = std::fs::canonicalize(path).ok();
        (0..self.tabs.len()).find(|i| {
            let Some(open) = self.tab_path(*i) else {
                return false;
            };
            if open == path {
                return true;
            }
            match (&canonical, std::fs::canonicalize(open).ok().as_ref()) {
                (Some(a), Some(b)) => a == b,
                _ => false,
            }
        })
    }

    /// Closes tab `index`; the last remaining tab is replaced by a blank one.
    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        if index == self.active_tab {
            self.tabs.remove(index);
            // The active slot is overwritten by the next `store_active`.
            if self.tabs.is_empty() {
                self.tabs.push(TabState::default());
                self.active_tab = 0;
                self.load_tab(TabState::blank());
                return;
            }
            let next = index.min(self.tabs.len() - 1);
            self.active_tab = next;
            let tab = std::mem::take(&mut self.tabs[next]);
            self.load_tab(tab);
        } else {
            self.tabs.remove(index);
            if index < self.active_tab {
                self.active_tab -= 1;
            }
        }
    }

    /// Closes a tab immediately, or asks first when it has unsaved changes.
    fn request_close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        if self.tab_dirty(index) {
            self.pending_tab_close = Some(index);
        } else {
            self.close_tab(index);
        }
    }

    /// Saves every dirty tab, switching to each one in turn. Returns `false` if
    /// a Save As dialog is cancelled, the target is open in another tab, or a
    /// write fails.
    fn save_all_tabs(&mut self) -> bool {
        for index in 0..self.tabs.len() {
            self.switch_tab(index);
            if self.dirty && !self.do_save() {
                return false;
            }
        }
        true
    }

    /// Loads `path` into the current (single, usually pristine) tab.
    fn open_path_in_current_tab(&mut self, path: PathBuf) {
        log::info!("opening {}", path.display());
        match crate::encoding::read_text(&path) {
            Ok((text, encoding)) => {
                self.text = text;
                self.path = Some(path.clone());
                self.encoding = encoding;
                self.dirty = false;
                self.jump_line = None;
                self.preview_line = None;
                self.preview_top_line = 1;
                self.last_cursor = (1, 1);
                self.reparse();
                self.store_active();
                self.set_status(format!("Opened: {}", path.display()), false);
            }
            Err(e) => self.set_status(format!("Could not open {}: {e}", path.display()), true),
        }
    }

    // ---------------- Actions ----------------

    /// Keeps the window open while any document has unsaved changes. The
    /// save-changes sheet opens only when no tab sheet is pending.
    fn guard_window_close(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && self.dirty_count() > 0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.pending_tab_close.is_none() {
                self.close_dialog = true;
            }
        }
    }

    /// Opens a local file that was clicked in a document. Markdown opens in a
    /// tab; other files go to the system's default application.
    fn open_linked_path(&mut self, ctx: &egui::Context, path: PathBuf) {
        if !path.is_file() {
            self.set_status(format!("File not found: {}", path.display()), true);
        } else if is_markdown(&path) {
            self.open_path(path);
        } else {
            ctx.open_url(egui::OpenUrl::new_tab(file_url(&path)));
        }
    }

    /// Switches the view mode.
    ///
    /// When leaving the split editor for the rendered preview, remember the
    /// editor caret line so the preview scrolls to the same section instead of
    /// restoring its old scroll offset.
    fn set_mode(&mut self, mode: ViewMode) {
        if mode != self.mode {
            match (self.mode, mode) {
                // Editor → Preview: show the section around the caret.
                (ViewMode::Editor, ViewMode::Preview) => {
                    self.preview_line = Some(self.last_cursor.0);
                }
                // Preview → split: show the section the user was reading in
                // both the editor and the preview pane.
                (ViewMode::Preview, ViewMode::Editor) => {
                    let line = self.preview_top_line.max(1);
                    self.preview_line = Some(line);
                    self.jump_line = Some(EditorJump { line, focus: true });
                }
                _ => {}
            }
            self.mode = mode;
        }
    }

    /// Re-parses the editor text and refreshes the cached statistics.
    ///
    /// Called after every editor change and when a file is opened in the
    /// current tab. Opening into a new tab parses the file in
    /// [`App::open_path`] instead.
    fn reparse(&mut self) {
        self.doc = parser::parse(&self.text);
        self.preview_revision = self.preview_revision.wrapping_add(1);
        (self.line_count, self.word_count) = text_stats(&self.text);
    }

    /// Shows a transient status message; it hides after [`STATUS_DURATION`].
    fn set_status(&mut self, msg: impl Into<String>, is_error: bool) {
        self.status = Some((msg.into(), is_error, Instant::now()));
    }

    /// Window title: file name plus an unsaved marker (or "Untitled").
    fn window_title(&self) -> String {
        match &self.path {
            Some(p) => format!(
                "{}{} — MD-Viewer",
                p.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                if self.dirty { " •" } else { "" }
            ),
            None => "Untitled — MD-Viewer".into(),
        }
    }

    /// Pushes the window title to the OS only when it actually changed.
    fn sync_window_title(&mut self, ctx: &egui::Context) {
        let title = self.window_title();
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
    }

    /// Opens a file via the native file dialog.
    fn open_dialog(&mut self) {
        let Some(p) = rfd::FileDialog::new()
            .set_title("Open Markdown")
            .add_filter("Markdown", MARKDOWN_EXTENSIONS)
            .pick_file()
        else {
            return;
        };
        self.open_path(p);
    }

    /// A pristine, empty "Untitled" tab (no path, no edits, and only
    /// whitespace or nothing in the buffer).
    fn is_blank_untitled(&self) -> bool {
        self.path.is_none() && !self.dirty && self.text.trim().is_empty()
    }

    /// Loads a file into a new tab (used by dialog, drag & drop and macOS
    /// open events). If the file is already open, its tab is activated. A
    /// pristine Untitled tab is reused instead of creating a new one.
    fn open_path(&mut self, path: PathBuf) {
        if let Some(index) = self.find_tab(&path) {
            if index != self.active_tab {
                self.switch_tab(index);
                self.set_status(format!("Switched to: {}", path.display()), false);
            }
            return;
        }
        // Reuse an empty Untitled tab instead of leaving a blank tab behind.
        if self.is_blank_untitled() {
            self.open_path_in_current_tab(path);
            return;
        }
        log::info!("opening {} in a new tab", path.display());
        match crate::encoding::read_text(&path) {
            Ok((text, encoding)) => {
                self.store_active();
                let tab = TabState {
                    encoding,
                    ..TabState::from_text(text, Some(path.clone()))
                };
                // The placeholder is overwritten when the tab is stored again.
                self.tabs.push(TabState::default());
                self.active_tab = self.tabs.len() - 1;
                self.load_tab(tab);
                self.set_status(format!("Opened: {}", path.display()), false);
            }
            Err(e) => self.set_status(format!("Could not open {}: {e}", path.display()), true),
        }
    }

    /// Saves to the current path (or asks for one if the file is new).
    fn save(&mut self) {
        self.do_save();
    }

    /// Saves to the current path, or through "Save as…" for a new document.
    /// Returns `false` if nothing was written; errors go to the status bar.
    fn do_save(&mut self) -> bool {
        match self.path.clone() {
            Some(path) => self.write_document(&path),
            None => self.save_as_dialog(),
        }
    }

    /// Writes the buffer to `path` and makes it the document's path.
    fn write_document(&mut self, path: &Path) -> bool {
        // Write the file back in the encoding it was read with. Text that the
        // encoding cannot represent is saved as UTF-8, and the user is told.
        let (bytes, fallback_from) = match crate::encoding::encode(&self.text, self.encoding) {
            Some(bytes) => (bytes, None),
            None => {
                let original = self.encoding.label();
                self.encoding = crate::encoding::TextEncoding::Utf8;
                (self.text.as_bytes().to_vec(), Some(original))
            }
        };
        match std::fs::write(path, bytes) {
            Ok(()) => {
                if let Some(original) = fallback_from {
                    self.set_status(
                        format!("Saved as UTF-8: {original} cannot represent every character."),
                        true,
                    );
                } else {
                    self.set_status(format!("Saved: {}", path.display()), false);
                }
                self.path = Some(path.to_path_buf());
                self.dirty = false;
                true
            }
            Err(e) => {
                self.set_status(format!("Save failed: {e}"), true);
                false
            }
        }
    }

    /// Default file name for a save or export dialog, based on the title.
    fn default_file_name(&self, ext: &str) -> String {
        let stem = self
            .doc
            .title
            .as_deref()
            .map(slug)
            .filter(|stem| !stem.is_empty());
        format!("{}.{ext}", stem.as_deref().unwrap_or("document"))
    }

    /// "Save as…" dialog with a file name derived from the document title.
    ///
    /// If the chosen file is already open in another tab, that tab is
    /// activated and nothing is written, so the file cannot be open twice.
    fn save_as_dialog(&mut self) -> bool {
        let default_name = self.default_file_name("md");
        let Some(p) = rfd::FileDialog::new()
            .set_title("Save Markdown as…")
            .add_filter("Markdown", MARKDOWN_EXTENSIONS)
            .set_file_name(&default_name)
            .save_file()
        else {
            return false;
        };
        if let Some(index) = self.find_tab(&p)
            && index != self.active_tab
        {
            self.switch_tab(index);
            self.set_status(
                format!("{} is already open in another tab.", p.display()),
                true,
            );
            return false;
        }
        self.write_document(&p)
    }

    /// Exports the current document as PDF via a native save dialog.
    ///
    /// WeasyPrint is mandatory: failures are reported in the status bar and
    /// nothing is written.
    fn export_pdf(&mut self) {
        if !self.require_weasyprint() {
            return;
        }
        let default_name = self.default_file_name("pdf");
        let Some(p) = rfd::FileDialog::new()
            .set_title("Export as PDF…")
            .add_filter("PDF", &["pdf"])
            .set_file_name(&default_name)
            .save_file()
        else {
            return;
        };
        self.start_export(p, false);
    }

    /// Runs the PDF export on a worker thread, so the window keeps responding
    /// while WeasyPrint works. [`App::poll_export`] collects the result.
    fn start_export(&mut self, target: PathBuf, print: bool) {
        if self.export_job.is_some() {
            self.set_status("An export is already running.", false);
            return;
        }
        let markdown = self.text.clone();
        let base_dir = self.base_dir();
        let source = self.path.clone();
        let (sender, result) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let outcome = crate::export::export_pdf(
                &markdown,
                base_dir.as_deref(),
                source.as_deref(),
                &target,
            );
            let _ = sender.send(outcome);
        });
        self.export_job = Some(ExportJob { result, print });
        let message = if print {
            "Preparing print…"
        } else {
            "Exporting PDF…"
        };
        self.set_status(message, false);
    }

    /// Collects a finished export and reports it. A finished print opens the
    /// macOS print dialog here, on the main thread.
    fn poll_export(&mut self, ctx: &egui::Context) {
        let Some(job) = self.export_job.as_ref() else {
            return;
        };
        let print = job.print;
        let outcome = match job.result.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(Duration::from_millis(100));
                return;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(std::io::Error::other("the export stopped unexpectedly"))
            }
        };
        self.export_job = None;
        match (print, outcome) {
            (false, Ok(path)) => {
                self.set_status(format!("PDF exported: {}", path.display()), false)
            }
            (false, Err(e)) => self.set_status(format!("PDF export failed: {e}"), true),
            (true, Ok(path)) => {
                #[cfg(target_os = "macos")]
                {
                    match crate::macos_print::print_pdf(&path) {
                        Ok(true) => self.set_status("Print dialog opened.", false),
                        Ok(false) => self.set_status("Print cancelled.", false),
                        Err(e) => self.set_status(format!("Print failed: {e}"), true),
                    }
                }
            }
            (true, Err(e)) => self.set_status(format!("Print failed: {e}"), true),
        }
    }

    /// Prints the current document: exports a temporary PDF and shows the
    /// native macOS print dialog (PDFKit).
    ///
    /// Stale `md-viewer-print-*.pdf` files older than an hour are removed from
    /// the temp directory first. The print sheet is non-blocking, so the file
    /// the current print uses cannot be deleted right away.
    fn print_document(&mut self) {
        if !self.require_weasyprint() {
            return;
        }
        remove_stale_print_files(&std::env::temp_dir(), Duration::from_secs(60 * 60));
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let pdf = std::env::temp_dir().join(format!(
            "md-viewer-print-{}-{stamp}.pdf",
            std::process::id()
        ));
        self.start_export(pdf, true);
    }

    /// Re-checks the mandatory WeasyPrint CLI and updates the toolbar state.
    fn refresh_weasyprint(&mut self) {
        self.weasyprint_ready = crate::export::weasyprint_path().is_some();
    }

    /// Re-checks WeasyPrint for an export. When it is missing, opens the setup
    /// dialog and returns `false`.
    fn require_weasyprint(&mut self) -> bool {
        self.refresh_weasyprint();
        if !self.weasyprint_ready {
            self.weasyprint_dialog = true;
        }
        self.weasyprint_ready
    }

    /// Shows the find bar and focuses its query field.
    fn open_find_bar(&mut self) {
        self.find_open = true;
        self.find_focus = true;
    }

    /// Changes the font size by `delta` points, clamped to the supported range.
    fn set_font_size(&mut self, delta: f32) {
        self.font_size = (self.font_size + delta).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
    }

    /// Keyboard shortcuts, active only when no modal owns the input.
    ///
    /// ⌘O open · ⌘S save · ⌘⇧S save as · ⌘E export PDF · ⌘W close tab ·
    /// ⌘P print · ⌘⇧P toggle Preview/Edit · ⌘⇧O outline · ⌘F find ·
    /// ⌘G next match · ⌘⇧G previous match · ⌘+ / ⌘= larger · ⌘− smaller ·
    /// ⌘0 reset zoom · Esc closes the find bar.
    ///
    /// The key is read inside `ctx.input`, but the action runs after the
    /// closure returns: dialogs and the print sheet must not run while egui's
    /// input lock is held.
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let (escape, key, shift) = ctx.input(|i| {
            let key = if i.modifiers.contains(Modifiers::COMMAND) {
                [
                    egui::Key::F,
                    egui::Key::G,
                    egui::Key::O,
                    egui::Key::S,
                    egui::Key::E,
                    egui::Key::W,
                    egui::Key::P,
                    egui::Key::Equals,
                    egui::Key::Plus,
                    egui::Key::Minus,
                    egui::Key::Num0,
                ]
                .into_iter()
                .find(|key| i.key_pressed(*key))
            } else {
                None
            };
            (i.key_pressed(egui::Key::Escape), key, i.modifiers.shift)
        });
        if escape {
            self.find_open = false;
        }
        let Some(key) = key else {
            return;
        };
        match key {
            egui::Key::F => self.open_find_bar(),
            egui::Key::G if shift => self.find_prev(),
            egui::Key::G => self.find_next(),
            egui::Key::O if shift => self.outline_open = !self.outline_open,
            egui::Key::O => self.open_dialog(),
            egui::Key::S if shift => {
                self.save_as_dialog();
            }
            egui::Key::S => self.save(),
            egui::Key::E => self.export_pdf(),
            egui::Key::W => self.request_close_tab(self.active_tab),
            egui::Key::P if shift => self.set_mode(self.mode.toggled()),
            egui::Key::P => self.print_document(),
            egui::Key::Equals | egui::Key::Plus => self.set_font_size(1.0),
            egui::Key::Minus => self.set_font_size(-1.0),
            egui::Key::Num0 => self.font_size = DEFAULT_FONT_SIZE,
            _ => {}
        }
    }

    /// Opens every Markdown file dropped onto the window (one tab per file).
    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let files = ctx.input(|i| i.raw.dropped_files.clone());
        for file in files {
            let path = file.path();
            if is_markdown(path) {
                self.open_path(path.to_path_buf());
            }
        }
    }

    /// Installs the macOS open-document hooks (once) and drains files opened
    /// from Finder.
    fn handle_macos_open(&mut self) {
        if !self.open_glue_installed {
            self.open_glue_installed = crate::macos_open::install();
        }
        for path in crate::macos_open::take_pending() {
            if is_markdown(&path) {
                self.open_path(path);
            }
        }
    }

    // ---------------- Panels ----------------

    /// Top toolbar: file actions, view mode, zoom and current file.
    fn toolbar(&mut self, ui: &mut Ui) {
        egui::Panel::top("toolbar")
            .frame(Frame::new().fill(palette().bg1).inner_margin(Margin {
                left: 10,
                right: 10,
                top: 5,
                bottom: 5,
            }))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Open").clicked() {
                        self.open_dialog();
                    }
                    let save_label = if self.dirty { "Save ●" } else { "Save" };
                    if ui.button(save_label).clicked() {
                        self.save();
                    }
                    // PDF export is unavailable without WeasyPrint. The button
                    // stays visible but greyed; clicking it reopens the setup
                    // dialog instead of a save dialog that can only fail.
                    let pdf_text = if self.weasyprint_ready {
                        RichText::new("Export PDF")
                    } else {
                        RichText::new("Export PDF").color(palette().faint)
                    };
                    let export = ui.add(egui::Button::new(pdf_text));
                    let export = if self.weasyprint_ready {
                        export
                    } else {
                        export.on_hover_text(
                            "WeasyPrint is not installed — click to set up PDF export",
                        )
                    };
                    if export.clicked() {
                        self.export_pdf();
                    }

                    // Print uses the same mandatory WeasyPrint path, then hands
                    // the PDF to Preview for the system print dialog.
                    let print_text = if self.weasyprint_ready {
                        RichText::new("Print")
                    } else {
                        RichText::new("Print").color(palette().faint)
                    };
                    let print = ui.add(egui::Button::new(print_text));
                    let print = if self.weasyprint_ready {
                        print.on_hover_text("Print (⌘P) — native print dialog")
                    } else {
                        print.on_hover_text(
                            "WeasyPrint is not installed — click to set up PDF export",
                        )
                    };
                    if print.clicked() {
                        self.print_document();
                    }
                    ui.separator();
                    let mut mode = self.mode;
                    ui.selectable_value(&mut mode, ViewMode::Preview, "Preview")
                        .on_hover_text("Rendered preview (⌘⇧P)");
                    ui.selectable_value(&mut mode, ViewMode::Editor, "Edit")
                        .on_hover_text("Source editor + live preview (⌘⇧P)");
                    if mode != self.mode {
                        self.set_mode(mode);
                    }
                    ui.separator();
                    ui.toggle_value(&mut self.outline_open, "Outline")
                        .on_hover_text("Outline sidebar (⌘⇧O)");
                    if ui
                        .button("Find")
                        .on_hover_text("Find & replace (⌘F)")
                        .clicked()
                    {
                        self.open_find_bar();
                    }
                    ui.separator();
                    if ui
                        .add_sized(ZOOM_CONTROL_SIZE, egui::Button::new("A−"))
                        .on_hover_text("Smaller font (⌘−)")
                        .clicked()
                    {
                        self.set_font_size(-1.0);
                    }
                    // Magnification readout. It is intentionally a button: a
                    // click restores 100% (the same action as ⌘0).
                    let percent = magnification_percent(self.font_size);
                    if ui
                        .add_sized(
                            ZOOM_CONTROL_SIZE,
                            egui::Button::new(
                                RichText::new(format!("{percent:.0}%"))
                                    .monospace()
                                    .size(12.0)
                                    .color(palette().muted),
                            ),
                        )
                        .on_hover_text("Magnification — click to reset to 100% (⌘0)")
                        .clicked()
                    {
                        self.font_size = DEFAULT_FONT_SIZE;
                    }
                    if ui
                        .add_sized(ZOOM_CONTROL_SIZE, egui::Button::new("A+"))
                        .on_hover_text("Larger font (⌘+)")
                        .clicked()
                    {
                        self.set_font_size(1.0);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let theme_mode = theme::mode();
                        if ui
                            .add(egui::Button::new(
                                RichText::new(theme_mode.icon()).size(15.0),
                            ))
                            .on_hover_text(theme_mode.hint())
                            .clicked()
                        {
                            theme::toggle();
                            apply_theme(ui.ctx());
                            ui.ctx().request_repaint();
                        }
                        if self.dirty {
                            ui.label(RichText::new("● unsaved").color(palette().orange));
                        }
                        let path_text = match &self.path {
                            Some(path) => ellipsize_path(path),
                            None => "Untitled".to_string(),
                        };
                        // Truncate instead of overflowing into the buttons when
                        // the window is narrow.
                        ui.add(Label::new(RichText::new(path_text).weak()).truncate());
                    });
                });
            });
    }

    /// Recomputes the find matches when the query or the document changed.
    ///
    /// Never moves the caret or the preview. Returns `true` when the query
    /// changed, so the caller can reveal the first match of the new query.
    fn update_find_matches(&mut self) -> bool {
        if self.find_cache.0 == self.find_query && self.find_cache.1 == self.preview_revision {
            return false;
        }
        let query_changed = self.find_cache.0 != self.find_query;
        if query_changed {
            self.find_index = 0;
        }
        self.find_cache = (self.find_query.clone(), self.preview_revision);
        self.find_matches = find_all_case_insensitive(&self.text, &self.find_query);
        if self.find_index >= self.find_matches.len() {
            self.find_index = 0;
        }
        query_changed
    }

    /// Scrolls the preview to source `line`. In Edit mode the caret moves there
    /// too and the editor takes focus.
    fn jump_to_line(&mut self, line: usize) {
        self.preview_line = Some(line);
        if self.mode == ViewMode::Editor {
            self.jump_line = Some(EditorJump { line, focus: true });
        }
    }

    /// Makes match `index` the current one and scrolls the preview to it.
    ///
    /// With `move_editor`, Edit mode also moves the caret to the match without
    /// taking focus, so the find bar keeps the keyboard.
    fn reveal_match(&mut self, index: usize, move_editor: bool) {
        let Some((start, _)) = self.find_matches.get(index).copied() else {
            return;
        };
        self.find_index = index;
        let line = 1 + self.text[..start]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        self.preview_line = Some(line);
        if move_editor && self.mode == ViewMode::Editor {
            self.jump_line = Some(EditorJump { line, focus: false });
        }
    }

    fn find_next(&mut self) {
        self.update_find_matches();
        let count = self.find_matches.len();
        if count > 0 {
            self.reveal_match((self.find_index + 1) % count, true);
        }
    }

    fn find_prev(&mut self) {
        self.update_find_matches();
        let count = self.find_matches.len();
        if count > 0 {
            self.reveal_match((self.find_index + count - 1) % count, true);
        }
    }

    /// Refreshes the matches for the query being typed. A changed query scrolls
    /// the preview to its first match, but never moves the editor.
    fn refresh_find(&mut self) {
        if self.update_find_matches() {
            self.reveal_match(self.find_index, false);
        }
    }

    /// Replaces the current match and advances to the next one.
    fn replace_current(&mut self) {
        self.update_find_matches();
        let Some((start, end)) = self.find_matches.get(self.find_index).copied() else {
            return;
        };
        if self.text[start..end] != *self.replace_text {
            self.text.replace_range(start..end, &self.replace_text);
            self.dirty = true;
            self.reparse();
        }
        // Search after the inserted text, so a replacement that contains the
        // query is not matched again.
        let after = start + self.replace_text.len();
        self.update_find_matches();
        let next = self
            .find_matches
            .iter()
            .position(|(match_start, _)| *match_start >= after)
            .unwrap_or(0);
        self.reveal_match(next, true);
    }

    /// Replaces every match in one pass.
    fn replace_all(&mut self) {
        self.update_find_matches();
        let changed = self
            .find_matches
            .iter()
            .any(|(start, end)| self.text[*start..*end] != *self.replace_text);
        if !changed {
            return;
        }
        let matches = std::mem::take(&mut self.find_matches);
        for (start, end) in matches.iter().rev() {
            self.text.replace_range(*start..*end, &self.replace_text);
        }
        self.dirty = true;
        self.reparse();
        self.update_find_matches();
        self.reveal_match(0, true);
    }

    /// Find/replace bar below the toolbar.
    fn find_bar(&mut self, ui: &mut Ui) {
        self.refresh_find();
        egui::Panel::top("findbar")
            .frame(
                Frame::new()
                    .fill(palette().bg2)
                    .inner_margin(Margin::symmetric(10, 5)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let find = ui.add(
                        egui::TextEdit::singleline(&mut self.find_query)
                            .hint_text("Find")
                            .desired_width(220.0),
                    );
                    if self.find_focus {
                        find.request_focus();
                        self.find_focus = false;
                    }
                    if find.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.find_next();
                        find.request_focus();
                    }
                    let total = self.find_matches.len();
                    let current = if total == 0 {
                        0
                    } else {
                        self.find_index.min(total - 1) + 1
                    };
                    ui.label(
                        RichText::new(format!("{current}/{total}"))
                            .monospace()
                            .size(12.0)
                            .color(palette().muted),
                    );
                    if ui
                        .button("↑")
                        .on_hover_text("Previous match (⇧⌘G)")
                        .clicked()
                    {
                        self.find_prev();
                    }
                    if ui.button("↓").on_hover_text("Next match (⌘G)").clicked() {
                        self.find_next();
                    }
                    ui.separator();
                    let replace = ui.add(
                        egui::TextEdit::singleline(&mut self.replace_text)
                            .hint_text("Replace")
                            .desired_width(180.0),
                    );
                    if replace.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.replace_current();
                        replace.request_focus();
                    }
                    if ui.button("Replace").clicked() {
                        self.replace_current();
                    }
                    if ui
                        .button("All")
                        .on_hover_text("Replace all matches")
                        .clicked()
                    {
                        self.replace_all();
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Close").on_hover_text("Close (Esc)").clicked() {
                            self.find_open = false;
                        }
                    });
                });
            });
    }

    /// Rebuilds the outline (top-level headings with their source lines) when
    /// the preview has changed.
    fn refresh_outline(&mut self) {
        if self.outline_revision == self.preview_revision {
            return;
        }
        self.outline_revision = self.preview_revision;
        self.outline = Arc::new(
            self.doc
                .blocks
                .iter()
                .zip(self.doc.block_lines.iter())
                .filter_map(|(block, line)| {
                    if let Block::Heading { level, inlines } = block {
                        let text: String = inlines.iter().map(|i| i.plain_text()).collect();
                        Some((*level, *line, text))
                    } else {
                        None
                    }
                })
                .collect(),
        );
    }

    /// Collapsible outline sidebar. Clicking a heading scrolls the preview and
    /// moves the editor caret to that section.
    fn outline_panel(&mut self, ui: &mut Ui) {
        self.refresh_outline();
        let entries = Arc::clone(&self.outline);
        let active_line = if self.mode == ViewMode::Editor {
            self.last_cursor.0
        } else {
            self.preview_top_line
        };
        let active = entries
            .iter()
            .rposition(|(_, line, _)| *line <= active_line);

        egui::Panel::left("outline")
            .resizable(true)
            .default_size(240.0)
            .min_size(160.0)
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Outline")
                        .strong()
                        .size(14.0)
                        .color(palette().text),
                );
                ui.add_space(4.0);
                ui.separator();
                if entries.is_empty() {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("No headings")
                            .size(13.0)
                            .color(palette().muted),
                    );
                    return;
                }
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (index, (level, line, text)) in entries.iter().enumerate() {
                            let indent = (level.saturating_sub(1) as f32) * 12.0;
                            let selected = active == Some(index);
                            ui.horizontal(|ui| {
                                ui.add_space(indent);
                                let response = ui.add(
                                    egui::Label::new(
                                        RichText::new(text)
                                            .size(if *level == 1 { 14.0 } else { 13.0 })
                                            .color(if selected {
                                                palette().accent
                                            } else {
                                                palette().text
                                            }),
                                    )
                                    .sense(egui::Sense::click())
                                    .truncate(),
                                );
                                if response.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                                if response.clicked() {
                                    self.jump_to_line(*line);
                                }
                            });
                        }
                    });
            });
    }

    /// Native-style tab strip below the toolbar.
    ///
    /// macOS window tabs only appear once a second document exists, so the
    /// strip is hidden for a single tab. When visible, all tabs share the full
    /// row width equally; the active tab blends into the content below, and
    /// each tab has a close button (also ⌘W / middle-click). A dirty tab shows
    /// a dot after its name.
    fn tab_bar(&mut self, ui: &mut Ui) {
        if !self.tab_bar_visible() {
            return;
        }
        egui::Panel::top("tabbar")
            .frame(Frame::new().fill(palette().bg1))
            .show(ui, |ui| {
                let height = 30.0;
                let width = ui.available_width();
                let count = self.tabs.len();
                let tab_w = width / count as f32;
                let (row_rect, _) =
                    ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::hover());
                let painter = ui.painter_at(row_rect);
                painter.rect_filled(row_rect, CornerRadius::ZERO, palette().bg1);
                // Baseline between the tabs and the document below. The active
                // tab paints over it so it visually connects to the content.
                painter.line_segment(
                    [row_rect.left_bottom(), row_rect.right_bottom()],
                    Stroke::new(1.0, palette().stroke),
                );

                let mut activate = None;
                let mut close = None;
                for index in 0..count {
                    let active = index == self.active_tab;
                    let x0 = row_rect.left() + index as f32 * tab_w;
                    let rect = Rect::from_min_size(
                        egui::pos2(x0, row_rect.top()),
                        Vec2::new(tab_w, height),
                    );
                    let tab_response = ui.interact(
                        rect,
                        ui.scope_id().with(("tab", index)),
                        egui::Sense::click(),
                    );
                    let hovered = tab_response.hovered();

                    let fill = if active {
                        palette().bg0
                    } else if hovered {
                        palette().bg3
                    } else {
                        palette().bg1
                    };
                    painter.rect_filled(
                        rect,
                        CornerRadius {
                            nw: 6,
                            ne: 6,
                            sw: 0,
                            se: 0,
                        },
                        fill,
                    );
                    // Separator between neighbouring tabs, but never across
                    // the active tab.
                    if index > 0 && !active && index - 1 != self.active_tab {
                        painter.line_segment(
                            [rect.left_top(), rect.left_bottom()],
                            Stroke::new(1.0, palette().stroke),
                        );
                    }

                    let mut label = tab_label(self.tab_path(index));
                    if self.tab_dirty(index) {
                        label.push_str(" ●");
                    }
                    let max_chars = ((tab_w - 44.0) / 7.0).max(1.0) as usize;
                    let label = truncate_label(&label, max_chars);
                    painter.text(
                        egui::pos2(rect.center().x - 6.0, rect.center().y),
                        egui::Align2::CENTER_CENTER,
                        label,
                        FontId::proportional(13.0),
                        if active {
                            palette().text
                        } else {
                            palette().muted
                        },
                    );

                    // Close button at the right edge of the tab.
                    let close_rect = Rect::from_center_size(
                        egui::pos2(rect.right() - 16.0, rect.center().y),
                        Vec2::new(18.0, 18.0),
                    );
                    let close_response = ui.interact(
                        close_rect,
                        ui.scope_id().with(("tab_close", index)),
                        egui::Sense::click(),
                    );
                    if close_response.hovered() {
                        painter.circle_filled(close_rect.center(), 8.0, palette().bg3);
                    }
                    painter.text(
                        close_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "×",
                        FontId::proportional(13.0),
                        if close_response.hovered() {
                            palette().text
                        } else {
                            palette().faint
                        },
                    );

                    if close_response.clicked() {
                        close = Some(index);
                    } else if tab_response.clicked() {
                        activate = Some(index);
                    }
                    if tab_response.middle_clicked() {
                        close = Some(index);
                    }
                    close_response.on_hover_text("Close tab (⌘W)");
                    tab_response.on_hover_text(
                        self.tab_path(index)
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "Untitled".into()),
                    );
                }

                if let Some(index) = activate {
                    self.switch_tab(index);
                }
                if let Some(index) = close {
                    self.request_close_tab(index);
                }
            });
    }

    /// macOS shows the tab bar only with more than one window/tab open.
    fn tab_bar_visible(&self) -> bool {
        self.tabs.len() > 1
    }

    /// Edit mode: manual split screen (raw editor left, live preview right).
    ///
    /// The split is stateful (`split_fraction`), starts at 50/50 and keeps the
    /// position after dragging the divider. A manual split avoids egui panel
    /// state issues and guarantees an exact 50/50 start.
    fn edit_split(&mut self, ui: &mut Ui) {
        egui::CentralPanel::default_margins()
            .frame(Frame::new().fill(palette().bg0))
            .show(ui, |ui| {
                let total_w = ui.available_width();
                let total_h = ui.available_height();
                let divider_w = 8.0;
                let min_side = 260.0_f32.min((total_w - divider_w) * 0.45);
                let max_left = (total_w - min_side - divider_w).max(min_side);
                let left_w = (total_w * self.split_fraction).clamp(min_side, max_left);
                let right_w = (total_w - left_w - divider_w).max(min_side);
                // Content width inside the 24 px frame margins. Without this
                // explicit cap the child UI can use the full pane width and
                // clip away the right margin after a window resize.
                let inner_left = (left_w - 48.0).max(0.0);
                let inner_right = (right_w - 48.0).max(0.0);

                let f = self.font_size;
                let base_dir = self.base_dir();
                let doc = &self.doc;
                let text = &mut self.text;
                let last_cursor = &mut self.last_cursor;
                let cursor_key = &mut self.cursor_key;
                let math_cache = &mut self.math_cache;
                let preview_line = &mut self.preview_line;
                let preview_top_line = &mut self.preview_top_line;
                let preview_metrics = &mut self.preview_metrics;
                let preview_revision = self.preview_revision;
                let mut changed = false;
                let mut new_fraction = self.split_fraction;
                let mut image_click = None;
                let mut source_jump = None;
                let mut jump_line = self.jump_line.take();

                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;

                    // Left: raw Markdown editor.
                    ui.allocate_ui_with_layout(
                        Vec2::new(left_w, total_h),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_width(left_w);
                            ui.set_height(total_h);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                ui.add_space(24.0);
                                ui.vertical(|ui| {
                                    ui.set_width(inner_left);
                                    ui.set_height(total_h);
                                    ui.add_space(16.0);
                                    changed = editor_ui(
                                        ui,
                                        text,
                                        f,
                                        last_cursor,
                                        cursor_key,
                                        preview_revision,
                                        &mut jump_line,
                                    );
                                });
                                ui.add_space(24.0);
                            });
                        },
                    );

                    // Draggable divider.
                    let (rect, response) =
                        ui.allocate_exact_size(Vec2::new(divider_w, total_h), egui::Sense::drag());
                    let color = if response.hovered() || response.dragged() {
                        palette().accent
                    } else {
                        palette().stroke
                    };
                    ui.painter().rect_filled(rect, CornerRadius::ZERO, color);
                    if response.hovered() || response.dragged() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    }
                    if response.dragged()
                        && let Some(pos) = ui.ctx().pointer_interact_pos()
                    {
                        let left_edge = rect.min.x - left_w;
                        new_fraction = ((pos.x - left_edge) / total_w).clamp(0.2, 0.8);
                    }

                    // Right: live preview.
                    ui.allocate_ui_with_layout(
                        Vec2::new(right_w, total_h),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_width(right_w);
                            ui.set_height(total_h);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                ui.add_space(24.0);
                                ui.vertical(|ui| {
                                    ui.set_width(inner_right);
                                    ui.set_height(total_h);
                                    ui.add_space(16.0);
                                    source_jump = preview_ui(
                                        ui,
                                        doc,
                                        f,
                                        base_dir.as_deref(),
                                        false,
                                        &mut image_click,
                                        math_cache,
                                        preview_line,
                                        preview_top_line,
                                        preview_metrics,
                                        preview_revision,
                                    );
                                });
                                ui.add_space(24.0);
                            });
                        },
                    );
                });

                self.split_fraction = new_fraction;
                self.jump_line = source_jump
                    .map(|line| EditorJump { line, focus: true })
                    .or(jump_line);
                if changed {
                    self.dirty = true;
                    self.reparse();
                }
                if let Some(request) = image_click {
                    self.image_viewer = Some(ImageViewer::new(request));
                }
            });
    }

    /// Renders the document in a full-window, centered reading column.
    fn preview_central(&mut self, ui: &mut Ui) {
        egui::CentralPanel::default_margins()
            .frame(Frame::new().fill(palette().bg0))
            .show(ui, |ui| {
                let base_dir = self.base_dir();
                let mut image_click = None;
                // Clicks in the preview only move the caret in Edit mode, so the
                // returned source line is ignored here.
                preview_ui(
                    ui,
                    &self.doc,
                    self.font_size,
                    base_dir.as_deref(),
                    true,
                    &mut image_click,
                    &mut self.math_cache,
                    &mut self.preview_line,
                    &mut self.preview_top_line,
                    &mut self.preview_metrics,
                    self.preview_revision,
                );
                if let Some(request) = image_click {
                    self.image_viewer = Some(ImageViewer::new(request));
                }
            });
    }

    /// Asks whether unsaved changes should be written before the window closes.
    ///
    /// A native window close is intercepted in [`App::guard_window_close`]
    /// while any tab is dirty. With multiple dirty tabs the sheet offers **Save All**; otherwise
    /// it mirrors the macOS save-changes sheet. Esc is Cancel, Enter is Save.
    fn show_close_dialog(&mut self, ctx: &egui::Context) {
        if !self.close_dialog {
            return;
        }
        let dirty = self.dirty_count();
        let message = if dirty > 1 {
            format!("{dirty} documents have unsaved changes. Save all of them before closing?")
        } else {
            // With a single dirty tab it may not be the active one.
            let file_name = (0..self.tabs.len())
                .find(|i| self.tab_dirty(*i))
                .and_then(|i| self.tab_path(i))
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("this document");
            format!(
                "Do you want to save the changes you made to \u{201c}{file_name}\u{201d} before closing?"
            )
        };
        match Self::unsaved_modal(
            ctx,
            "window",
            &message,
            if dirty > 1 { "Save All" } else { "Save" },
        ) {
            Some(UnsavedChoice::Save) => {
                if self.save_all_tabs() {
                    self.close_dialog = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Some(UnsavedChoice::Discard) => {
                self.dirty = false;
                for tab in &mut self.tabs {
                    tab.dirty = false;
                }
                self.close_dialog = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Some(UnsavedChoice::Cancel) => self.close_dialog = false,
            None => {}
        }
    }

    /// One-time onboarding for the mandatory WeasyPrint CLI.
    ///
    /// WeasyPrint is not bundled. The dialog appears at launch when the binary
    /// is missing (and again if PDF export is attempted), explains the single
    /// Homebrew command and can re-check without restarting the app.
    fn show_weasyprint_dialog(&mut self, ctx: &egui::Context) {
        if !self.weasyprint_dialog {
            return;
        }
        let dialog_w = (ctx.content_rect().width() - 200.0).clamp(440.0, 600.0);
        let mut close = false;
        let mut check = false;
        let modal = egui::Modal::new(egui::Id::unique("weasyprint_onboarding"))
            .backdrop_color(Color32::from_black_alpha(150))
            .frame(modal_frame())
            .show(ctx, |ui| {
                ui.set_width(dialog_w);
                ui.label(
                    RichText::new("PDF export needs WeasyPrint")
                        .font(bold_font(19.0))
                        .color(palette().text),
                );
                ui.add_space(8.0);
                hairline(ui);
                ui.add_space(14.0);
                ui.label(
                    RichText::new(
                        "MD-Viewer creates PDF files with WeasyPrint, a free command-line tool. \
                         It is not bundled with the app, so it has to be installed once.",
                    )
                    .size(15.0)
                    .color(palette().text)
                    .line_height(Some(21.0)),
                );
                ui.add_space(12.0);
                Frame::new()
                    .fill(palette().bg1)
                    .stroke(Stroke::new(1.0, palette().faint))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(12, 8))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new("brew install weasyprint")
                                    .monospace()
                                    .size(14.0)
                                    .color(palette().text),
                            );
                            ui.add_space(8.0);
                            if ui.small_button("Copy").clicked() {
                                ui.ctx().copy_text("brew install weasyprint".to_owned());
                            }
                        });
                    });
                ui.add_space(10.0);
                ui.label(
                    RichText::new(
                        "Homebrew (brew.sh) is required. After installing, press Check again \
                         — no restart is needed. The editor and preview work without WeasyPrint; \
                         only PDF export needs it.",
                    )
                    .size(13.5)
                    .color(palette().muted)
                    .line_height(Some(19.0)),
                );
                ui.add_space(16.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.spacing_mut().button_padding = Vec2::new(14.0, 6.0);

                    if modal_button(ui, "Continue without PDF", 14.0, false, 170.0).clicked() {
                        close = true;
                    }
                    if modal_button(ui, "Check again", 14.0, true, 120.0).clicked() {
                        check = true;
                    }
                    if modal_button(ui, "weasyprint.org", 14.0, false, 130.0).clicked() {
                        ui.ctx()
                            .open_url(egui::OpenUrl::new_tab("https://weasyprint.org/"));
                    }
                });
            });

        if modal.should_close() {
            close = true;
        }
        if check {
            self.refresh_weasyprint();
            if self.weasyprint_ready {
                self.weasyprint_dialog = false;
                self.set_status("WeasyPrint found — PDF export is ready.", false);
            } else {
                self.set_status(
                    "WeasyPrint still not found. Install it with `brew install weasyprint`.",
                    true,
                );
            }
        }
        if close {
            self.weasyprint_dialog = false;
        }
    }

    /// Asks whether a dirty tab should be saved before it is closed.
    fn show_tab_close_dialog(&mut self, ctx: &egui::Context) {
        let Some(index) = self.pending_tab_close else {
            return;
        };
        if index >= self.tabs.len() {
            self.pending_tab_close = None;
            return;
        }
        let label = tab_label(self.tab_path(index));
        let message = format!(
            "Do you want to save the changes you made to \u{201c}{label}\u{201d} before closing this tab?"
        );
        match Self::unsaved_modal(ctx, "tab", &message, "Save") {
            Some(UnsavedChoice::Save) => {
                self.switch_tab(index);
                if self.do_save() {
                    self.pending_tab_close = None;
                    self.close_tab(index);
                }
            }
            Some(UnsavedChoice::Discard) => {
                self.pending_tab_close = None;
                self.close_tab(index);
            }
            Some(UnsavedChoice::Cancel) => self.pending_tab_close = None,
            None => {}
        }
    }

    /// Shared save-changes sheet used by the window-close and tab-close
    /// prompts. Returns `None` while the modal is still open.
    fn unsaved_modal(
        ctx: &egui::Context,
        id: &str,
        message: &str,
        save_label: &str,
    ) -> Option<UnsavedChoice> {
        let mut choice = None;
        let dialog_w = (ctx.content_rect().width() - 200.0).clamp(400.0, 540.0);
        let modal = egui::Modal::new(egui::Id::unique(("unsaved_modal", id)))
            .backdrop_color(Color32::from_black_alpha(150))
            .frame(modal_frame())
            .show(ctx, |ui| {
                ui.set_width(dialog_w);
                ui.label(
                    RichText::new("Unsaved changes")
                        .font(bold_font(19.0))
                        .color(palette().text),
                );
                ui.add_space(8.0);
                let (rule, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 1.0),
                    egui::Sense::hover(),
                );
                ui.painter()
                    .rect_filled(rule, CornerRadius::ZERO, palette().stroke);
                ui.add_space(14.0);
                ui.label(
                    RichText::new(message)
                        .size(15.0)
                        .color(palette().text)
                        .line_height(Some(21.0)),
                );
                ui.add_space(18.0);

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.spacing_mut().button_padding = Vec2::new(18.0, 7.0);

                    if modal_button(ui, save_label, 14.5, true, 120.0).clicked() {
                        choice = Some(UnsavedChoice::Save);
                    }
                    if modal_button(ui, "Cancel", 14.5, false, 100.0).clicked() {
                        choice = Some(UnsavedChoice::Cancel);
                    }
                    if modal_button(ui, "Don't Save", 14.5, false, 130.0).clicked() {
                        choice = Some(UnsavedChoice::Discard);
                    }
                });

                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    choice = Some(UnsavedChoice::Save);
                }
            });

        if modal.should_close() {
            choice = Some(UnsavedChoice::Cancel);
        }
        choice
    }

    /// Shows the modal picture viewer and handles its close conditions.
    ///
    /// The viewer is an [`egui::Modal`]: it dims the page, blocks input to the
    /// Markdown view underneath, and closes on **Esc**, a click on the
    /// backdrop, or the **Close** button. [`egui::Scene`] provides drag-to-pan
    /// and scroll-to-zoom; the zoom buttons and **Reset** act on the same
    /// scene rectangle.
    fn show_image_viewer(&mut self, ctx: &egui::Context) {
        let Some(viewer) = self.image_viewer.as_mut() else {
            return;
        };

        let screen = ctx.content_rect().size();
        let viewport = Vec2::new(
            (screen.x - 80.0).clamp(240.0, 1100.0),
            (screen.y - 190.0).clamp(180.0, 780.0),
        );

        let mut close = false;
        let mut reset = false;
        let mut zoom_factor = None;

        let modal = egui::Modal::new(egui::Id::unique("image_viewer"))
            .backdrop_color(Color32::from_black_alpha(150))
            .frame(
                Frame::new()
                    .fill(palette().bg1)
                    .stroke(Stroke::new(1.0, palette().stroke))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(16)),
            )
            .show(ctx, |ui| {
                ui.set_width(viewport.x);

                // Header: title, zoom controls and close button.
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(viewer.title())
                            .font(bold_font(15.0))
                            .color(palette().text),
                    )
                    .on_hover_text(&viewer.request.url);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Close (Esc)").clicked() {
                            close = true;
                        }
                        if ui.button("Reset").clicked() {
                            reset = true;
                        }
                        if ui.button("+").on_hover_text("Zoom in").clicked() {
                            zoom_factor = Some(1.25);
                        }
                        if ui.button("−").on_hover_text("Zoom out").clicked() {
                            zoom_factor = Some(0.8);
                        }
                        let percent = if viewer.scene_rect.is_positive() {
                            viewport.x / viewer.scene_rect.width() * 100.0
                        } else {
                            100.0
                        };
                        ui.label(
                            RichText::new(format!("{percent:.0}%"))
                                .size(12.0)
                                .color(palette().muted),
                        );
                    });
                });
                ui.add_space(8.0);

                if reset {
                    viewer.scene_rect = Rect::ZERO;
                }
                if let Some(factor) = zoom_factor {
                    viewer.zoom(factor, viewport);
                }

                // Fixed, clipped viewport so panning never paints over the
                // surrounding page or the modal frame.
                let (rect, _) = ui.allocate_exact_size(viewport, egui::Sense::hover());
                ui.painter()
                    .rect_filled(rect, CornerRadius::same(6), palette().bg0);
                let clip = rect.shrink(1.0);
                ui.scope_builder(egui::UiBuilder::new().max_rect(clip), |ui| {
                    ui.set_clip_rect(clip);
                    let image = egui::Image::new(viewer.request.uri.as_str())
                        .fit_to_original_size(1.0)
                        .sense(egui::Sense::hover())
                        .alt_text(viewer.title());

                    // Fit once the texture is actually ready: while loading,
                    // egui reports a small placeholder size which would make
                    // the first fit wrong.
                    let ready = image
                        .load_for_size(ui.ctx(), Vec2::INFINITY)
                        .ok()
                        .and_then(|poll| poll.texture_id())
                        .is_some();
                    let mut scene_rect = if !viewer.fitted && ready {
                        viewer.fitted = true;
                        Rect::ZERO
                    } else {
                        viewer.scene_rect
                    };

                    egui::Scene::new()
                        .zoom_range(IMAGE_ZOOM_MIN..=IMAGE_ZOOM_MAX)
                        .scroll_zooms(true)
                        .show(ui, &mut scene_rect, |ui| {
                            ui.add(image);
                        });
                    viewer.scene_rect = scene_rect;
                });

                ui.add_space(6.0);
                ui.label(
                    RichText::new("Scroll to zoom · Drag to pan · Esc to close")
                        .size(11.0)
                        .color(palette().faint),
                );
            });

        if close || modal.should_close() {
            self.image_viewer = None;
        }
    }

    /// Bottom status bar: transient messages, cursor position, statistics.
    fn status_bar(&mut self, ui: &mut Ui) {
        if let Some((_, _, at)) = self.status
            && at.elapsed() > STATUS_DURATION
        {
            self.status = None;
        }
        egui::Panel::bottom("status")
            .frame(Frame::new().fill(palette().bg1).inner_margin(Margin {
                left: 10,
                right: 10,
                top: 4,
                bottom: 4,
            }))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some((msg, is_err, _)) = &self.status {
                        ui.label(RichText::new(if *is_err { "✖" } else { "✓" }).color(
                            if *is_err {
                                palette().red
                            } else {
                                palette().green
                            },
                        ));
                        ui.label(RichText::new(msg).color(if *is_err {
                            palette().red
                        } else {
                            palette().muted
                        }));
                        ui.separator();
                    }
                    let (line, col) = self.last_cursor;
                    ui.weak(format!(
                        "Ln {line}, Col {col}  ·  {} lines · {} words",
                        self.line_count, self.word_count
                    ));
                });
            });
    }
}

/// Installs the system fonts used for the UI, with fallbacks.
///
/// * Body text: **SF Pro** (`SFNS.ttf`, a variable font), with real bold and
///   italic cuts from `SFNSItalic.ttf`; the optical size is fixed at 17 for
///   comfortable reading. Without SF Pro, **Helvetica Neue** (TTC faces 0–3:
///   regular/bold/italic/bold-italic) is used.
/// * Code: **SF Mono**.
///
/// The font files are read from the system at runtime (never bundled), so the
/// app keeps the native macOS look without licensing issues. Custom families
/// (`bold`, `italic`, `bold-italic`) make real bold/italic text possible —
/// egui's default font has no separate bold face.
fn install_system_fonts(ctx: &egui::Context) {
    use egui::epaint::text::VariationCoords;
    use egui::{FontData, FontDefinitions, FontFamily};

    /// Adds the face `index` of `blob` to the front of `family`. `weight` sets
    /// the variable-font axes; `None` keeps a static face as it is.
    fn add_face(
        fonts: &mut FontDefinitions,
        blob: &Arc<Vec<u8>>,
        index: u32,
        name: &str,
        family: FontFamily,
        weight: Option<f32>,
    ) {
        let mut data = FontData::from_blob(blob.clone(), index);
        if let Some(weight) = weight {
            data.tweak.coords = VariationCoords::new([(b"wght", weight), (b"opsz", 17.0)]);
        }
        fonts.font_data.insert(name.to_owned(), Arc::new(data));
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, name.to_owned());
    }

    let mut fonts = FontDefinitions::default();

    // SF Pro regular + bold (variable weight).
    let sf = std::fs::read("/System/Library/Fonts/SFNS.ttf")
        .ok()
        .map(Arc::new);
    if let Some(sf) = &sf {
        add_face(
            &mut fonts,
            sf,
            0,
            "sf-pro",
            FontFamily::Proportional,
            Some(400.0),
        );
        add_face(
            &mut fonts,
            sf,
            0,
            "sf-pro-bold",
            FontFamily::Name("bold".into()),
            Some(700.0),
        );
    }
    // SF Pro italic + bold italic.
    if let Ok(italic) = std::fs::read("/System/Library/Fonts/SFNSItalic.ttf") {
        let italic = Arc::new(italic);
        add_face(
            &mut fonts,
            &italic,
            0,
            "sf-pro-italic",
            FontFamily::Name("italic".into()),
            Some(400.0),
        );
        add_face(
            &mut fonts,
            &italic,
            0,
            "sf-pro-bold-italic",
            FontFamily::Name("bold-italic".into()),
            Some(700.0),
        );
    }

    // Fallback for macOS installs without SF Pro: Helvetica Neue's static faces.
    if sf.is_none()
        && let Ok(bytes) = std::fs::read("/System/Library/Fonts/HelveticaNeue.ttc")
    {
        let blob = Arc::new(bytes);
        let faces = [
            ("helv-neue", 0, FontFamily::Proportional),
            ("helv-neue-bold", 1, FontFamily::Name("bold".into())),
            ("helv-neue-italic", 2, FontFamily::Name("italic".into())),
            (
                "helv-neue-bold-italic",
                3,
                FontFamily::Name("bold-italic".into()),
            ),
        ];
        for (name, index, family) in faces {
            add_face(&mut fonts, &blob, index, name, family, None);
        }
    }

    if let Ok(mono) = std::fs::read("/System/Library/Fonts/SFNSMono.ttf") {
        add_face(
            &mut fonts,
            &Arc::new(mono),
            0,
            "sf-mono",
            FontFamily::Monospace,
            None,
        );
    }

    ctx.set_fonts(fonts);
}

/// Bold text font (SF Pro, weight 700).
fn bold_font(size: f32) -> FontId {
    FontId::new(size, egui::FontFamily::Name("bold".into()))
}

/// Italic text font (SF Pro Italic).
fn italic_font(size: f32) -> FontId {
    FontId::new(size, egui::FontFamily::Name("italic".into()))
}

/// Clipboard action triggered from the editor context menu or a Ctrl shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorClipboardAction {
    Copy,
    Cut,
    Paste,
    SelectAll,
}

/// Copies the selected text to the OS clipboard.
///
/// Returns `false` if there is no selection (nothing is copied).
fn editor_copy(ctx: &egui::Context, text: &str, range: Option<egui::text::CCursorRange>) -> bool {
    let Some(range) = range.filter(|r| !r.is_empty()) else {
        return false;
    };
    ctx.copy_text(range.slice_str(text).to_string());
    true
}

/// Id of the cached editor selection, restored when a context menu is open.
fn selection_cache_id() -> egui::Id {
    egui::Id::unique("md_editor_selection_cache")
}

/// Forgets the cached editor selection.
fn clear_selection_cache(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<egui::text::CCursorRange>(selection_cache_id()));
}

/// Moves the caret or selection of the editor `id`, and focuses the editor
/// when `focus` is set.
fn set_editor_cursor(
    ctx: &egui::Context,
    id: egui::Id,
    range: egui::text::CCursorRange,
    focus: bool,
) {
    let mut state = egui::widgets::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(range));
    state.store(ctx, id);
    if focus {
        ctx.memory_mut(|memory| memory.request_focus(id));
    }
}

/// Cuts the selected text and updates the stored cursor position.
///
/// Returns the new cursor (for the status bar), or `None` if nothing was
/// selected.
fn editor_cut(
    ctx: &egui::Context,
    text: &mut String,
    id: egui::Id,
    range: Option<egui::text::CCursorRange>,
) -> Option<egui::text::CCursor> {
    let range = range.filter(|r| !r.is_empty())?;
    ctx.copy_text(range.slice_str(text).to_string());
    let new_cursor = egui::TextBuffer::delete_selected(text, &range);
    set_editor_cursor(ctx, id, egui::text::CCursorRange::one(new_cursor), true);
    clear_selection_cache(ctx);
    Some(new_cursor)
}

/// Requests an OS-clipboard paste for the next frame.
///
/// eframe translates `ViewportCommand::RequestPaste` into an `Event::Paste`,
/// which the focused `TextEdit` then applies with its normal undo/selection
/// handling.
fn editor_paste(ctx: &egui::Context, id: egui::Id) {
    clear_selection_cache(ctx);
    ctx.memory_mut(|memory| memory.request_focus(id));
    ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
}

/// Selects the whole editor buffer.
fn editor_select_all(ctx: &egui::Context, text: &str, id: egui::Id) {
    let selection = egui::text::CCursorRange::select_all_str(text);
    set_editor_cursor(ctx, id, selection, true);
    ctx.data_mut(|d| d.insert_temp(selection_cache_id(), selection));
}

/// Raw Markdown editor body. Returns `true` if the text changed.
///
/// Used inside the Edit split screen (left half). `cursor_key` caches the
/// status-bar line and column per character index and preview revision.
///
/// Keyboard Cmd+X/C/V is handled by egui's `TextEdit`. This function adds a
/// right-click menu and Ctrl+X/C/V fallbacks so clipboard actions also work
/// for users who do not use the macOS Cmd shortcuts.
fn editor_ui(
    ui: &mut Ui,
    text: &mut String,
    font_size: f32,
    last_cursor: &mut (usize, usize),
    cursor_key: &mut Option<(usize, u64)>,
    revision: u64,
    jump_line: &mut Option<EditorJump>,
) -> bool {
    let ctx = ui.ctx().clone();
    let mut changed = false;
    let jump = jump_line.take();
    ScrollArea::vertical()
        .id_salt("editor_scroll")
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            // Capture the selection *before* the TextEdit processes input for
            // this frame. A secondary (right) click collapses the selection;
            // restoring it keeps the selection visibly highlighted while the
            // context menu is open.
            let editor_id = ui.make_persistent_id(egui::IdSalt::new("md_editor"));
            let menu_pos_id = egui::Id::unique("md_editor_context_menu_pos");
            let menu_open = ctx
                .data(|d| d.get_temp::<egui::Pos2>(menu_pos_id))
                .is_some();
            let secondary_active = ui.input(|i| {
                i.pointer.button_down(egui::PointerButton::Secondary)
                    || i.pointer.button_clicked(egui::PointerButton::Secondary)
            });
            let cached_selection = ctx
                .data(|d| d.get_temp::<egui::text::CCursorRange>(selection_cache_id()))
                .filter(|range| !range.is_empty());

            // Right-clicking normally collapses the TextEdit selection. While
            // the context menu is open (or the right button is active), write
            // the cached selection back *before* the TextEdit is shown, so it
            // is painted highlighted and Cut/Copy stay enabled.
            if (menu_open || secondary_active)
                && let Some(selection) = cached_selection
            {
                set_editor_cursor(&ctx, editor_id, selection, true);
            }

            // A click in the preview requests a jump; move the text cursor
            // before the widget is shown so the selection and caret follow.
            if let Some(EditorJump { line, focus }) = jump {
                let index = line_to_char_index(text, line);
                set_editor_cursor(
                    &ctx,
                    editor_id,
                    egui::text::CCursorRange::one(egui::text::CCursor::new(index)),
                    focus,
                );
            }

            let out = egui::TextEdit::multiline(text)
                .id_salt("md_editor")
                .code_editor()
                .font(FontId::monospace(font_size * 0.9))
                .hint_text("Write Markdown here …")
                .desired_width(ui.available_width())
                .show(ui);

            // Scroll the target line into the middle of the editor pane.
            if jump.is_some()
                && let Some(range) = out.cursor_range
            {
                let cursor_rect = out
                    .galley
                    .pos_from_cursor(range.primary)
                    .translate(out.galley_pos.to_vec2());
                ui.scroll_to_rect(cursor_rect, Some(Align::Center));
            }
            if out.response.response.changed() {
                changed = true;
                clear_selection_cache(&ctx);
            }
            if let Some(range) = &out.cursor_range {
                let index = range.primary.index.0;
                if changed || *cursor_key != Some((index, revision)) {
                    *cursor_key = Some((index, revision));
                    *last_cursor = char_to_line_col(text, index);
                }
            }

            let id = out.response.response.id;
            let secondary_down =
                ui.input(|i| i.pointer.button_down(egui::PointerButton::Secondary));
            let secondary_click = out.response.response.secondary_clicked();
            let mut range = out.cursor_range;

            // Keep a real selection so it can be restored while the context
            // menu is open. An empty live selection (caret moved by keys or
            // clicks) drops it, unless a right-click or the menu is in progress.
            match out.cursor_range.filter(|r| !r.is_empty()) {
                Some(selection) if !menu_open && !secondary_down => {
                    ctx.data_mut(|d| d.insert_temp(selection_cache_id(), selection));
                }
                None if !menu_open && !secondary_down && !secondary_click => {
                    clear_selection_cache(&ctx);
                }
                _ => {}
            }

            if (menu_open || secondary_down || secondary_click)
                && let Some(selection) = cached_selection
            {
                range = Some(selection);
            }

            let menu_has_selection = range.is_some_and(|r| !r.is_empty());
            let mut action = None;

            // Lightweight context menu. Using an `Area` instead of a popup
            // keeps keyboard focus in the TextEdit, so the selection stays
            // visibly highlighted behind the menu.
            let mut menu_pos = ctx.data(|d| d.get_temp::<egui::Pos2>(menu_pos_id));
            if secondary_click && let Some(pos) = ui.ctx().pointer_interact_pos() {
                menu_pos = Some(pos);
                ctx.data_mut(|d| d.insert_temp(menu_pos_id, pos));
            }

            let mut close_menu = false;
            if let Some(pos) = menu_pos {
                let screen = ctx.content_rect();
                let pos = egui::pos2(
                    pos.x
                        .clamp(screen.min.x, (screen.max.x - 190.0).max(screen.min.x)),
                    pos.y
                        .clamp(screen.min.y, (screen.max.y - 180.0).max(screen.min.y)),
                );
                let area = egui::Area::new(egui::Id::unique("md_editor_context_menu"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(pos)
                    .show(&ctx, |ui| {
                        Frame::popup(ui.style()).show(ui, |ui| {
                            ui.set_min_width(160.0);
                            if ui
                                .add_enabled(menu_has_selection, egui::Button::new("Cut   ⌘X"))
                                .clicked()
                            {
                                action = Some(EditorClipboardAction::Cut);
                                close_menu = true;
                            }
                            if ui
                                .add_enabled(menu_has_selection, egui::Button::new("Copy   ⌘C"))
                                .clicked()
                            {
                                action = Some(EditorClipboardAction::Copy);
                                close_menu = true;
                            }
                            if ui.button("Paste   ⌘V").clicked() {
                                action = Some(EditorClipboardAction::Paste);
                                close_menu = true;
                            }
                            ui.separator();
                            if ui.button("Select All   ⌘A").clicked() {
                                action = Some(EditorClipboardAction::SelectAll);
                                close_menu = true;
                            }
                        });
                    });

                if ctx.input(|i| {
                    i.pointer.button_clicked(egui::PointerButton::Primary)
                        && !area.response.hovered()
                }) || ctx.input(|i| i.key_pressed(egui::Key::Escape))
                    || !ctx.input(|i| i.focused)
                {
                    close_menu = true;
                }

                // Keep the editor's selection painted while the menu is open.
                ctx.memory_mut(|memory| memory.request_focus(id));
            }

            if close_menu {
                ctx.data_mut(|d| d.remove::<egui::Pos2>(menu_pos_id));
            }

            // Ctrl fallbacks: egui-winit maps only Cmd on macOS.
            let (copy, cut, paste, select_all) = ui.input(|i| {
                let ctrl = i.modifiers.ctrl && !i.modifiers.command;
                (
                    ctrl && i.key_pressed(egui::Key::C),
                    ctrl && i.key_pressed(egui::Key::X),
                    ctrl && i.key_pressed(egui::Key::V),
                    ctrl && i.key_pressed(egui::Key::A),
                )
            });
            if copy {
                action = Some(EditorClipboardAction::Copy);
            } else if cut {
                action = Some(EditorClipboardAction::Cut);
            } else if paste {
                action = Some(EditorClipboardAction::Paste);
            } else if select_all {
                action = Some(EditorClipboardAction::SelectAll);
            }

            match action {
                Some(EditorClipboardAction::Copy) => {
                    editor_copy(&ctx, text, range);
                }
                Some(EditorClipboardAction::Cut) => {
                    if let Some(cursor) = editor_cut(&ctx, text, id, range) {
                        *last_cursor = char_to_line_col(text, cursor.index.0);
                        changed = true;
                    }
                }
                Some(EditorClipboardAction::Paste) => editor_paste(&ctx, id),
                Some(EditorClipboardAction::SelectAll) => {
                    editor_select_all(&ctx, text, id);
                }
                None => {}
            }
        });
    changed
}

/// Rendered Markdown body (scroll area + centered reading column).
///
/// Used by the full-window Preview and by the right half of the Edit split.
/// A click on an inline image is written to `image_click`; the caller opens
/// the modal viewer after rendering (this function only borrows the document).
/// Returns the source line of a click on a block; only Edit mode acts on it.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors the render pipeline; each argument is a distinct input"
)]
fn preview_ui(
    ui: &mut Ui,
    doc: &Document,
    f: f32,
    base_dir: Option<&Path>,
    full_width: bool,
    image_click: &mut Option<ImageRequest>,
    math_cache: &mut MathCache,
    preview_line: &mut Option<usize>,
    preview_top_line: &mut usize,
    metrics: &mut PreviewMetrics,
    revision: u64,
) -> Option<usize> {
    let mut hits: Vec<SourceHit> = Vec::new();
    let virtualize = doc.blocks.len() > VIRTUALIZE_THRESHOLD;

    let output = ScrollArea::vertical()
        .id_salt("preview_scroll")
        .auto_shrink([false; 2])
        .show_viewport(ui, |ui, viewport| {
            ui.add_space(24.0);
            preview_column(ui, f, full_width, |ui| {
                if virtualize {
                    let width = ui.available_width().max(1.0);
                    let key = PreviewKey {
                        revision,
                        width: width.round() as u32,
                        font: f.to_bits(),
                        theme: theme::mode() as u8,
                    };
                    if metrics.key != key || metrics.heights.len() != doc.blocks.len() {
                        metrics.key = key;
                        metrics.heights = doc
                            .blocks
                            .iter()
                            .map(|block| estimate_block_height(block, f, width))
                            .collect();
                    }
                }

                let before_front = ui.cursor().top();
                render_front_matter(ui, doc, f);
                let front_height = (ui.cursor().top() - before_front).max(0.0);
                let mut y = 24.0 + front_height;

                // Scroll to the block containing the requested source line. The
                // virtual path needs the offset up front; the full path scrolls
                // to the rendered block after the loop.
                let target = preview_line.take();
                if let Some(target) = target.filter(|_| virtualize) {
                    let index = preview_index_for_line(&doc.block_lines, target)
                        .min(metrics.heights.len() - 1);
                    let offset = 24.0 + front_height + metrics.heights[..index].iter().sum::<f32>();
                    let height = metrics.heights[index];
                    let screen_y = ui.clip_rect().top() + (offset - viewport.min.y);
                    ui.scroll_to_rect(
                        Rect::from_min_size(
                            egui::pos2(ui.clip_rect().left(), screen_y),
                            Vec2::new(1.0, height),
                        ),
                        Some(Align::Min),
                    );
                }

                for (index, block) in doc.blocks.iter().enumerate() {
                    if virtualize {
                        let height = metrics.heights[index];
                        if y > viewport.max.y + PREVIEW_OVERSCAN {
                            ui.add_space(metrics.heights[index..].iter().sum());
                            break;
                        }
                        if y + height < viewport.min.y - PREVIEW_OVERSCAN {
                            ui.add_space(height);
                            y += height;
                            continue;
                        }
                    }
                    let rect =
                        render_top_level(ui, index, block, f, base_dir, image_click, math_cache);
                    if virtualize {
                        metrics.heights[index] = rect.height();
                        y += rect.height();
                    }
                    hits.push((rect, doc.block_lines.get(index).copied().unwrap_or(1)));
                }

                // Mode switch Editor → Preview: scroll to the block that
                // contains the editor caret line (the last block starting at
                // or before it).
                if let Some(target) = target.filter(|_| !virtualize)
                    && let Some((rect, _)) = block_at_line(&hits, target)
                {
                    ui.scroll_to_rect(rect, Some(Align::Min));
                }
                render_footnotes(ui, doc, f, base_dir, image_click, math_cache);
                ui.add_space(72.0);
            });
        });

    if let Some(line) = top_visible_line(&hits, output.inner_rect) {
        *preview_top_line = line;
    }

    // Click in the preview → the source line the caller should jump to.
    if ui.input(|i| i.pointer.primary_clicked())
        && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
        && let Some(line) = source_line_at(&hits, pos)
    {
        ui.ctx().request_repaint();
        return Some(line);
    }
    None
}

/// Index of the top-level block containing source line `target`.
fn preview_index_for_line(lines: &[usize], target: usize) -> usize {
    lines
        .iter()
        .rposition(|line| *line <= target)
        .unwrap_or(0)
        .min(lines.len().saturating_sub(1))
}

/// Rough starting height for a block before it has been measured. Only used to
/// size the scroll bar of very large documents; visible blocks are measured
/// precisely after rendering.
fn estimate_block_height(block: &Block, f: f32, width: f32) -> f32 {
    match block {
        Block::Heading { .. } => f * 1.8 + 18.0,
        Block::Paragraph(_) => {
            let chars_per_line = (width / (f * 0.55)).max(8.0);
            let text = block.summarize();
            let lines = (text.chars().count() as f32 / chars_per_line)
                .ceil()
                .max(1.0);
            lines * f * 1.5 + 10.0
        }
        Block::CodeBlock { code, .. } => (code.lines().count().max(1) as f32) * (f * 1.35) + 22.0,
        Block::List { items, .. } => (items.len().max(1) as f32) * (f * 1.8) + 10.0,
        Block::Table { rows, .. } => ((rows.len() + 1) as f32) * (f * 2.0) + 16.0,
        Block::BlockQuote { blocks, .. }
        | Block::Group(blocks)
        | Block::FencedDiv { blocks, .. } => {
            blocks
                .iter()
                .map(|block| estimate_block_height(block, f, width))
                .sum::<f32>()
                + 20.0
        }
        Block::Details { .. } => f * 2.0 + 20.0,
        Block::Media { .. } => 70.0,
        Block::Toc(entries) => (entries.len().max(1) as f32) * (f * 1.6) + 30.0,
        Block::DefinitionList(items) => (items.len().max(1) as f32) * (f * 2.2) + 10.0,
        Block::Math(_) => 90.0,
        Block::Mermaid(_) => 180.0,
        Block::Image { .. } => 160.0,
        Block::Html(src) => (src.lines().count().max(1) as f32) * (f * 1.3) + 20.0,
        Block::Rule => 24.0,
    }
}

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

/// Applies the active theme (dark or Paper) to both egui style variants.
///
/// Colors live in the `theme` module; this function wires them into egui's
/// `Visuals` and text styles. Called at startup and after a theme toggle.
fn apply_theme(ctx: &egui::Context) {
    let p = palette();
    ctx.all_styles_mut(|style| {
        style.visuals = {
            let mut v = match theme::mode() {
                theme::Mode::Dark => egui::Visuals::dark(),
                theme::Mode::Paper => egui::Visuals::light(),
            };
            v.panel_fill = p.bg1;
            v.window_fill = p.bg2;
            v.extreme_bg_color = p.bg2;
            v.faint_bg_color = p.bg2;
            v.override_text_color = Some(p.text);
            v.hyperlink_color = p.accent;
            v.selection.bg_fill =
                Color32::from_rgba_unmultiplied(p.accent.r(), p.accent.g(), p.accent.b(), 48);
            v.selection.stroke = Stroke::new(1.0, p.accent);
            v.window_stroke = Stroke::new(1.0, p.stroke);
            v.window_corner_radius = CornerRadius::same(10);
            v.widgets.noninteractive.bg_fill = p.bg2;
            v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.stroke);
            v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
            v.widgets.inactive.bg_fill = p.bg2;
            v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.stroke);
            v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text);
            v.widgets.hovered.bg_fill = p.bg3;
            v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.faint);
            v.widgets.hovered.fg_stroke = Stroke::new(1.2, p.text);
            v.widgets.active.bg_fill = p.bg_active;
            v.widgets.active.fg_stroke = Stroke::new(1.2, p.accent);
            v.widgets.open.bg_fill = p.bg3;
            v.widgets.open.fg_stroke = Stroke::new(1.2, p.text);
            // Text cursor/selection color for the raw editor.
            v.text_cursor.stroke = Stroke::new(1.5, p.accent);
            v
        };
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(14.0));
        style
            .text_styles
            .insert(TextStyle::Monospace, FontId::monospace(13.0));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(14.0));
        style
            .text_styles
            .insert(TextStyle::Heading, FontId::proportional(18.0));
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(11.0));
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(10.0, 5.0);
        style.spacing.indent = 20.0;
        // egui enables the "Unaligned" debug overlay by default in debug
        // builds. It paints orange lines over fractional-pixel rects, which
        // looks like a rendering bug in normal use. (The field only exists
        // when debug assertions are enabled.)
        #[cfg(debug_assertions)]
        {
            style.debug.show_unaligned = false;
        }
    });
}

// ---------------------------------------------------------------------------
// Preview renderer
// ---------------------------------------------------------------------------

/// Lays out the reading column: centered, roughly 820 px wide.
///
/// The width scales slightly with the font size so zooming keeps a pleasant
/// line length.
fn preview_column(ui: &mut Ui, f: f32, full_width: bool, add: impl FnOnce(&mut Ui)) {
    // Full preview: keep a proportional side margin (never collapse to zero)
    // while capping the line length. Split panes are already inset by their
    // frame margins and use their available width.
    let max_w = (f * 62.0).clamp(640.0, 1080.0);
    let avail = ui.available_width();
    let (width, left) = if full_width {
        let pad = 80.0_f32.min(avail * 0.15);
        let width = (avail - 2.0 * pad).min(max_w).max(0.0);
        (width, ((avail - width) * 0.5).max(0.0))
    } else {
        let width = avail.min(max_w);
        (width, ((avail - width) * 0.5).max(0.0))
    };
    ui.horizontal(|ui| {
        ui.add_space(left);
        ui.vertical(|ui| {
            ui.set_width(width);
            // Hard-clamp the reading column: a single non-wrapping child (a
            // wide Grid, table or image) must never widen every following
            // block and push the text under the right window edge.
            ui.set_max_width(width);
            ui.spacing_mut().item_spacing.y = f * 0.35;
            add(ui);
        });
    });
}

/// A clickable area in the preview mapped to a 1-based Markdown source line.
type SourceHit = (Rect, usize);

/// Finds the source line nearest to a click in the preview.
///
/// Top-level blocks tile the reading column vertically; clicks in the small
/// gaps select the closest block. Clicks outside the reading column (e.g. on
/// the scrollbar) are ignored.
fn source_line_at(hits: &[SourceHit], pos: egui::Pos2) -> Option<usize> {
    let mut nearest: Option<(f32, usize)> = None;
    for &(rect, line) in hits {
        if pos.x < rect.left() || pos.x > rect.right() {
            continue;
        }
        let dy = if pos.y < rect.top() {
            rect.top() - pos.y
        } else if pos.y > rect.bottom() {
            pos.y - rect.bottom()
        } else {
            return Some(line);
        };
        if nearest.is_none_or(|(best, _)| dy < best) {
            nearest = Some((dy, line));
        }
    }
    nearest.map(|(_, line)| line)
}

/// Source line of the first top-level block visible in `viewport`.
fn top_visible_line(hits: &[SourceHit], viewport: Rect) -> Option<usize> {
    hits.iter()
        .find(|(rect, _)| rect.bottom() > viewport.top() + 1.0)
        .map(|(_, line)| *line)
        .or_else(|| hits.last().map(|(_, line)| *line))
}

/// Returns the top-level block that contains `target` (the last block whose
/// source line is at or before the target), falling back to the first block.
fn block_at_line(hits: &[SourceHit], target: usize) -> Option<SourceHit> {
    hits.iter()
        .filter(|(_, line)| *line <= target)
        .max_by_key(|(_, line)| *line)
        .or_else(|| hits.first())
        .copied()
}

/// Renders one top-level block in its own id scope and returns the rectangle
/// it occupies. Both preview paths use it, so the id and the measurement stay
/// identical whether or not the document is virtualized.
fn render_top_level(
    ui: &mut Ui,
    index: usize,
    block: &Block,
    f: f32,
    base_dir: Option<&Path>,
    image_click: &mut Option<ImageRequest>,
    math_cache: &mut MathCache,
) -> Rect {
    let top = ui.cursor().top();
    let x0 = ui.cursor().left();
    let width = ui.available_width();
    ui.push_id(("preview_block", index), |ui| {
        render_block(ui, block, f, base_dir, image_click, math_cache);
    });
    let height = (ui.cursor().top() - top).max(1.0);
    Rect::from_min_size(egui::pos2(x0, top), Vec2::new(width, height))
}

/// Renders nested blocks (inside containers), each with a stable id scope.
fn render_blocks(
    ui: &mut Ui,
    blocks: &[Block],
    f: f32,
    base_dir: Option<&Path>,
    image_click: &mut Option<ImageRequest>,
    math_cache: &mut MathCache,
) {
    for (i, block) in blocks.iter().enumerate() {
        ui.push_id(("nested_block", i), |ui| {
            render_block(ui, block, f, base_dir, image_click, math_cache);
        });
    }
}

/// Renders a paragraph's inlines as a wrapping run. A paragraph that is only a
/// wiki embed (`![[Note]]`) is rendered as an embedded note block instead.
fn render_paragraph(
    ui: &mut Ui,
    inlines: &[Inline],
    f: f32,
    base_dir: Option<&Path>,
    image_click: &mut Option<ImageRequest>,
    math_cache: &mut MathCache,
) {
    if let Some(path) = standalone_transclusion(inlines, base_dir) {
        render_transclusion(ui, &path, f, image_click, math_cache);
        return;
    }
    ui.horizontal_wrapped(|ui| {
        // Spaces are part of the inline stream; no extra widget gap.
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.spacing_mut().item_spacing.y = f * 0.35;
        render_inlines(ui, inlines, f, base_dir, image_click, math_cache);
    });
}

/// Renders one block. This is the on-screen counterpart of the print HTML
/// rendering in `html_export.rs` and must handle every [`Block`] variant.
fn render_block(
    ui: &mut Ui,
    block: &Block,
    f: f32,
    base_dir: Option<&Path>,
    image_click: &mut Option<ImageRequest>,
    math_cache: &mut MathCache,
) {
    match block {
        Block::Heading { level, inlines } => {
            let (size, top, bottom) = match level {
                1 => (f * 2.0, 30.0, 12.0),
                2 => (f * 1.55, 24.0, 9.0),
                3 => (f * 1.28, 20.0, 7.0),
                4 => (f * 1.1, 16.0, 5.0),
                5 => (f, 13.0, 4.0),
                _ => (f * 0.95, 11.0, 4.0),
            };
            ui.add_space(top);
            // Same inline path as paragraphs, so links, code and find-matches
            // inside a heading are kept and highlighted.
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let bold = InlineStyle {
                    bold: true,
                    ..InlineStyle::default()
                };
                render_inlines_styled(
                    ui,
                    visual_inlines(inlines).as_ref(),
                    size,
                    bold,
                    base_dir,
                    image_click,
                    math_cache,
                );
            });
            // Atx/setext headings are not underlined in the reference layout.
            // The only horizontal rules in a document are explicit `---`
            // blocks (Block::Rule), which are rendered separately.
            ui.add_space(bottom);
        }

        Block::Paragraph(inlines) => {
            render_paragraph(ui, inlines, f, base_dir, image_click, math_cache);
            ui.add_space(f * 0.35);
        }

        Block::Image { image, center } => {
            let mut add = |ui: &mut Ui| {
                render_inlines(
                    ui,
                    std::slice::from_ref(image),
                    f,
                    base_dir,
                    image_click,
                    math_cache,
                );
            };
            if *center {
                // A centered image must not be wrapped in `horizontal_wrapped`:
                // that layout always starts at the left edge.
                ui.vertical_centered(|ui| add(ui));
            } else {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.spacing_mut().item_spacing.y = f * 0.35;
                    add(ui);
                });
            }
            ui.add_space(f * 0.35);
        }

        Block::Math(tex) => {
            ui.add_space(f * 0.45);
            ui.vertical_centered(|ui| {
                render_math(ui, tex, f * 1.18, true, math_cache);
            });
            ui.add_space(f * 0.55);
        }

        Block::CodeBlock { lang, code } => render_code(ui, lang.as_deref(), code, f),

        Block::Mermaid(code) => render_mermaid(ui, code, f),

        Block::Details {
            summary,
            open,
            blocks,
        } => {
            let id = ui.scope_id().with("details_open");
            let mut expanded = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(*open);
            card_frame(Margin {
                left: 14,
                right: 14,
                top: 10,
                bottom: 12,
            })
            .stroke(Stroke::NONE)
            .show(ui, |ui| {
                let heading = bold_font(f * 0.92);
                if disclosure_button(ui, expanded, summary, heading, palette().text) {
                    expanded = !expanded;
                    ui.data_mut(|d| d.insert_temp(id, expanded));
                }
                if expanded {
                    ui.add_space(4.0);
                    render_blocks(ui, blocks, f, base_dir, image_click, math_cache);
                }
            });
            ui.add_space(f * 0.5);
        }

        Block::Media { kind, url } => {
            let response = card_frame(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(media_icon(kind)).size(f));
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(kind)
                                    .font(bold_font(f * 0.85))
                                    .color(palette().text),
                            );
                            ui.label(
                                RichText::new(url)
                                    .size(f * 0.82)
                                    .color(palette().accent)
                                    .underline(),
                            );
                        });
                    });
                })
                .response;
            let response = ui.interact(
                response.rect,
                ui.scope_id().with("media"),
                egui::Sense::click(),
            );
            if response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if response.clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
            }
            response.on_hover_text("Open media");
        }

        Block::Toc(entries) => {
            ui.add_space(f * 0.6);
            ui.label(
                RichText::new("Contents")
                    .font(bold_font(f))
                    .color(palette().text),
            );
            if entries.is_empty() {
                ui.label(RichText::new("No headings found.").color(palette().muted));
            }
            for (level, title) in entries {
                ui.horizontal(|ui| {
                    ui.add_space((*level as f32 - 1.0).max(0.0) * 16.0);
                    ui.label(
                        RichText::new(format!("• {title}"))
                            .size(f * 0.95)
                            .color(palette().accent),
                    );
                });
            }
            ui.add_space(f * 0.6);
        }

        Block::FencedDiv { class, blocks } => {
            let label = fenced_div_label(class);
            let color = fenced_div_color(&label);
            ui.add_space(f * 0.4);
            card_frame(Margin {
                left: 14,
                right: 14,
                top: 10,
                bottom: 12,
            })
            .stroke(Stroke::new(1.0, color))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.label(
                    RichText::new(label.to_uppercase())
                        .font(bold_font(f * 0.8))
                        .color(color),
                );
                ui.add_space(4.0);
                render_blocks(ui, blocks, f, base_dir, image_click, math_cache);
            });
            ui.add_space(f * 0.5);
        }

        Block::DefinitionList(items) => {
            for item in items {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    render_inlines(ui, &item.term, f, base_dir, image_click, math_cache);
                });
                for def in &item.definitions {
                    ui.horizontal(|ui| {
                        ui.add_space(18.0);
                        ui.vertical(|ui| {
                            render_blocks(ui, def, f, base_dir, image_click, math_cache);
                        });
                    });
                }
            }
            ui.add_space(f * 0.3);
        }

        Block::Group(blocks) => render_blocks(ui, blocks, f, base_dir, image_click, math_cache),

        Block::Html(src) => render_code(ui, Some("html — sanitized"), src, f),

        Block::Rule => {
            ui.add_space(f * 0.7);
            hairline(ui);
            ui.add_space(f * 0.7);
        }

        Block::BlockQuote {
            kind,
            title,
            fold,
            blocks,
        } => {
            let (active, icon, label): (Color32, &str, &str) = match kind {
                Some(AlertKind::Note) => (palette().accent, "ⓘ", "Note"),
                Some(AlertKind::Tip) => (palette().green, "💡", "Tip"),
                Some(AlertKind::Important) => (palette().purple, "❗", "Important"),
                Some(AlertKind::Warning) => (palette().orange, "⚠", "Warning"),
                Some(AlertKind::Caution) => (palette().red, "⚠", "Caution"),
                Some(AlertKind::Question) => (palette().accent, "❓", "Question"),
                Some(AlertKind::Success) => (palette().green, "✅", "Success"),
                Some(AlertKind::Failure) => (palette().red, "❌", "Failure"),
                Some(AlertKind::Danger) => (palette().red, "⚡", "Danger"),
                Some(AlertKind::Bug) => (palette().orange, "🐛", "Bug"),
                Some(AlertKind::Example) => (palette().purple, "📋", "Example"),
                Some(AlertKind::Quote) => (palette().faint, "❝", "Quote"),
                None => (palette().faint, "", ""),
            };
            let heading = match (kind.is_some(), title) {
                (true, Some(t)) if !t.trim().is_empty() => t.trim().to_string(),
                (true, _) => label.to_string(),
                (false, _) => String::new(),
            };
            let heading = if icon.is_empty() {
                heading
            } else {
                format!("{icon}  {heading}")
            };
            // Obsidian fold markers (`-` collapsed, `+` expanded) make the
            // callout collapsible. Start expanded so the body is visible even
            // for `-` callouts (browser renderers usually ignore the marker);
            // the arrow still lets the user collapse it.
            let open_id = ui.scope_id().with("callout_open");
            let mut open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(true);
            let inner = card_frame(Margin {
                left: 18,
                right: 16,
                top: 12,
                bottom: 12,
            })
            .stroke(Stroke::NONE)
            .show(ui, |ui| {
                // Fill the reading column instead of shrinking to the text.
                ui.set_min_width(ui.available_width());
                if kind.is_some() {
                    if fold.is_some() {
                        // The fold marker makes the heading a toggle; the
                        // arrow shows the current collapsed/expanded state.
                        if disclosure_button(ui, open, &heading, bold_font(f * 0.92), active) {
                            open = !open;
                            ui.data_mut(|d| d.insert_temp(open_id, open));
                        }
                    } else {
                        ui.label(
                            RichText::new(heading)
                                .font(bold_font(f * 0.92))
                                .color(active),
                        );
                    }
                    if open {
                        ui.add_space(4.0);
                    }
                }
                if open {
                    render_blocks(ui, blocks, f, base_dir, image_click, math_cache);
                }
            });
            // Left accent bar inside the frame.
            let r = inner.response.rect;
            let bar_h = (r.height() - 20.0).max(0.0);
            ui.painter().rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(r.min.x + 6.0, r.min.y + 10.0),
                    Vec2::new(3.0, bar_h),
                ),
                CornerRadius::same(2),
                active,
            );
            ui.add_space(f * 0.4);
        }

        Block::List {
            ordered,
            start,
            tight,
            items,
        } => {
            let mut n = *start;
            for item in items {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    // Fixed marker column so text aligns across items.
                    ui.allocate_ui_with_layout(
                        Vec2::new(26.0, f * 1.5),
                        Layout::right_to_left(Align::TOP),
                        |ui| match (ordered, item.task) {
                            (true, _) => {
                                let text = format!("{n}.");
                                n += 1;
                                ui.label(RichText::new(text).size(f * 0.95).color(palette().muted));
                            }
                            (false, Some(checked)) => {
                                let mut b = checked;
                                ui.add_enabled(false, Checkbox::new(&mut b, ""));
                            }
                            (false, None) => {
                                ui.label(RichText::new("•").size(f * 1.1).color(palette().accent));
                            }
                        },
                    );
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = if *tight { 0.0 } else { f * 0.25 };
                        for b in &item.blocks {
                            // Tight list items are single paragraphs without
                            // block spacing; render them as a plain inline run
                            // so consecutive items sit on consecutive lines.
                            if *tight && let Block::Paragraph(inlines) = b {
                                render_paragraph(ui, inlines, f, base_dir, image_click, math_cache);
                            } else {
                                render_block(ui, b, f, base_dir, image_click, math_cache);
                            }
                        }
                    });
                });
                if !*tight {
                    ui.add_space(f * 0.15);
                }
            }
            ui.add_space(f * 0.2);
        }

        Block::Table { aligns, head, rows } => {
            render_table(ui, aligns, head, rows, f, base_dir, image_click, math_cache);
            ui.add_space(f * 0.5);
        }
    }
}

// ---------------------------------------------------------------------------
// GFM table renderer
// ---------------------------------------------------------------------------

/// Horizontal padding inside a table cell (left and right), in points.
const TABLE_CELL_PAD_X: f32 = 10.0;
/// Vertical padding inside a table cell (top and bottom), in points.
const TABLE_CELL_PAD_Y: f32 = 6.0;
/// Minimum width of a table column, in points.
const TABLE_MIN_COL_W: f32 = 48.0;

/// Renders a GFM table with content-sized columns and wrapping cells.
///
/// `egui::Grid` is not a good fit for Markdown tables: it derives column
/// widths from the *unwrapped* content and, unless `max_col_width` is set,
/// forces labels into [`egui::TextWrapMode::Extend`]. Cells inside a
/// horizontal `Frame` therefore never wrapped and the table grew past the
/// reading column. This renderer instead:
///
/// 1. measures the natural (single-line) width of every column,
/// 2. distributes the available width across the columns (growing a narrow
///    table to the full column width, shrinking wide tables proportionally,
///    never below [`TABLE_MIN_COL_W`]),
/// 3. lays out every row with the final column widths so labels wrap, and
/// 4. sizes each row to the tallest wrapped cell.
///
/// Inline markup inside cells is preserved through a [`LayoutJob`]
/// (bold/italic/code/strikethrough/highlight/footnotes). Cells with links,
/// images or formulas are laid out with the widget-based inline renderer
/// instead, so the links stay clickable.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors render_block; each argument is a distinct rendering input"
)]
fn render_table(
    ui: &mut Ui,
    aligns: &[TableAlign],
    head: &[Vec<TableCell>],
    rows: &[Vec<TableCell>],
    f: f32,
    base_dir: Option<&Path>,
    image_click: &mut Option<ImageRequest>,
    math_cache: &mut MathCache,
) {
    let header_rows = head.len();
    let all_rows: Vec<&Vec<TableCell>> = head.iter().chain(rows.iter()).collect();
    let nrows = all_rows.len();
    if nrows == 0 {
        return;
    }
    // HTML cells may span columns/rows; GFM cells are always 1×1.
    let ncols = all_rows
        .iter()
        .flat_map(|r| r.iter())
        .map(|c| c.col + c.colspan.max(1))
        .max()
        .unwrap_or(1)
        .max(1);
    let avail = ui.available_width();
    if avail <= 1.0 {
        return;
    }

    // 1. Natural width per column: widest 1×1 cell on a single line + padding.
    let mut natural = vec![TABLE_MIN_COL_W; ncols];
    for (ri, row) in all_rows.iter().enumerate() {
        let header = ri < header_rows;
        for cell in row.iter() {
            if cell.colspan == 1 {
                natural[cell.col] = natural[cell.col].max(
                    measure_table_cell(ui, &cell.inlines, f, header) + 2.0 * TABLE_CELL_PAD_X + 1.0,
                );
            }
        }
    }
    // Spanning cells distribute their width requirement across the columns
    // they cover (only when the spanned columns are too narrow).
    for (ri, row) in all_rows.iter().enumerate() {
        let header = ri < header_rows;
        for cell in row.iter() {
            if cell.colspan <= 1 {
                continue;
            }
            let end = (cell.col + cell.colspan).min(ncols);
            if end <= cell.col {
                continue;
            }
            let needed =
                measure_table_cell(ui, &cell.inlines, f, header) + 2.0 * TABLE_CELL_PAD_X + 1.0;
            let sum: f32 = natural[cell.col..end].iter().sum();
            if needed > sum {
                let add = (needed - sum) / (end - cell.col) as f32;
                for w in &mut natural[cell.col..end] {
                    *w += add;
                }
            }
        }
    }
    let widths = fit_table_columns(&natural, avail);

    // 2. Column positions (the last column absorbs sub-pixel rounding).
    let left = ui.cursor().left();
    let table_top = ui.cursor().top();
    let mut col_x = Vec::with_capacity(ncols + 1);
    col_x.push(left);
    for &w in &widths {
        col_x.push(col_x.last().copied().unwrap_or(left) + w);
    }
    col_x[ncols] = left + avail;

    // 3. Row heights: start from 1×1 cells, then distribute the height of
    // row-spanning cells over the rows they cover.
    let mut heights = vec![f * 1.6; nrows];
    for (ri, row) in all_rows.iter().enumerate() {
        let header = ri < header_rows;
        for cell in row.iter().filter(|c| c.rowspan == 1) {
            let end = (cell.col + cell.colspan).min(ncols);
            let w = (col_x[end] - col_x[cell.col] - 2.0 * TABLE_CELL_PAD_X).max(8.0);
            let h = table_cell_height(ui, &cell.inlines, f, header, w) + 2.0 * TABLE_CELL_PAD_Y;
            heights[ri] = heights[ri].max(h);
        }
    }
    for (ri, row) in all_rows.iter().enumerate() {
        let header = ri < header_rows;
        for cell in row.iter().filter(|c| c.rowspan > 1) {
            let end_row = (ri + cell.rowspan).min(nrows);
            let end_col = (cell.col + cell.colspan).min(ncols);
            if end_row <= ri || end_col <= cell.col {
                continue;
            }
            let w = (col_x[end_col] - col_x[cell.col] - 2.0 * TABLE_CELL_PAD_X).max(8.0);
            let needed =
                table_cell_height(ui, &cell.inlines, f, header, w) + 2.0 * TABLE_CELL_PAD_Y;
            let sum: f32 = heights[ri..end_row].iter().sum();
            if needed > sum {
                let add = (needed - sum) / (end_row - ri) as f32;
                for h in &mut heights[ri..end_row] {
                    *h += add;
                }
            }
        }
    }

    let mut row_y = Vec::with_capacity(nrows + 1);
    row_y.push(table_top);
    for &h in &heights {
        row_y.push(row_y.last().copied().unwrap_or(table_top) + h);
    }
    let table_bottom = row_y[nrows];

    // Which cell owns each grid slot? Used to suppress grid lines that run
    // through a merged (colspan/rowspan) cell.
    let mut owner = vec![vec![None::<(usize, usize)>; ncols]; nrows];
    for (ri, row) in all_rows.iter().enumerate() {
        for cell in row.iter() {
            let end_row = (ri + cell.rowspan).min(nrows);
            let end_col = (cell.col + cell.colspan).min(ncols);
            for slots in owner.iter_mut().take(end_row).skip(ri) {
                for slot in slots.iter_mut().take(end_col).skip(cell.col) {
                    *slot = Some((ri, cell.col));
                }
            }
        }
    }

    // 4. Row fills (painted before the cell content).
    let saved_spacing = ui.spacing().item_spacing;
    ui.spacing_mut().item_spacing = Vec2::ZERO;
    for ri in 0..nrows {
        let fill = if ri < header_rows {
            palette().bg3
        } else if (ri - header_rows) % 2 == 1 {
            palette().bg2
        } else {
            palette().bg1
        };
        ui.painter().rect_filled(
            Rect::from_min_max(
                egui::pos2(left, row_y[ri]),
                egui::pos2(left + avail, row_y[ri + 1]),
            ),
            CornerRadius::ZERO,
            fill,
        );
    }

    // 5. Cell content. Merged cells get the full spanned rectangle; content is
    // centred vertically inside it.
    for (ri, row) in all_rows.iter().enumerate() {
        let header = ri < header_rows;
        for cell in row.iter() {
            let end_row = (ri + cell.rowspan).min(nrows);
            let end_col = (cell.col + cell.colspan).min(ncols);
            if end_row <= ri || end_col <= cell.col {
                continue;
            }
            let cell_rect = Rect::from_min_max(
                egui::pos2(col_x[cell.col], row_y[ri]),
                egui::pos2(col_x[end_col], row_y[end_row]),
            );
            let inner = cell_rect.shrink2(Vec2::new(TABLE_CELL_PAD_X, TABLE_CELL_PAD_Y));
            if inner.width() <= 1.0 || inner.height() <= 1.0 {
                continue;
            }
            let align = table_align(aligns.get(cell.col).copied());
            let content_h = table_cell_height(ui, &cell.inlines, f, header, inner.width());
            let dy = ((inner.height() - content_h) / 2.0).max(0.0);
            let content_rect = Rect::from_min_max(
                egui::pos2(inner.left(), inner.top() + dy),
                egui::pos2(inner.right(), inner.bottom()),
            )
            .round_ui();
            let inner_w = content_rect.width();
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(content_rect)
                    .layout(Layout::top_down(align)),
                |ui| {
                    ui.set_max_width(inner_w);
                    let needs_widgets = cell_needs_widgets(&cell.inlines);
                    if !needs_widgets && plain_text(&cell.inlines).trim().is_empty() {
                        return;
                    }
                    if needs_widgets {
                        // Widget-based fallback (images/formulas/links):
                        // an inline flow, left aligned.
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            render_inlines(ui, &cell.inlines, f, base_dir, image_click, math_cache);
                        });
                    } else {
                        ui.add(Label::new(table_cell_job(&cell.inlines, f, header)).halign(align));
                    }
                },
            );
        }
    }

    // 6. Grid lines. A line is skipped where the same merged cell occupies
    // both sides of the boundary.
    let stroke = Stroke::new(1.0, palette().stroke);
    for c in 1..ncols {
        for r in 0..nrows {
            if owner[r][c - 1] == owner[r][c] {
                continue;
            }
            ui.painter().line_segment(
                [
                    egui::pos2(col_x[c], row_y[r]),
                    egui::pos2(col_x[c], row_y[r + 1]),
                ],
                stroke,
            );
        }
    }
    for r in 1..nrows {
        for c in 0..ncols {
            if owner[r - 1][c] == owner[r][c] {
                continue;
            }
            ui.painter().line_segment(
                [
                    egui::pos2(col_x[c], row_y[r]),
                    egui::pos2(col_x[c + 1], row_y[r]),
                ],
                stroke,
            );
        }
    }
    ui.painter().rect_stroke(
        Rect::from_min_max(
            egui::pos2(left, table_top),
            egui::pos2(left + avail, table_bottom),
        ),
        CornerRadius::ZERO,
        stroke,
        egui::StrokeKind::Inside,
    );

    ui.advance_cursor_after_rect(Rect::from_min_max(
        egui::pos2(left, table_top),
        egui::pos2(left + avail, table_bottom),
    ));
    ui.spacing_mut().item_spacing = saved_spacing;
}

/// Distributes the available width across table columns.
///
/// * Narrow tables grow proportionally so they span the reading column.
/// * Wide tables shrink proportionally, but no column drops below
///   [`TABLE_MIN_COL_W`] while there is room for it.
/// * Extremely narrow windows split the width evenly (text wraps hard).
fn fit_table_columns(natural: &[f32], avail: f32) -> Vec<f32> {
    let n = natural.len();
    if n == 0 {
        return Vec::new();
    }
    let total: f32 = natural.iter().sum();
    let min_total = TABLE_MIN_COL_W * n as f32;

    let mut widths: Vec<f32> = if total <= avail {
        let extra = avail - total;
        let denom = total.max(1.0);
        natural.iter().map(|w| w + extra * (w / denom)).collect()
    } else if avail <= min_total {
        vec![avail / n as f32; n]
    } else {
        // Water-filling: shrink only the widest columns. Narrow columns keep
        // their natural width, so short header words ("Default") are not
        // broken in the middle. `cap` is the width at which the sum of
        // `min(natural, cap)` equals `avail`.
        let mut sorted = natural.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mut remaining = avail;
        let mut cap = avail / n as f32;
        for (i, &w) in sorted.iter().enumerate() {
            let count = n - i;
            if w * count as f32 >= remaining {
                cap = remaining / count as f32;
                break;
            }
            remaining -= w;
        }
        natural.iter().map(|w| w.min(cap)).collect()
    };

    // Normalize so the columns exactly fill `avail` (sub-pixel gaps).
    let sum: f32 = widths.iter().sum();
    if sum > 0.0 {
        let scale = avail / sum;
        for w in &mut widths {
            *w *= scale;
        }
    }
    widths
}

/// Column alignment as an egui cross-axis alignment.
fn table_align(align: Option<TableAlign>) -> Align {
    match align.unwrap_or(TableAlign::None) {
        TableAlign::Center => Align::Center,
        TableAlign::Right => Align::Max,
        TableAlign::Left | TableAlign::None => Align::Min,
    }
}

/// Measures the single-line width of a cell (without wrapping).
fn measure_table_cell(ui: &Ui, cell: &[Inline], f: f32, header: bool) -> f32 {
    if cell.is_empty() {
        return 0.0;
    }
    if cell_needs_widgets(cell) {
        let font = if header {
            bold_font(f * 0.92)
        } else {
            FontId::proportional(f * 0.9)
        };
        let text = plain_text(cell);
        return ui.fonts_mut(|fonts| fonts.layout_no_wrap(text, font, palette().text).size().x);
    }
    let job = table_cell_job(cell, f, header);
    ui.fonts_mut(|fonts| fonts.layout_job(job).size().x)
}

/// Measures the wrapped height of a cell at `width` (used for row heights and
/// vertical centring of merged HTML cells).
fn table_cell_height(ui: &Ui, cell: &[Inline], f: f32, header: bool, width: f32) -> f32 {
    if cell.is_empty() {
        return 0.0;
    }
    let width = width.max(1.0);
    if cell_needs_widgets(cell) {
        let font = if header {
            bold_font(f * 0.92)
        } else {
            FontId::proportional(f * 0.9)
        };
        let text = plain_text(cell);
        return ui.fonts_mut(|fonts| fonts.layout(text, font, palette().text, width).size().y);
    }
    let mut job = table_cell_job(cell, f, header);
    job.wrap.max_width = width;
    ui.fonts_mut(|fonts| fonts.layout_job(job)).size().y
}

/// True if a cell must be rendered as widgets instead of a text layout job.
///
/// Images and formulas need real widgets. Links are routed here too so they
/// stay clickable inside table cells; the trade-off is that such a cell is
/// laid out left-aligned even in a centered/right-aligned column.
fn cell_needs_widgets(inlines: &[Inline]) -> bool {
    inlines.iter().any(|i| match i {
        Inline::Image { .. } | Inline::Math { .. } | Inline::Link { .. } => true,
        Inline::Emph(v)
        | Inline::Strong(v)
        | Inline::Strike(v)
        | Inline::Highlight(v)
        | Inline::Underline(v)
        | Inline::Kbd(v)
        | Inline::Super(v)
        | Inline::Sub(v) => cell_needs_widgets(v),
        _ => false,
    })
}

/// Builds the styled text job for a table cell.
fn table_cell_job(cell: &[Inline], f: f32, header: bool) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let style = TableTextStyle {
        size: if header { f * 0.92 } else { f * 0.9 },
        bold: header,
        italic: false,
        mono: false,
        color: palette().text,
        background: Color32::TRANSPARENT,
        underline: Stroke::NONE,
        strikethrough: Stroke::NONE,
        valign: Align::BOTTOM,
        line_height: Some(f * 1.45),
    };
    let visual = visual_inlines(cell);
    append_table_inlines(&mut job, visual.as_ref(), &style);
    job
}

/// Text style used while flattening inline elements into a [`LayoutJob`].
#[derive(Clone)]
struct TableTextStyle {
    size: f32,
    bold: bool,
    italic: bool,
    mono: bool,
    color: Color32,
    background: Color32,
    underline: Stroke,
    strikethrough: Stroke,
    valign: Align,
    line_height: Option<f32>,
}

impl TableTextStyle {
    fn font(&self) -> FontId {
        if self.mono {
            return FontId::monospace(self.size);
        }
        let family = match (self.bold, self.italic) {
            (true, true) => egui::FontFamily::Name("bold-italic".into()),
            (true, false) => egui::FontFamily::Name("bold".into()),
            (false, true) => egui::FontFamily::Name("italic".into()),
            (false, false) => return FontId::proportional(self.size),
        };
        FontId::new(self.size, family)
    }

    fn push(&self, job: &mut egui::text::LayoutJob, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut format = TextFormat::simple(self.font(), self.color);
        format.background = self.background;
        format.underline = self.underline;
        format.strikethrough = self.strikethrough;
        format.valign = self.valign;
        format.line_height = self.line_height;
        job.append(text, 0.0, format);
    }
}

/// Recursively flattens inline elements into `job`, preserving styling.
fn append_table_inlines(
    job: &mut egui::text::LayoutJob,
    inlines: &[Inline],
    style: &TableTextStyle,
) {
    for inl in inlines {
        match inl {
            Inline::Text(t) => style.push(job, t),
            Inline::Citation(cite) => style.push(job, &format!("[{cite}]")),
            Inline::Abbr { text, .. } => style.push(job, text),
            Inline::Tag(tag) => style.push(job, &format!("#{tag}")),
            Inline::Emph(v) => {
                let mut s = style.clone();
                s.italic = true;
                append_table_inlines(job, v, &s);
            }
            Inline::Strong(v) => {
                let mut s = style.clone();
                s.bold = true;
                append_table_inlines(job, v, &s);
            }
            Inline::Strike(v) => {
                let mut s = style.clone();
                s.color = palette().muted;
                s.strikethrough = Stroke::new(1.0, palette().muted);
                append_table_inlines(job, v, &s);
            }
            Inline::Highlight(v) => {
                let mut s = style.clone();
                s.background = palette().highlight_bg;
                s.color = palette().highlight_text;
                append_table_inlines(job, v, &s);
            }
            Inline::Underline(v) => {
                let mut s = style.clone();
                s.underline = Stroke::new(1.0, palette().text);
                append_table_inlines(job, v, &s);
            }
            Inline::Kbd(v) => {
                let mut s = style.clone();
                s.mono = true;
                s.size = style.size * 0.9;
                s.background = palette().bg3;
                append_table_inlines(job, v, &s);
            }
            Inline::Code(c) => {
                let mut s = style.clone();
                s.mono = true;
                s.size = style.size * 0.95;
                s.background = palette().bg3;
                s.push(job, c);
            }
            // Images, formulas and links are rendered as widgets instead (see
            // `cell_needs_widgets`), so they never reach this flattening.
            Inline::Image { .. } | Inline::Math { .. } | Inline::Link { .. } => {}
            Inline::WikiLink { target, label } => {
                let mut s = style.clone();
                s.color = palette().accent;
                s.underline = Stroke::new(1.0, palette().accent);
                s.push(job, non_empty_or(label, target));
            }
            Inline::WikiEmbed { target, label } => {
                style.push(job, &format!("⧉ {}", non_empty_or(label, target)));
            }
            Inline::FootnoteRef { label, number } => {
                let mut s = style.clone();
                s.size = style.size * 0.75;
                s.color = palette().accent;
                s.valign = Align::TOP;
                let text = if *number > 0 {
                    number.to_string()
                } else {
                    label.clone()
                };
                s.push(job, &text);
            }
            Inline::Super(v) => {
                let mut s = style.clone();
                s.size = style.size * 0.75;
                s.valign = Align::TOP;
                s.line_height = None;
                append_table_inlines(job, v, &s);
            }
            Inline::Sub(v) => {
                let mut s = style.clone();
                s.size = style.size * 0.75;
                s.valign = Align::BOTTOM;
                s.line_height = None;
                append_table_inlines(job, v, &s);
            }
        }
    }
}

/// Renders the footnote section at the end of the preview.
fn render_footnotes(
    ui: &mut Ui,
    doc: &Document,
    f: f32,
    base_dir: Option<&Path>,
    image_click: &mut Option<ImageRequest>,
    math_cache: &mut MathCache,
) {
    if doc.footnotes.is_empty() {
        return;
    }
    ui.add_space(f * 0.8);
    hairline(ui);
    ui.add_space(f * 0.5);
    ui.label(
        RichText::new("Footnotes")
            .font(bold_font(f * 0.8))
            .color(palette().muted),
    );
    ui.add_space(f * 0.25);
    let mut entries: Vec<_> = doc.footnotes.iter().collect();
    entries.sort_by_key(|(label, _)| {
        doc.footnote_numbers
            .get(*label)
            .copied()
            .unwrap_or(usize::MAX)
    });
    for (label, blocks) in entries {
        let number = doc.footnote_numbers.get(label).copied().unwrap_or(0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(
                RichText::new(format!("{number}."))
                    .size(f * 0.82)
                    .color(palette().accent),
            );
            ui.vertical(|ui| {
                ui.label(RichText::new(label).size(f * 0.75).color(palette().faint));
                render_blocks(ui, blocks, f * 0.92, base_dir, image_click, math_cache);
            });
        });
        ui.add_space(f * 0.15);
    }
}

// ---------------------------------------------------------------------------
// Inline renderer
// ---------------------------------------------------------------------------

/// Case-insensitive prefix test that never splits a UTF-8 character.
fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

/// True for `http://` and `https://` URLs.
fn is_remote_url(url: &str) -> bool {
    starts_with_ignore_case(url, "http://") || starts_with_ignore_case(url, "https://")
}

/// True for links that should be handed to the system browser.
fn is_external_url(url: &str) -> bool {
    is_remote_url(url)
        || ["mailto:", "tel:", "data:"]
            .iter()
            .any(|scheme| starts_with_ignore_case(url, scheme))
}

/// Decodes `%20`-style escapes in local link paths.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The file a local link points to, resolved against the document folder.
/// `None` for external URLs, anchors and empty targets. The file need not exist.
fn local_link_path(url: &str, base_dir: Option<&Path>) -> Option<PathBuf> {
    if is_external_url(url) {
        return None;
    }
    let path = url.split('#').next().unwrap_or("");
    let path = path.strip_prefix("file://").unwrap_or(path);
    if path.is_empty() {
        return None;
    }
    let base = base_dir.unwrap_or_else(|| Path::new("."));
    Some(base.join(percent_decode(path)))
}

/// Resolves a local Markdown link (`[x](note.md)`, `[[Note]]`) against the
/// document folder. Returns `None` for external URLs, anchors and files that
/// do not exist.
fn resolve_link_target(url: &str, base_dir: Option<&Path>, wiki: bool) -> Option<PathBuf> {
    let candidate = local_link_path(url, base_dir)?;
    if is_markdown(&candidate) && candidate.is_file() {
        return Some(candidate);
    }
    // Wiki-links (`[[Note]]`) and extension-less links name the note without
    // its file extension. The extension is appended, because `v1.2 notes` is
    // a note name, not a file named `v1` with extension `2 notes`.
    if wiki || candidate.extension().is_none() {
        for ext in MARKDOWN_EXTENSIONS {
            let mut name = candidate.clone().into_os_string();
            name.push(format!(".{ext}"));
            let with_ext = PathBuf::from(name);
            if with_ext.is_file() {
                return Some(with_ext);
            }
        }
    }
    None
}

/// Context key used to hand a clicked local link back to [`App`].
fn pending_open_id() -> egui::Id {
    egui::Id::unique("mdviewer_open_document")
}

/// Queues a local file to be opened after this frame: Markdown in a tab,
/// anything else in the system's default application.
fn queue_open_document(ui: &Ui, path: PathBuf) {
    ui.ctx()
        .data_mut(|data| data.insert_temp(pending_open_id(), path));
}

/// Renders one inline link, opening local files and sending everything else
/// to the system browser.
fn render_link(
    ui: &mut Ui,
    label: &str,
    url: &str,
    f: f32,
    mut style: InlineStyle,
    line_h: Option<f32>,
    base_dir: Option<&Path>,
) {
    style.color = Some(palette().accent);
    style.underline = true;
    let response = ui.add(egui::Link::new(styled_inline_text(label, f, style, line_h)));
    if response.clicked() {
        if let Some(path) = resolve_link_target(url, base_dir, false) {
            queue_open_document(ui, path);
        } else if is_external_url(url) {
            ui.ctx().open_url(egui::OpenUrl::new_tab(url));
        } else if let Some(path) = local_link_path(url, base_dir) {
            queue_open_document(ui, path);
        }
    }
    response.on_hover_text(url);
}

#[cfg(test)]
mod bidi_tests {
    use super::*;

    #[test]
    fn mixed_direction_line_is_reordered() {
        let inlines = vec![Inline::Text("Hello مرحبا world שלום 123".into())];
        let visual = visual_inlines(&inlines);
        let text: String = visual.iter().map(|i| i.plain_text()).collect();
        assert_eq!(text, "Hello مرحبا world 123 שלום");
    }

    #[test]
    fn pure_ltr_and_single_rtl_runs_are_unchanged() {
        let ltr = vec![Inline::Text("plain text 123".into())];
        assert_eq!(visual_inlines(&ltr).as_ref(), ltr.as_slice());

        let rtl = vec![Inline::Text("مرحبا بالعالم".into())];
        assert_eq!(visual_inlines(&rtl).as_ref(), rtl.as_slice());
    }

    #[test]
    fn each_line_is_reordered_independently() {
        let inlines = vec![Inline::Text(
            "مرحبا بالعالم\nHello مرحبا world שלום 123".into(),
        )];
        let visual = visual_inlines(&inlines);
        let text: String = visual.iter().map(|i| i.plain_text()).collect();
        assert_eq!(text, "مرحبا بالعالم\nHello مرحبا world 123 שלום");
    }

    #[test]
    fn images_with_empty_alt_text_survive_reordering() {
        let image = Inline::Image {
            alt: String::new(),
            url: "pic.png".into(),
            title: None,
            width: None,
            height: None,
        };
        let inlines = vec![
            Inline::Text("مرحبا ".into()),
            image,
            Inline::Text(" שלום".into()),
        ];
        let visual = visual_inlines(&inlines);
        assert!(visual.iter().any(|i| matches!(i, Inline::Image { .. })));
    }

    #[test]
    fn styled_rtl_runs_keep_their_style() {
        let inlines = vec![
            Inline::Text("Hello ".into()),
            Inline::Strong(vec![Inline::Text("مرحبا".into())]),
            Inline::Text(" world".into()),
        ];
        let visual = visual_inlines(&inlines);
        let text: String = visual.iter().map(|i| i.plain_text()).collect();
        assert_eq!(text, "Hello مرحبا world");
        assert!(visual.iter().any(|i| matches!(i, Inline::Strong(_))));
    }
}

#[cfg(test)]
mod link_tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "mdviewer-link-test-{tag}-{}-{ts}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn local_markdown_links_resolve() {
        let dir = temp_dir("resolve");
        let note = dir.join("Other Note.md");
        std::fs::write(&note, "# Other\n").unwrap();

        assert_eq!(
            resolve_link_target("Other%20Note.md#section", Some(&dir), false),
            Some(note.clone())
        );
        assert_eq!(
            resolve_link_target("Other Note.md", Some(&dir), false),
            Some(note)
        );
    }

    #[test]
    fn wiki_links_resolve_without_extension() {
        let dir = temp_dir("wiki");
        let note = dir.join("My Note.md");
        std::fs::write(&note, "# Note\n").unwrap();
        assert_eq!(
            resolve_link_target("My Note", Some(&dir), true),
            Some(note.clone())
        );
        assert_eq!(
            resolve_link_target("My Note#Heading", Some(&dir), true),
            Some(note)
        );
    }

    #[test]
    fn data_uri_images_are_materialized() {
        let uri = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/p9sAAAAASUVORK5CYII=";
        let file = data_uri_to_temp_file(uri).expect("data URI should decode");
        let path = file.trim_start_matches("file://");
        assert!(std::fs::read(path).unwrap().starts_with(b"\x89PNG"));
    }

    #[test]
    fn wiki_links_keep_dots_in_note_names() {
        let dir = temp_dir("dotted");
        let note = dir.join("v1.2 release notes.md");
        std::fs::write(&note, "# Notes\n").unwrap();
        assert_eq!(
            resolve_link_target("v1.2 release notes", Some(&dir), true),
            Some(note)
        );
    }

    #[test]
    fn local_files_resolve_without_being_markdown() {
        let dir = temp_dir("pdf");
        assert_eq!(
            local_link_path("spec%20sheet.pdf#page2", Some(&dir)),
            Some(dir.join("spec sheet.pdf"))
        );
        assert_eq!(resolve_link_target("spec.pdf", Some(&dir), false), None);
        assert_eq!(local_link_path("#section", Some(&dir)), None);
    }

    #[test]
    fn standalone_wiki_embed_resolves_for_block_transclusion() {
        let dir = temp_dir("transclude");
        let note = dir.join("Note.md");
        std::fs::write(&note, "# Note\n").unwrap();
        let inlines = vec![
            Inline::Text("\n".into()),
            Inline::WikiEmbed {
                target: "Note".into(),
                label: String::new(),
            },
        ];
        assert_eq!(standalone_transclusion(&inlines, Some(&dir)), Some(note));
    }

    #[test]
    fn external_and_anchor_links_are_not_local() {
        let dir = temp_dir("external");
        assert_eq!(
            resolve_link_target("https://example.org", Some(&dir), false),
            None
        );
        assert_eq!(resolve_link_target("#section", Some(&dir), false), None);
        assert_eq!(
            resolve_link_target("mailto:x@example.org", Some(&dir), false),
            None
        );
    }
}

/// One display leaf after flattening inline styles, used for bidi reordering.
struct BidiLeaf {
    inline: Inline,
    /// Text used to compute bidi levels (a placeholder for atomic elements).
    text: String,
}

fn contains_rtl(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(
            c as u32,
            0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF
        )
    })
}

fn flatten_bidi(inlines: &[Inline], out: &mut Vec<BidiLeaf>) {
    for inline in inlines {
        match inline {
            Inline::Text(text) => out.push(BidiLeaf {
                inline: inline.clone(),
                text: text.clone(),
            }),
            Inline::Link { inlines, url } => {
                let start = out.len();
                flatten_bidi(inlines, out);
                for leaf in &mut out[start..] {
                    leaf.inline = Inline::Link {
                        inlines: vec![leaf.inline.clone()],
                        url: url.clone(),
                    };
                }
            }
            _ => match styled_children(inline) {
                Some((children, wrap)) => wrap_bidi(children, out, wrap),
                // Atomic elements: keep them as one unit. U+FFFC (object
                // replacement character) participates as neutral in bidi.
                None => out.push(BidiLeaf {
                    inline: inline.clone(),
                    text: "\u{FFFC}".into(),
                }),
            },
        }
    }
}

/// Rebuilds a style-only inline around its children.
type InlineWrap = fn(Vec<Inline>) -> Inline;

/// Children and rebuilding constructor of a style-only inline (emphasis,
/// strikethrough, sub/superscript, …). `None` for every other inline.
fn styled_children(inline: &Inline) -> Option<(&[Inline], InlineWrap)> {
    let wrap: InlineWrap = match inline {
        Inline::Emph(_) => Inline::Emph,
        Inline::Strong(_) => Inline::Strong,
        Inline::Strike(_) => Inline::Strike,
        Inline::Highlight(_) => Inline::Highlight,
        Inline::Underline(_) => Inline::Underline,
        Inline::Kbd(_) => Inline::Kbd,
        Inline::Super(_) => Inline::Super,
        Inline::Sub(_) => Inline::Sub,
        _ => return None,
    };
    match inline {
        Inline::Emph(v)
        | Inline::Strong(v)
        | Inline::Strike(v)
        | Inline::Highlight(v)
        | Inline::Underline(v)
        | Inline::Kbd(v)
        | Inline::Super(v)
        | Inline::Sub(v) => Some((v.as_slice(), wrap)),
        _ => None,
    }
}

/// Flattens `inlines` into `out`, wrapping every produced leaf with `wrap`.
fn wrap_bidi(inlines: &[Inline], out: &mut Vec<BidiLeaf>, wrap: InlineWrap) {
    let start = out.len();
    flatten_bidi(inlines, out);
    for leaf in &mut out[start..] {
        leaf.inline = wrap(vec![leaf.inline.clone()]);
    }
}

/// Reorders inline runs for mixed left-to-right / right-to-left text.
///
/// epaint shapes each run correctly (including Arabic joining) but lays runs
/// out in logical order. For a mixed line like `Hello مرحبا world 123 שלום`
/// this puts the RTL runs in the wrong visual position. This function splits
/// text at bidi-level boundaries and reorders the resulting runs; the run
/// contents stay in logical order so shaping remains correct.
fn visual_inlines(inlines: &[Inline]) -> std::borrow::Cow<'_, [Inline]> {
    let combined: String = inlines.iter().map(|inline| inline.plain_text()).collect();
    if !contains_rtl(&combined) {
        return std::borrow::Cow::Borrowed(inlines);
    }
    if combined.contains('\n') {
        let lines = split_inline_lines(inlines);
        if lines.len() > 1 {
            let mut out = Vec::new();
            for (index, line) in lines.iter().enumerate() {
                if index > 0 {
                    out.push(Inline::Text("\n".into()));
                }
                out.extend(visual_inlines_line(line).into_owned());
            }
            return std::borrow::Cow::Owned(out);
        }
    }
    visual_inlines_line(inlines)
}

/// Splits an inline run at hard line breaks so each line can be reordered
/// independently.
fn split_inline_lines(inlines: &[Inline]) -> Vec<Vec<Inline>> {
    let mut lines = vec![Vec::new()];
    for inline in inlines {
        if let Inline::Text(text) = inline {
            for (index, part) in text.split('\n').enumerate() {
                if index > 0 {
                    lines.push(Vec::new());
                }
                if !part.is_empty() {
                    lines
                        .last_mut()
                        .unwrap()
                        .push(Inline::Text(part.to_string()));
                }
            }
        } else {
            lines.last_mut().unwrap().push(inline.clone());
        }
    }
    lines
}

fn visual_inlines_line(inlines: &[Inline]) -> std::borrow::Cow<'_, [Inline]> {
    let mut leaves = Vec::new();
    flatten_bidi(inlines, &mut leaves);
    let combined: String = leaves.iter().map(|leaf| leaf.text.as_str()).collect();
    if !contains_rtl(&combined) {
        return std::borrow::Cow::Borrowed(inlines);
    }

    let bidi = unicode_bidi::BidiInfo::new(&combined, None);
    let mut segments: Vec<(Inline, u8)> = Vec::new();
    let mut base = 0usize;
    for leaf in leaves {
        if let Inline::Text(text) = &leaf.inline {
            let mut start = 0usize;
            let mut last_level = None;
            for (index, _) in text.char_indices() {
                let level = bidi
                    .levels
                    .get(base + index)
                    .map(|l| l.number())
                    .unwrap_or(0)
                    .min(125);
                if last_level.is_some_and(|previous| previous != level) {
                    segments.push((
                        Inline::Text(text[start..index].to_string()),
                        last_level.unwrap(),
                    ));
                    start = index;
                }
                last_level = Some(level);
            }
            if let Some(level) = last_level {
                segments.push((Inline::Text(text[start..].to_string()), level));
            }
        } else {
            let level = bidi
                .levels
                .get(base)
                .map(|l| l.number())
                .unwrap_or(0)
                .min(125);
            segments.push((leaf.inline, level));
        }
        base += leaf.text.len();
    }

    // Standard bidi reordering over homogeneous-level runs.
    let max_level = segments.iter().map(|(_, level)| *level).max().unwrap_or(0);
    let min_odd = segments
        .iter()
        .map(|(_, level)| *level)
        .filter(|level| level % 2 == 1)
        .min();
    if let Some(min_odd) = min_odd {
        for level in (min_odd..=max_level).rev() {
            let mut i = 0;
            while i < segments.len() {
                if segments[i].1 >= level {
                    let start = i;
                    while i < segments.len() && segments[i].1 >= level {
                        i += 1;
                    }
                    segments[start..i].reverse();
                } else {
                    i += 1;
                }
            }
        }
    }

    let mut out: Vec<Inline> = Vec::new();
    for (inline, _) in segments {
        let inline = trim_visual_inline(inline);
        // Only blank text is dropped; an image with empty alt text is kept.
        if matches!(&inline, Inline::Text(text) if text.trim().is_empty()) {
            continue;
        }
        if let Some(previous) = out.last()
            && needs_visual_space(previous, &inline)
        {
            out.push(Inline::Text(" ".into()));
        }
        out.push(inline);
    }
    std::borrow::Cow::Owned(out)
}

/// Removes surrounding whitespace from a reordered inline segment.
fn trim_visual_inline(inline: Inline) -> Inline {
    fn trim_vec(values: Vec<Inline>) -> Vec<Inline> {
        values.into_iter().map(trim_visual_inline).collect()
    }
    match inline {
        Inline::Text(text) => Inline::Text(text.trim().to_string()),
        Inline::Emph(v) => Inline::Emph(trim_vec(v)),
        Inline::Strong(v) => Inline::Strong(trim_vec(v)),
        Inline::Strike(v) => Inline::Strike(trim_vec(v)),
        Inline::Highlight(v) => Inline::Highlight(trim_vec(v)),
        Inline::Underline(v) => Inline::Underline(trim_vec(v)),
        Inline::Kbd(v) => Inline::Kbd(trim_vec(v)),
        Inline::Super(v) => Inline::Super(trim_vec(v)),
        Inline::Sub(v) => Inline::Sub(trim_vec(v)),
        Inline::Link { inlines, url } => Inline::Link {
            inlines: trim_vec(inlines),
            url,
        },
        other => other,
    }
}

/// Whether two visual segments need a separating space. Punctuation stays
/// attached; words from different direction runs do not.
fn needs_visual_space(left: &Inline, right: &Inline) -> bool {
    fn edge(value: &Inline, first: bool) -> Option<char> {
        let text = value.plain_text();
        if first {
            text.chars().next()
        } else {
            text.chars().last()
        }
    }
    let Some(left_edge) = edge(left, false) else {
        return false;
    };
    let Some(right_edge) = edge(right, true) else {
        return false;
    };
    // Closing punctuation attaches to the preceding run; opening punctuation
    // attaches to the following run. Everything else (including commas and
    // colons) keeps the separating space.
    let closing = matches!(
        right_edge,
        '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '»' | '”' | '\'' | '"'
    );
    let opening = matches!(left_edge, '(' | '[' | '{' | '«' | '“' | '\'' | '"');
    !closing && !opening
}

/// Flattens inline elements to their plain text.
fn plain_text(inlines: &[Inline]) -> String {
    inlines
        .iter()
        .map(|i| i.plain_text())
        .collect::<String>()
        .trim()
        .to_string()
}

/// Combined inline style while walking nested spans.
///
/// Nested Markdown (`***bold italic***`, `**bold *italic* ~~strike~~**`, …)
/// must keep *all* enclosing styles. Flattening each span to plain text (the
/// old behaviour) dropped the inner emphasis, strikethrough, code, …
#[derive(Clone, Copy)]
struct InlineStyle {
    bold: bool,
    italic: bool,
    strike: bool,
    underline: bool,
    highlight: bool,
    /// Relative font size (super/subscript shrink the text).
    scale: f32,
    /// Explicit text colour override.
    color: Option<Color32>,
    /// Monospace (raw HTML fallback).
    mono: bool,
    /// Vertical alignment inside the line (`Some(TOP)` = superscript,
    /// `Some(BOTTOM)` = subscript); `None` = normal baseline.
    valign: Option<Align>,
}

impl Default for InlineStyle {
    fn default() -> Self {
        Self {
            bold: false,
            italic: false,
            strike: false,
            underline: false,
            highlight: false,
            scale: 1.0,
            color: None,
            mono: false,
            valign: None,
        }
    }
}

/// Font for a run with the accumulated bold/italic style.
fn inline_font(size: f32, st: InlineStyle) -> FontId {
    if st.bold && st.italic {
        FontId::new(size, egui::FontFamily::Name("bold-italic".into()))
    } else if st.bold {
        bold_font(size)
    } else if st.italic {
        italic_font(size)
    } else {
        FontId::proportional(size)
    }
}

/// Builds a [`RichText`] run with the accumulated inline style.
fn styled_inline_text(text: &str, f: f32, st: InlineStyle, line_h: Option<f32>) -> RichText {
    let size = f * st.scale;
    let font = inline_font(size, st);
    let color = st.color.unwrap_or(if st.strike && !st.bold {
        palette().muted
    } else {
        palette().text
    });
    let color = if st.highlight {
        palette().highlight_text
    } else {
        color
    };
    let mut rt = RichText::new(text)
        .font(font)
        .color(color)
        .line_height(line_h);
    if st.highlight {
        rt = rt.background_color(palette().highlight_bg);
    }
    if st.strike {
        rt = rt.strikethrough();
    }
    if st.underline {
        rt = rt.underline();
    }
    rt
}

/// True if an inline subtree can be laid out as one text job (no widgets).
fn job_capable(inl: &Inline) -> bool {
    match inl {
        // A line break cannot be laid out inside a single horizontal job:
        // flush it and call `Ui::end_row` instead so the next inline starts a
        // new visual line.
        Inline::Text(t) => !t.contains('\n'),
        Inline::Emph(v)
        | Inline::Strong(v)
        | Inline::Strike(v)
        | Inline::Highlight(v)
        | Inline::Underline(v)
        | Inline::Super(v)
        | Inline::Sub(v) => v.iter().all(job_capable),
        _ => false,
    }
}

/// Builds a [`TextFormat`] for one run, including script alignment.
fn inline_text_format(f: f32, st: InlineStyle) -> TextFormat {
    let size = f * st.scale;
    let font = if st.mono {
        FontId::monospace(size)
    } else {
        inline_font(size, st)
    };
    let color = st.color.unwrap_or(if st.strike && !st.bold {
        palette().muted
    } else {
        palette().text
    });
    let color = if st.highlight {
        palette().highlight_text
    } else {
        color
    };
    // An explicit line height centres the text and defeats `valign`; script
    // runs therefore leave it unset so TOP/BOTTOM really raises/lowers them.
    let line_height = if st.valign.is_some() {
        None
    } else {
        Some(f * 1.55)
    };
    TextFormat {
        font_id: font,
        color,
        background: if st.highlight {
            palette().highlight_bg
        } else {
            Color32::TRANSPARENT
        },
        underline: if st.underline {
            Stroke::new(1.0, color)
        } else {
            Stroke::NONE
        },
        strikethrough: if st.strike {
            Stroke::new(1.0, color)
        } else {
            Stroke::NONE
        },
        valign: st.valign.unwrap_or(Align::BOTTOM),
        line_height,
        ..Default::default()
    }
}

thread_local! {
    /// Find query used to paint matches in the preview, as the user typed it.
    static PREVIEW_FIND: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Sets (or clears) the find query used by the preview renderer. Matching is
/// case-insensitive (see [`find_all_case_insensitive`]).
fn set_preview_find(query: Option<String>) {
    PREVIEW_FIND.with(|slot| *slot.borrow_mut() = query);
}

/// Byte ranges of the non-overlapping matches of `query` in `text`.
///
/// Characters are compared after simple Unicode lowercasing, so `É` matches
/// `é`. The ranges index `text` itself, even where lowercasing changes byte
/// lengths.
fn find_all_case_insensitive(text: &str, query: &str) -> Vec<(usize, usize)> {
    let needle: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let mut matches = Vec::new();
    if needle.is_empty() {
        return matches;
    }
    let mut pos = 0usize;
    while pos < text.len() {
        match case_insensitive_match_end(text, pos, &needle) {
            Some(end) => {
                matches.push((pos, end));
                pos = end;
            }
            None => pos += text[pos..].chars().next().map_or(1, char::len_utf8),
        }
    }
    matches
}

/// End byte of a match of the lowercased `needle` starting at byte `start`.
fn case_insensitive_match_end(text: &str, start: usize, needle: &[char]) -> Option<usize> {
    let mut rest = needle;
    for (offset, ch) in text[start..].char_indices() {
        for lower in ch.to_lowercase() {
            match rest.split_first() {
                Some((first, tail)) if *first == lower => rest = tail,
                _ => return None,
            }
        }
        if rest.is_empty() {
            return Some(start + offset + ch.len_utf8());
        }
    }
    None
}

/// Appends text, painting occurrences of the active find query.
fn append_text_highlighted(job: &mut egui::text::LayoutJob, text: &str, f: f32, st: InlineStyle) {
    PREVIEW_FIND.with(|slot| {
        let query = slot.borrow();
        let Some(query) = query.as_deref().filter(|q| !q.is_empty()) else {
            if !text.is_empty() {
                job.append(text, 0.0, inline_text_format(f, st));
            }
            return;
        };
        let mut start = 0usize;
        for (begin, end) in find_all_case_insensitive(text, query) {
            if begin > start {
                job.append(&text[start..begin], 0.0, inline_text_format(f, st));
            }
            let mut format = inline_text_format(f, st);
            format.background = palette().highlight_bg;
            format.color = palette().highlight_text;
            job.append(&text[begin..end], 0.0, format);
            start = end;
        }
        if start < text.len() {
            job.append(&text[start..], 0.0, inline_text_format(f, st));
        }
    });
}

/// Appends text-like inlines to a layout job, carrying nested styles.
fn append_inline_job(job: &mut egui::text::LayoutJob, inlines: &[Inline], f: f32, st: InlineStyle) {
    for inl in inlines {
        match inl {
            Inline::Text(t) => append_text_highlighted(job, t, f, st),
            Inline::Emph(v) => {
                let mut s = st;
                s.italic = true;
                append_inline_job(job, v, f, s);
            }
            Inline::Strong(v) => {
                let mut s = st;
                s.bold = true;
                append_inline_job(job, v, f, s);
            }
            Inline::Strike(v) => {
                let mut s = st;
                s.strike = true;
                append_inline_job(job, v, f, s);
            }
            Inline::Highlight(v) => {
                let mut s = st;
                s.highlight = true;
                append_inline_job(job, v, f, s);
            }
            Inline::Underline(v) => {
                let mut s = st;
                s.underline = true;
                append_inline_job(job, v, f, s);
            }
            Inline::Super(v) => {
                let mut s = st;
                s.scale *= 0.72;
                s.color = Some(palette().accent);
                s.valign = Some(Align::TOP);
                append_inline_job(job, v, f, s);
            }
            Inline::Sub(v) => {
                let mut s = st;
                s.scale *= 0.72;
                s.color = Some(palette().accent);
                s.valign = Some(Align::BOTTOM);
                append_inline_job(job, v, f, s);
            }
            _ => {}
        }
    }
}

/// Emits the accumulated text job as one label, then clears it.
fn flush_inline_job(ui: &mut Ui, job: &mut egui::text::LayoutJob) {
    if job.text.is_empty() {
        return;
    }
    let mut job = std::mem::take(job);
    job.wrap.max_width = ui.available_width().max(1.0);
    ui.add(Label::new(job));
}

/// Renders inline elements into the current (wrapping) horizontal layout.
///
/// Must handle every [`Inline`] variant (see the extension guide in
/// `parser.rs`). Clickable images report their resolved URI, alt text and
/// original URL through `image_click`.
fn render_inlines(
    ui: &mut Ui,
    inlines: &[Inline],
    f: f32,
    base_dir: Option<&Path>,
    image_click: &mut Option<ImageRequest>,
    math_cache: &mut MathCache,
) {
    let visual = visual_inlines(inlines);
    render_inlines_styled(
        ui,
        visual.as_ref(),
        f,
        InlineStyle::default(),
        base_dir,
        image_click,
        math_cache,
    );
}

/// Recursive inline renderer carrying the enclosing style.
fn render_inlines_styled(
    ui: &mut Ui,
    inlines: &[Inline],
    f: f32,
    st: InlineStyle,
    base_dir: Option<&Path>,
    image_click: &mut Option<ImageRequest>,
    math_cache: &mut MathCache,
) {
    let line_h = Some(f * 1.55);
    let mut job = egui::text::LayoutJob::default();
    for inl in inlines {
        // Pure text runs (including nested styles and sub/superscript) are
        // laid out as one job so baselines and wrapping stay consistent.
        if job_capable(inl) {
            append_inline_job(&mut job, std::slice::from_ref(inl), f, st);
            continue;
        }
        flush_inline_job(ui, &mut job);
        match inl {
            Inline::Text(t) => {
                for (li, line) in t.split('\n').enumerate() {
                    if li > 0 {
                        ui.end_row();
                    }
                    if !line.is_empty() {
                        let mut line_job = egui::text::LayoutJob::default();
                        append_text_highlighted(&mut line_job, line, f, st);
                        line_job.wrap.max_width = ui.available_width().max(1.0);
                        ui.add(Label::new(line_job));
                    }
                }
            }
            Inline::Emph(v) => {
                let mut s = st;
                s.italic = true;
                render_inlines_styled(ui, v, f, s, base_dir, image_click, math_cache);
            }
            Inline::Strong(v) => {
                let mut s = st;
                s.bold = true;
                render_inlines_styled(ui, v, f, s, base_dir, image_click, math_cache);
            }
            Inline::Strike(v) => {
                let mut s = st;
                s.strike = true;
                render_inlines_styled(ui, v, f, s, base_dir, image_click, math_cache);
            }
            Inline::Highlight(v) => {
                let mut s = st;
                s.highlight = true;
                render_inlines_styled(ui, v, f, s, base_dir, image_click, math_cache);
            }
            Inline::Underline(v) => {
                let mut s = st;
                s.underline = true;
                render_inlines_styled(ui, v, f, s, base_dir, image_click, math_cache);
            }
            Inline::Kbd(v) => {
                Frame::new()
                    .fill(palette().bg3)
                    .corner_radius(CornerRadius::same(4))
                    .stroke(Stroke::new(1.0, palette().stroke))
                    .inner_margin(Margin::symmetric(6, 2))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(plain_text(v))
                                .monospace()
                                .size(f * 0.8)
                                .color(palette().text),
                        );
                    });
            }
            Inline::Super(v) => {
                let mut s = st;
                s.scale *= 0.72;
                s.color = Some(palette().accent);
                render_inlines_styled(ui, v, f, s, base_dir, image_click, math_cache);
            }
            Inline::Sub(v) => {
                let mut s = st;
                s.scale *= 0.72;
                s.color = Some(palette().accent);
                render_inlines_styled(ui, v, f, s, base_dir, image_click, math_cache);
            }
            Inline::Code(c) => {
                let size = f * 0.88 * st.scale;
                // Code that fits the column stays on one line and moves to the
                // next row as a whole. Only code wider than the column wraps
                // inside its frame.
                let column = ui.max_rect().width();
                let text_width = ui
                    .painter()
                    .layout_no_wrap(c.clone(), FontId::monospace(size), palette().text)
                    .size()
                    .x;
                let remaining = ui.available_rect_before_wrap().width();
                // Not enough room left on this row: start a new one.
                if text_width > remaining && remaining < column - 1.0 {
                    ui.end_row();
                }
                let wrap = if text_width <= column {
                    egui::TextWrapMode::Extend
                } else {
                    egui::TextWrapMode::Wrap
                };
                Frame::new()
                    .fill(palette().bg3)
                    .corner_radius(CornerRadius::same(4))
                    .inner_margin(Margin::symmetric(5, 1))
                    .show(ui, |ui| {
                        ui.add(
                            Label::new(
                                RichText::new(c)
                                    .monospace()
                                    .size(size)
                                    .color(palette().text),
                            )
                            .wrap_mode(wrap),
                        );
                    });
            }
            Inline::Math { tex, display } => {
                let size = if *display { f * 1.12 } else { f };
                render_math(ui, tex, size, *display, math_cache);
            }
            Inline::Link { inlines, url } => {
                let label = plain_text(inlines);
                render_link(ui, non_empty_or(&label, url), url, f, st, line_h, base_dir);
            }
            Inline::WikiLink { target, label } => {
                let mut s = st;
                s.color = Some(palette().accent);
                s.underline = true;
                let text = styled_inline_text(non_empty_or(label, target), f, s, line_h);
                let resp = ui.add(Label::new(text).sense(egui::Sense::click()));
                wiki_target_response(ui, resp, target, base_dir)
                    .on_hover_text(format!("Obsidian wiki-link: {target}"));
            }
            Inline::Image {
                alt,
                url,
                title,
                width,
                height,
            } => {
                render_image(
                    ui,
                    alt,
                    url,
                    title,
                    *width,
                    *height,
                    f,
                    base_dir,
                    image_click,
                );
            }
            Inline::WikiEmbed { target, label } => {
                let text = non_empty_or(label, target);
                let response = Frame::new()
                    .fill(palette().bg2)
                    .corner_radius(CornerRadius::same(6))
                    .stroke(Stroke::new(1.0, palette().stroke))
                    .inner_margin(Margin {
                        left: 10,
                        right: 10,
                        top: 6,
                        bottom: 6,
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("⧉").size(f * 0.95).color(palette().faint));
                            ui.label(
                                RichText::new(text)
                                    .size(f * 0.85)
                                    .color(palette().accent)
                                    .line_height(line_h),
                            );
                        });
                    })
                    .response;
                let response = ui.interact(
                    response.rect,
                    ui.scope_id().with("wiki_embed"),
                    egui::Sense::click(),
                );
                wiki_target_response(ui, response, target, base_dir)
                    .on_hover_text(format!("Obsidian embed: {target}"));
            }
            Inline::Tag(tag) => {
                Frame::new()
                    .fill(palette().bg3)
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(6, 1))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(format!("#{tag}"))
                                .size(f * 0.82 * st.scale)
                                .color(palette().accent),
                        );
                    });
            }
            Inline::Citation(cite) => {
                ui.label(
                    RichText::new(format!("[{cite}]"))
                        .size(f * 0.9 * st.scale)
                        .italics()
                        .color(palette().muted),
                );
            }
            Inline::Abbr { text, title } => {
                ui.label(
                    RichText::new(text)
                        .size(f * st.scale)
                        .color(palette().text)
                        .underline(),
                )
                .on_hover_text(title);
            }
            Inline::FootnoteRef { label, number } => {
                let text = if *number > 0 {
                    number.to_string()
                } else {
                    label.clone()
                };
                ui.label(
                    RichText::new(text)
                        .size(f * 0.7)
                        .color(palette().accent)
                        .raised(),
                )
                .on_hover_text(format!("Footnote: {label}"));
            }
        }
    }
    flush_inline_job(ui, &mut job);
}

/// Remote-image byte loader backed by the system `curl`.
///
/// `egui_extras`' HTTP loader uses `ureq`, whose non-blocking connect can
/// return `EBADF` in release builds of the GUI app on macOS (observed both
/// with and without a VPN). curl ships with macOS, respects the system proxy
/// and VPN settings, and avoids the whole socket/timeout code path.
///
/// Results are cached per URI and fetched on a background thread so the UI
/// thread never blocks. A failed download is retried after
/// [`REMOTE_RETRY_AFTER`], and at most [`MAX_CONCURRENT_FETCHES`] run at once.
#[derive(Default)]
struct CurlBytesLoader {
    cache: Arc<Mutex<HashMap<String, CurlEntry>>>,
}

#[derive(Clone)]
enum CurlEntry {
    Pending,
    Ready(CachedBytes),
    Failed { error: String, at: Instant },
}

#[derive(Clone)]
struct CachedBytes {
    data: Arc<[u8]>,
    mime: Option<String>,
}

/// Downloads running at once. Further images wait and are requested again on
/// the next frame, so a page with many images does not start many `curl`s.
const MAX_CONCURRENT_FETCHES: usize = 4;
/// A failed download is retried once it is this old.
const REMOTE_RETRY_AFTER: Duration = Duration::from_secs(30);
static ACTIVE_FETCHES: AtomicUsize = AtomicUsize::new(0);

impl BytesLoader for CurlBytesLoader {
    fn id(&self) -> &str {
        "md_viewer::ui::CurlBytesLoader"
    }

    fn load(&self, ctx: &egui::Context, uri: &str) -> BytesLoadResult {
        if !is_remote_url(uri) {
            return Err(LoadError::NotSupported);
        }

        let cached = self
            .cache
            .lock()
            .ok()
            .and_then(|cache| cache.get(uri).cloned());
        match cached {
            Some(CurlEntry::Pending) => Ok(BytesPoll::Pending { size: None }),
            Some(CurlEntry::Ready(bytes)) => Ok(BytesPoll::Ready {
                size: None,
                bytes: Bytes::Shared(bytes.data),
                mime: bytes.mime,
            }),
            Some(CurlEntry::Failed { error, at }) if at.elapsed() < REMOTE_RETRY_AFTER => {
                Err(LoadError::Loading(error))
            }
            _ => {
                if ACTIVE_FETCHES.load(Ordering::Relaxed) >= MAX_CONCURRENT_FETCHES {
                    return Ok(BytesPoll::Pending { size: None });
                }
                ACTIVE_FETCHES.fetch_add(1, Ordering::Relaxed);
                if let Ok(mut cache) = self.cache.lock() {
                    cache.insert(uri.to_owned(), CurlEntry::Pending);
                }
                let url = uri.to_owned();
                let cache = Arc::clone(&self.cache);
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let result = fetch_with_curl(&url);
                    ACTIVE_FETCHES.fetch_sub(1, Ordering::Relaxed);
                    if let Ok(mut cache) = cache.lock() {
                        let entry = match result {
                            Ok(bytes) => CurlEntry::Ready(bytes),
                            Err(error) => CurlEntry::Failed {
                                error,
                                at: Instant::now(),
                            },
                        };
                        cache.insert(url, entry);
                    }
                    ctx.request_repaint();
                });
                Ok(BytesPoll::Pending { size: None })
            }
        }
    }

    fn forget(&self, uri: &str) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.remove(uri);
        }
    }

    fn forget_all(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
    }

    fn byte_size(&self) -> usize {
        self.cache
            .lock()
            .map(|cache| {
                cache
                    .values()
                    .filter_map(|entry| match entry {
                        CurlEntry::Ready(bytes) => Some(bytes.data.len()),
                        _ => None,
                    })
                    .sum()
            })
            .unwrap_or(0)
    }

    fn has_pending(&self) -> bool {
        self.cache
            .lock()
            .map(|cache| {
                cache
                    .values()
                    .any(|entry| matches!(entry, CurlEntry::Pending))
            })
            .unwrap_or(false)
    }
}

/// Downloads one URL with `/usr/bin/curl` (always present on macOS).
/// True when `url` points at this machine or a link-local address (loopback,
/// `localhost`, `0.0.0.0`, `169.254.0.0/16` such as cloud metadata endpoints,
/// `fe80::/10`). Private LAN addresses are allowed, since intranet images are
/// legitimate. Redirects are followed by curl and are not re-checked.
fn is_local_host(url: &str) -> bool {
    let Some((_, rest)) = url.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    let host = if let Some(bracketed) = host.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or("")
    } else {
        host.split(':').next().unwrap_or("")
    }
    .to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback() || v4.is_unspecified() || v4.is_link_local()
            }
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback() || v6.is_unspecified() || (v6.segments()[0] & 0xffc0) == 0xfe80
            }
        };
    }
    false
}

fn fetch_with_curl(url: &str) -> Result<CachedBytes, String> {
    if is_local_host(url) {
        return Err("images from this machine are not fetched".into());
    }
    let max_bytes = MAX_REMOTE_BYTES.to_string();
    let output = std::process::Command::new("/usr/bin/curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--compressed",
            "--max-time",
            "30",
            "--connect-timeout",
            "10",
            "--proto",
            "=https,http",
            "--proto-redir",
            "=https,http",
            "--max-filesize",
            max_bytes.as_str(),
            // Report the URL after redirects on stderr, so a redirect to a
            // local address is refused as well.
            "--write-out",
            "%{stderr}%{url_effective}",
            "--",
        ])
        .arg(url)
        .output()
        .map_err(|err| format!("curl could not be started: {err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        return Err(if detail.is_empty() {
            format!("curl exited with {}", output.status)
        } else {
            format!("curl: {detail}")
        });
    }
    let final_url = String::from_utf8_lossy(&output.stderr);
    if is_local_host(final_url.trim()) {
        return Err("redirected to an address on this machine".into());
    }
    check_download_limits(&output.stdout)?;
    let data: Arc<[u8]> = output.stdout.into();
    let mime = sniff_mime(&data).map(str::to_owned);
    Ok(CachedBytes { data, mime })
}

/// Maximum bytes accepted from a remote image download or data URI.
const MAX_REMOTE_BYTES: usize = 25 * 1024 * 1024;

/// Maximum width or height of a remote raster image or SVG.
const MAX_REMOTE_DIMENSION: u32 = 12_000;

/// Maximum pixel count of a remote raster image (~40 MP).
const MAX_REMOTE_PIXELS: u64 = 40_000_000;

/// Rejects image bytes that are too large to load safely: the byte size, and
/// the pixel size of raster images. SVG sizes are checked by [`svg_size_error`].
fn check_download_limits(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_REMOTE_BYTES {
        return Err(format!(
            "image is larger than {} MB",
            MAX_REMOTE_BYTES / (1024 * 1024)
        ));
    }
    if let Some((width, height)) = raster_dimensions(bytes)
        && (width > MAX_REMOTE_DIMENSION
            || height > MAX_REMOTE_DIMENSION
            || u64::from(width) * u64::from(height) > MAX_REMOTE_PIXELS)
    {
        return Err(format!(
            "image is too large ({width}×{height} px); the limit is \
             {MAX_REMOTE_DIMENSION}×{MAX_REMOTE_DIMENSION} px / \
             {} megapixels",
            MAX_REMOTE_PIXELS / 1_000_000
        ));
    }
    Ok(())
}

/// Error message if an SVG's natural size exceeds [`MAX_REMOTE_DIMENSION`].
/// Documents that do not parse return `None`; the renderer reports those.
fn svg_size_error(bytes: &[u8]) -> Option<String> {
    let tree = resvg::usvg::Tree::from_data(bytes, &crate::svg::options()).ok()?;
    let size = tree.size();
    let limit = MAX_REMOTE_DIMENSION as f32;
    (size.width() > limit || size.height() > limit).then(|| {
        format!(
            "SVG is too large ({:.0}×{:.0} px); the limit is {MAX_REMOTE_DIMENSION} px per side",
            size.width(),
            size.height()
        )
    })
}

/// Reads the intrinsic dimensions of a raster image without decoding pixels.
///
/// SVG payloads are skipped; [`svg_size_error`] checks them instead.
fn raster_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if looks_like_svg(bytes) {
        return None;
    }
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    reader.into_dimensions().ok()
}

/// Best-effort content type from the downloaded bytes.
fn sniff_mime(bytes: &[u8]) -> Option<&'static str> {
    if looks_like_svg(bytes) {
        Some("image/svg+xml")
    } else {
        image::guess_format(bytes)
            .ok()
            .map(|format| format.to_mime_type())
    }
}

type SvgCache = Mutex<HashMap<(String, SizeHint), Result<Arc<egui::ColorImage>, String>>>;

/// Image loader that rasterizes SVG responses whose URL has no `.svg`
/// suffix (e.g. `https://img.shields.io/badge/Rust-2024`).
///
/// The stock [`egui_extras`] SVG loader decides support from the URI extension
/// only. Services commonly serve SVG with a generic URL, which made README
/// badges fall through to the raster decoder and disappear behind an error
/// placeholder. This loader takes the bytes from egui's bytes loader (which
/// may start a download), checks the MIME type or XML payload, rejects SVGs
/// whose natural size exceeds [`MAX_REMOTE_DIMENSION`], and rasterizes with
/// the same `resvg` backend.
#[derive(Default)]
struct SvgSniffLoader {
    cache: SvgCache,
}

impl ImageLoader for SvgSniffLoader {
    fn id(&self) -> &str {
        "md_viewer::ui::SvgSniffLoader"
    }

    fn load(&self, ctx: &egui::Context, uri: &str, size_hint: SizeHint) -> ImageLoadResult {
        if !is_remote_url(uri) || egui::load::has_extension(uri, "svg") {
            // Explicit `.svg` URIs are handled by egui_extras' SvgLoader.
            return Err(LoadError::NotSupported);
        }

        let key = (uri.to_owned(), size_hint);
        if let Some(cached) = self.cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
            return match cached {
                Ok(image) => Ok(ImagePoll::Ready { image }),
                Err(err) => Err(LoadError::Loading(err)),
            };
        }

        match ctx.try_load_bytes(uri) {
            Ok(BytesPoll::Pending { size }) => Ok(ImagePoll::Pending { size }),
            Ok(BytesPoll::Ready { bytes, mime, .. }) => {
                let is_svg =
                    mime.as_deref().is_some_and(|m| m.contains("svg")) || looks_like_svg(&bytes);
                if !is_svg {
                    // Let ImageCrateLoader decode PNG/JPEG/GIF/WebP.
                    return Err(LoadError::NotSupported);
                }

                let result: Result<Arc<egui::ColorImage>, String> = match svg_size_error(&bytes) {
                    Some(error) => Err(error),
                    None => egui_extras::image::load_svg_bytes_with_size(
                        &bytes,
                        size_hint,
                        &crate::svg::options(),
                    )
                    .map(Arc::new),
                };
                if let Ok(mut cache) = self.cache.lock() {
                    cache.insert(key, result.clone());
                }
                match result {
                    Ok(image) => Ok(ImagePoll::Ready { image }),
                    Err(err) => Err(LoadError::Loading(err)),
                }
            }
            Err(LoadError::NotSupported) => Err(LoadError::NotSupported),
            Err(err) => Err(err),
        }
    }

    fn forget(&self, uri: &str) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.retain(|(cached_uri, _), _| cached_uri != uri);
        }
    }

    fn forget_all(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
    }

    fn byte_size(&self) -> usize {
        self.cache
            .lock()
            .map(|cache| {
                cache
                    .values()
                    .filter_map(|r| r.as_ref().ok())
                    .map(|image| image.pixels.len() * std::mem::size_of::<Color32>())
                    .sum()
            })
            .unwrap_or(0)
    }
}

/// True if a downloaded payload looks like an SVG document. A UTF-8 byte order
/// mark, whitespace and an XML or DOCTYPE prolog are allowed before `<svg`.
fn looks_like_svg(bytes: &[u8]) -> bool {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let sniff = &bytes[start..bytes.len().min(start + 4096)];
    let text = String::from_utf8_lossy(sniff).to_ascii_lowercase();
    text.starts_with("<svg")
        || ((text.starts_with("<?xml") || text.starts_with("<!doctype")) && text.contains("<svg"))
}

/// Starts a new wrapped row if an image of `desired_w` does not fit into the
/// remaining horizontal space.
fn start_image_line_if_needed(ui: &mut Ui, desired_w: f32) {
    let at_line_start = (ui.next_widget_position().x - ui.max_rect().left()).abs() < 1.0;
    if !at_line_start && ui.available_width() + 1.0 < desired_w {
        ui.end_row();
    }
}

/// Renders one inline image with explicit dimensions and a readable fallback
/// when the file cannot be loaded (missing relative path, no network, …).
#[expect(clippy::too_many_arguments)]
fn render_image(
    ui: &mut Ui,
    alt: &str,
    url: &str,
    title: &Option<String>,
    width: Option<f32>,
    height: Option<f32>,
    f: f32,
    base_dir: Option<&Path>,
    image_click: &mut Option<ImageRequest>,
) {
    let uri = if url.starts_with("data:") {
        data_uri_to_temp_file(url).unwrap_or_else(|| resolve_uri(url, base_dir))
    } else {
        convert_avif_preview(url, base_dir).unwrap_or_else(|| resolve_uri(url, base_dir))
    };
    let load = ui
        .ctx()
        .try_load_image(&uri, egui::load::SizeHint::default());

    // Move the image to the next line when it cannot fit after the preceding
    // inline content. Without this, egui's wrapping layout squeezes the widget
    // into the few remaining pixels and placeholder text wraps letter by
    // letter (e.g. a row of README badges).
    let desired_w = width
        .or_else(|| match &load {
            Ok(egui::load::ImagePoll::Ready { image }) => Some(image.source_size.x),
            _ => None,
        })
        .unwrap_or_else(|| (f * 16.0).max(220.0))
        .min(ui.max_rect().width().max(1.0));
    start_image_line_if_needed(ui, desired_w);

    match &load {
        Err(_) => {
            image_placeholder(
                ui,
                alt,
                url,
                "The image could not be loaded (missing file or unavailable network).",
                f,
            );
            return;
        }
        Ok(egui::load::ImagePoll::Pending { .. }) => {
            // Reserve a frame while bytes are fetched so the layout does not
            // jump when the image arrives.
            image_placeholder(ui, alt, url, "Loading image…", f);
            ui.ctx().request_repaint();
            return;
        }
        Ok(egui::load::ImagePoll::Ready { .. }) => {}
    }

    // Compute the display size from the explicit HTML attributes or the
    // image's own size. `.max_width()` alone was not enough: a later call with
    // the available width overrode `<img width="180">` and the README icon
    // filled the whole column.
    let natural = match &load {
        Ok(egui::load::ImagePoll::Ready { image }) => image.source_size,
        _ => Vec2::ZERO,
    };
    let mut display = match (width, height) {
        (Some(w), Some(h)) => Vec2::new(w, h),
        (Some(w), None) => Vec2::new(
            w,
            if natural.x > 0.0 {
                w * natural.y / natural.x
            } else {
                w
            },
        ),
        (None, Some(h)) => Vec2::new(
            if natural.y > 0.0 {
                h * natural.x / natural.y
            } else {
                h
            },
            h,
        ),
        (None, None) => natural,
    };
    let max_w = ui.available_width().max(1.0);
    if display.x > max_w && display.x > 0.0 {
        let scale = max_w / display.x;
        display *= scale;
    }

    let mut img = egui::Image::new(egui::ImageSource::Uri(uri.clone().into()))
        // Use the image's own pixel size (capped to the reading column). The
        // default `Image` fit shrinks to the current row height, which turned
        // previews into 24 px thumbnails.
        .fit_to_original_size(1.0)
        .sense(egui::Sense::click());
    if display.x > 0.0 && display.y > 0.0 {
        img = img.fit_to_exact_size(display);
    } else {
        img = img.max_width(max_w);
    }
    let mut resp = ui.add(img);

    let mut tip = String::new();
    if !alt.trim().is_empty() {
        tip.push_str(alt);
    }
    if let Some(t) = title.as_ref().filter(|t| !t.trim().is_empty()) {
        if !tip.is_empty() {
            tip.push_str(" — ");
        }
        tip.push_str(t);
    }
    if !url.trim().is_empty() {
        if !tip.is_empty() {
            tip.push_str(" — ");
        }
        tip.push_str(url);
    }
    if tip.is_empty() {
        tip.push_str("Click to open the picture viewer");
    }
    resp = resp.on_hover_text(tip);

    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ZoomIn);
        // Subtle "picture frame" affordance: images are clickable.
        ui.painter().rect_stroke(
            resp.rect,
            CornerRadius::same(4),
            Stroke::new(
                1.0,
                Color32::from_rgba_unmultiplied(
                    palette().accent.r(),
                    palette().accent.g(),
                    palette().accent.b(),
                    160,
                ),
            ),
            egui::StrokeKind::Inside,
        );
    }
    if resp.clicked() {
        *image_click = Some(ImageRequest {
            alt: alt.to_string(),
            url: url.to_string(),
            uri,
        });
    }
}

/// Draws a framed placeholder for an image that cannot be resolved.
fn image_placeholder(ui: &mut Ui, alt: &str, url: &str, reason: &str, f: f32) {
    card_frame(Margin {
        left: 14,
        right: 14,
        top: 10,
        bottom: 10,
    })
    .show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.label(RichText::new("🖼").size(f * 1.3).color(palette().faint));
            ui.vertical(|ui| {
                let name = if alt.trim().is_empty() { "Image" } else { alt };
                ui.label(
                    RichText::new(name)
                        .font(bold_font(f * 0.9))
                        .color(palette().text),
                );
                ui.label(
                    RichText::new(url)
                        .monospace()
                        .size(f * 0.72)
                        .color(palette().faint),
                );
                ui.label(RichText::new(reason).size(f * 0.72).color(palette().muted));
            });
        });
    });
}

/// Renders the front-matter properties at the top of the preview.
fn render_front_matter(ui: &mut Ui, doc: &Document, f: f32) {
    if doc.metadata.is_empty() {
        return;
    }
    card_frame(Margin {
        left: 14,
        right: 14,
        top: 10,
        bottom: 10,
    })
    .show(ui, |ui| {
        let full = ui.available_width();
        ui.set_min_width(full);
        ui.set_max_width(full);
        ui.label(
            RichText::new("Properties")
                .font(bold_font(f * 0.72))
                .color(palette().faint),
        );
        ui.add_space(4.0);
        // A Grid uses non-wrapping labels and can widen the whole reading
        // column when a front-matter value is long (e.g. `purpose`). Use
        // wrapping labels instead so the properties never push the text
        // under the right window edge.
        let key_w = (f * 6.2).clamp(64.0, 118.0);
        for (k, v) in &doc.metadata {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                ui.allocate_ui_with_layout(
                    Vec2::new(key_w, 0.0),
                    Layout::left_to_right(Align::TOP),
                    |ui| {
                        ui.set_max_width(key_w);
                        ui.add(
                            Label::new(
                                RichText::new(k)
                                    .font(bold_font(f * 0.78))
                                    .color(palette().muted),
                            )
                            .wrap(),
                        );
                    },
                );
                ui.add(Label::new(RichText::new(v).size(f * 0.82).color(palette().text)).wrap());
            });
        }
    });
    ui.add_space(f * 0.7);
}

// ---------------------------------------------------------------------------
// Mermaid diagrams
// ---------------------------------------------------------------------------

/// A rasterized Mermaid diagram ready to be drawn with `egui::Image`.
#[derive(Clone)]
struct MermaidTexture {
    handle: egui::TextureHandle,
    /// Logical size in points (texture pixels divided by pixels-per-point).
    size: Vec2,
}

/// Lazily rendered Mermaid diagrams, stored in the egui context so no extra
/// parameter has to be threaded through the whole renderer.
#[derive(Default, Clone)]
struct MermaidCache {
    entries: HashMap<u64, Option<MermaidTexture>>,
}

/// Mermaid theme tuned to the current app palette.
///
/// Dark: `mermaid_svg`'s dark theme with neutral charcoal accents. Paper: a
/// warm light theme with the same calmer categorical palette (darkened where
/// needed for contrast on off-white).
fn mermaid_theme() -> mermaid_svg::Theme {
    use std::borrow::Cow;

    let c = |s: &'static str| Cow::Borrowed(s);
    let paper = theme::mode() == theme::Mode::Paper;
    let mut t = if paper {
        mermaid_svg::Theme::default_theme()
    } else {
        mermaid_svg::Theme::dark()
    };
    if paper {
        t.fg = c("#2D2A24");
        t.fg_muted = c("#6A6358");
        t.bg = c("#FBF8F1");
        t.actor_fill = c("#F1EADB");
        t.actor_stroke = c("#B9AC93");
        t.actor_text_color = Some(c("#2D2A24"));
        t.lifeline = c("#C3B69E");
        t.arrow_stroke = c("#6A6358");
        t.signal_text_color = Some(c("#2D2A24"));
        t.note_fill = c("#FBF0C9");
        t.note_stroke = c("#C9A227");
        t.activation_fill = c("#F1EADB");
        t.activation_stroke = c("#B9AC93");
        t.frame_label_fill = c("#F1EADB");
        t.title_color = Some(c("#2D2A24"));
        t.flow_node_fill = c("#F1EADB");
        t.flow_node_stroke = c("#B9AC93");
        t.flow_edge_stroke = c("#6A6358");
        t.flow_label_bg = c("#FBF8F1");
        t.flow_cluster_fill = c("#F7F2E7");
        t.flow_cluster_stroke = c("#D2C4AB");
        t.pie_stroke = Some(c("#FBF8F1"));
    } else {
        t.fg = c("#E4E6EB");
        t.fg_muted = c("#9AA0AB");
        t.bg = c("#24262B");
        t.actor_fill = c("#30333A");
        t.actor_stroke = c("#5A6070");
        t.actor_text_color = Some(c("#E4E6EB"));
        t.lifeline = c("#5A6070");
        t.arrow_stroke = c("#A0A6B4");
        t.signal_text_color = Some(c("#E4E6EB"));
        t.note_fill = c("#33382A");
        t.note_stroke = c("#8A8F5A");
        t.activation_fill = c("#30333A");
        t.activation_stroke = c("#5A6070");
        t.frame_label_fill = c("#30333A");
        t.title_color = Some(c("#E4E6EB"));
        t.flow_node_fill = c("#30333A");
        t.flow_node_stroke = c("#5A6070");
        t.flow_edge_stroke = c("#A0A6B4");
        t.flow_label_bg = c("#2A2D34");
        t.flow_cluster_fill = c("#282A31");
        t.flow_cluster_stroke = c("#4A4F5C");
        t.pie_stroke = Some(c("#24262B"));
    }
    // Calm blue / pink / mint palette (matches the reference dark layout)
    // instead of the default bright yellow/green mix.
    let colors: Vec<Cow<'static, str>> = [
        "#60A5FA", "#F472B6", "#6EE7B7", "#C084FC", "#FBBF24", "#34D399", "#F87171", "#38BDF8",
        "#A78BFA", "#FB923C",
    ]
    .into_iter()
    .map(c)
    .collect();
    t.cscale_palette = Cow::Owned(colors.clone());
    t.pie_palette = Cow::Owned(colors.clone());
    t.git_palette = Cow::Owned(colors);
    t
}

/// Renders a ```mermaid block as a diagram, falling back to a code block if
/// the Mermaid source cannot be parsed.
fn render_mermaid(ui: &mut Ui, src: &str, f: f32) {
    let Some(tex) = mermaid_texture(ui.ctx(), src) else {
        render_code(ui, Some("mermaid"), src, f);
        return;
    };
    ui.add_space(f * 0.35);
    card_frame(Margin {
        left: 12,
        right: 12,
        top: 10,
        bottom: 10,
    })
    .show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        let avail = (ui.available_width() - 4.0).max(32.0);
        let scale = (avail / tex.size.x).min(1.0);
        let size = tex.size * scale;
        ui.vertical_centered(|ui| {
            ui.add(
                egui::Image::new(egui::load::SizedTexture::new(tex.handle.id(), tex.size))
                    .fit_to_exact_size(size)
                    .sense(egui::Sense::hover()),
            )
            .on_hover_text("Mermaid diagram (rendered natively)");
        });
    });
    ui.add_space(f * 0.55);
}

/// Mermaid textures kept in egui memory. Edits in split view add a texture
/// per changed diagram, so the cache is cleared once it reaches this size.
const MERMAID_CACHE_LIMIT: usize = 32;

/// Returns (and caches) the rasterized texture for a Mermaid source.
fn mermaid_texture(ctx: &egui::Context, src: &str) -> Option<MermaidTexture> {
    // Theme is part of the key: Paper diagrams use a light palette.
    let key = hash_key((theme::mode() as u8, src));
    let cache_id = egui::Id::unique("mdviewer_mermaid_cache");

    if let Some(hit) = ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<MermaidCache>(cache_id)
            .entries
            .get(&key)
            .cloned()
    }) {
        return hit;
    }

    let rendered = rasterize_mermaid(ctx, src);
    ctx.data_mut(|d| {
        let cache = d.get_temp_mut_or_default::<MermaidCache>(cache_id);
        if cache.entries.len() >= MERMAID_CACHE_LIMIT {
            cache.entries.clear();
        }
        cache.entries.insert(key, rendered.clone());
    });
    rendered
}

/// Runs the pure-Rust Mermaid renderer and rasterizes its SVG. The bitmap is
/// bounded on its longer side, so a huge Gantt diagram cannot allocate a
/// gigantic texture.
fn rasterize_mermaid(ctx: &egui::Context, src: &str) -> Option<MermaidTexture> {
    let svg = mermaid_svg::render_with(src, &mermaid_theme()).ok()?;
    // Crisp on Retina.
    let raster = crate::svg::rasterize(&svg, ctx.pixels_per_point() * 1.2, 2048.0)?;
    let (w, h) = (raster.pixmap.width(), raster.pixmap.height());
    let color_image =
        egui::ColorImage::from_rgba_premultiplied([w as usize, h as usize], raster.pixmap.data());
    let handle = ctx.load_texture(
        format!("mermaid:{}:{w}x{h}", hash_key(src)),
        color_image,
        egui::TextureOptions::LINEAR,
    );
    Some(MermaidTexture {
        handle,
        size: Vec2::new(raster.width, raster.height),
    })
}

/// Resolves an image URL relative to the document's directory.
///
/// Remote URLs and `file://` URIs are passed through. Relative paths and
/// root-relative paths (`/assets/…`) are joined with `base_dir` (the folder
/// that contains the Markdown file) and converted to a `file://` URI.
fn resolve_uri(url: &str, base_dir: Option<&Path>) -> String {
    if is_remote_url(url) || url.starts_with("file://") || url.starts_with("data:") {
        return url.to_string();
    }
    file_uri(&resolve_local_path(url, base_dir))
}

/// Local path of an image or link URL. Root-relative paths (`/assets/…`) are
/// relative to `base_dir`, not to the file system root.
fn resolve_local_path(url: &str, base_dir: Option<&Path>) -> PathBuf {
    match base_dir {
        Some(dir) if url.starts_with('/') && !url.starts_with("//") => {
            dir.join(url.trim_start_matches('/'))
        }
        Some(dir) if !Path::new(url).is_absolute() => dir.join(url),
        _ => PathBuf::from(url),
    }
}

/// Decodes a base64 `data:` URI into a cached temporary file so the regular
/// file image loader can use it (egui_extras does not load data URIs directly).
/// Returns `None` if the payload is too large or is not a valid image.
fn data_uri_to_temp_file(uri: &str) -> Option<String> {
    let data = uri.strip_prefix("data:")?;
    let (meta, payload) = data.split_once(',')?;
    if !meta.ends_with(";base64") {
        return None;
    }
    let mime = meta.trim_end_matches(";base64");
    // A fixed list keeps the MIME type from choosing the file name. Unknown
    // types get `bin`; the image loaders detect the format from the bytes.
    let extension = match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "image/svg+xml" => "svg",
        _ => "bin",
    };
    let bytes = base64_decode(payload)?;
    check_download_limits(&bytes).ok()?;
    if extension == "svg" && svg_size_error(&bytes).is_some() {
        return None;
    }
    let path =
        std::env::temp_dir().join(format!("mdviewer-data-{:x}.{extension}", hash_key(&bytes)));
    if !path.exists() {
        write_atomically(&path, &bytes)?;
    }
    Some(file_uri(&path))
}

/// Writes `bytes` to `path` through a partial file, so an interrupted write
/// never leaves a truncated file that later loads would reuse.
fn write_atomically(path: &Path, bytes: &[u8]) -> Option<()> {
    let partial = path.with_extension(format!(
        "{}.part{}",
        path.extension()?.to_str()?,
        std::process::id()
    ));
    std::fs::write(&partial, bytes).ok()?;
    std::fs::rename(&partial, path).ok()
}

fn base64_decode(value: &str) -> Option<Vec<u8>> {
    fn digit(byte: u8) -> Option<u8> {
        Some(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    }
    let bytes: Vec<u8> = value
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut index = 0usize;
    while index + 1 < bytes.len() {
        let a = digit(bytes[index])?;
        let b = digit(bytes[index + 1])?;
        out.push((a << 2) | (b >> 4));
        if index + 2 < bytes.len() && bytes[index + 2] != b'=' {
            let c = digit(bytes[index + 2])?;
            out.push((b << 4) | (c >> 2));
            if index + 3 < bytes.len() && bytes[index + 3] != b'=' {
                let d = digit(bytes[index + 3])?;
                out.push((c << 6) | d);
            }
        }
        index += 4;
    }
    Some(out)
}

/// AVIF files whose `sips` conversion failed. They are not retried on every
/// frame; the entry is keyed by the output path, which changes with the file's
/// modification time.
static FAILED_AVIF: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Mutex::default);

/// macOS `sips` can decode AVIF (the pure-Rust image stack cannot without a
/// heavy native decoder). Convert once to a cached PNG in the temp dir; a
/// failed conversion is not retried for the rest of the session.
fn convert_avif_preview(url: &str, base_dir: Option<&Path>) -> Option<String> {
    if !url.to_ascii_lowercase().ends_with(".avif") {
        return None;
    }
    let path = resolve_local_path(url, Some(base_dir?));
    let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
    let out = std::env::temp_dir().join(format!(
        "mdviewer-avif-{:x}.png",
        hash_key((&path, modified))
    ));
    if out.exists() {
        return Some(file_uri(&out));
    }
    if FAILED_AVIF.lock().is_ok_and(|failed| failed.contains(&out)) {
        return None;
    }
    let partial = out.with_extension(format!("png.part{}", std::process::id()));
    let converted = std::process::Command::new("/usr/bin/sips")
        .args(["-s", "format", "png"])
        .arg(&path)
        .arg("--out")
        .arg(&partial)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
        && std::fs::rename(&partial, &out).is_ok();
    if !converted {
        let _ = std::fs::remove_file(&partial);
        if let Ok(mut failed) = FAILED_AVIF.lock() {
            failed.insert(out);
        }
        return None;
    }
    Some(file_uri(&out))
}

/// Syntax-highlighted code blocks, keyed by code, font size and theme. Laying
/// out the highlight is the expensive part of a code block, so it is reused.
#[derive(Clone, Default)]
struct HighlightCache {
    entries: HashMap<u64, egui::text::LayoutJob>,
}

/// Highlighted code blocks kept in egui memory before the cache is cleared.
const HIGHLIGHT_CACHE_LIMIT: usize = 64;

/// Returns the highlighted job for a code block, reusing the cached one while
/// the code, font size and theme are unchanged.
fn highlighted_code(ctx: &egui::Context, code: &str, f: f32) -> egui::text::LayoutJob {
    let key = hash_key((code, f.to_bits(), theme::mode() as u8));
    let cache_id = egui::Id::unique("mdviewer_highlight_cache");
    if let Some(job) = ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<HighlightCache>(cache_id)
            .entries
            .get(&key)
            .cloned()
    }) {
        return job;
    }
    let job = highlight(code, FontId::monospace(f * 0.88));
    ctx.data_mut(|d| {
        let cache = d.get_temp_mut_or_default::<HighlightCache>(cache_id);
        if cache.entries.len() >= HIGHLIGHT_CACHE_LIMIT {
            cache.entries.clear();
        }
        cache.entries.insert(key, job.clone());
    });
    job
}

/// Renders a code block with a language label and horizontal scrolling.
fn render_code(ui: &mut Ui, lang: Option<&str>, code: &str, f: f32) {
    ui.add_space(f * 0.25);
    card_frame(Margin {
        left: 16,
        right: 16,
        top: 12,
        bottom: 14,
    })
    .show(ui, |ui| {
        if let Some(l) = lang {
            ui.label(
                RichText::new(l)
                    .monospace()
                    .size(f * 0.72)
                    .color(palette().faint),
            );
            ui.add_space(6.0);
        }
        ScrollArea::horizontal()
            .id_salt(ui.scope_id().with("code_scroll"))
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                let job = highlighted_code(ui.ctx(), code, f);
                ui.add(Label::new(job));
            });
    });
    ui.add_space(f * 0.5);
}

// ---------------------------------------------------------------------------
// Mini syntax highlighting (deliberately without syntect — zero extra deps)
// ---------------------------------------------------------------------------

/// Language keywords recognized by the heuristic highlighter.
fn is_keyword(w: &str) -> bool {
    matches!(
        w,
        "fn" | "let"
            | "mut"
            | "if"
            | "else"
            | "for"
            | "while"
            | "loop"
            | "return"
            | "match"
            | "struct"
            | "enum"
            | "impl"
            | "trait"
            | "pub"
            | "use"
            | "mod"
            | "const"
            | "static"
            | "self"
            | "Self"
            | "in"
            | "as"
            | "where"
            | "async"
            | "await"
            | "move"
            | "ref"
            | "def"
            | "class"
            | "import"
            | "from"
            | "pass"
            | "lambda"
            | "with"
            | "yield"
            | "function"
            | "var"
            | "new"
            | "this"
            | "typeof"
            | "package"
            | "func"
            | "defer"
            | "range"
            | "select"
            | "switch"
            | "case"
            | "break"
            | "continue"
            | "default"
            | "try"
            | "catch"
            | "finally"
            | "throw"
            | "throws"
            | "interface"
            | "type"
            | "void"
            | "not"
            | "and"
            | "or"
            | "elif"
            | "except"
            | "raise"
            | "True"
            | "False"
            | "None"
            | "null"
            | "true"
            | "false"
    )
}

/// Heuristic for type-like identifiers (CamelCase, not a keyword).
fn is_type_word(w: &str) -> bool {
    w.len() > 1 && w.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && !is_keyword(w)
}

/// Minimal but robust tokenizer producing a colored [`egui::text::LayoutJob`].
///
/// Handles line comments (`//`, `#`), single- and double-quoted strings,
/// numbers, keywords and CamelCase types. Block comments (`/* … */`) are only
/// recognized within one line: the state resets at every line break, so a
/// comment that spans lines is highlighted line by line. It is intentionally
/// language-agnostic: no grammar, no external dependency.
fn highlight(code: &str, font: FontId) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    fn push_fmt(job: &mut egui::text::LayoutJob, font: &FontId, s: &str, c: Color32) {
        if !s.is_empty() {
            job.append(s, 0.0, TextFormat::simple(font.clone(), c));
        }
    }

    for (li, line) in code.lines().enumerate() {
        if li > 0 {
            push_fmt(&mut job, &font, "\n", palette().sy_plain);
        }
        let chars: Vec<char> = line.chars().collect();
        let mut plain = String::new();
        let mut i = 0usize;

        macro_rules! flush {
            () => {{
                push_fmt(&mut job, &font, &plain, palette().sy_plain);
                plain.clear();
            }};
        }

        let is_line_comment = |c: char, next: Option<char>| -> bool {
            (c == '/' && next == Some('/')) || (c == '#' && next != Some('['))
        };

        while i < chars.len() {
            let c = chars[i];
            let next = chars.get(i + 1).copied();
            match c {
                '/' if next == Some('/') => {
                    flush!();
                    let rest: String = chars[i..].iter().collect();
                    push_fmt(&mut job, &font, &rest, palette().sy_com);
                    i = chars.len();
                }
                '/' if next == Some('*') => {
                    flush!();
                    let mut comm = String::from("/*");
                    let mut j = i + 2;
                    let mut closed = false;
                    while j < chars.len() {
                        let end_here =
                            j + 1 < chars.len() && chars[j] == '*' && chars[j + 1] == '/';
                        if end_here {
                            comm.push_str("*/");
                            j += 2;
                            closed = true;
                            break;
                        }
                        comm.push(chars[j]);
                        j += 1;
                    }
                    push_fmt(&mut job, &font, &comm, palette().sy_com);
                    i = if closed { j } else { chars.len() };
                }
                c if is_line_comment(c, next) => {
                    flush!();
                    let rest: String = chars[i..].iter().collect();
                    push_fmt(&mut job, &font, &rest, palette().sy_com);
                    i = chars.len();
                }
                '"' | '\'' => {
                    flush!();
                    let mut s = c.to_string();
                    let mut j = i + 1;
                    while j < chars.len() {
                        match chars[j] {
                            '\\' if j + 1 < chars.len() => {
                                s.push('\\');
                                s.push(chars[j + 1]);
                                j += 2;
                            }
                            q2 if q2 == c => {
                                s.push(q2);
                                j += 1;
                                break;
                            }
                            other => {
                                s.push(other);
                                j += 1;
                            }
                        }
                    }
                    push_fmt(&mut job, &font, &s, palette().sy_str);
                    i = j;
                }
                '0'..='9' => {
                    flush!();
                    let mut s = c.to_string();
                    let mut j = i + 1;
                    while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '.') {
                        s.push(chars[j]);
                        j += 1;
                    }
                    push_fmt(&mut job, &font, &s, palette().sy_num);
                    i = j;
                }
                c if c.is_alphabetic() || c == '_' || c == '$' || c == '@' => {
                    // Flush pending whitespace/punctuation first — otherwise
                    // spaces before identifiers would be swallowed.
                    flush!();
                    let mut s = c.to_string();
                    let mut j = i + 1;
                    while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                        s.push(chars[j]);
                        j += 1;
                    }
                    let color = if is_keyword(&s) {
                        palette().sy_kw
                    } else if is_type_word(&s) {
                        palette().sy_type
                    } else {
                        palette().sy_plain
                    };
                    push_fmt(&mut job, &font, &s, color);
                    i = j;
                }
                other => {
                    plain.push(other);
                    i += 1;
                }
            }
        }
        push_fmt(&mut job, &font, &plain, palette().sy_plain);
    }
    job
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// Non-blank `value`, or `fallback` when `value` is empty or only whitespace.
fn non_empty_or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}

/// Hash used for temp file names and cache keys. Stable within one run.
fn hash_key(value: impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// `file://` URI for a local image path, as egui's file loader reads it.
fn file_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

/// `file://` URL for handing a local file to the system. Spaces and the
/// characters that end a URL path are escaped.
fn file_url(path: &Path) -> String {
    let escaped = path
        .display()
        .to_string()
        .replace('%', "%25")
        .replace(' ', "%20")
        .replace('#', "%23")
        .replace('?', "%3F");
    format!("file://{escaped}")
}

/// Rounded panel for code blocks, diagrams, media and other cards.
fn card_frame(margin: Margin) -> Frame {
    Frame::new()
        .fill(palette().bg2)
        .stroke(Stroke::new(1.0, palette().stroke))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(margin)
}

/// Frame of the blocking sheets (save changes, WeasyPrint setup).
fn modal_frame() -> Frame {
    Frame::new()
        .fill(palette().bg3)
        .stroke(Stroke::new(1.0, palette().faint))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(20))
}

/// Draws a 1 px horizontal rule across the available width.
fn hairline(ui: &mut Ui) {
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, palette().stroke);
}

/// Pill-shaped button of the blocking sheets. The primary action is filled
/// with the accent color; the others are outlined.
fn modal_button(
    ui: &mut Ui,
    label: &str,
    size: f32,
    primary: bool,
    min_width: f32,
) -> egui::Response {
    let (text, fill, stroke, radius, height) = if primary {
        (
            Color32::WHITE,
            palette().accent,
            Stroke::new(2.0, palette().accent),
            18,
            34.0,
        )
    } else {
        (
            palette().text,
            palette().bg2,
            Stroke::new(1.2, palette().faint),
            16,
            32.0,
        )
    };
    ui.add(
        egui::Button::new(RichText::new(label).size(size).color(text))
            .fill(fill)
            .stroke(stroke)
            .corner_radius(CornerRadius::same(radius))
            .min_size(Vec2::new(min_width, height)),
    )
}

/// Borderless heading button with a ▼/▶ arrow. Returns `true` when clicked.
fn disclosure_button(ui: &mut Ui, open: bool, label: &str, font: FontId, color: Color32) -> bool {
    let arrow = if open { "▼" } else { "▶" };
    ui.add(
        egui::Button::new(
            RichText::new(format!("{arrow} {label}"))
                .font(font)
                .color(color),
        )
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(4)),
    )
    .clicked()
}

/// Shared click handling for wiki links and embeds: pointing-hand cursor, and
/// a click opens the resolved note in a tab.
fn wiki_target_response(
    ui: &mut Ui,
    response: egui::Response,
    target: &str,
    base_dir: Option<&Path>,
) -> egui::Response {
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if response.clicked()
        && let Some(path) = resolve_link_target(target, base_dir, true)
    {
        queue_open_document(ui, path);
    }
    response
}

/// Magnification shown in the toolbar, relative to [`DEFAULT_FONT_SIZE`].
///
/// Rounded to whole percent: the underlying font size changes in 1 px steps
/// (16 px = 100 %), so fractional percentages would be visual noise.
fn magnification_percent(font_size: f32) -> f32 {
    (font_size / DEFAULT_FONT_SIZE * 100.0).round()
}

/// Turns a title into a safe file-name stem: letters, digits and spaces are
/// kept (spaces become underscores), other characters become dashes. The
/// result is capped at 200 bytes and is empty for an empty title.
fn slug(s: &str) -> String {
    const MAX_BYTES: usize = 200;
    let mut cleaned = String::new();
    for c in s.trim().chars() {
        let c = if c.is_alphanumeric() || c == ' ' {
            c
        } else {
            '-'
        };
        if cleaned.len() + c.len_utf8() > MAX_BYTES {
            break;
        }
        cleaned.push(c);
    }
    cleaned.replace(' ', "_")
}

/// Shortens long paths for the toolbar, keeping the last 47 characters.
fn ellipsize_path(p: &Path) -> String {
    let s = p.display().to_string();
    if s.len() <= 48 {
        return s;
    }
    let skip = s.chars().count().saturating_sub(47);
    format!("…{}", s.chars().skip(skip).collect::<String>())
}

/// Returns `true` for file extensions listed in [`MARKDOWN_EXTENSIONS`].
fn is_markdown(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|ext| {
        MARKDOWN_EXTENSIONS
            .iter()
            .any(|known| ext.eq_ignore_ascii_case(known))
    })
}

/// Removes `md-viewer-print-*.pdf` files in `dir` that are older than `max_age`.
fn remove_stale_print_files(dir: &Path, max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let is_print_pdf = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("md-viewer-print-") && name.ends_with(".pdf"));
        let age = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok());
        if is_print_pdf && age.is_some_and(|age| age > max_age) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Character index (as used by egui's `TextEdit`) at the start of `line`.
///
/// `line` is 1-based and clamped to the document length.
fn line_to_char_index(text: &str, line: usize) -> usize {
    let target = line.saturating_sub(1);
    let mut line_no = 0usize;
    let mut index = 0usize;
    for ch in text.chars() {
        if line_no >= target {
            break;
        }
        index += 1;
        if ch == '\n' {
            line_no += 1;
        }
    }
    index
}

/// Converts a character index into a 1-based (line, column) pair.
fn char_to_line_col(text: &str, index: usize) -> (usize, usize) {
    let (mut line, mut col) = (1usize, 1usize);
    for (n, c) in text.chars().enumerate() {
        if n >= index {
            break;
        }
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

// ---------------------------------------------------------------------------
// eframe::App implementation
// ---------------------------------------------------------------------------

impl eframe::App for App {
    /// Called once per frame with the root `Ui`.
    ///
    /// Panel order matters: top and bottom panels claim their space first,
    /// then side panels, and the central panel fills the remainder.
    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        self.guard_window_close(&ctx);

        // While a modal is open it owns the input: no document shortcuts, no
        // split/view changes behind it.
        if self.image_viewer.is_none()
            && !self.close_dialog
            && !self.weasyprint_dialog
            && self.pending_tab_close.is_none()
        {
            self.handle_shortcuts(&ctx);
        }
        self.handle_dropped_files(&ctx);
        self.handle_macos_open();
        self.poll_export(&ctx);
        self.sync_window_title(&ctx);

        self.toolbar(ui);
        if self.find_open {
            self.find_bar(ui);
        }
        self.tab_bar(ui);
        self.status_bar(ui);
        if self.outline_open {
            self.outline_panel(ui);
        }
        set_preview_find(if self.find_open && !self.find_query.is_empty() {
            Some(self.find_query.clone())
        } else {
            None
        });
        match self.mode {
            ViewMode::Preview => self.preview_central(ui),
            ViewMode::Editor => self.edit_split(ui),
        }
        // A link click in the preview may target another document.
        if let Some(path) = ctx.data_mut(|data| data.remove_temp::<PathBuf>(pending_open_id())) {
            self.open_linked_path(&ctx, path);
        }
        set_preview_find(None);
        if let Some((_, _, at)) = &self.status {
            ctx.request_repaint_after(STATUS_DURATION.saturating_sub(at.elapsed()));
        }
        self.show_image_viewer(&ctx);
        self.show_close_dialog(&ctx);
        self.show_tab_close_dialog(&ctx);
        self.show_weasyprint_dialog(&ctx);
    }
}

#[cfg(test)]
mod render_smoke_tests {
    use super::*;

    /// Lays out the full preview headlessly (no window/GPU). This executes the
    /// render path for every block and inline in the corpus, so panics,
    /// infinite loops or layout errors in rarely used features fail the test
    /// suite instead of waiting for a manual click-through.
    /// Recursively collects all text galleys from the produced draw list.
    fn collect_text(shape: &egui::Shape, out: &mut String) {
        match shape {
            egui::Shape::Text(text) => {
                out.push_str(text.galley.text());
                out.push('\n');
            }
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    collect_text(shape, out);
                }
            }
            _ => {}
        }
    }

    fn run_preview(markdown: &str, width: f32, mode: theme::Mode) -> String {
        let doc = parser::parse(markdown);
        theme::set_mode(mode);
        let ctx = egui::Context::default();
        apply_theme(&ctx);
        install_system_fonts(&ctx);

        let mut image_click = None;
        let mut math_cache = MathCache::default();
        let mut preview_line = None;
        let mut preview_top_line = 1usize;
        let mut metrics = PreviewMetrics::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                // Tall surface so the scroll area lays out (and paints) every
                // block; egui culls widgets outside the viewport.
                Vec2::new(width, 20_000.0),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                preview_ui(
                    ui,
                    &doc,
                    16.0,
                    None,
                    true,
                    &mut image_click,
                    &mut math_cache,
                    &mut preview_line,
                    &mut preview_top_line,
                    &mut metrics,
                    0,
                );
            });
        });
        assert!(
            !output.shapes.is_empty(),
            "preview produced no draw commands"
        );
        let mut rendered_text = String::new();
        for clipped in &output.shapes {
            collect_text(&clipped.shape, &mut rendered_text);
        }
        // No renderer is attached in this headless test; acknowledge the
        // texture uploads so the output can be dropped cleanly.
        let mut output = output;
        output.textures_delta.clear();
        theme::set_mode(theme::Mode::Dark);
        rendered_text
    }

    /// One compact document containing every supported construct. Rendering
    /// this in both themes and at two widths exercises the complete preview
    /// renderer quickly (the full corpus is available as an ignored test).
    use crate::test_fixtures::FEATURE_GALLERY;

    #[test]
    fn every_feature_renders_headlessly_in_both_themes() {
        let mut first = String::new();
        for (index, (width, mode)) in [
            (1000.0, theme::Mode::Dark),
            (760.0, theme::Mode::Paper),
            (560.0, theme::Mode::Dark),
            (420.0, theme::Mode::Paper),
        ]
        .into_iter()
        .enumerate()
        {
            let text = run_preview(FEATURE_GALLERY, width, mode);
            if index == 0 {
                first = text;
            }
        }
        // The renderer must actually emit the content of every construct, not
        // just finish without panicking.
        for needle in [
            "Heading 1",
            "Heading 6",
            "bold",
            "italic",
            "strike",
            "highlight",
            "underline",
            "Ctrl",
            "link",
            "Footnote body",
            "Unordered item",
            "Nested item",
            "Done task",
            "Open task",
            "Ordered",
            "A quote.",
            "Nested quote.",
            "A callout",
            "println!",
            "Left",
            "Center",
            "Right",
            "Definition body",
            "Details summary",
            "Body inside details",
            "banner",
            "inline markup",
            "section",
            "list item",
            "#project",
            "Wiki Link",
            "Embedded Note",
            "Missing image",
            "Done.",
        ] {
            assert!(
                first.contains(needle),
                "preview did not render {needle:?}; rendered text was:\n{first}"
            );
        }
        // LaTeX and Mermaid must be typeset, not fall back to source text.
        for source in ["graph TD", "a^2 + b^2", "E = mc^2"] {
            assert!(
                !first.contains(source),
                "math/Mermaid fell back to source ({source:?}):\n{first}"
            );
        }
    }

    #[test]
    fn headings_keep_inline_code_and_links() {
        let text = run_preview(
            "## Title with `code` and [link](https://example.com)",
            800.0,
            theme::Mode::Dark,
        );
        assert!(text.contains("code"), "{text:?}");
        assert!(text.contains("link"), "{text:?}");
    }

    /// Large documents take the virtualized preview path (only the visible
    /// window is laid out). Smoke-test that it renders without panicking.
    #[test]
    fn large_document_uses_the_virtualized_path() {
        let mut markdown = String::new();
        for i in 0..600 {
            markdown.push_str(&format!(
                "Paragraph {i} with enough text to occupy a line in the preview.\n\n"
            ));
        }
        let text = run_preview(&markdown, 1000.0, theme::Mode::Dark);
        assert!(text.contains("Paragraph 0"));
        assert!(!text.is_empty());
    }

    /// Deep (slow) pass over the complete fixture corpus. Run with
    /// `cargo test full_documents_render_headlessly -- --ignored`.
    ///
    /// The fixtures are compiled into the test binary, so this works in a
    /// minimal checkout that ships only the application sources.
    #[test]
    #[ignore = "slow: renders the full feature and compatibility fixtures"]
    fn full_documents_render_headlessly() {
        let corpus = [
            crate::test_fixtures::FEATURE_GALLERY,
            crate::test_fixtures::COMPATIBILITY_SAMPLE,
            include_str!("../README.md"),
        ];
        for markdown in corpus {
            run_preview(markdown, 1000.0, theme::Mode::Dark);
            run_preview(markdown, 760.0, theme::Mode::Paper);
        }
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mdviewer-startup-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// A missing start-up file is not an error: the app opens empty.
    #[test]
    fn missing_startup_file_is_not_an_error() {
        let dir = temp_dir("missing");
        let path = dir.join(STARTUP_FILE);
        assert_eq!(startup_document_from(&[path]), (String::new(), None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The first readable candidate is used.
    #[test]
    fn startup_file_is_read_when_present() {
        let dir = temp_dir("present");
        let path = dir.join(STARTUP_FILE);
        std::fs::write(&path, "# Hello\n\nBody.\n").expect("write start-up.md");
        let (text, base) = startup_document_from(&[path]);
        assert_eq!(text, "# Hello\n\nBody.\n");
        assert_eq!(base.as_deref(), Some(dir.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The published start-up document must parse. If it was deleted, the app
    /// (and this test) must still work, so the test simply skips.
    #[test]
    fn published_startup_document_parses() {
        let Ok(markdown) = std::fs::read_to_string(STARTUP_FILE) else {
            return;
        };
        let doc = crate::parser::parse(&markdown);
        assert!(!markdown.trim().is_empty(), "start-up.md is empty");
        assert!(!doc.blocks.is_empty(), "start-up.md produced no blocks");
    }
}

#[cfg(test)]
mod tab_tests {
    use super::*;

    pub(super) fn test_app() -> App {
        let text = "# One\n\nbody\n".to_string();
        let doc = parser::parse(&text);
        let mut app = App {
            text,
            doc,
            path: None,
            encoding: crate::encoding::TextEncoding::Utf8,
            startup_base: None,
            dirty: false,
            tabs: Vec::new(),
            active_tab: 0,
            pending_tab_close: None,
            mode: ViewMode::Preview,
            outline_open: false,
            outline: Arc::new(Vec::new()),
            outline_revision: 0,
            find_open: false,
            find_query: String::new(),
            replace_text: String::new(),
            find_matches: Vec::new(),
            find_index: 0,
            find_cache: (String::new(), 0),
            find_focus: false,
            split_fraction: 0.5,
            font_size: DEFAULT_FONT_SIZE,
            status: None,
            export_job: None,
            line_count: 2,
            word_count: 2,
            last_cursor: (1, 1),
            cursor_key: None,
            last_title: String::new(),
            image_viewer: None,
            math_cache: MathCache::default(),
            preview_revision: 1,
            preview_metrics: PreviewMetrics::default(),
            jump_line: None,
            preview_line: None,
            preview_top_line: 1,
            close_dialog: false,
            weasyprint_dialog: false,
            weasyprint_ready: false,
            open_glue_installed: false,
        };
        app.store_active();
        app
    }

    fn temp_file(tag: &str, body: &str) -> PathBuf {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "mdviewer-tab-test-{tag}-{}-{ts}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{tag}.md"));
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn tab_label_uses_file_name_or_untitled() {
        assert_eq!(tab_label(None), "Untitled");
        assert_eq!(tab_label(Some(Path::new("/tmp/Note.md"))), "Note.md");
        assert_eq!(truncate_label("short.md", 12), "short.md");
        assert_eq!(truncate_label("a-very-long-file-name.md", 8), "a-very-l…");
    }

    #[test]
    fn tab_bar_hides_with_a_single_tab() {
        let mut app = test_app();
        assert!(!app.tab_bar_visible());
        let a = temp_file("bar-a", "# A\n");
        app.open_path(a);
        assert!(app.tab_bar_visible());
        app.close_tab(0);
        assert_eq!(app.tabs.len(), 1);
        assert!(!app.tab_bar_visible());
    }

    #[test]
    fn export_runs_on_a_worker_and_reports_back() {
        let target =
            std::env::temp_dir().join(format!("md-viewer-async-test-{}.pdf", std::process::id()));
        let mut app = test_app();
        app.text = "# Hello\n".into();
        let ctx = egui::Context::default();
        app.start_export(target.clone(), false);
        assert!(app.export_job.is_some());
        app.start_export(target.clone(), false);
        assert!(
            app.status
                .as_ref()
                .is_some_and(|(msg, _, _)| msg.contains("already running"))
        );
        for _ in 0..500 {
            app.poll_export(&ctx);
            if app.export_job.is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(app.export_job.is_none(), "the export did not finish");
        let _ = std::fs::remove_file(&target);
    }

    #[test]
    fn saving_writes_back_the_original_encoding() {
        let path = temp_file("cp1252-roundtrip", "");
        let mut app = test_app();
        app.text = "café\n".into();
        app.encoding = crate::encoding::TextEncoding::Windows1252;
        assert!(app.write_document(&path));
        assert_eq!(std::fs::read(&path).unwrap(), b"caf\xE9\n");
        assert_eq!(app.encoding, crate::encoding::TextEncoding::Windows1252);

        app.text = "中文\n".into();
        assert!(app.write_document(&path));
        assert_eq!(std::fs::read(&path).unwrap(), "中文\n".as_bytes());
        assert_eq!(app.encoding, crate::encoding::TextEncoding::Utf8);
        assert!(
            app.status
                .as_ref()
                .is_some_and(|(msg, _, _)| msg.contains("Windows-1252"))
        );
    }

    #[test]
    fn opening_a_file_reuses_a_blank_untitled_tab() {
        let mut app = test_app();
        // Make the tab a pristine empty Untitled one.
        app.text.clear();
        app.path = None;
        app.dirty = false;
        app.store_active();
        let a = temp_file("reuse-a", "# A\n");
        app.open_path(a.clone());
        assert_eq!(app.tabs.len(), 1, "no extra blank tab");
        assert_eq!(app.path.as_deref(), Some(a.as_path()));
    }

    #[test]
    fn opening_files_creates_tabs_and_preserves_edits() {
        let a = temp_file("a", "# A\n");
        let b = temp_file("b", "# B\n");
        let mut app = test_app();
        app.open_path(a.clone());
        app.open_path(b.clone());
        assert_eq!(app.tabs.len(), 3, "initial tab + two files");
        assert_eq!(app.active_tab, 2);
        assert_eq!(app.path.as_deref(), Some(b.as_path()));

        app.text.push_str("edit b");
        app.dirty = true;
        app.switch_tab(1);
        assert_eq!(app.path.as_deref(), Some(a.as_path()));
        app.text.push_str("edit a");
        app.dirty = true;

        app.switch_tab(2);
        assert!(app.text.contains("edit b"));
        assert!(app.dirty);
        assert_eq!(app.dirty_count(), 2);

        app.switch_tab(1);
        assert!(app.text.contains("edit a"));
        app.close_tab(2);
        assert_eq!(app.dirty_count(), 1);
    }

    #[test]
    fn reopening_a_file_switches_to_its_tab() {
        let a = temp_file("dup-a", "# A\n");
        let b = temp_file("dup-b", "# B\n");
        let mut app = test_app();
        app.open_path(a.clone());
        app.open_path(b);
        let count = app.tabs.len();
        app.open_path(a.clone());
        assert_eq!(app.tabs.len(), count, "no duplicate tab");
        assert_eq!(app.path.as_deref(), Some(a.as_path()));
    }

    #[test]
    fn closing_the_last_tab_creates_a_blank_untitled_tab() {
        let mut app = test_app();
        app.close_tab(0);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.active_tab, 0);
        assert!(app.text.is_empty());
        assert!(app.path.is_none());
        assert!(!app.dirty);
    }

    #[test]
    fn dirty_tabs_are_closed_through_the_dialog() {
        let mut app = test_app();
        app.request_close_tab(0);
        // The helper replaced the closed tab with a blank one.
        assert_eq!(app.tabs.len(), 1);
        assert!(app.text.is_empty());
        app.dirty = true;
        app.request_close_tab(0);
        assert_eq!(
            app.pending_tab_close,
            Some(0),
            "dirty tab waits for the dialog"
        );
    }

    #[test]
    fn outline_entries_follow_headings() {
        let mut app = test_app();
        app.refresh_outline();
        assert_eq!(app.outline.len(), 1);
        assert_eq!(app.outline[0].1, 1);
        assert_eq!(app.outline[0].2, "One");
    }

    #[test]
    fn find_and_replace_updates_the_document() {
        let mut app = test_app();
        app.find_open = true;
        app.find_query = "body".into();
        app.update_find_matches();
        assert_eq!(app.find_matches.len(), 1);
        app.replace_text = "text".into();
        app.replace_current();
        assert!(app.text.contains("text"));
        assert!(!app.text.contains("body"));
        assert!(app.dirty);
    }

    #[test]
    fn find_is_case_insensitive_and_wraps() {
        let mut app = test_app();
        app.find_query = "ONE".into();
        app.update_find_matches();
        assert_eq!(app.find_matches.len(), 1);
        app.find_next();
        assert_eq!(app.find_index, 0);
    }

    /// Runs one frame with a native close request and returns whether the
    /// close was cancelled and whether the save-changes sheet was requested.
    fn close_request(app: &mut App) -> (bool, bool) {
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .events
            .push(egui::ViewportEvent::Close);
        let mut output = ctx.run_ui(raw, |ui| app.guard_window_close(ui.ctx()));
        let cancelled = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|out| {
                out.commands
                    .iter()
                    .any(|command| matches!(command, egui::ViewportCommand::CancelClose))
            });
        output.textures_delta.clear();
        (cancelled, app.close_dialog)
    }

    #[test]
    fn window_close_is_cancelled_while_documents_are_dirty() {
        let mut app = test_app();
        app.dirty = true;
        assert_eq!(close_request(&mut app), (true, true));
    }

    #[test]
    fn window_close_leaves_the_decision_to_a_pending_tab_sheet() {
        let mut app = test_app();
        app.dirty = true;
        app.pending_tab_close = Some(0);
        assert_eq!(close_request(&mut app), (true, false));
    }

    #[test]
    fn editing_with_an_active_query_does_not_move_the_caret() {
        let mut app = test_app();
        app.find_query = "body".into();
        assert!(app.update_find_matches(), "a new query reports a change");
        assert_eq!(app.find_matches.len(), 1);

        app.text.push_str("body again\n");
        app.reparse();
        assert!(!app.update_find_matches(), "an edit only refreshes matches");
        assert_eq!(app.find_matches.len(), 2);
        assert_eq!((app.preview_line, app.jump_line), (None, None));
    }

    #[test]
    fn a_new_query_starts_at_the_first_match() {
        let mut app = test_app();
        app.find_query = "a".into();
        app.update_find_matches();
        app.find_index = 1;
        app.find_query = "ab".into();
        app.update_find_matches();
        assert_eq!(app.find_index, 0);
    }

    #[test]
    fn replacing_with_text_that_contains_the_query_moves_past_it() {
        let mut app = test_app();
        app.text = "body body\n".into();
        app.reparse();
        app.find_query = "body".into();
        app.replace_text = "body!".into();
        app.replace_current();
        assert_eq!(app.text, "body! body\n");
        assert_eq!(app.find_index, 1, "the second original match is next");
        assert!(app.dirty);
    }

    #[test]
    fn replacing_with_the_same_text_is_not_an_edit() {
        let mut app = test_app();
        app.find_query = "body".into();
        app.replace_text = "body".into();
        app.replace_all();
        app.replace_current();
        assert!(!app.dirty);
    }

    #[test]
    fn opening_a_file_in_place_resets_the_cursor_position() {
        let mut app = test_app();
        app.last_cursor = (42, 7);
        app.open_path_in_current_tab(temp_file("cursor", "# C\n"));
        assert_eq!(app.last_cursor, (1, 1));
    }

    #[test]
    fn preview_navigation_queues_a_caret_jump_only_in_edit_mode() {
        let mut app = test_app();
        app.jump_to_line(3);
        assert_eq!(app.preview_line, Some(3));
        assert_eq!(app.jump_line, None);

        app.mode = ViewMode::Editor;
        app.jump_to_line(5);
        assert_eq!(
            app.jump_line,
            Some(EditorJump {
                line: 5,
                focus: true
            })
        );
    }

    #[test]
    fn typing_in_find_never_moves_the_editor() {
        let mut app = test_app();
        app.text = "alpha\nbeta\ngamma beta\n".into();
        app.reparse();
        app.mode = ViewMode::Editor;

        app.find_query = "beta".into();
        app.refresh_find();
        assert_eq!(app.preview_line, Some(2));
        assert_eq!(app.jump_line, None);

        app.find_next();
        assert_eq!(
            app.jump_line,
            Some(EditorJump {
                line: 3,
                focus: false
            })
        );
    }

    #[test]
    fn switching_tabs_does_not_replay_a_stale_caret_jump() {
        let mut app = test_app();
        app.jump_line = Some(EditorJump {
            line: 3,
            focus: true,
        });
        app.load_tab(TabState::blank());
        assert_eq!(app.jump_line, None);
    }
}

#[cfg(test)]
mod helper_tests {
    use super::*;

    #[test]
    fn find_is_case_insensitive_for_non_ascii_text() {
        let text = "Café CAFÉ café";
        let matches = find_all_case_insensitive(text, "café");
        assert_eq!(matches.len(), 3);
        for (start, end) in matches {
            assert!(text[start..end].to_lowercase() == "café");
        }
        assert!(find_all_case_insensitive(text, "").is_empty());
        assert_eq!(
            find_all_case_insensitive("aaaa", "aa"),
            vec![(0, 2), (2, 4)]
        );
    }

    #[test]
    fn long_paths_are_shortened_on_character_boundaries() {
        let ascii = Path::new("/Users/someone/Documents/projects/markdown/notes/file.md");
        let label = ellipsize_path(ascii);
        assert!(label.starts_with('…') && label.ends_with("file.md"));

        let accented =
            PathBuf::from("/Users/someone/Dokumente/Größe-Übersicht-αβγδ-😀/Notiz-ähnlich.md");
        assert!(accented.display().to_string().len() > 48);
        let label = ellipsize_path(&accented);
        assert!(label.starts_with('…') && label.ends_with("Notiz-ähnlich.md"));
    }

    #[test]
    fn file_names_from_titles_are_safe() {
        assert_eq!(slug(""), "");
        assert_eq!(slug("My Note: Draft/1"), "My_Note-_Draft-1");
        assert!(slug(&"ü".repeat(300)).len() <= 200);

        let mut app = super::tab_tests::test_app();
        assert_eq!(app.default_file_name("md"), "One.md");
        app.doc = parser::parse("body only\n");
        assert_eq!(app.default_file_name("pdf"), "document.pdf");
    }

    #[test]
    fn stale_print_pdfs_are_removed_and_other_files_kept() {
        let dir = std::env::temp_dir().join(format!("mdviewer-print-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join("md-viewer-print-1-1.pdf");
        let fresh = dir.join("md-viewer-print-1-2.pdf");
        let other = dir.join("notes.pdf");
        for path in [&old, &fresh, &other] {
            std::fs::write(path, b"%PDF").unwrap();
        }
        let two_hours_ago = std::time::SystemTime::now() - Duration::from_secs(2 * 60 * 60);
        for path in [&old, &other] {
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(two_hours_ago)
                .unwrap();
        }

        remove_stale_print_files(&dir, Duration::from_secs(60 * 60));
        assert!(!old.exists());
        assert!(fresh.exists());
        assert!(other.exists(), "only print files are removed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fenced_div_colors_match_whole_words() {
        assert_eq!(fenced_div_color("multiple"), palette().accent);
        assert_eq!(fenced_div_color("stipulation"), palette().accent);
        assert_eq!(fenced_div_color("tip"), palette().green);
        assert_eq!(fenced_div_color("warning-box"), palette().red);
    }

    #[test]
    fn remote_urls_are_matched_without_case_or_slicing_errors() {
        assert!(is_remote_url("HTTPS://example.org/a.png"));
        assert!(!is_remote_url("ftp://example.org/a.png"));
        assert!(!is_remote_url("héééé.png"));
    }

    #[test]
    fn svg_detection_skips_a_byte_order_mark_and_doctype() {
        let svg = b"\xEF\xBB\xBF<?xml version=\"1.0\"?>\n<!DOCTYPE svg>\n<svg xmlns=\"http://www.w3.org/2000/svg\"/>";
        assert!(looks_like_svg(svg));
        assert!(!looks_like_svg(b"<html></html>"));
    }

    #[test]
    fn oversized_svg_documents_are_rejected() {
        let huge =
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="20000" height="20000"></svg>"#;
        assert!(svg_size_error(huge).is_some());
        let small = br#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"></svg>"#;
        assert!(svg_size_error(small).is_none());
    }

    #[test]
    fn download_limits_reject_oversized_payloads() {
        assert!(check_download_limits(&vec![0u8; MAX_REMOTE_BYTES + 1]).is_err());
        let png = include_bytes!("../assets/AppIcon-1024.png");
        assert!(check_download_limits(png).is_ok());
    }
}

#[cfg(test)]
mod remote_tests {
    use super::*;

    #[test]
    fn raster_dimensions_read_png_headers() {
        let png = include_bytes!("../assets/AppIcon-1024.png");
        assert_eq!(raster_dimensions(png), Some((1024, 1024)));
    }

    #[test]
    fn local_and_link_local_hosts_are_not_fetched() {
        assert!(is_local_host("http://localhost:8080/x.png"));
        assert!(is_local_host("https://127.0.0.1/x.png"));
        assert!(is_local_host("http://169.254.169.254/latest/meta-data"));
        assert!(is_local_host("http://[::1]:8080/x.png"));
        assert!(is_local_host("http://user@0.0.0.0/x"));
        assert!(!is_local_host("https://img.shields.io/badge/Rust-2024"));
        assert!(!is_local_host("https://192.168.1.10/logo.png"));
    }

    #[test]
    fn svg_payloads_are_not_treated_as_raster() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
        assert_eq!(raster_dimensions(svg), None);
    }
}

#[cfg(test)]
mod table_tests {
    use super::*;

    #[test]
    fn columns_grow_to_fill_the_available_width() {
        let widths = fit_table_columns(&[80.0, 80.0, 80.0], 900.0);
        assert_eq!(widths.len(), 3);
        let sum: f32 = widths.iter().sum();
        assert!((sum - 900.0).abs() < 0.01, "sum was {sum}");
        // A narrow table is stretched proportionally.
        assert!(widths.iter().all(|w| *w > 80.0));
    }

    #[test]
    fn wide_columns_shrink_but_keep_the_minimum() {
        let widths = fit_table_columns(&[1200.0, 60.0, 60.0], 600.0);
        let sum: f32 = widths.iter().sum();
        assert!((sum - 600.0).abs() < 0.01, "sum was {sum}");
        assert!(widths[0] > widths[1]);
        assert!(widths[1] >= TABLE_MIN_COL_W - 0.01);
    }

    #[test]
    fn narrow_columns_keep_their_natural_width() {
        let widths = fit_table_columns(&[60.0, 60.0, 1000.0], 300.0);
        assert!((widths[0] - 60.0).abs() < 0.01);
        assert!((widths[1] - 60.0).abs() < 0.01);
        assert!((widths[2] - 180.0).abs() < 0.01);
        let sum: f32 = widths.iter().sum();
        assert!((sum - 300.0).abs() < 0.01, "sum was {sum}");
    }

    #[test]
    fn extremely_narrow_windows_split_evenly() {
        let widths = fit_table_columns(&[200.0, 200.0], 100.0);
        let sum: f32 = widths.iter().sum();
        assert!((sum - 100.0).abs() < 0.01, "sum was {sum}");
        assert!((widths[0] - widths[1]).abs() < 0.01);
    }

    #[test]
    fn html_table_with_colspan_and_rowspan_renders() {
        let md = "<table style=\"text-align: center;\">\n\
<thead>\n\
<tr><th rowspan=\"2\">Host</th><th colspan=\"2\">Cluster A</th></tr>\n\
<tr><th>CPU</th><th>RAM</th></tr>\n\
</thead>\n\
<tbody>\n\
<tr><td><b>Mainframe 01</b></td><td>80%</td><td>16 GB</td></tr>\n\
<tr><td>Mainframe 02</td><td colspan=\"2\">OFFLINE</td></tr>\n\
</tbody>\n\
</table>\n";
        let doc = crate::parser::parse(md);
        let Block::Table { aligns, head, rows } = &doc.blocks[0] else {
            panic!("expected HTML table, got {:?}", doc.blocks[0]);
        };
        assert_eq!(head.len(), 2);
        assert_eq!(rows.len(), 2);

        let ctx = egui::Context::default();
        let mut image_click = None;
        let mut math_cache = MathCache::default();
        let mut bottom = None;
        for _ in 0..2 {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    Vec2::new(900.0, 700.0),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let top = ui.cursor().top();
                    render_table(
                        ui,
                        aligns,
                        head,
                        rows,
                        16.0,
                        None,
                        &mut image_click,
                        &mut math_cache,
                    );
                    bottom = Some(ui.cursor().top() - top);
                });
            });
            output.textures_delta.clear();
        }
        // The table consumed vertical space and did not panic.
        assert!(bottom.is_some_and(|h| h > 40.0), "height was {bottom:?}");
    }
}

#[cfg(test)]
mod viewer_tests {
    use super::*;

    fn viewer(alt: &str, url: &str) -> ImageViewer {
        ImageViewer::new(ImageRequest {
            alt: alt.to_string(),
            url: url.to_string(),
            uri: String::new(),
        })
    }

    #[test]
    fn viewer_title_prefers_alt_text() {
        assert_eq!(
            viewer("A diagram", "images/diagram.png").title(),
            "A diagram"
        );
    }

    #[test]
    fn viewer_title_falls_back_to_file_name() {
        assert_eq!(viewer("", "images/diagram.png").title(), "diagram.png");
        assert_eq!(viewer("  ", "file:///tmp/").title(), "Image");
    }

    #[test]
    fn viewer_zoom_keeps_center_and_clamps() {
        let mut v = viewer("", "diagram.png");
        v.scene_rect = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(1000.0, 500.0));
        let viewport = Vec2::new(500.0, 500.0);
        let center = v.scene_rect.center();

        v.zoom(2.0, viewport);
        assert!((v.scene_rect.center() - center).length() < 0.001);
        assert!((v.scene_rect.width() - 500.0).abs() < 0.001);

        // The scene's zoom range is 0.05..=8.0; repeated zoom-in stops at 8x.
        for _ in 0..50 {
            v.zoom(2.0, viewport);
        }
        assert!((v.scene_rect.width() - viewport.x / 8.0).abs() < 0.001);
    }

    #[test]
    fn math_cache_renders_formula() {
        let ctx = egui::Context::default();
        let mut cache = MathCache::default();
        let math = cache
            .texture(&ctx, r"E = mc^2", 16.0, false)
            .expect("formula should render");
        assert!(math.size.x > 0.0 && math.size.y > 0.0);
    }

    #[test]
    fn math_cache_renders_row_broken_display_math() {
        let ctx = egui::Context::default();
        let mut cache = MathCache::default();
        let tex = crate::parser::normalize_display_math(
            "H \\mathrm{e}^{-\\alpha L/2} = B \\cos(kL/2)\n\\\\\n\\Rightarrow \\alpha = k \\tan(kL/2)",
        );
        let math = cache
            .texture(&ctx, &tex, 16.0, true)
            .expect("row-broken display math should render");
        assert!(math.size.x > 0.0 && math.size.y > 0.0);
    }
}

#[cfg(test)]
mod jump_tests {
    use super::*;

    #[test]
    fn line_to_char_index_maps_lines() {
        let text = "# a\nb\nc";
        assert_eq!(line_to_char_index(text, 1), 0);
        assert_eq!(line_to_char_index(text, 2), 4);
        assert_eq!(line_to_char_index(text, 3), 6);
        // Clamped past the end.
        assert_eq!(line_to_char_index(text, 99), 7);
    }

    #[test]
    fn clicking_preview_requests_a_source_jump() {
        let doc = crate::parser::parse("# Heading\n\nParagraph one.\n\nParagraph two.\n");
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(900.0, 700.0));
        let pointer = egui::pos2(450.0, 100.0);
        let mut image_click = None;
        let mut math_cache = MathCache::default();
        let mut source_jump = None;
        let mut preview_line = None;
        let mut preview_top_line = 1usize;
        let mut preview_metrics = PreviewMetrics::default();

        // Frame 1: press. Frame 2: release → egui reports `primary_clicked`.
        for pressed in [true, false] {
            let mut raw = egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            };
            raw.events.push(egui::Event::PointerMoved(pointer));
            raw.events.push(egui::Event::PointerButton {
                pos: pointer,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
            let mut output = ctx.run_ui(raw, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    if let Some(line) = preview_ui(
                        ui,
                        &doc,
                        16.0,
                        None,
                        true,
                        &mut image_click,
                        &mut math_cache,
                        &mut preview_line,
                        &mut preview_top_line,
                        &mut preview_metrics,
                        0,
                    ) {
                        source_jump = Some(line);
                    }
                });
            });
            output.textures_delta.clear();
        }
        assert!(
            source_jump.is_some(),
            "a click inside the preview should request a source line"
        );
    }

    #[test]
    fn top_visible_line_tracks_the_preview_viewport() {
        let hits = vec![
            (
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 40.0)),
                1,
            ),
            (
                Rect::from_min_max(egui::pos2(0.0, 40.0), egui::pos2(100.0, 80.0)),
                10,
            ),
            (
                Rect::from_min_max(egui::pos2(0.0, 80.0), egui::pos2(100.0, 120.0)),
                20,
            ),
        ];
        let viewport =
            |top: f32| Rect::from_min_max(egui::pos2(0.0, top), egui::pos2(100.0, 600.0));
        assert_eq!(top_visible_line(&hits, viewport(0.0)), Some(1));
        assert_eq!(top_visible_line(&hits, viewport(50.0)), Some(10));
        assert_eq!(top_visible_line(&hits, viewport(100.0)), Some(20));
        // Below the last block → keep the last visible section.
        assert_eq!(top_visible_line(&hits, viewport(500.0)), Some(20));
    }

    #[test]
    fn block_at_line_selects_the_containing_block() {
        let hits = vec![
            (
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 20.0)),
                1,
            ),
            (
                Rect::from_min_max(egui::pos2(0.0, 20.0), egui::pos2(100.0, 40.0)),
                10,
            ),
            (
                Rect::from_min_max(egui::pos2(0.0, 40.0), egui::pos2(100.0, 60.0)),
                20,
            ),
        ];
        assert_eq!(block_at_line(&hits, 5).map(|h| h.1), Some(1));
        assert_eq!(block_at_line(&hits, 10).map(|h| h.1), Some(10));
        assert_eq!(block_at_line(&hits, 19).map(|h| h.1), Some(10));
        assert_eq!(block_at_line(&hits, 99).map(|h| h.1), Some(20));
    }

    #[test]
    fn source_line_at_prefers_containing_and_nearest_blocks() {
        let hits = vec![
            (
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 40.0)),
                10,
            ),
            (
                Rect::from_min_max(egui::pos2(0.0, 50.0), egui::pos2(100.0, 90.0)),
                20,
            ),
        ];
        assert_eq!(source_line_at(&hits, egui::pos2(50.0, 20.0)), Some(10));
        assert_eq!(source_line_at(&hits, egui::pos2(50.0, 70.0)), Some(20));
        // The gap between blocks selects the nearest one.
        assert_eq!(source_line_at(&hits, egui::pos2(50.0, 43.0)), Some(10));
        assert_eq!(source_line_at(&hits, egui::pos2(50.0, 47.0)), Some(20));
        // Outside the reading column (scrollbar, margins) is ignored.
        assert_eq!(source_line_at(&hits, egui::pos2(150.0, 20.0)), None);
    }
}

#[cfg(test)]
mod zoom_tests {
    use super::*;

    #[test]
    fn magnification_is_relative_to_the_default_font_size() {
        assert_eq!(magnification_percent(DEFAULT_FONT_SIZE), 100.0);
        assert_eq!(magnification_percent(DEFAULT_FONT_SIZE * 0.5), 50.0);
        assert_eq!(magnification_percent(DEFAULT_FONT_SIZE * 2.0), 200.0);
        assert_eq!(magnification_percent(MIN_FONT_SIZE), 56.0);
        assert_eq!(magnification_percent(MAX_FONT_SIZE), 225.0);
    }
}
