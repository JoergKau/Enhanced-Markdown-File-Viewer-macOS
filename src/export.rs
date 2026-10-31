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
//!        └─ temp file
//!             └─ weasyprint CLI       → PDF at the target path
//! ```
//!
//! # Extending
//!
//! * Styling/layout: edit the template and CSS in `html_export.rs`
//!   (`prepare_print_html`, `MARKDOWN_BODY_CSS`, `PRINT_CSS`).
//! * WeasyPrint lookup: extend [`WEASYPRINT_CANDIDATES`].

use crate::html_export;
use std::io;
use std::path::{Path, PathBuf};

/// Candidate executables for the WeasyPrint CLI.
///
/// GUI apps launched from Finder inherit a minimal `PATH` (`/usr/bin:/bin`),
/// so Homebrew locations are probed explicitly in addition to `PATH`.
const WEASYPRINT_CANDIDATES: [&str; 4] = [
    "weasyprint",
    "/opt/homebrew/bin/weasyprint", // Apple Silicon Homebrew
    "/usr/local/bin/weasyprint",    // Intel Homebrew / manual install
    "/usr/bin/weasyprint",          // system install
];

/// Footer label used for a source without a usable file name.
const DEFAULT_LABEL: &str = "document";

/// Exports the raw Markdown source `markdown` as PDF to `path`.
///
/// WeasyPrint is mandatory: on failure an `io::Error` is returned (shown in
/// the status bar) and no output file is written — there is no fallback
/// writer.
///
/// A missing `.pdf` extension is added automatically. `base_dir` is the
/// folder of the Markdown source (for relative image paths), `source_path`
/// supplies the file name used in the running footer and defaults to
/// [`DEFAULT_LABEL`] when absent. `source_path` is usually `App::path`.
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

/// Adds a `.pdf` extension unless `path` already ends in one (any case).
fn pdf_output_path(path: &Path) -> PathBuf {
    let mut p = path.to_path_buf();
    if p.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("pdf"))
        != Some(true)
    {
        p.set_extension("pdf");
    }
    p
}

/// File name shown in the running footer (source stem or [`DEFAULT_LABEL`]).
fn footer_label(source_path: Option<&Path>) -> &str {
    source_path
        .and_then(|s| s.file_stem().and_then(|s| s.to_str()))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(DEFAULT_LABEL)
}

/// Markdown → HTML → WeasyPrint CLI.
///
/// Renders the print HTML (`html_export::print_html_for`), writes it to a
/// temporary file, runs WeasyPrint and deletes the temporary file again.
/// Every failure (temporary file, missing binary, non-zero exit) is logged
/// with context and returned as an error carrying WeasyPrint's stderr.
fn weasyprint_export(
    markdown: &str,
    base_dir: Option<&Path>,
    label: &str,
    out: &Path,
) -> io::Result<()> {
    log::info!("[pdf_export] Exporting to {} via WeasyPrint", out.display());
    let html = html_export::print_html_for(markdown, base_dir, label);
    let tmp = temp_html_path(out);
    std::fs::write(&tmp, html).map_err(|e| {
        io::Error::other(format!(
            "cannot write temporary HTML to {}: {e}",
            tmp.display()
        ))
    })?;
    let result = run_weasyprint(&WEASYPRINT_CANDIDATES, &tmp, out);
    let _ = std::fs::remove_file(&tmp);
    result.map_err(|msg| {
        log::error!("[pdf_export] WeasyPrint export failed: {msg}");
        io::Error::other(msg)
    })
}

/// Returns the WeasyPrint executable that PDF export will use, if present.
///
/// GUI apps launched from Finder inherit a minimal `PATH`, so the Homebrew
/// locations are probed explicitly in addition to `PATH`. This is a cheap
/// file-system check (no process is spawned), used by the startup onboarding
/// dialog and before opening the export dialog.
pub fn weasyprint_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("weasyprint");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    WEASYPRINT_CANDIDATES
        .iter()
        .filter(|c| c.starts_with('/'))
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

/// Invokes the first available WeasyPrint candidate.
///
/// `NotFound` spawn errors move on to the next candidate; a running but
/// failing WeasyPrint aborts immediately with its stderr (all candidates
/// would fail identically on the same HTML).
fn run_weasyprint(candidates: &[&str], html: &Path, pdf: &Path) -> Result<(), String> {
    use std::io::ErrorKind::NotFound;
    use std::process::Command;

    for cand in candidates {
        match Command::new(cand).arg(html).arg(pdf).output() {
            Err(e) if e.kind() == NotFound => continue,
            Err(e) => return Err(format!("{cand}: {e}")),
            Ok(o) if o.status.success() => return Ok(()),
            Ok(o) => {
                return Err(format!(
                    "{cand} exited with {}: {}",
                    o.status,
                    String::from_utf8_lossy(&o.stderr).trim()
                ));
            }
        }
    }
    Err(
        "WeasyPrint not found (checked PATH, /opt/homebrew/bin, /usr/local/bin, /usr/bin). \
         Please install it, e.g. `brew install weasyprint`."
            .into(),
    )
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

    #[test]
    fn footer_label_from_source_path() {
        assert_eq!(
            footer_label(Some(Path::new("docs/My Notes.md"))),
            "My Notes"
        );
        assert_eq!(footer_label(None), DEFAULT_LABEL);
        assert_eq!(footer_label(Some(Path::new("/tmp/.md"))), ".md");
    }

    /// True when the first WeasyPrint candidate runs successfully.
    fn weasyprint_available() -> bool {
        WEASYPRINT_CANDIDATES.iter().any(|c| {
            std::process::Command::new(c)
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        })
    }

    #[test]
    fn weasyprint_not_found_is_an_error() {
        let dir = unique_test_dir("notfound");
        let (html, pdf) = (dir.join("in.html"), dir.join("out.pdf"));
        std::fs::write(&html, "<p>x</p>").unwrap();
        let err = run_weasyprint(&["/nonexistent/weasyprint-xyz"], &html, &pdf).unwrap_err();
        assert!(err.contains("not found"), "unexpected error message: {err}");
    }

    #[test]
    fn failing_weasyprint_reports_stderr() {
        let dir = unique_test_dir("failing");
        let (html, pdf) = (dir.join("in.html"), dir.join("out.pdf"));
        std::fs::write(&html, "<p>x</p>").unwrap();
        // `/usr/bin/false` exists on macOS and exits non-zero immediately.
        let err = run_weasyprint(&["/usr/bin/false"], &html, &pdf).unwrap_err();
        assert!(
            err.contains("exited with"),
            "unexpected error message: {err}"
        );
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
