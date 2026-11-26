//! Image export (PNG/WebP/JPEG/GIF/BMP/TIFF, vector SVG, Kitty graphics, Braille, ASCII) and stats.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::{self, Cursor, IsTerminal};
use std::path::Path;

use anyhow::{Context, Result};
use image::{DynamicImage, ImageFormat, RgbaImage};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::color::Rgb;
use crate::deflate::Effort;
use crate::png_optimize::write_chunk;

/// Maximum Base64 payload bytes per Kitty graphics protocol chunk (per Kitty spec).
const KITTY_CHUNK_SIZE: usize = 4096;

/// Supported output file and terminal formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
pub enum OutputFormat {
    /// Lossless PNG (recommended for crisp pixel-exact dithers).
    #[default]
    Png,
    /// Lossless WebP.
    Webp,
    /// JPEG (flattens alpha).
    Jpeg,
    /// Animated-capable GIF (crisp indexed output).
    Gif,
    /// Lossless BMP (palettized 1/4/8-bit when opaque with at most 256 colors).
    Bmp,
    /// Lossless Deflate-compressed TIFF.
    Tiff,
    /// Crisp vector SVG (run-length merged rects or halftone circles/dots).
    Svg,
    /// Pixel-exact inline terminal preview using the Kitty graphics protocol (Kitty, Ghostty, `WezTerm`).
    Kitty,
    /// High-density Unicode Braille text art (2x4 subpixels per glyph).
    Braille,
    /// Classic shaded ASCII text art.
    Ascii,
}

/// Shape primitives used when exporting `--format svg`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "kebab-case")]
pub enum SvgShape {
    /// Horizontal run-length merged crisp pixel runs, layered by color (smallest file size).
    #[default]
    Rect,
    /// Circular stipple/halftone dots per non-background pixel.
    Circle,
    /// Slightly inset rounded dots with delicate spacing.
    Dot,
}

/// Options controlling Kitty graphics protocol terminal rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KittyOptions {
    /// Optional display width in terminal character columns (`c=`).
    pub cols: Option<u32>,
    /// Optional display height in terminal character rows (`r=`).
    pub rows: Option<u32>,
    /// Wrap escape sequences in tmux DCS passthrough (`\x1bPtmux;...\x1b\\`).
    pub tmux_passthrough: bool,
    /// Append a trailing newline after the final chunk (disable for exact cursor placement in fullscreen).
    pub trailing_newline: bool,
    /// Emit `C=1` so the terminal does not advance the text cursor after placing the image.
    pub no_cursor_move: bool,
    /// Optional stable Kitty image ID (`i=N,p=1`) for flicker-free atomic in-place frame updates.
    pub image_id: Option<u32>,
}

impl Default for KittyOptions {
    fn default() -> Self {
        Self {
            cols: None,
            rows: None,
            tmux_passthrough: false,
            trailing_newline: true,
            no_cursor_move: false,
            image_id: None,
        }
    }
}

impl KittyOptions {
    /// Enable tmux passthrough automatically only when running interactively inside tmux.
    #[must_use]
    pub fn should_use_tmux_passthrough() -> bool {
        std::env::var_os("TMUX").is_some() && io::stdout().is_terminal()
    }
}

/// Escape sequence that clears all Kitty graphics placements from the current screen.
#[must_use]
pub const fn kitty_clear_sequence(tmux_passthrough: bool) -> &'static str {
    if tmux_passthrough {
        "\x1bPtmux;\x1b\x1b_Ga=d,d=A,q=2\x1b\x1b\\\x1b\\"
    } else {
        "\x1b_Ga=d,d=A,q=2\x1b\\"
    }
}

/// Options controlling vector SVG, Kitty graphics, and text rendering.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ExportOptions {
    pub format: Option<OutputFormat>,
    pub svg_shape: SvgShape,
    /// Skip emitting SVG shapes for the most frequent color (or explicit transparent background).
    pub svg_skip_bg: bool,
    /// Maximum character columns when rendering `braille` or `ascii`.
    pub text_width: Option<u32>,
    /// Display width in terminal columns when rendering inline with Kitty graphics (`c=`).
    pub view_cols: Option<u32>,
    /// Display height in terminal rows when rendering inline with Kitty graphics (`r=`).
    pub view_rows: Option<u32>,
    /// Spend extra CPU on the smallest lossless PNG/APNG (palette, filter, and Zopfli search).
    #[serde(default)]
    pub optimize: bool,
}

/// Per-color usage statistics returned in structured JSON output.
#[derive(Debug, Clone, Serialize)]
pub struct ColorUsage {
    pub hex: String,
    pub pixels: u64,
    pub percent: f64,
}

impl OutputFormat {
    /// Infer format from file extension when `--format` is not explicitly specified.
    #[must_use]
    pub fn infer_from_path(path: &Path) -> Self {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("webp") => Self::Webp,
            Some("jpg" | "jpeg") => Self::Jpeg,
            Some("gif") => Self::Gif,
            Some("bmp") => Self::Bmp,
            Some("tif" | "tiff") => Self::Tiff,
            Some("svg") => Self::Svg,
            Some("kitty") => Self::Kitty,
            Some("braille") => Self::Braille,
            Some("txt" | "asc" | "ascii") => Self::Ascii,
            _ => Self::Png,
        }
    }

    /// Canonical CLI slug.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Webp => "webp",
            Self::Jpeg => "jpeg",
            Self::Gif => "gif",
            Self::Bmp => "bmp",
            Self::Tiff => "tiff",
            Self::Svg => "svg",
            Self::Kitty => "kitty",
            Self::Braille => "braille",
            Self::Ascii => "ascii",
        }
    }

    /// Whether this format emits UTF-8 text or terminal escape sequences directly to stdout.
    #[must_use]
    pub const fn is_text(self) -> bool {
        matches!(self, Self::Svg | Self::Kitty | Self::Braille | Self::Ascii)
    }
}

/// Compute palette color usage histogram (opaque pixels) sorted by descending frequency.
#[must_use]
pub fn color_histogram(img: &RgbaImage) -> Vec<ColorUsage> {
    type Counts = HashMap<u32, u64, BuildHasherDefault<PixelKeyHasher>>;
    let add = |mut into: Counts, from: Counts| {
        for (key, pixels) in from {
            *into.entry(key).or_insert(0) += pixels;
        }
        into
    };
    let counts = img
        .as_raw()
        .par_chunks(4 * 16_384)
        .fold(Counts::default, |mut counts, chunk| {
            // Dithered output repeats a few colors, so count runs before touching the map.
            let mut run_key = 0u32;
            let mut run = 0u64;
            for px in chunk.as_chunks::<4>().0 {
                if px[3] == 0 {
                    continue;
                }
                let key = u32::from_be_bytes([0, px[0], px[1], px[2]]);
                if key == run_key {
                    run += 1;
                } else {
                    if run > 0 {
                        *counts.entry(run_key).or_insert(0) += run;
                    }
                    run_key = key;
                    run = 1;
                }
            }
            if run > 0 {
                *counts.entry(run_key).or_insert(0) += run;
            }
            counts
        })
        .reduce(Counts::default, add);
    let total: u64 = counts.values().sum();
    let denom = total.max(1) as f64;
    let mut rows: Vec<ColorUsage> = counts
        .into_iter()
        .map(|(key, pixels)| {
            let [_, r, g, b] = key.to_be_bytes();
            ColorUsage {
                hex: Rgb::new(r, g, b).to_hex(),
                pixels,
                percent: ((pixels as f64 / denom) * 10_000.0).round() / 100.0,
            }
        })
        .collect();
    rows.sort_by(|a, b| b.pixels.cmp(&a.pixels).then_with(|| a.hex.cmp(&b.hex)));
    rows
}

/// Hasher for packed `0x00RRGGBB` keys: one multiply, folded so every input bit reaches the
/// low bits the table indexes by.
#[derive(Default)]
struct PixelKeyHasher(u64);

