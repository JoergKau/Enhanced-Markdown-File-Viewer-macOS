//! Native macOS print dialog for a generated PDF.
//!
//! PDFKit's `PDFDocument` can build an `NSPrintOperation` for a PDF file, so
//! **Print** shows the standard macOS print panel instead of opening
//! Preview.app. This keeps MD-Viewer frontmost and lets the user choose the
//! printer, copies and page range.
//!
//! Must be called on the main thread (the UI thread).

/// Shows the macOS print dialog for `path`. Returns an error when the PDF
/// cannot be opened or the operation fails.
#[cfg(target_os = "macos")]
pub fn print_pdf(path: &std::path::Path) -> Result<(), String> {
    imp::print_pdf(path)
}

#[cfg(target_os = "macos")]
mod imp {
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSPrintInfo};
    use objc2_foundation::{NSString, NSURL};
    use objc2_pdf_kit::{PDFDocument, PDFPrintScalingMode};
    use std::path::Path;

    pub fn print_pdf(path: &Path) -> Result<(), String> {
        let mtm = MainThreadMarker::new().ok_or("printing must run on the main thread")?;
        let path_str = path.to_str().ok_or("the PDF path is not valid UTF-8")?;
        let ns_path = NSString::from_str(path_str);
        let url = NSURL::fileURLWithPath(&ns_path);

        // SAFETY: main thread; the URL points at the PDF we just wrote.
        let document = unsafe { PDFDocument::initWithURL(PDFDocument::alloc(), &url) }
            .ok_or("could not open the generated PDF")?;
        let print_info = NSPrintInfo::sharedPrintInfo();
        // SAFETY: PDFKit creates the operation on the main thread; the print
        // info is the shared system configuration.
        let operation = unsafe {
            document.printOperationForPrintInfo_scalingMode_autoRotate(
                Some(&print_info),
                PDFPrintScalingMode::PageScaleDownToFit,
                true,
                mtm,
            )
        }
        .ok_or("could not create the print operation")?;

        // Attach the panel as a sheet to the main window. A free-standing
        // panel can end up behind the window, leaving the app blocked with no
        // visible dialog.
        let app = NSApplication::sharedApplication(mtm);
        if let Some(window) = app.keyWindow().or_else(|| app.mainWindow()) {
            // SAFETY: main thread; the sheet is retained by AppKit.
            unsafe {
                operation.runOperationModalForWindow_delegate_didRunSelector_contextInfo(
                    &window,
                    None,
                    None,
                    std::ptr::null_mut(),
                );
            }
            Ok(())
        } else if operation.runOperation() {
            Ok(())
        } else {
            Err("printing was cancelled".into())
        }
    }
}
