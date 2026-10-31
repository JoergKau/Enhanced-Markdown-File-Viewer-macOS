//! macOS About panel customization.
//!
//! winit installs the standard application menu. Its **About MD-Viewer** item
//! calls `orderFrontStandardAboutPanel:` on `NSApplication`, which builds the
//! panel from the bundle's `Info.plist` and shows no extra information.
//!
//! AppKit has no setter for panel options on macOS — only
//! `orderFrontStandardAboutPanelWithOptions:`. We therefore swizzle the
//! no-options method: when the menu item fires, our implementation shows the
//! same standard panel with a credits block attached. The credits occupy the
//! bottom of the panel and make AppKit grow it to fit both lines.
//!
//! The module is macOS-only and deliberately isolated: no other part of the
//! app depends on Objective-C.

/// Project page shown in the About panel.
const PROJECT_PAGE: &str = "https://github.com/JoergKau";

/// Credits block. Two lines at the bottom of the standard About panel.
fn credits_text() -> String {
    format!("Enhanced Markdown File Viewer for macOS\nProject Page: {PROJECT_PAGE}")
}

/// Installs the About panel credits. Returns `true` on success.
#[cfg(target_os = "macos")]
pub fn install() -> bool {
    imp::install()
}

#[cfg(target_os = "macos")]
mod imp {
    use objc2::ffi::{class_getInstanceMethod, method_setImplementation};
    use objc2::runtime::{AnyClass, AnyObject, Imp, Method, Sel};
    use objc2::{class, msg_send};
    use objc2_app_kit::NSAboutPanelOptionCredits;
    use objc2_foundation::{NSAttributedString, NSDictionary, NSString};

    pub fn install() -> bool {
        // SAFETY: main thread, NSApplication is a singleton. We replace one
        // method implementation; the replacement calls the options variant.
        unsafe {
            let cls = class!(NSApplication) as *const AnyClass as *mut AnyClass;
            let method: *const Method =
                class_getInstanceMethod(cls, Sel::register(c"orderFrontStandardAboutPanel:"));
            if method.is_null() {
                log::warn!("NSApplication.orderFrontStandardAboutPanel: not found");
                return false;
            }
            let imp: Imp = std::mem::transmute(order_front_about as *const ());
            method_setImplementation(method, imp);
        }
        log::info!("installed About panel credits");
        true
    }

    /// Replacement for `orderFrontStandardAboutPanel:`.
    ///
    /// Shows the standard panel with the project credits attached, so the
    /// About window grows and ends with the two project lines.
    unsafe extern "C-unwind" fn order_front_about(this: *mut AnyObject, _cmd: Sel) {
        log::info!("showing About panel with credits");
        let text = NSString::from_str(&super::credits_text());
        let credits = NSAttributedString::from_nsstring(&text);
        // The options dictionary stores objects as `AnyObject`.
        let credits_object: &AnyObject =
            unsafe { &*(&*credits as *const NSAttributedString).cast::<AnyObject>() };

        // SAFETY: `NSAboutPanelOptionCredits` is an AppKit constant.
        let key: &NSString = unsafe { NSAboutPanelOptionCredits };
        let options = NSDictionary::from_slices(&[key], &[credits_object]);

        let _: () = msg_send![
            this,
            orderFrontStandardAboutPanelWithOptions: &*options
        ];
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn credits_contain_the_project_page() {
        let text = super::credits_text();
        assert!(text.contains("Project Page: https://github.com/JoergKau"));
        assert_eq!(text.lines().count(), 2);
    }
}