impl Hasher for PixelKeyHasher {
    fn finish(&self) -> u64 {
        self.0 ^ (self.0 >> 29)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.write_u32(u32::from(byte));
        }
    }

    fn write_u32(&mut self, value: u32) {
        self.0 = (self.0 ^ u64::from(value)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

/// Encode `img` into raw bytes in the requested format.
///
/// # Errors
/// Returns an error if the image encoder fails.
pub fn encode_image(
    img: &RgbaImage,
    format: OutputFormat,
    export: &ExportOptions,
) -> Result<Vec<u8>> {
    match format {
        OutputFormat::Svg => Ok(render_svg(img, export.svg_shape, export.svg_skip_bg).into_bytes()),
        OutputFormat::Kitty => {
            let kitty_opts = KittyOptions {
                cols: export.view_cols.or(export.text_width),
                rows: export.view_rows,
                tmux_passthrough: KittyOptions::should_use_tmux_passthrough(),
                trailing_newline: true,
                no_cursor_move: false,
                image_id: None,
            };
            Ok(render_kitty(img, kitty_opts)?.into_bytes())
        }
        OutputFormat::Braille => Ok(render_braille(img, export.text_width).into_bytes()),
        OutputFormat::Ascii => Ok(render_ascii(img, export.text_width).into_bytes()),
        OutputFormat::Png if export.optimize => encode_optimized_png_bytes(img),
        OutputFormat::Png => encode_png_bytes(img),
        OutputFormat::Webp => crate::webp::encode_lossless(img),
        OutputFormat::Bmp => encode_bmp(img),
        OutputFormat::Tiff => encode_tiff(img),
        OutputFormat::Jpeg | OutputFormat::Gif => {
            let dyn_img = if matches!(format, OutputFormat::Jpeg) {
                DynamicImage::ImageRgba8(img.clone()).to_rgb8().into()
            } else {
                DynamicImage::ImageRgba8(img.clone())
            };
            let img_fmt = match format {
                OutputFormat::Jpeg => ImageFormat::Jpeg,
                OutputFormat::Gif => ImageFormat::Gif,
                OutputFormat::Png
                | OutputFormat::Webp
                | OutputFormat::Bmp
                | OutputFormat::Tiff
                | OutputFormat::Svg
                | OutputFormat::Kitty
                | OutputFormat::Braille
                | OutputFormat::Ascii => {
                    unreachable!("PNG, WebP, BMP, TIFF, and text/terminal formats handled above")
                }
            };
            let mut cursor = Cursor::new(Vec::new());
            dyn_img
                .write_to(&mut cursor, img_fmt)
                .with_context(|| format!("encoding {} output", format.id()))?;
            Ok(cursor.into_inner())
        }
    }
}

/// Write encoded bytes to disk, creating parent directories automatically.
///
/// # Errors
/// Returns an error if directories cannot be created or the file cannot be written.
pub fn write_output_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating parent directory '{}'", parent.display()))?;
    }
    fs::write(path, bytes).with_context(|| format!("writing output file '{}'", path.display()))
}

/// Paint class of pixels no SVG run may cover: transparent holes and the background color.
const SVG_BARRIER: u32 = u32::MAX;

/// Render a dithered image as a compact, crisp SVG.
///
/// `Rect` mode strokes horizontal pixel runs as 1px-wide lines through the row centres, one
/// `<path>` per color. Opaque colors are painted most frequent first and a run may pass under
/// pixels a later color repaints, so runs merge.
/// Dot modes trace every dot as two filled semicircle arcs in one `<path>` per color. Colors use
/// the shortest hex form, so files stay lean for web and design tooling.
#[must_use]
pub fn render_svg(img: &RgbaImage, shape: SvgShape, skip_bg: bool) -> String {
    let (w, h) = img.dimensions();
    let IndexedColors {
        colors,
        counts,
        indices,
    } = index_colors(img);

    let bg_color: Option<[u8; 3]> = {
        let mut by_rgb: BTreeMap<[u8; 3], u64> = BTreeMap::new();
        for (c, &count) in colors.iter().zip(&counts) {
            if c[3] > 0 {
                *by_rgb.entry([c[0], c[1], c[2]]).or_insert(0) += count;
            }
        }
        if colors.iter().any(|c| c[3] < 255) && !skip_bg {
            None
        } else {
            by_rgb
                .into_iter()
                .max_by_key(|&(_, c)| c)
                .map(|(rgb, _)| rgb)
        }
    };
    let skipped = |c: &[u8; 4]| c[3] == 0 || bg_color == Some([c[0], c[1], c[2]]);

    let body = match shape {
        SvgShape::Rect => svg_runs(w, &colors, &counts, indices, skipped),
        SvgShape::Circle | SvgShape::Dot => svg_dots(w, shape, &colors, &indices, skipped),
    };

    let mut out = String::with_capacity(body.len() + 256);
    let _ = write!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {w} {h}\" width=\"{w}\" height=\"{h}\" shape-rendering=\"{}\">",
        if shape == SvgShape::Rect {
            "crispEdges"
        } else {
            "geometricPrecision"
        }
    );
    if let Some(bg) = bg_color
        && !skip_bg
    {
        let _ = write!(
            out,
            "<rect width=\"{w}\" height=\"{h}\" fill=\"{}\"/>",
            svg_hex(bg)
        );
    }
    out.push_str(&body);
    out.push_str("</svg>\n");
    out
}

/// Distinct RGBA colors of an image in first-seen order, their pixel counts, and each pixel's
/// index into that list.
struct IndexedColors {
    colors: Vec<[u8; 4]>,
    counts: Vec<u64>,
    indices: Vec<u32>,
}

fn index_colors(img: &RgbaImage) -> IndexedColors {
    const fn slot_of(key: u32, bits: u32) -> usize {
        (key.wrapping_mul(0x9E37_79B1) >> (32 - bits)) as usize
    }

    let (pixels, _) = img.as_raw().as_chunks::<4>();
    let mut colors: Vec<[u8; 4]> = Vec::new();
    let mut counts: Vec<u64> = Vec::new();
    let mut indices = Vec::with_capacity(pixels.len());
    let mut bits = 10;
    let mut slots: Vec<(u32, u32)> = vec![(0, 0); 1 << bits];
    let mut last = (0u32, u32::MAX);
    for &px in pixels {
        let key = u32::from_le_bytes(px);
        if last.0 != key || last.1 == u32::MAX {
            let mut slot = slot_of(key, bits);
            while slots[slot].1 != 0 && slots[slot].0 != key {
                slot = (slot + 1) & (slots.len() - 1);
            }
            if slots[slot].1 == 0 {
                colors.push(px);
                counts.push(0);
                slots[slot] = (key, colors.len() as u32);
                if colors.len() * 2 > slots.len() {
                    bits += 1;
                    slots = vec![(0, 0); 1 << bits];
                    for (c, index) in colors.iter().zip(1u32..) {
                        let key = u32::from_le_bytes(*c);
                        let mut slot = slot_of(key, bits);
                        while slots[slot].1 != 0 {
                            slot = (slot + 1) & (slots.len() - 1);
                        }
                        slots[slot] = (key, index);
                    }
                }
                last = (key, colors.len() as u32 - 1);
            } else {
                last = (key, slots[slot].1 - 1);
            }
        }
        counts[last.1 as usize] += 1;
        indices.push(last.1);
    }
    IndexedColors {
        colors,
        counts,
        indices,
    }
}

