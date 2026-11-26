//! SVG input: rasterize hand-written vector art with `resvg` (pure Rust, no system fonts, so the
//! same file renders to the same pixels everywhere). Text elements are not rendered; convert text
//! to paths first.

use anyhow::{Context, Result};
use image::RgbaImage;
use resvg::{tiny_skia, usvg};

/// Whether `bytes` look like an SVG document (an `<svg` tag near the start).
#[must_use]
pub fn is_svg_data(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(1024)];
    let text = String::from_utf8_lossy(head);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    (trimmed.starts_with("<?xml") || trimmed.starts_with("<svg") || trimmed.starts_with("<!--"))
        && text.contains("<svg")
}

fn parse(data: &[u8]) -> Result<usvg::Tree> {
    usvg::Tree::from_data(data, &usvg::Options::default()).context("parsing SVG")
}

fn tree_size(tree: &usvg::Tree) -> (u32, u32) {
    let size = tree.size();
    (
        size.width().round().max(1.0) as u32,
        size.height().round().max(1.0) as u32,
    )
}

/// Natural pixel size of an SVG: its `width`/`height`, or its `viewBox` when those are absent.
///
/// # Errors
/// Returns an error if the document is not valid SVG.
pub fn svg_size(data: &[u8]) -> Result<(u32, u32)> {
    Ok(tree_size(&parse(data)?))
}

fn canvas_transform(tree: &usvg::Tree, width: u32, height: u32) -> tiny_skia::Transform {
    let size = tree.size();
    tiny_skia::Transform::from_scale(width as f32 / size.width(), height as f32 / size.height())
}

fn new_pixmap(width: u32, height: u32) -> Result<tiny_skia::Pixmap> {
    tiny_skia::Pixmap::new(width.max(1), height.max(1))
        .with_context(|| format!("allocating a {width}x{height} SVG canvas"))
}

fn pixmap_to_image(pixmap: &tiny_skia::Pixmap) -> RgbaImage {
    let raw = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    RgbaImage::from_raw(pixmap.width(), pixmap.height(), raw)
        .unwrap_or_else(|| RgbaImage::new(pixmap.width(), pixmap.height()))
}

/// Rasterize a whole SVG, stretched to exactly `width` x `height`, keeping its transparency.
///
/// # Errors
/// Returns an error if the document is not valid SVG.
pub fn rasterize(data: &[u8], width: u32, height: u32) -> Result<RgbaImage> {
    let tree = parse(data)?;
    let mut pixmap = new_pixmap(width, height)?;
    resvg::render(
        &tree,
        canvas_transform(&tree, width, height),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap_to_image(&pixmap))
}

/// Rasterize each top-level element of an SVG on its own `width` x `height` canvas, in document
/// order. Wrapper groups that hold a single child are looked through, so a document whose shapes
/// sit in one `<g>` still yields one image per shape; any other `<g>` counts as one shape.
///
/// # Errors
/// Returns an error if the document is not valid SVG.
pub fn rasterize_parts(data: &[u8], width: u32, height: u32) -> Result<Vec<RgbaImage>> {
    let tree = parse(data)?;
    let mut level = tree.root();
    while let [usvg::Node::Group(only)] = level.children() {
        level = only;
    }
    let base = canvas_transform(&tree, width, height);
    let mut parts = Vec::new();
    for node in level.children() {
        let Some(bbox) = node.abs_layer_bounding_box() else {
            continue;
        };
        let mut pixmap = new_pixmap(width, height)?;
        let transform = base
            .pre_concat(level.abs_transform())
            .pre_translate(bbox.x(), bbox.y());
        resvg::render_node(node, transform, &mut pixmap.as_mut());
        parts.push(pixmap_to_image(&pixmap));
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_DOTS: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="-1 -1 2 2">
  <g transform="translate(0.1 0)"><circle cx="-0.5" cy="0" r="0.3" fill="#000"/></g>
  <circle cx="0.5" cy="0" r="0.3" fill="#fff" fill-opacity="0.5"/>
</svg>"##;

    #[test]
    fn detects_svg_and_reads_view_box_size() {
        assert!(is_svg_data(TWO_DOTS.as_bytes()));
        assert!(is_svg_data(b"<?xml version=\"1.0\"?>\n<svg/>"));
        assert!(!is_svg_data(b"\x89PNG\r\n"));
        assert_eq!(svg_size(TWO_DOTS.as_bytes()).unwrap(), (2, 2));
    }

    #[test]
    fn parts_line_up_with_the_whole_render() {
        let whole = rasterize(TWO_DOTS.as_bytes(), 64, 64).unwrap();
        let parts = rasterize_parts(TWO_DOTS.as_bytes(), 64, 64).unwrap();
        assert_eq!(parts.len(), 2);
        // Left dot is opaque black around (-0.4, 0) -> pixel (19, 32); right dot half-transparent white.
        assert_eq!(whole.get_pixel(19, 32).0, [0, 0, 0, 255]);
        assert_eq!(parts[0].get_pixel(19, 32).0, [0, 0, 0, 255]);
        assert_eq!(parts[0].get_pixel(48, 32)[3], 0);
        let right = parts[1].get_pixel(48, 32);
        assert!((120..=135).contains(&right[3]), "{right:?}");
        assert_eq!(right.0, whole.get_pixel(48, 32).0);
        assert_eq!(whole.get_pixel(0, 0)[3], 0);
        assert_eq!(
            rasterize(TWO_DOTS.as_bytes(), 64, 64).unwrap(),
            whole,
            "deterministic"
        );
    }

    #[test]
    fn invalid_svg_is_an_error() {
        assert!(rasterize(b"<svg", 4, 4).is_err());
    }
}
