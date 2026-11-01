//! SVG rasterization shared by the preview, the PDF export and the image
//! loaders.
//!
//! Every caller goes through [`options`], which loads the system font database
//! once for the whole process. Scanning the system fonts for every diagram was
//! the dominant cost of rendering documents with several Mermaid blocks.

use std::sync::{Arc, LazyLock};

use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg;

static FONTS: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
    let mut db = usvg::fontdb::Database::new();
    db.load_system_fonts();
    Arc::new(db)
});

/// usvg options backed by the shared, system-font-aware database.
pub(crate) fn options() -> usvg::Options<'static> {
    usvg::Options {
        fontdb: FONTS.clone(),
        ..Default::default()
    }
}

/// A rasterized SVG and its natural (intrinsic) size in CSS pixels.
pub(crate) struct Raster {
    pub pixmap: Pixmap,
    pub width: f32,
    pub height: f32,
}

/// Parses and renders `svg` at `scale` pixels per CSS pixel.
///
/// The scale is lowered so the bitmap's longer side stays within `max_side`,
/// which bounds memory and GPU texture size for very large or very tall
/// diagrams. Returns `None` for invalid or empty documents.
pub(crate) fn rasterize(svg: &str, scale: f32, max_side: f32) -> Option<Raster> {
    let tree = usvg::Tree::from_str(svg, &options()).ok()?;
    let natural = tree.size();
    if natural.width() <= 0.0 || natural.height() <= 0.0 {
        return None;
    }
    let longer = natural.width().max(natural.height()).max(1.0);
    let scale = scale.min(max_side / longer);
    let w = (natural.width() * scale).ceil().max(1.0) as u32;
    let h = (natural.height() * scale).ceil().max(1.0) as u32;
    let mut pixmap = Pixmap::new(w, h)?;
    resvg::render(
        &tree,
        Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Some(Raster {
        pixmap,
        width: natural.width(),
        height: natural.height(),
    })
}