/// Stroke every pixel run of a `Rect` SVG, returning the `<g>` element (empty when nothing is drawn).
///
/// Opaque colors become layers ordered by descending pixel count; translucent colors follow and
/// keep their exact coverage. `skipped` marks colors that are never painted.
fn svg_runs(
    w: u32,
    colors: &[[u8; 4]],
    counts: &[u64],
    mut codes: Vec<u32>,
    skipped: impl Fn(&[u8; 4]) -> bool,
) -> String {
    let painted = |opaque: bool| {
        let mut layer: Vec<usize> = (0..colors.len())
            .filter(|&i| !skipped(&colors[i]) && (colors[i][3] == 255) == opaque)
            .collect();
        layer.sort_by_key(|&i| {
            (
                std::cmp::Reverse(if opaque { counts[i] } else { 0 }),
                colors[i],
            )
        });
        layer
    };
    let mut layers = painted(true);
    let opaque = layers.len() as u32;
    layers.extend(painted(false));
    if layers.is_empty() {
        return String::new();
    }
    let mut code_of = vec![SVG_BARRIER; colors.len()];
    for (&i, code) in layers.iter().zip(0u32..) {
        code_of[i] = code;
    }
    for code in &mut codes {
        *code = code_of[*code as usize];
    }

    let paths = trace_runs(&codes, w as usize, opaque, layers.len());

    let mut out = String::from("<g fill=\"none\">");
    for (path, &i) in paths.iter().zip(&layers) {
        let rgba = colors[i];
        let mut stroke = format!("stroke=\"{}\"", svg_hex([rgba[0], rgba[1], rgba[2]]));
        if rgba[3] < 255 {
            let opacity = format!("{:.2}", f32::from(rgba[3]) / 255.0);
            let opacity = opacity.trim_end_matches('0').trim_end_matches('.');
            let opacity = match opacity.strip_prefix("0.") {
                Some(fraction) => format!(".{fraction}"),
                None => opacity.to_string(),
            };
            let _ = write!(stroke, " stroke-opacity=\"{opacity}\"");
        }
        let _ = write!(out, "<path {stroke} d=\"{}\"/>", path.d);
    }
    out.push_str("</g>");
    out
}

/// Trace the stroked runs of every layer along rows.
///
/// `codes` holds one paint class per pixel: a layer index, or [`SVG_BARRIER`]. Layers below
/// `opaque` may run under pixels of any later opaque layer; the rest cover their own pixels only.
fn trace_runs(codes: &[u32], width: usize, opaque: u32, layers: usize) -> Vec<RunPath> {
    let mut out: Vec<RunPath> = (0..layers).map(|_| RunPath::default()).collect();
    let mut stack: Vec<(u32, u32, u32)> = Vec::new();
    for (row, line) in codes.chunks(width.max(1)).zip(0u32..) {
        let mut close = |(code, start, end): (u32, u32, u32)| {
            out[code as usize].push(line, start, end - start);
        };
        let mut previous = SVG_BARRIER;
        for (&code, end) in row.iter().zip(1u32..) {
            if code == previous {
                if let Some(top) = stack.last_mut() {
                    top.2 = end;
                }
                continue;
            }
            previous = code;
            while let Some(&top) = stack.last()
                && (top.0 > code || (code >= opaque && top.0 != code))
            {
                close(top);
                stack.pop();
            }
            match stack.last_mut() {
                Some(top) if top.0 == code => top.2 = end,
                _ if code != SVG_BARRIER => stack.push((code, end - 1, end)),
                _ => {}
            }
        }
        while let Some(top) = stack.pop() {
            close(top);
        }
    }
    out
}

/// Path data for 1px-wide horizontally stroked runs, with the pen position after the last run.
#[derive(Default)]
struct RunPath {
    d: String,
    along: u32,
    line: u32,
}

impl RunPath {
    /// Append a run of `length` pixels starting `start` pixels into row `line`, moving there with
    /// whichever of an absolute or a relative move is shorter.
    fn push(&mut self, line: u32, start: u32, length: u32) {
        let d_along = i64::from(start) - i64::from(self.along);
        let d_line = i64::from(line) - i64::from(self.line);
        let absolute = 4
            + decimal_len(i64::from(start))
            + if line == 0 {
                0
            } else {
                decimal_len(i64::from(line))
            };
        let relative = 2 + decimal_len(d_along) + decimal_len(d_line);
        let d = &mut self.d;
        if d.is_empty() || absolute < relative {
            d.push('M');
            push_decimal(d, i64::from(start));
            d.push(' ');
            if line > 0 {
                push_decimal(d, i64::from(line));
            }
            d.push_str(".5");
        } else {
            d.push('m');
            push_decimal(d, d_along);
            d.push(' ');
            push_decimal(d, d_line);
        }
        d.push('h');
        push_decimal(d, i64::from(length));
        self.along = start + length;
        self.line = line;
    }
}

/// Trace every dot of a `Circle` or `Dot` SVG as two filled semicircle arcs, one `<path>` per
/// color (alpha is ignored, as dots are always opaque).
fn svg_dots(
    w: u32,
    shape: SvgShape,
    colors: &[[u8; 4]],
    indices: &[u32],
    skipped: impl Fn(&[u8; 4]) -> bool,
) -> String {
    let (left, arcs) = if shape == SvgShape::Circle {
        (".02", "a.48.48 0 10.96 0 .48.48 0 10-.96 0")
    } else {
        (".12", "a.38.38 0 10.76 0 .38.38 0 10-.76 0")
    };
    let mut by_rgb: BTreeMap<[u8; 3], usize> = BTreeMap::new();
    for c in colors.iter().filter(|c| !skipped(c)) {
        let next = by_rgb.len();
        by_rgb.entry([c[0], c[1], c[2]]).or_insert(next);
    }
    let path_of: Vec<usize> = colors
        .iter()
        .map(|c| {
            let path = by_rgb.get(&[c[0], c[1], c[2]]).filter(|_| !skipped(c));
            path.copied().unwrap_or(usize::MAX)
        })
        .collect();
    let mut dots: Vec<(String, u32, u32)> = vec![Default::default(); by_rgb.len()];
    for (row, y) in indices.chunks(w.max(1) as usize).zip(0u32..) {
        for (&index, x) in row.iter().zip(0u32..) {
            let Some((d, last_x, last_y)) = dots.get_mut(path_of[index as usize]) else {
                continue;
            };
            if d.is_empty() {
                d.push('M');
                push_decimal(d, i64::from(x));
                d.push_str(left);
                d.push(' ');
                if y > 0 {
                    push_decimal(d, i64::from(y));
                }
                d.push_str(".5");
            } else {
                d.push('m');
                push_decimal(d, i64::from(x) - i64::from(*last_x));
                d.push(' ');
                push_decimal(d, i64::from(y - *last_y));
            }
            d.push_str(arcs);
            (*last_x, *last_y) = (x, y);
        }
    }
    let mut out = String::with_capacity(dots.iter().map(|(d, _, _)| d.len() + 32).sum());
    for (rgb, i) in by_rgb {
        let _ = write!(out, "<path fill=\"{}\" d=\"{}\"/>", svg_hex(rgb), dots[i].0);
    }
    out
}

/// Characters [`push_decimal`] writes for `n`.
fn decimal_len(n: i64) -> usize {
    let digits = n.unsigned_abs().checked_ilog10().map_or(1, |log| log + 1);
    usize::from(n < 0) + digits as usize
}

