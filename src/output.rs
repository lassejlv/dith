//! Image export (PNG/WebP/JPEG/GIF/BMP/TIFF, vector SVG, Kitty graphics, Braille, ASCII) and stats.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Cursor, IsTerminal};
use std::path::Path;

use anyhow::{Context, Result};
use clap::ValueEnum;
use image::{DynamicImage, ImageFormat, RgbaImage};
use serde::{Deserialize, Serialize};

use crate::color::Rgb;

/// Maximum Base64 payload bytes per Kitty graphics protocol chunk (per Kitty spec).
const KITTY_CHUNK_SIZE: usize = 4096;

/// Supported output file and terminal formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, ValueEnum, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SvgShape {
    /// Horizontal run-length merged crisp pixel rectangles (smallest file size).
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
    let mut counts: BTreeMap<[u8; 3], u64> = BTreeMap::new();
    let mut total = 0u64;
    for px in img.pixels() {
        if px[3] == 0 {
            continue;
        }
        *counts.entry([px[0], px[1], px[2]]).or_insert(0) += 1;
        total += 1;
    }
    let denom = total.max(1) as f64;
    let mut rows: Vec<ColorUsage> = counts
        .into_iter()
        .map(|(rgb, pixels)| ColorUsage {
            hex: Rgb::new(rgb[0], rgb[1], rgb[2]).to_hex(),
            pixels,
            percent: ((pixels as f64 / denom) * 10_000.0).round() / 100.0,
        })
        .collect();
    rows.sort_by(|a, b| b.pixels.cmp(&a.pixels).then_with(|| a.hex.cmp(&b.hex)));
    rows
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
        OutputFormat::Bmp => encode_bmp(img),
        OutputFormat::Tiff => encode_tiff(img),
        OutputFormat::Webp | OutputFormat::Jpeg | OutputFormat::Gif => {
            let dyn_img = if matches!(format, OutputFormat::Jpeg) {
                DynamicImage::ImageRgba8(img.clone()).to_rgb8().into()
            } else {
                DynamicImage::ImageRgba8(img.clone())
            };
            let img_fmt = match format {
                OutputFormat::Webp => ImageFormat::WebP,
                OutputFormat::Jpeg => ImageFormat::Jpeg,
                OutputFormat::Gif => ImageFormat::Gif,
                OutputFormat::Png
                | OutputFormat::Bmp
                | OutputFormat::Tiff
                | OutputFormat::Svg
                | OutputFormat::Kitty
                | OutputFormat::Braille
                | OutputFormat::Ascii => {
                    unreachable!("PNG, BMP, TIFF, and text/terminal formats handled above")
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

/// Render a dithered image as a compact, crisp SVG.
///
/// `Rect` mode strokes each horizontal run as a 1px-wide line through the row centre, one
/// `<path>` per color; dot modes trace every dot as two filled semicircle arcs in one `<path>`
/// per color. Colors use the shortest hex form, so files stay lean for web and design tooling.
#[must_use]
pub fn render_svg(img: &RgbaImage, shape: SvgShape, skip_bg: bool) -> String {
    let w = img.width();
    let h = img.height();

    // Identify dominant opaque color for optional background rect or skip-bg omission.
    let bg_color: Option<[u8; 3]> = {
        let mut counts: BTreeMap<[u8; 3], u64> = BTreeMap::new();
        let mut has_transparency = false;
        for px in img.pixels() {
            if px[3] < 255 {
                has_transparency = true;
            }
            if px[3] > 0 {
                *counts.entry([px[0], px[1], px[2]]).or_insert(0) += 1;
            }
        }
        if has_transparency && !skip_bg {
            None
        } else {
            counts
                .into_iter()
                .max_by_key(|&(_, c)| c)
                .map(|(rgb, _)| rgb)
        }
    };

    let mut out = String::with_capacity((w * h) as usize * 4);
    let _ = writeln!(
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
        let _ = writeln!(
            out,
            "<rect width=\"{w}\" height=\"{h}\" fill=\"{}\"/>",
            svg_hex(bg)
        );
    }

    match shape {
        SvgShape::Rect => {
            let mut paths: BTreeMap<[u8; 4], (String, u32, u32)> = BTreeMap::new();
            for y in 0..h {
                let mut x = 0u32;
                while x < w {
                    let px = *img.get_pixel(x, y);
                    if px[3] == 0 || bg_color == Some([px[0], px[1], px[2]]) {
                        x += 1;
                        continue;
                    }
                    let start_x = x;
                    x += 1;
                    while x < w && *img.get_pixel(x, y) == px {
                        x += 1;
                    }
                    let run_w = x - start_x;
                    let (d, end_x, last_y) = paths.entry(px.0).or_default();
                    if d.is_empty() {
                        let _ = write!(d, "M{start_x} {y}.5h{run_w}");
                    } else {
                        let dx = i64::from(start_x) - i64::from(*end_x);
                        let dy = y - *last_y;
                        let _ = write!(d, "m{dx} {dy}h{run_w}");
                    }
                    (*end_x, *last_y) = (x, y);
                }
            }
            if !paths.is_empty() {
                out.push_str("<g fill=\"none\">\n");
                for (rgba, (d, _, _)) in paths {
                    let _ = write!(
                        out,
                        "<path stroke=\"{}\"",
                        svg_hex([rgba[0], rgba[1], rgba[2]])
                    );
                    if rgba[3] < 255 {
                        let _ =
                            write!(out, " stroke-opacity=\"{:.2}\"", f32::from(rgba[3]) / 255.0);
                    }
                    let _ = writeln!(out, " d=\"{d}\"/>");
                }
                out.push_str("</g>\n");
            }
        }
        SvgShape::Circle | SvgShape::Dot => {
            let (left, arcs) = if shape == SvgShape::Circle {
                (".02", "a.48.48 0 1 0 .96 0 .48.48 0 1 0-.96 0")
            } else {
                (".12", "a.38.38 0 1 0 .76 0 .38.38 0 1 0-.76 0")
            };
            let mut dots: BTreeMap<[u8; 3], (String, u32, u32)> = BTreeMap::new();
            for y in 0..h {
                for x in 0..w {
                    let px = *img.get_pixel(x, y);
                    let rgb = [px[0], px[1], px[2]];
                    if px[3] == 0 || bg_color == Some(rgb) {
                        continue;
                    }
                    let (d, last_x, last_y) = dots.entry(rgb).or_default();
                    if d.is_empty() {
                        let _ = write!(d, "M{x}{left} {y}.5{arcs}");
                    } else {
                        let dx = i64::from(x) - i64::from(*last_x);
                        let dy = y - *last_y;
                        let _ = write!(d, "m{dx} {dy}{arcs}");
                    }
                    (*last_x, *last_y) = (x, y);
                }
            }
            for (rgb, (d, _, _)) in dots {
                let _ = writeln!(out, "<path fill=\"{}\" d=\"{d}\"/>", svg_hex(rgb));
            }
        }
    }

    out.push_str("</svg>\n");
    out
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
    encode_png(img, png::Compression::High)
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
        let mut seen: HashSet<[u8; 4]> = HashSet::new();
        let mut opaque = true;
        let mut indexable = true;
        for img in images {
            let (pixels, _) = img.as_raw().as_chunks::<4>();
            for &px in pixels {
                opaque &= px[3] == 255;
                if indexable && seen.insert(px) {
                    indexable = palette.len() < 256;
                    palette.push(px);
                }
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
        let (pixels, _) = img.as_raw().as_chunks::<4>();
        match self {
            Self::Indexed(palette) => {
                let lookup: HashMap<[u8; 4], u8> = palette
                    .iter()
                    .enumerate()
                    .map(|(i, &c)| (c, i as u8))
                    .collect();
                let depth = usize::from(self.bit_depth());
                let per_byte = 8 / depth;
                let width = img.width() as usize;
                let row_bytes = width.div_ceil(per_byte);
                let mut out = vec![0u8; row_bytes * img.height() as usize];
                for (row, out_row) in pixels.chunks(width.max(1)).zip(out.chunks_mut(row_bytes)) {
                    for (x, px) in row.iter().enumerate() {
                        let shift = 8 - depth * (x % per_byte + 1);
                        out_row[x / per_byte] |= lookup[px] << shift;
                    }
                }
                out
            }
            Self::Rgb => pixels.iter().flat_map(|px| [px[0], px[1], px[2]]).collect(),
            Self::Rgba => img.as_raw().clone(),
        }
    }
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
            svg.contains("<path stroke=\"#000\" d=\"M2 0.5h4m-5 1h2\"/>"),
            "expected stroked RLE runs: {svg}"
        );

        img.put_pixel(7, 1, Rgba([0x12, 0x34, 0x56, 128]));
        let svg = render_svg(&img, SvgShape::Rect, false);
        assert!(
            svg.contains("<path stroke=\"#123456\" stroke-opacity=\"0.50\" d=\"M7 1.5h1\"/>"),
            "expected translucent run: {svg}"
        );
    }

    #[test]
    fn svg_dot_export_traces_each_dot_as_filled_arcs() {
        let mut img = RgbaImage::from_pixel(4, 3, Rgba([255, 255, 255, 255]));
        img.put_pixel(1, 0, Rgba([0xaa, 0xbb, 0xcc, 255]));
        img.put_pixel(3, 0, Rgba([0xaa, 0xbb, 0xcc, 255]));
        img.put_pixel(0, 2, Rgba([0xaa, 0xbb, 0xcc, 255]));
        let arcs = "a.48.48 0 1 0 .96 0 .48.48 0 1 0-.96 0";
        let svg = render_svg(&img, SvgShape::Circle, true);
        assert!(!svg.contains("<rect"), "{svg}");
        assert!(
            svg.contains(&format!(
                "<path fill=\"#abc\" d=\"M1.02 0.5{arcs}m2 0{arcs}m-3 2{arcs}\"/>"
            )),
            "expected one arc pair per dot: {svg}"
        );
        let dot = render_svg(&img, SvgShape::Dot, true);
        assert!(
            dot.contains("d=\"M1.12 0.5a.38.38 0 1 0 .76 0 .38.38 0 1 0-.76 0m2 0a.38"),
            "{dot}"
        );
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
