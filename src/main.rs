//! # MD-Viewer — native Markdown editor & viewer for macOS (Apple Silicon)
//!
//! A lean desktop application written in Rust with a live preview and PDF
//! export. The binary itself is self-contained, but PDF export depends on
//! one external tool: the WeasyPrint CLI (`brew install weasyprint`). It is
//! mandatory, with no fallback writer.
//!
//! ## Architecture at a glance
//!
//! ```text
//!  ┌────────────┐   text    ┌────────────┐   Document   ┌───────────────────────┐
//!  │  ui::App   │ ────────► │ parser.rs  │ ───────────► │ ui.rs (preview)       │
//!  │ (editor)   │           │ pulldown-  │              └───────────────────────┘
//!  └────────────┘ ◄──────── │ cmark      │ ◄───────────
//!         ▲        doc      └────────────┘   re-parsed     ┌─► export.rs (PDF:
//!                               on every change           │   raw Markdown →
//!                                                         │   html_export.rs →
//!                                                         │   WeasyPrint CLI)
//! ```
//!
//! * [`parser`] translates Markdown (CommonMark + GFM) into a dedicated,
//!   UI-independent document model ([`parser::Document`]).
//! * [`ui`] draws the editor, preview, tab strip, toolbar and status bar with
//!   egui.
//! * [`html_export`] renders the raw Markdown source as print-ready HTML
//!   (based on the reference implementation's pipeline: preprocessors, sanitizer, print
//!   template with `.markdown-body` CSS, plus typeset math and Mermaid images).
//! * [`export`] invokes the `weasyprint` CLI on that HTML and writes the PDF
//!   only on success; a missing or failing WeasyPrint is reported as an error.
//! * [`encoding`] decodes UTF-8/UTF-16/legacy files before parsing.
//! * [`macos_open`] integrates the Finder "Open With" Apple Events.
//! * [`macos_about`] adds the project credits to the standard About panel.
//!
//! ## Data flow
//!
//! 1. The user types into `App::text` (a `String`) for the active tab.
//! 2. On every change, `App::reparse()` calls [`parser::parse`] and replaces
//!    `App::doc`.
//! 3. The preview renders `App::doc`; the PDF export renders the raw
//!    `App::text` through the ported print pipeline (see
//!    [`html_export`]), so both views follow the same source. Inactive tabs
//!    are kept as snapshots in `App::tabs`.
//!
//! ## Rendering stack
//!
//! `eframe` opens a `winit` window and paints through `egui` using the **wgpu**
//! backend, which uses Metal on macOS. [`main`] requests the high-performance
//! GPU preference (the discrete/performance GPU or performance core cluster on
//! Apple Silicon).
//!
//! ## Extending
//!
//! * New Markdown block/inline type: see the docs on [`parser::Block`] and
//!   [`parser::Inline`] — there are exactly three integration points (parser,
//!   preview, print HTML).
//! * New UI element: `App::toolbar` / `App::tab_bar` / `App::status_bar` in
//!   `ui.rs`.
//! * New keyboard shortcut: `App::handle_shortcuts` in `ui.rs`.
//! * New export format: add another function next to `export_pdf` in
//!   `export.rs`, based on the raw Markdown source.

mod encoding;
mod export;
mod html_export;
#[cfg(target_os = "macos")]
mod macos_about;
mod macos_open;
#[cfg(target_os = "macos")]
mod macos_print;
mod parser;
mod svg;
#[cfg(test)]
mod test_fixtures;
mod ui;

use eframe::egui;
use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup};

fn main() {
    // Logging only on demand (use RUST_LOG=debug for details).
    // `MD_VIEWER_LOG_FILE=/path/to/log` redirects the log to a file, which is
    // useful when the app is launched by Finder (stderr is not visible then).
    let mut logger =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"));
    if let Ok(path) = std::env::var("MD_VIEWER_LOG_FILE")
        && let Ok(file) = std::fs::File::create(path)
    {
        logger.target(env_logger::Target::Pipe(Box::new(file)));
    }
    logger.init();

    // Register the macOS open-documents Apple Event handler before the event
    // loop starts, so cold-start "Open With" launches are not lost.
    #[cfg(target_os = "macos")]
    macos_open::install_early();

    // Add the project credits to the standard "About MD-Viewer" panel. The
    // credits block also makes AppKit grow the panel to fit both lines.
    #[cfg(target_os = "macos")]
    macos_about::install();

    // On Apple Silicon: HighPerformance selects the more capable GPU.
    // (On machines with only an integrated GPU this is a no-op.)
    let mut wgpu_options = WgpuConfiguration::default();
    if let WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
        setup.power_preference = eframe::egui_wgpu::wgpu::PowerPreference::HighPerformance;
    }

    // eframe installs its own default (the egui logo) as the *runtime* macOS
    // app icon when none is supplied. Finder, Launchpad and the Apps view use
    // the bundle's AppIcon.icns instead, so without this the Dock and the
    // minimized-window tile would show a different icon than the app bundle.
    // Supplying the same artwork also makes `cargo run` look correct, even
    // though a bare executable has no bundle resources.
    let app_icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/AppIcon-1024.png"))
        .expect("bundled app icon must be a valid PNG");

    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        wgpu_options,
        viewport: egui::ViewportBuilder::default()
            .with_title("MD-Viewer")
            .with_icon(app_icon)
            .with_inner_size([1280.0, 860.0])
            .with_min_inner_size([680.0, 440.0]),
        ..Default::default()
    };

    // Optional first argument: open that file at startup.
    let initial = std::env::args_os().nth(1).map(std::path::PathBuf::from);

    eframe::run_native(
        "MD-Viewer",
        native_options,
        // The app state is created lazily on the first frame.
        Box::new(move |cc| Ok(Box::new(ui::App::new(cc, initial)))),
    )
    .expect("eframe: failed to start the GUI");
}
