//! PDF export — mandatory WeasyPrint pipeline (ported from the reference implementation).
//!
//! [`export_pdf`] renders the raw Markdown source into a complete,
//! print-ready HTML file ([`crate::html_export`]), writes it to a temporary
//! file and invokes the external `weasyprint` CLI. This gains real typography
//! for free: `@page` pagination with running footers, `break-inside: avoid`,
//! clickable link annotations, selectable text and system-font Unicode
//! fallback.
//!
//! WeasyPrint is the *only* supported export path: there is deliberately no
//! fallback writer, so a missing or failing `weasyprint` binary is reported
//! to the user as an error instead of producing a low-quality (or invalid)
//! PDF. Keeping a single export path means there is exactly one renderer to
//! keep in sync with the preview.
//!
//! # Pipeline
//!
//! ```text
//! raw Markdown
//!   └─ html_export::print_html_for()  → sanitized print HTML
//!        └─ temp HTML file
//!             └─ weasyprint CLI      → temp PDF beside the target
//!                  └─ rename          → PDF at the target path
//! ```
//!
//! # Extending
//!
//! * Styling/layout: edit the template and CSS in `html_export.rs`
//!   (`prepare_print_html`, `MARKDOWN_BODY_CSS`, `PRINT_CSS`).
//! * WeasyPrint lookup: `PATH` is always searched first; add a fixed
//!   location to [`WEASYPRINT_FALLBACKS`].

use crate::html_export;
use std::io;
use std::path::{Path, PathBuf};

/// Fixed locations probed when `weasyprint` is not on `PATH`.
///
/// GUI apps launched from Finder inherit a minimal `PATH` (`/usr/bin:/bin`),
/// so Homebrew locations are probed explicitly.
const WEASYPRINT_FALLBACKS: [&str; 3] = [
    "/opt/homebrew/bin/weasyprint", // Apple Silicon Homebrew
    "/usr/local/bin/weasyprint",    // Intel Homebrew / manual install
    "/usr/bin/weasyprint",          // system install
];

/// Error text when no WeasyPrint executable is found.
const MISSING_WEASYPRINT: &str = "WeasyPrint not found (checked PATH, /opt/homebrew/bin, /usr/local/bin, /usr/bin). \
     Please install it, e.g. `brew install weasyprint`.";

/// Footer label used for a source without a usable file name.
const DEFAULT_LABEL: &str = "document";

/// Exports the raw Markdown source `markdown` as PDF to `path`.
///
/// WeasyPrint is mandatory: on failure an `io::Error` is returned (shown in
/// the status bar), and an existing file at `path` is left untouched — there
/// is no fallback writer.
///
/// `.pdf` is appended to the file name unless it already ends in `.pdf` (any
/// case). `base_dir` is the folder of the Markdown source (for relative image
/// paths), `source_path` supplies the file name used in the running footer
/// and defaults to [`DEFAULT_LABEL`] when absent. `source_path` is usually
/// `App::path`.
pub fn export_pdf(
    markdown: &str,
    base_dir: Option<&Path>,
    source_path: Option<&Path>,
    path: &Path,
) -> io::Result<PathBuf> {
    let p = pdf_output_path(path);
    let label = footer_label(source_path);
    weasyprint_export(markdown, base_dir, label, &p)?;
    Ok(p)
}

/// Appends `.pdf` to the file name unless it already ends in one (any case).
/// Existing extensions are kept: `Q3.final` becomes `Q3.final.pdf`.
fn pdf_output_path(path: &Path) -> PathBuf {
    if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
    {
        return path.to_path_buf();
    }
    let mut name = path.as_os_str().to_owned();
    name.push(".pdf");
    PathBuf::from(name)
}

/// File name shown in the running footer (source stem or [`DEFAULT_LABEL`]).
fn footer_label(source_path: Option<&Path>) -> &str {
    source_path
        .and_then(|s| s.file_stem().and_then(|s| s.to_str()))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(DEFAULT_LABEL)
}

/// Markdown → HTML → WeasyPrint CLI, with every failure logged and returned
/// as an error carrying the cause (WeasyPrint's stderr when it ran).
fn weasyprint_export(
    markdown: &str,
    base_dir: Option<&Path>,
    label: &str,
    out: &Path,
) -> io::Result<()> {
    let result = match weasyprint_path() {
        Some(program) => render_pdf_with(&program, markdown, base_dir, label, out),
        None => Err(MISSING_WEASYPRINT.to_string()),
    };
    result.map_err(|msg| {
        log::error!("[pdf_export] WeasyPrint export failed: {msg}");
        io::Error::other(msg)
    })
}

/// Renders the print HTML, runs `program` on it and moves the result to `out`.
///
/// The PDF is written beside `out` and only renamed over it on success, so a
/// failed run never truncates an existing file. Temporary files are removed
/// in every case.
fn render_pdf_with(
    program: &Path,
    markdown: &str,
    base_dir: Option<&Path>,
    label: &str,
    out: &Path,
) -> Result<(), String> {
    log::info!(
        "[pdf_export] Exporting to {} via {}",
        out.display(),
        program.display()
    );
    let html = html_export::print_html_for(markdown, base_dir, label);
    let tmp_html = temp_html_path(out);
    std::fs::write(&tmp_html, html)
        .map_err(|e| format!("cannot write temporary HTML to {}: {e}", tmp_html.display()))?;
    let tmp_pdf = temp_pdf_path(out);
    let result = run_weasyprint(program, &tmp_html, &tmp_pdf).and_then(|()| {
        std::fs::rename(&tmp_pdf, out).map_err(|e| format!("cannot write {}: {e}", out.display()))
    });
    let _ = std::fs::remove_file(&tmp_html);
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_pdf);
    }
    result
}