/// Append `n` in decimal.
fn push_decimal(out: &mut String, n: i64) {
    let mut digits = [0u8; 20];
    let mut at = digits.len();
    let mut rest = n.unsigned_abs();
    loop {
        at -= 1;
        digits[at] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    if n < 0 {
        out.push('-');
    }
    for &digit in &digits[at..] {
        out.push(char::from(digit));
    }
}

/// Format a color as the shortest SVG hex literal, `#rgb` when every channel repeats its nibble.
fn svg_hex([r, g, b]: [u8; 3]) -> String {
    if [r, g, b].iter().all(|c| c >> 4 == c & 0xf) {
        format!("#{:x}{:x}{:x}", r & 0xf, g & 0xf, b & 0xf)
    } else {
        Rgb::new(r, g, b).to_hex()
    }
}

/// Render an image as high-resolution Unicode Braille characters (`U+2800..U+28FF`, 2x4 subpixels).
#[must_use]
pub fn render_braille(img: &RgbaImage, max_cols: Option<u32>) -> String {
    // Standard Braille dot bit mapping for (dx, dy) in 2x4 cell.
    const DOT_BITS: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

    let target_cols = max_cols.unwrap_or(60).clamp(4, 240);
    let target_w = target_cols * 2;
    let src = if img.width() > target_w {
        let ratio = target_w as f32 / img.width() as f32;
        let target_h = ((img.height() as f32) * ratio).round().max(4.0) as u32;
        image::imageops::resize(
            img,
            target_w,
            target_h,
            image::imageops::FilterType::Nearest,
        )
    } else {
        img.clone()
    };

    // Adaptive luminance midpoint so both dark-on-light and light-on-dark palettes render clearly.
    let mut min_l = 1.0f32;
    let mut max_l = 0.0f32;
    for px in src.pixels() {
        if px[3] >= 128 {
            let l = Rgb::new(px[0], px[1], px[2]).lightness();
            min_l = min_l.min(l);
            max_l = max_l.max(l);
        }
    }
    let mid = f32::midpoint(min_l, max_l);

    let w = src.width();
    let h = src.height();
    let mut out = String::new();

    for cy in (0..h).step_by(4) {
        for cx in (0..w).step_by(2) {
            let mut mask = 0u8;
            for dx in 0..2u32 {
                for dy in 0..4u32 {
                    let x = cx + dx;
                    let y = cy + dy;
                    if x < w && y < h {
                        let px = *src.get_pixel(x, y);
                        if px[3] >= 128 && Rgb::new(px[0], px[1], px[2]).lightness() > mid {
                            mask |= DOT_BITS[dx as usize][dy as usize];
                        }
                    }
                }
            }
            let ch = char::from_u32(0x2800 + u32::from(mask)).unwrap_or(' ');
            out.push(ch);
        }
        out.push('\n');
    }

    out
}

/// Render an image as classic density-ramp ASCII text art.
#[must_use]
pub fn render_ascii(img: &RgbaImage, max_cols: Option<u32>) -> String {
    const RAMP: &[u8] = b" .:-=+*#%@";
    let target_w = max_cols.unwrap_or(80).clamp(8, 240);
    // Account for ~2:1 terminal character cell aspect ratio.
    let ratio = target_w as f32 / img.width().max(1) as f32;
    let target_h = ((img.height() as f32) * ratio * 0.5).round().max(1.0) as u32;
    let resized = image::imageops::resize(
        img,
        target_w,
        target_h,
        image::imageops::FilterType::Nearest,
    );

    let mut out = String::with_capacity((target_w as usize + 1) * target_h as usize);
    for y in 0..resized.height() {
        for x in 0..resized.width() {
            let px = *resized.get_pixel(x, y);
            if px[3] < 64 {
                out.push(' ');
            } else {
                let l = Rgb::new(px[0], px[1], px[2]).lightness().clamp(0.0, 1.0);
                let idx = ((l * (RAMP.len() - 1) as f32).round() as usize).min(RAMP.len() - 1);
                out.push(char::from(RAMP[idx]));
            }
        }
        out.push('\n');
    }
    out
}

/// Render an image as chunked Kitty graphics protocol escape sequences (PNG `f=100`, direct `t=d`).
///
/// Works natively in Kitty, Ghostty, `WezTerm`, Konsole, and other terminals supporting the Kitty
/// graphics protocol. Set `opts.tmux_passthrough` to wrap chunks in tmux DCS passthrough.
///
/// # Errors
/// Returns an error if lossless PNG encoding fails.
pub fn render_kitty(img: &RgbaImage, opts: KittyOptions) -> Result<String> {
    let png_bytes =
        encode_fast_png_bytes(img).context("encoding PNG payload for Kitty graphics protocol")?;
    Ok(encode_kitty_png(&png_bytes, opts))
}

/// Wrap pre-encoded PNG bytes into chunked Kitty graphics protocol APC escape sequences.
#[must_use]
pub fn encode_kitty_png(png_bytes: &[u8], opts: KittyOptions) -> String {
    let encoded = base64_encode(png_bytes);
    let total_len = encoded.len();
    let chunk_count = total_len.div_ceil(KITTY_CHUNK_SIZE).max(1);

    let mut out = String::with_capacity(total_len + chunk_count * 48);

    for (idx, chunk) in encoded.as_bytes().chunks(KITTY_CHUNK_SIZE).enumerate() {
        let is_first = idx == 0;
        let is_last = idx + 1 == chunk_count;
        let more = u8::from(!is_last);
        let chunk_str = std::str::from_utf8(chunk).unwrap_or("");

        if opts.tmux_passthrough {
            out.push_str("\x1bPtmux;\x1b\x1b_G");
        } else {
            out.push_str("\x1b_G");
        }

        if is_first {
            // a=T: transmit and display, f=100: PNG, t=d: direct payload, q=2: quiet (no acks).
            out.push_str("a=T,f=100,t=d,q=2");
            if let Some(id) = opts.image_id
                && id > 0
            {
                let _ = write!(out, ",i={id},p=1");
            }
            if opts.no_cursor_move {
                out.push_str(",C=1");
            }
            if let Some(cols) = opts.cols
                && cols > 0
            {
                let _ = write!(out, ",c={cols}");
            }
            if let Some(rows) = opts.rows
                && rows > 0
            {
                let _ = write!(out, ",r={rows}");
            }
            let _ = write!(out, ",m={more};{chunk_str}");
        } else {
            let _ = write!(out, "m={more};{chunk_str}");
        }

        if opts.tmux_passthrough {
            out.push_str("\x1b\x1b\\\x1b\\");
        } else {
            out.push_str("\x1b\\");
        }
    }

    if opts.trailing_newline {
        out.push('\n');
    }
    out
}

/// Encode an `RgbaImage` as the smallest lossless PNG layout that holds it exactly (for saved files).
///
/// See [`PngLayout`] for how the layout is chosen.
///
/// # Errors
/// Returns an error if PNG encoding fails.
pub fn encode_png_bytes(img: &RgbaImage) -> Result<Vec<u8>> {
    let layout = PngLayout::for_images([img]);
    if !matches!(layout, PngLayout::Indexed(_)) {
        return encode_png(img, png::Compression::High);
    }
    let scanlines = layout.unfiltered_scanlines(img);
    let mut out = layout.png_header(img.width(), img.height());
    for part in crate::deflate::zlib_compress(
        &scanlines,
        scanlines.len() / img.height().max(1) as usize,
        Effort::DEFAULT,
    )
    .chunks(i32::MAX as usize)
    {
        write_chunk(&mut out, *b"IDAT", part);
    }
    write_chunk(&mut out, *b"IEND", &[]);
    Ok(out)
}

/// Like [`encode_png_bytes`], but spends far more CPU for the smallest possible file: the palette
/// order and PNG filter strategy are searched, and the image data is compressed with Zopfli.
///
/// # Errors
/// Returns an error if PNG encoding fails.
pub fn encode_optimized_png_bytes(img: &RgbaImage) -> Result<Vec<u8>> {
    crate::png_optimize::recompress(&encode_png(img, png::Compression::NoCompression)?)
}

/// Fast lossless PNG encoding tailored for live interactive Kitty graphics previews.
///
/// Uses the same compact layout as [`encode_png_bytes`] with much cheaper DEFLATE settings.
///
/// # Errors
/// Returns an error if PNG encoding fails.
pub fn encode_fast_png_bytes(img: &RgbaImage) -> Result<Vec<u8>> {
    encode_png(img, png::Compression::Fast)
}

fn encode_png(img: &RgbaImage, compression: png::Compression) -> Result<Vec<u8>> {
    let layout = PngLayout::for_images([img]);
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, img.width(), img.height());
        layout.configure(&mut encoder, compression);
        let mut writer = encoder.write_header().context("writing PNG header")?;
        writer
            .write_image_data(&layout.pixel_data(img))
            .context("writing PNG pixel data")?;
    }
    Ok(out)
}

