//! Labeled contact-sheet compositor with built-in crisp 5x7 pixel font (zero external fonts).

use image::{Rgba, RgbaImage};

use crate::color::Rgb;

/// A single rendered variant tile for a comparison contact sheet.
#[derive(Debug, Clone)]
pub struct CompareTile {
    pub label: String,
    pub image: RgbaImage,
}

/// Compose multiple tiles into a cleanly framed, labeled contact sheet.
#[must_use]
pub fn compose_contact_sheet(
    tiles: &[CompareTile],
    columns: Option<u32>,
    dark_theme: bool,
) -> RgbaImage {
    if tiles.is_empty() {
        return RgbaImage::from_pixel(1, 1, Rgba([0, 0, 0, 255]));
    }

    let count = tiles.len() as u32;
    let cols = columns
        .unwrap_or_else(|| (count as f32).sqrt().ceil() as u32)
        .clamp(1, count);
    let rows = count.div_ceil(cols);

    let tile_w = tiles.iter().map(|t| t.image.width()).max().unwrap_or(1);
    let tile_h = tiles.iter().map(|t| t.image.height()).max().unwrap_or(1);

    let padding = 16u32;
    let label_h = 28u32;
    let cell_w = tile_w + padding;
    let cell_h = tile_h + label_h + padding;

    let sheet_w = cols * cell_w + padding;
    let sheet_h = rows * cell_h + padding;

    let (bg, frame, text_color) = if dark_theme {
        (
            Rgb::from_u32(0x111215),
            Rgb::from_u32(0x282B32),
            Rgb::from_u32(0xE8EAEF),
        )
    } else {
        (
            Rgb::from_u32(0xF5F3EE),
            Rgb::from_u32(0xD8D3C8),
            Rgb::from_u32(0x1B1917),
        )
    };

    let mut sheet = RgbaImage::from_pixel(sheet_w, sheet_h, Rgba([bg.r, bg.g, bg.b, 255]));

    for (idx, tile) in tiles.iter().enumerate() {
        let col = (idx as u32) % cols;
        let row = (idx as u32) / cols;
        let ox = padding + col * cell_w;
        let oy = padding + row * cell_h;

        // Subtle 1px hairline border around the tile image.
        draw_border(
            &mut sheet,
            ox.saturating_sub(1),
            oy.saturating_sub(1),
            tile.image.width() + 2,
            tile.image.height() + 2,
            frame,
        );

        // Blit tile image.
        for (tx, ty, &px) in tile.image.enumerate_pixels() {
            let sx = ox + tx;
            let sy = oy + ty;
            if sx < sheet_w && sy < sheet_h {
                let dst = sheet.get_pixel_mut(sx, sy);
                if px[3] == 255 {
                    *dst = px;
                } else if px[3] > 0 {
                    let a = f32::from(px[3]) / 255.0;
                    let inv = 1.0 - a;
                    dst[0] = (f32::from(px[0]) * a + f32::from(dst[0]) * inv) as u8;
                    dst[1] = (f32::from(px[1]) * a + f32::from(dst[1]) * inv) as u8;
                    dst[2] = (f32::from(px[2]) * a + f32::from(dst[2]) * inv) as u8;
                }
            }
        }

        // Draw crisp 2x scaled pixel-font label underneath.
        draw_text(
            &mut sheet,
            ox,
            oy + tile_h + 7,
            &tile.label,
            text_color,
            2,
            tile_w,
        );
    }

    sheet
}

fn draw_border(img: &mut RgbaImage, x: u32, y: u32, w: u32, h: u32, color: Rgb) {
    let px = Rgba([color.r, color.g, color.b, 255]);
    let iw = img.width();
    let ih = img.height();
    for dx in 0..w {
        if x + dx < iw {
            if y < ih {
                img.put_pixel(x + dx, y, px);
            }
            if y + h > 0 && y + h - 1 < ih {
                img.put_pixel(x + dx, y + h - 1, px);
            }
        }
    }
    for dy in 0..h {
        if y + dy < ih {
            if x < iw {
                img.put_pixel(x, y + dy, px);
            }
            if x + w > 0 && x + w - 1 < iw {
                img.put_pixel(x + w - 1, y + dy, px);
            }
        }
    }
}

fn draw_text(
    img: &mut RgbaImage,
    start_x: u32,
    start_y: u32,
    text: &str,
    color: Rgb,
    scale: u32,
    max_width: u32,
) {
    let px = Rgba([color.r, color.g, color.b, 255]);
    let advance = 6 * scale;
    let mut cursor_x = start_x;
    for ch in text.chars() {
        if cursor_x + 5 * scale > start_x + max_width {
            break;
        }
        let glyph = glyph_5x7(ch.to_ascii_uppercase());
        for (row, &bits) in glyph.iter().enumerate() {
            for col in 0..5u32 {
                if (bits & (1 << (4 - col))) != 0 {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            let gx = cursor_x + col * scale + sx;
                            let gy = start_y + (row as u32) * scale + sy;
                            if gx < img.width() && gy < img.height() {
                                img.put_pixel(gx, gy, px);
                            }
                        }
                    }
                }
            }
        }
        cursor_x += advance;
    }
}

/// Compact 5x7 uppercase ASCII bitmap font (each byte stores 5 pixels in bits 4..=0).
const fn glyph_5x7(ch: char) -> [u8; 7] {
    match ch {
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x12, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x1B, 0x11],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1E, 0x01, 0x01, 0x0E, 0x01, 0x01, 0x1E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        '_' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1F],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        ':' => [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00],
        '/' => [0x01, 0x02, 0x02, 0x04, 0x08, 0x08, 0x10],
        '#' => [0x0A, 0x0A, 0x1F, 0x0A, 0x1F, 0x0A, 0x0A],
        '+' => [0x00, 0x04, 0x04, 0x1F, 0x04, 0x04, 0x00],
        _ => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_sheet_composes_tiles_with_labels() {
        let tiles = vec![
            CompareTile {
                label: "atkinson".to_string(),
                image: RgbaImage::from_pixel(40, 30, Rgba([20, 20, 20, 255])),
            },
            CompareTile {
                label: "bayer4".to_string(),
                image: RgbaImage::from_pixel(40, 30, Rgba([220, 220, 220, 255])),
            },
        ];
        let sheet = compose_contact_sheet(&tiles, Some(2), true);
        assert!(sheet.width() > 80);
        assert!(sheet.height() > 40);
    }
}