/// Returns the WeasyPrint executable that PDF export will use, if present.
///
/// `PATH` is searched first, then [`WEASYPRINT_FALLBACKS`]. This is a cheap
/// file-system check (no process is spawned), used by the startup onboarding
/// dialog and before opening the export dialog.
pub fn weasyprint_path() -> Option<PathBuf> {
    let on_path = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("weasyprint"))
            .find(|candidate| candidate.is_file())
    });
    on_path.or_else(|| {
        WEASYPRINT_FALLBACKS
            .iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
    })
}

/// Runs `program html pdf`. A spawn failure names the program; a non-zero
/// exit returns WeasyPrint's stderr.
fn run_weasyprint(program: &Path, html: &Path, pdf: &Path) -> Result<(), String> {
    let output = std::process::Command::new(program)
        .arg(html)
        .arg(pdf)
        .output()
        .map_err(|e| format!("{}: {e}", program.display()))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{} exited with {}: {}",
        program.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

/// Unique temporary HTML file in the system temp directory:
/// `mdviewer-print-<slug>-<pid>-<ms>.html`. Reuses nothing, so concurrent
/// exports in different windows cannot collide.
fn temp_html_path(out: &Path) -> PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let stem = out
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("document");
    let slug: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ' ' {
                c
            } else {
                '-'
            }
        })
        .collect();
    std::env::temp_dir().join(format!(
        "mdviewer-print-{slug}-{}-{ts}.html",
        std::process::id()
    ))
}

/// Hidden temporary PDF next to `out`, so the final rename stays on one
/// volume: `.<name>.<pid>.partial.pdf`.
fn temp_pdf_path(out: &Path) -> PathBuf {
    let name = out
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("document.pdf");
    out.with_file_name(format!(".{name}.{}.partial.pdf", std::process::id()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_extension_is_added() {
        assert_eq!(
            pdf_output_path(Path::new("/tmp/report"))
                .file_name()
                .unwrap(),
            "report.pdf"
        );
        assert_eq!(
            pdf_output_path(Path::new("/tmp/report.PDF"))
                .file_name()
                .unwrap(),
            "report.PDF"
        );
        assert_eq!(
            pdf_output_path(Path::new("/tmp/report.pdf"))
                .file_name()
                .unwrap(),
            "report.pdf"
        );
    }

    /// The existing extension is kept and `.pdf` is appended after it.
    #[test]
    fn pdf_extension_is_appended_after_existing_extension() {
        assert_eq!(
            pdf_output_path(Path::new("/tmp/Q3.final")),
            Path::new("/tmp/Q3.final.pdf")
        );
    }

    #[test]
    fn footer_label_from_source_path() {
        assert_eq!(
            footer_label(Some(Path::new("docs/My Notes.md"))),
            "My Notes"
        );
        assert_eq!(footer_label(None), DEFAULT_LABEL);
        assert_eq!(footer_label(Some(Path::new("/tmp/.md"))), ".md");
    }

    /// True when WeasyPrint is installed and runs.
    fn weasyprint_available() -> bool {
        weasyprint_path().is_some_and(|program| {
            std::process::Command::new(program)
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        })
    }

    #[test]
    fn missing_program_is_an_error() {
        let dir = unique_test_dir("notfound");
        let (html, pdf) = (dir.join("in.html"), dir.join("out.pdf"));
        std::fs::write(&html, "<p>x</p>").unwrap();
        let err =
            run_weasyprint(Path::new("/nonexistent/weasyprint-xyz"), &html, &pdf).unwrap_err();
        assert!(
            err.contains("/nonexistent/weasyprint-xyz"),
            "unexpected error message: {err}"
        );
    }

    #[test]
    fn failing_weasyprint_reports_stderr() {
        let dir = unique_test_dir("failing");
        let (html, pdf) = (dir.join("in.html"), dir.join("out.pdf"));
        std::fs::write(&html, "<p>x</p>").unwrap();
        // `/usr/bin/false` exists on macOS and exits non-zero immediately.
        let err = run_weasyprint(Path::new("/usr/bin/false"), &html, &pdf).unwrap_err();
        assert!(
            err.contains("exited with"),
            "unexpected error message: {err}"
        );
    }

    /// A failing run must leave an existing PDF untouched and no temporary
    /// file behind.
    #[test]
    fn failed_export_keeps_existing_pdf() {
        let dir = unique_test_dir("keep");
        let out = dir.join("out.pdf");
        std::fs::write(&out, b"old").unwrap();
        let err = render_pdf_with(Path::new("/usr/bin/false"), "# x", None, "x", &out).unwrap_err();
        assert!(
            err.contains("exited with"),
            "unexpected error message: {err}"
        );
        assert_eq!(std::fs::read(&out).unwrap(), b"old");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }

    /// The real export path on a machine with WeasyPrint installed must
    /// produce an actual PDF (skipped otherwise).
    #[test]
    fn weasyprint_export_writes_valid_pdf() {
        if !weasyprint_available() {
            eprintln!("skipping: WeasyPrint not installed");
            return;
        }
        let dir = unique_test_dir("e2e");
        let source = dir.join("source.md");
        let markdown = "# Export Test\n\nSome **content** and a [link](https://example.org).\n";
        std::fs::write(&source, markdown).unwrap();
        let written = export_pdf(markdown, Some(&dir), Some(&source), &dir.join("out")).unwrap();
        let bytes = std::fs::read(&written).unwrap();
        assert!(bytes.starts_with(b"%PDF-"), "not a valid PDF");
        assert!(bytes.len() > 1_000, "PDF suspiciously small");
    }

    fn unique_test_dir(tag: &str) -> PathBuf {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "mdviewer-export-test-{tag}-{}-{ts}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