/// Encode `img` as the most compact exact BMP: a 1-, 4-, or 8-bit palette when it is opaque with at
/// most 256 colors, else 24-bit RGB when opaque, else 32-bit BGRA.
fn encode_bmp(img: &RgbaImage) -> Result<Vec<u8>> {
    let layout = PngLayout::for_images([img]);
    if let PngLayout::Indexed(palette) = &layout
        && palette.iter().all(|c| c[3] == 255)
    {
        return Ok(encode_indexed_bmp(img, palette));
    }
    let (pixels, _) = img.as_raw().as_chunks::<4>();
    let (data, color) = if matches!(layout, PngLayout::Rgb) {
        let rgb: Vec<u8> = pixels.iter().flat_map(|px| [px[0], px[1], px[2]]).collect();
        (rgb, image::ExtendedColorType::Rgb8)
    } else {
        (img.as_raw().clone(), image::ExtendedColorType::Rgba8)
    };
    let mut out = Vec::new();
    image::codecs::bmp::BmpEncoder::new(&mut out)
        .encode(&data, img.width(), img.height(), color)
        .context("encoding bmp output")?;
    Ok(out)
}

/// Write an uncompressed bottom-up palettized BMP (`BITMAPINFOHEADER`) at the smallest bit depth
/// BMP supports for `palette` (1, 4, or 8 bits per pixel).
fn encode_indexed_bmp(img: &RgbaImage, palette: &[[u8; 4]]) -> Vec<u8> {
    const HEADERS_SIZE: usize = 14 + 40;
    let depth: usize = match palette.len() {
        0..=2 => 1,
        3..=16 => 4,
        _ => 8,
    };
    let lookup: HashMap<[u8; 4], u8> = palette
        .iter()
        .enumerate()
        .map(|(i, &c)| (c, i as u8))
        .collect();
    let width = img.width() as usize;
    let per_byte = 8 / depth;
    let row_len = width.div_ceil(per_byte).next_multiple_of(4);
    let pixel_offset = HEADERS_SIZE + palette.len() * 4;
    let image_size = row_len * img.height() as usize;

    let mut out = Vec::with_capacity(pixel_offset + image_size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((pixel_offset + image_size) as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(pixel_offset as u32).to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&img.width().to_le_bytes());
    out.extend_from_slice(&img.height().to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(depth as u16).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(image_size as u32).to_le_bytes());
    out.extend_from_slice(&[0; 8]);
    out.extend_from_slice(&(palette.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for c in palette {
        out.extend_from_slice(&[c[2], c[1], c[0], 0]);
    }
    let (pixels, _) = img.as_raw().as_chunks::<4>();
    for row in pixels.chunks(width.max(1)).rev() {
        let start = out.len();
        out.resize(start + row_len, 0);
        for (x, px) in row.iter().enumerate() {
            out[start + x / per_byte] |= lookup[px] << (8 - depth * (x % per_byte + 1));
        }
    }
    out
}

/// Encode `img` as a Deflate-compressed TIFF in the smallest exact sample layout: 8-bit gray when
/// it is opaque and colorless, else RGB when opaque, else RGBA.
fn encode_tiff(img: &RgbaImage) -> Result<Vec<u8>> {
    use tiff::encoder::{Compression, DeflateLevel, TiffEncoder, colortype};

    let (width, height) = img.dimensions();
    let (pixels, _) = img.as_raw().as_chunks::<4>();
    let opaque = pixels.iter().all(|px| px[3] == 255);
    let mut out = Cursor::new(Vec::new());
    let mut encoder = TiffEncoder::new(&mut out)
        .context("encoding tiff output")?
        .with_compression(Compression::Deflate(DeflateLevel::Best));
    if opaque && pixels.iter().all(|px| px[0] == px[1] && px[1] == px[2]) {
        let gray: Vec<u8> = pixels.iter().map(|px| px[0]).collect();
        encoder.write_image::<colortype::Gray8>(width, height, &gray)
    } else if opaque {
        let rgb: Vec<u8> = pixels.iter().flat_map(|px| [px[0], px[1], px[2]]).collect();
        encoder.write_image::<colortype::RGB8>(width, height, &rgb)
    } else {
        encoder.write_image::<colortype::RGBA8>(width, height, img.as_raw())
    }
    .context("encoding tiff output")?;
    Ok(out.into_inner())
}

/// The most compact exact PNG pixel layout for a set of images (one still, or every animation frame).
///
/// Dithered output almost always uses a handful of colors, so it becomes an indexed PNG at the
/// smallest bit depth that fits the palette (1, 2, 4, or 8 bits per pixel) with a `tRNS` chunk for
/// any translucent entries. Images with more than 256 distinct colors fall back to 24-bit RGB when
/// fully opaque, else 32-bit RGBA.
#[derive(Debug)]
pub(crate) enum PngLayout {
    Indexed(Vec<[u8; 4]>),
    Rgb,
    Rgba,
}

impl PngLayout {
    pub(crate) fn for_images<'a>(images: impl IntoIterator<Item = &'a RgbaImage>) -> Self {
        let mut palette: Vec<[u8; 4]> = Vec::new();
        let mut seen = ColorIndex::default();
        let mut opaque = true;
        let mut indexable = true;
        for img in images {
            let (pixels, _) = img.as_raw().as_chunks::<4>();
            let mut last = None;
            for &px in pixels {
                if last == Some(px) {
                    continue;
                }
                last = Some(px);
                opaque &= px[3] == 255;
                if indexable && seen.get(px).is_none() {
                    indexable = palette.len() < 256;
                    if indexable {
                        seen.insert(px, palette.len() as u8);
                    }
                    palette.push(px);
                }
            }
            if !indexable && !opaque {
                break;
            }
        }
        if indexable && !palette.is_empty() {
            palette.sort_by_key(|px| px[3] == 255);
            Self::Indexed(palette)
        } else if opaque {
            Self::Rgb
        } else {
            Self::Rgba
        }
    }

    fn bit_depth(&self) -> u8 {
        match self {
            Self::Indexed(palette) => match palette.len() {
                0..=2 => 1,
                3..=4 => 2,
                5..=16 => 4,
                _ => 8,
            },
            Self::Rgb | Self::Rgba => 8,
        }
    }

    pub(crate) fn configure<W: std::io::Write>(
        &self,
        encoder: &mut png::Encoder<'_, W>,
        compression: png::Compression,
    ) {
        encoder.set_compression(compression);
        match self {
            Self::Indexed(palette) => {
                encoder.set_color(png::ColorType::Indexed);
                encoder.set_depth(match self.bit_depth() {
                    1 => png::BitDepth::One,
                    2 => png::BitDepth::Two,
                    4 => png::BitDepth::Four,
                    _ => png::BitDepth::Eight,
                });
                encoder.set_palette(
                    palette
                        .iter()
                        .flat_map(|c| [c[0], c[1], c[2]])
                        .collect::<Vec<_>>(),
                );
                let trns: Vec<u8> = palette
                    .iter()
                    .map(|c| c[3])
                    .take_while(|&a| a < 255)
                    .collect();
                if !trns.is_empty() {
                    encoder.set_trns(trns);
                }
                encoder.set_filter(png::Filter::NoFilter);
            }
            Self::Rgb => {
                encoder.set_color(png::ColorType::Rgb);
                encoder.set_depth(png::BitDepth::Eight);
            }
            Self::Rgba => {
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
            }
        }
    }

    /// Raw PNG scanline data (before filtering) for `img`, whose colors must be covered by this layout.
    pub(crate) fn pixel_data(&self, img: &RgbaImage) -> Vec<u8> {
        self.scanlines(img, false)
    }

    /// PNG scanlines of `img` with the filter type byte 0 (None) in front of every row.
    pub(crate) fn unfiltered_scanlines(&self, img: &RgbaImage) -> Vec<u8> {
        self.scanlines(img, true)
    }

    fn scanlines(&self, img: &RgbaImage, filter_bytes: bool) -> Vec<u8> {
        let (pixels, _) = img.as_raw().as_chunks::<4>();
        let width = img.width() as usize;
        let lead = usize::from(filter_bytes);
        match self {
            Self::Indexed(palette) => {
                let indices = palette_indices(img, palette);
                let depth = usize::from(self.bit_depth());
                let per_byte = 8 / depth;
                let row_bytes = lead + width.div_ceil(per_byte);
                let mut out = vec![0u8; row_bytes * img.height() as usize];
                for (row, out_row) in indices.chunks(width.max(1)).zip(out.chunks_mut(row_bytes)) {
                    for (group, byte) in row.chunks(per_byte).zip(&mut out_row[lead..]) {
                        for (k, &index) in group.iter().enumerate() {
                            *byte |= index << (8 - depth * (k + 1));
                        }
                    }
                }
                out
            }
            Self::Rgb | Self::Rgba => {
                let channels = if matches!(self, Self::Rgb) { 3 } else { 4 };
                let mut out = Vec::with_capacity((lead + width * channels) * img.height() as usize);
                for row in pixels.chunks(width.max(1)) {
                    out.extend_from_slice(&[0][..lead]);
                    for px in row {
                        out.extend_from_slice(&px[..channels]);
                    }
                }
                out
            }
        }
    }

    /// The PNG signature, `IHDR` and, for indexed layouts, `PLTE` and `tRNS` chunks.
    pub(crate) fn png_header(&self, width: u32, height: u32) -> Vec<u8> {
        let mut out = crate::png_optimize::SIGNATURE.to_vec();
        let mut ihdr = [0u8; 13];
        ihdr[..4].copy_from_slice(&width.to_be_bytes());
        ihdr[4..8].copy_from_slice(&height.to_be_bytes());
        ihdr[8] = self.bit_depth();
        ihdr[9] = match self {
            Self::Indexed(_) => 3,
            Self::Rgb => 2,
            Self::Rgba => 6,
        };
        write_chunk(&mut out, *b"IHDR", &ihdr);
        if let Self::Indexed(palette) = self {
            let plte: Vec<u8> = palette.iter().flat_map(|c| [c[0], c[1], c[2]]).collect();
            write_chunk(&mut out, *b"PLTE", &plte);
            let trns: Vec<u8> = palette
                .iter()
                .map(|c| c[3])
                .take_while(|&a| a < 255)
                .collect();
            if !trns.is_empty() {
                write_chunk(&mut out, *b"tRNS", &trns);
            }
        }
        out
    }
}

/// Open-addressed map from RGBA colors to palette indices, for palettes of at most 256 colors.
#[derive(Debug)]
pub(crate) struct ColorIndex {
    slots: Vec<(u32, u16)>,
}

impl Default for ColorIndex {
    fn default() -> Self {
        Self {
            slots: vec![(0, Self::EMPTY); 1 << Self::BITS],
        }
    }
}

impl ColorIndex {
    const BITS: u32 = 10;
    const EMPTY: u16 = u16::MAX;

    pub(crate) fn from_palette(palette: &[[u8; 4]]) -> Self {
        let mut index = Self::default();
        for (i, &color) in palette.iter().enumerate().take(256) {
            index.insert(color, i as u8);
        }
        index
    }

    fn slot(&self, key: u32) -> usize {
        let mut slot = (key.wrapping_mul(0x9E37_79B1) >> (32 - Self::BITS)) as usize;
        while self.slots[slot].1 != Self::EMPTY && self.slots[slot].0 != key {
            slot = (slot + 1) & ((1 << Self::BITS) - 1);
        }
        slot
    }

    pub(crate) fn get(&self, color: [u8; 4]) -> Option<u8> {
        let key = u32::from_le_bytes(color);
        let (_, value) = self.slots[self.slot(key)];
        (value != Self::EMPTY).then_some(value as u8)
    }

    pub(crate) fn insert(&mut self, color: [u8; 4], index: u8) {
        let key = u32::from_le_bytes(color);
        let slot = self.slot(key);
        self.slots[slot] = (key, u16::from(index));
    }
}

/// The palette index of every pixel of `img`, whose colors must all be in `palette`.
pub(crate) fn palette_indices(img: &RgbaImage, palette: &[[u8; 4]]) -> Vec<u8> {
    let lookup = ColorIndex::from_palette(palette);
    let (pixels, _) = img.as_raw().as_chunks::<4>();
    let mut last = (palette.first().copied().unwrap_or_default(), 0u8);
    pixels
        .iter()
        .map(|&px| {
            if px != last.0 {
                last = (px, lookup.get(px).unwrap_or(0));
            }
            last.1
        })
        .collect()
}

/// Standard RFC 4648 Base64 encoder (zero extra dependencies).
fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let (chunks, rem) = input.as_chunks::<3>();
    for chunk in chunks {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        out.push(char::from(TABLE[((n >> 18) & 0x3F) as usize]));
        out.push(char::from(TABLE[((n >> 12) & 0x3F) as usize]));
        out.push(char::from(TABLE[((n >> 6) & 0x3F) as usize]));
        out.push(char::from(TABLE[(n & 0x3F) as usize]));
    }
    match rem.len() {
        1 => {
            let n = u32::from(rem[0]) << 16;
            out.push(char::from(TABLE[((n >> 18) & 0x3F) as usize]));
            out.push(char::from(TABLE[((n >> 12) & 0x3F) as usize]));
            out.push('=');
            out.push('=');
        }
        2 => {
            let n = (u32::from(rem[0]) << 16) | (u32::from(rem[1]) << 8);
            out.push(char::from(TABLE[((n >> 18) & 0x3F) as usize]));
            out.push(char::from(TABLE[((n >> 12) & 0x3F) as usize]));
            out.push(char::from(TABLE[((n >> 6) & 0x3F) as usize]));
            out.push('=');
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_histogram_counts_and_orders_like_a_sorted_map() {
        let mut state = 0x1234_5678_u32;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        let img = RgbaImage::from_fn(333, 211, |_, _| {
            let v = next();
            // A handful of colors in runs, some stray ones, and transparent pixels.
            let [a, b, c, d] = v.to_le_bytes();
            let rgb = if a < 200 {
                [b & 0xC0, 0x40, c & 0x80]
            } else {
                [b, c, d]
            };
            image::Rgba([rgb[0], rgb[1], rgb[2], if a % 7 == 0 { 0 } else { 255 }])
        });
        let mut reference: BTreeMap<[u8; 3], u64> = BTreeMap::new();
        for px in img.pixels().filter(|px| px[3] != 0) {
            *reference.entry([px[0], px[1], px[2]]).or_insert(0) += 1;
        }
        let mut expected: Vec<(String, u64)> = reference
            .into_iter()
            .map(|(rgb, n)| (Rgb::new(rgb[0], rgb[1], rgb[2]).to_hex(), n))
            .collect();
        expected.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let got: Vec<(String, u64)> = color_histogram(&img)
            .into_iter()
            .map(|row| (row.hex, row.pixels))
            .collect();
        assert_eq!(got, expected);
    }

    use image::Rgba;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn kitty_graphics_chunks_and_headers_follow_spec() {
        let img = RgbaImage::from_pixel(8, 8, Rgba([20, 40, 180, 255]));
        let single = render_kitty(
            &img,
            KittyOptions {
                cols: Some(40),
                rows: None,
                tmux_passthrough: false,
                trailing_newline: true,
                no_cursor_move: false,
                image_id: None,
            },
        )
        .unwrap();
        assert!(single.starts_with("\x1b_Ga=T,f=100,t=d,q=2,c=40,m=0;"));
        assert!(single.ends_with("\x1b\\\n"));

        // Verify stable image_id framing for flicker-free animation replacement.
        let atomic = encode_kitty_png(
            b"hi",
            KittyOptions {
                image_id: Some(1),
                no_cursor_move: true,
                trailing_newline: false,
                ..KittyOptions::default()
            },
        );
        assert!(atomic.starts_with("\x1b_Ga=T,f=100,t=d,q=2,i=1,p=1,C=1,m=0;"));

        // Verify multi-chunk framing (>4096 base64 chars).
        let big_payload = vec![0xABu8; 4000];
        let multi = encode_kitty_png(&big_payload, KittyOptions::default());
        assert!(multi.starts_with("\x1b_Ga=T,f=100,t=d,q=2,m=1;"));
        assert!(multi.contains("\x1b\\\x1b_Gm=0;"));
        assert!(multi.ends_with("\x1b\\\n"));

        // Verify tmux DCS passthrough doubles ESC bytes.
        let tmux = encode_kitty_png(
            b"hi",
            KittyOptions {
                cols: None,
                rows: None,
                tmux_passthrough: true,
                trailing_newline: true,
                no_cursor_move: false,
                image_id: None,
            },
        );
        assert!(tmux.starts_with("\x1bPtmux;\x1b\x1b_Ga=T,f=100,t=d,q=2,m=0;"));
        assert!(tmux.ends_with("\x1b\x1b\\\x1b\\\n"));
    }

    #[test]
    fn svg_export_merges_horizontal_runs() {
        let mut img = RgbaImage::from_pixel(8, 2, Rgba([255, 255, 255, 255]));
        for x in 2..6 {
            img.put_pixel(x, 0, Rgba([0, 0, 0, 255]));
        }
        for x in 1..3 {
            img.put_pixel(x, 1, Rgba([0, 0, 0, 255]));
        }
        let svg = render_svg(&img, SvgShape::Rect, false);
        assert!(
            svg.contains("<rect width=\"8\" height=\"2\" fill=\"#fff\"/>"),
            "{svg}"
        );
        assert!(
            svg.contains("<path stroke=\"#000\" d=\"M2 .5h4m-5 1h2\"/>"),
            "expected stroked RLE runs: {svg}"
        );

        img.put_pixel(7, 1, Rgba([0x12, 0x34, 0x56, 128]));
        let svg = render_svg(&img, SvgShape::Rect, false);
        assert!(
            svg.contains("<path stroke=\"#123456\" stroke-opacity=\".5\" d=\"M7 1.5h1\"/>"),
            "expected translucent run: {svg}"
        );
    }

    #[test]
    fn svg_dot_export_traces_each_dot_as_filled_arcs() {
        let mut img = RgbaImage::from_pixel(4, 3, Rgba([255, 255, 255, 255]));
        img.put_pixel(1, 0, Rgba([0xaa, 0xbb, 0xcc, 255]));
        img.put_pixel(3, 0, Rgba([0xaa, 0xbb, 0xcc, 255]));
        img.put_pixel(0, 2, Rgba([0xaa, 0xbb, 0xcc, 255]));
        let arcs = "a.48.48 0 10.96 0 .48.48 0 10-.96 0";
        let svg = render_svg(&img, SvgShape::Circle, true);
        assert!(!svg.contains("<rect"), "{svg}");
        assert!(
            svg.contains(&format!(
                "<path fill=\"#abc\" d=\"M1.02 .5{arcs}m2 0{arcs}m-3 2{arcs}\"/>"
            )),
            "expected one arc pair per dot: {svg}"
        );
        let dot = render_svg(&img, SvgShape::Dot, true);
        assert!(
            dot.contains("d=\"M1.12 .5a.38.38 0 10.76 0 .38.38 0 10-.76 0m2 0a.38"),
            "{dot}"
        );
    }

    /// A stroke color and, when translucent, its opacity.
    type SvgPaint = ([u8; 3], Option<f32>);

    /// Every paint a `Rect` SVG from [`render_svg`] applies to each pixel, in document order: the
    /// stroke color and its opacity when translucent. Also returns the background rect color.
    fn paint_svg_runs(svg: &str, w: u32, h: u32) -> (Option<[u8; 3]>, Vec<Vec<SvgPaint>>) {
        fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
            let start = tag.find(&format!(" {name}=\""))? + name.len() + 3;
            Some(&tag[start..start + tag[start..].find('"')?])
        }
        fn rgb(hex: &str) -> [u8; 3] {
            let digits: Vec<u8> = hex[1..]
                .chars()
                .map(|c| c.to_digit(16).unwrap() as u8)
                .collect();
            match digits.len() {
                3 => [digits[0] * 17, digits[1] * 17, digits[2] * 17],
                _ => [0, 2, 4].map(|i| digits[i] * 16 + digits[i + 1]),
            }
        }
        let mut painted = vec![Vec::new(); (w * h) as usize];
        let mut rect = None;
        let mut groups: Vec<(Option<&str>, Option<&str>)> = vec![(None, None)];
        for tag in svg.split('<').skip(1) {
            let (stroke, opacity) = *groups.last().unwrap();
            let stroke = attr(tag, "stroke").or(stroke);
            let opacity = attr(tag, "stroke-opacity").or(opacity);
            if tag.starts_with("rect ") {
                rect = Some(rgb(attr(tag, "fill").unwrap()));
            } else if tag.starts_with("g ") {
                groups.push((stroke, opacity));
            } else if tag.starts_with("/g") {
                groups.pop();
            } else if tag.starts_with("path ") {
                let paint = (
                    rgb(stroke.unwrap()),
                    opacity.map(|o| o.parse::<f32>().unwrap()),
                );
                let dash: Option<Vec<u32>> = attr(tag, "stroke-dasharray")
                    .map(|a| a.split(' ').map(|n| n.parse().unwrap()).collect());
                let d = attr(tag, "d").unwrap();
                let mut numbers: Vec<f64> = Vec::new();
                let (mut x, mut y) = (0.0, 0.0);
                let mut command = ' ';
                let mut chars = d.char_indices().peekable();
                while let Some((at, c)) = chars.next() {
                    if c.is_ascii_alphabetic() {
                        command = c;
                        continue;
                    }
                    if c == ' ' {
                        continue;
                    }
                    let mut end = at + 1;
                    let mut dot = c == '.';
                    while let Some(&(next_at, next)) = chars.peek()
                        && (next.is_ascii_digit() || (next == '.' && !dot))
                    {
                        dot |= next == '.';
                        end = next_at + 1;
                        chars.next();
                    }
                    numbers.push(d[at..end].parse().unwrap());
                    match (command, numbers.as_slice()) {
                        ('M', &[nx, ny]) => (x, y) = (nx, ny),
                        ('m', &[dx, dy]) => (x, y) = (x + dx, y + dy),
                        ('h' | 'v', &[length]) => {
                            let vertical = command == 'v';
                            assert_eq!(
                                (x.fract(), y.fract()),
                                if vertical { (0.5, 0.0) } else { (0.0, 0.5) }
                            );
                            for step in 0..length as u32 {
                                let on = dash.as_ref().is_none_or(|dash| {
                                    let (on, off) = (dash[0], *dash.last().unwrap());
                                    step % (on + off) < on
                                });
                                let (cx, cy) = if vertical {
                                    (x as u32, y as u32 + step)
                                } else {
                                    (x as u32 + step, y as u32)
                                };
                                if on {
                                    assert!(cx < w && cy < h, "{cx},{cy} outside {w}x{h}");
                                    painted[(cy * w + cx) as usize].push(paint);
                                }
                            }
                            if vertical {
                                y += length;
                            } else {
                                x += length;
                            }
                        }
                        _ => continue,
                    }
                    numbers.clear();
                }
            }
        }
        (rect, painted)
    }

    #[test]
    fn svg_rect_strokes_reproduce_every_pixel() {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut random = move |n: u32| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((state >> 33) % u64::from(n)) as u32
        };
        let ink = |i: u32| Rgba([(i * 53) as u8, (255 - i * 31) as u8, (i * 97) as u8, 255]);
        let noise = RgbaImage::from_fn(61, 37, |_, _| {
            let i = random(9);
            ink(if i > 4 { 0 } else { i })
        });
        let ordered = RgbaImage::from_fn(96, 64, |x, y| {
            let i = [0, 2, 3, 1][((x % 4) ^ (y % 2)) as usize];
            ink(if (x / 24 + y / 16) % 3 == 0 { 0 } else { i })
        });
        let columns = RgbaImage::from_fn(48, 70, |x, y| ink(u32::from(x % 3 == 0 && y % 23 != 5)));
        let mut holes = noise.clone();
        for (x, y, px) in holes.enumerate_pixels_mut() {
            match random(7) {
                0 => px[3] = 0,
                1 if px[0] != 0 => px[3] = 90 + (x + y) as u8 % 3,
                _ => {}
            }
        }

        for (name, img) in [
            ("noise", noise),
            ("ordered", ordered),
            ("columns", columns),
            ("holes", holes),
        ] {
            for skip_bg in [false, true] {
                let svg = render_svg(&img, SvgShape::Rect, skip_bg);
                let (rect, painted) = paint_svg_runs(&svg, img.width(), img.height());
                let bg = (skip_bg || img.pixels().all(|px| px[3] == 255)).then_some([0, 255, 0]);
                assert_eq!(rect, bg.filter(|_| !skip_bg), "{name}: {svg}");
                for ((x, y, px), paints) in img.enumerate_pixels().zip(&painted) {
                    let rgb = [px[0], px[1], px[2]];
                    let at = format!("{name} skip_bg={skip_bg} at {x},{y}");
                    match px[3] {
                        0 => assert!(paints.is_empty(), "{at}"),
                        255 if skip_bg && Some(rgb) == bg => assert!(paints.is_empty(), "{at}"),
                        255 => assert_eq!(
                            paints.last().map_or(rect, |p| p.1.is_none().then_some(p.0)),
                            Some(rgb),
                            "{at}"
                        ),
                        alpha => {
                            assert_eq!(paints.len(), 1, "{at}");
                            assert_eq!(paints[0].0, rgb, "{at}");
                            assert!(
                                (paints[0].1.unwrap() - f32::from(alpha) / 255.0).abs() < 0.006,
                                "{at}"
                            );
                        }
                    }
                }
                if name == "noise" {
                    assert!(
                        painted.iter().any(|p| p.len() > 1),
                        "{name}: no run passes under a later color"
                    );
                }
            }
        }
    }

    #[test]
    fn fast_png_indexed_and_rgba_round_trip_identically() {
        let mut dithered = RgbaImage::from_pixel(16, 12, Rgba([244, 240, 228, 255]));
        for y in 0..12 {
            for x in 0..16 {
                if (x + y) % 2 == 0 {
                    dithered.put_pixel(x, y, Rgba([26, 43, 140, 255]));
                }
            }
        }
        let indexed_bytes = encode_fast_png_bytes(&dithered).unwrap();
        let decoded = image::load_from_memory(&indexed_bytes).unwrap().to_rgba8();
        assert_eq!(decoded, dithered);

        let mut translucent = dithered;
        translucent.put_pixel(0, 0, Rgba([10, 20, 30, 128]));
        let rgba_bytes = encode_fast_png_bytes(&translucent).unwrap();
        let decoded_rgba = image::load_from_memory(&rgba_bytes).unwrap().to_rgba8();
        assert_eq!(decoded_rgba, translucent);
    }

    #[test]
    fn saved_png_uses_smallest_exact_layout() {
        let decode = |img: &RgbaImage| {
            let bytes = encode_png_bytes(img).unwrap();
            let reader = png::Decoder::new(Cursor::new(bytes.clone()))
                .read_info()
                .unwrap();
            let info = reader.info();
            let layout = (info.color_type, info.bit_depth, info.trns.is_some());
            (layout, image::load_from_memory(&bytes).unwrap().to_rgba8())
        };

        let mut two_tone = RgbaImage::from_pixel(13, 5, Rgba([244, 240, 228, 255]));
        two_tone.put_pixel(12, 4, Rgba([26, 43, 140, 255]));
        let (layout, decoded) = decode(&two_tone);
        assert_eq!(layout, (png::ColorType::Indexed, png::BitDepth::One, false));
        assert_eq!(decoded, two_tone);

        let mut cutout = two_tone.clone();
        cutout.put_pixel(0, 0, Rgba([0, 0, 0, 0]));
        cutout.put_pixel(1, 0, Rgba([9, 9, 9, 100]));
        let (layout, decoded) = decode(&cutout);
        assert_eq!(layout, (png::ColorType::Indexed, png::BitDepth::Two, true));
        assert_eq!(decoded, cutout);

        let photo = RgbaImage::from_fn(32, 32, |x, y| Rgba([(x * 8) as u8, (y * 8) as u8, 7, 255]));
        let (layout, decoded) = decode(&photo);
        assert_eq!(layout, (png::ColorType::Rgb, png::BitDepth::Eight, false));
        assert_eq!(decoded, photo);
    }

    fn layout_samples() -> Vec<RgbaImage> {
        let mut two_tone = RgbaImage::from_pixel(13, 5, Rgba([244, 240, 228, 255]));
        two_tone.put_pixel(12, 4, Rgba([26, 43, 140, 255]));
        let mut cutout = two_tone.clone();
        cutout.put_pixel(0, 0, Rgba([0, 0, 0, 0]));
        cutout.put_pixel(1, 0, Rgba([9, 9, 9, 100]));
        let tones = |n: u32| {
            RgbaImage::from_fn(37, 23, move |x, y| {
                let i = ((x * 7 + y * 3) % n) as u8;
                Rgba([i * 11, 255 - i * 5, i, 255])
            })
        };
        let photo = RgbaImage::from_fn(32, 32, |x, y| Rgba([(x * 8) as u8, (y * 8) as u8, 7, 255]));
        let translucent =
            RgbaImage::from_fn(32, 32, |x, y| Rgba([(x * 8) as u8, 90, 7, (y * 8) as u8]));
        let gray = RgbaImage::from_fn(30, 20, |x, y| {
            let v = if (x + y) % 3 == 0 { 0 } else { 200 };
            Rgba([v, v, v, 255])
        });
        vec![
            two_tone,
            cutout,
            tones(5),
            tones(20),
            photo,
            translucent,
            gray,
        ]
    }

    #[test]
    fn optimized_png_round_trips_and_never_grows() {
        for img in layout_samples() {
            let optimized = encode_optimized_png_bytes(&img).unwrap();
            assert_eq!(image::load_from_memory(&optimized).unwrap().to_rgba8(), img);
            assert!(optimized.len() <= encode_png_bytes(&img).unwrap().len());
        }
    }

    #[test]
    fn bmp_uses_smallest_exact_layout() {
        let bits_per_pixel: Vec<u16> = layout_samples()
            .iter()
            .map(|img| {
                let bytes =
                    encode_image(img, OutputFormat::Bmp, &ExportOptions::default()).unwrap();
                assert_eq!(image::load_from_memory(&bytes).unwrap().to_rgba8(), *img);
                u16::from_le_bytes([bytes[28], bytes[29]])
            })
            .collect();
        assert_eq!(bits_per_pixel, [1, 32, 4, 8, 24, 32, 1]);
    }

    #[test]
    fn tiff_is_deflate_compressed_and_exact() {
        for img in layout_samples() {
            let bytes = encode_image(&img, OutputFormat::Tiff, &ExportOptions::default()).unwrap();
            assert_eq!(image::load_from_memory(&bytes).unwrap().to_rgba8(), img);
            assert!(bytes.len() < img.as_raw().len());
        }
    }

    #[test]
    fn braille_and_ascii_render_non_empty() {
        let mut img = RgbaImage::from_pixel(16, 16, Rgba([0, 0, 0, 255]));
        for y in 0..8 {
            for x in 0..16 {
                img.put_pixel(x, y, Rgba([255, 255, 255, 255]));
            }
        }
        let braille = render_braille(&img, Some(8));
        assert!(braille.contains('\u{28ff}'));
        let ascii = render_ascii(&img, Some(16));
        assert!(ascii.contains('@'));
    }
}
