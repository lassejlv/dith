//! GIF container writer with its own LZW encoder.
//!
//! The encoder produces streams every GIF decoder accepts, but spends its freedom on size:
//!
//! - When the 4096-entry code table fills, the textbook encoder clears it at once. This one keeps
//!   the full table (a deferred clear) for as long as it still compresses better than a fresh
//!   table would, measured by encoding the next stretch both ways.
//! - Pixels of an animation frame that already show the right color may be written either as that
//!   color or as the transparent index. The encoder picks, pixel by pixel, whichever continues the
//!   longest table match.
//!
//! Every decision uses integer arithmetic on the input alone, so the output is identical on every
//! platform and thread count.

use image::RgbaImage;

use crate::output::ColorIndex;

/// Number of codes a GIF LZW table can hold.
const MAX_CODES: u16 = 4096;

/// Width in bits of every code once the table is full.
const MAX_WIDTH: u8 = 12;

/// Largest minimum code size whose code table is a dense array (32 colors: 256 KiB).
const DENSE_MAX_CODE_SIZE: u8 = 5;

/// Log2 of the number of hash slots backing the code table of larger alphabets.
const HASH_BITS: u32 = 14;

const EMPTY: u32 = u32::MAX;

/// How many candidate matches are followed at once through pixels with two allowed values.
const BEAM: usize = 8;

/// A full table is kept while its cost per pixel stays below `KEEP_BIAS / 1024` times the cost of
/// starting over. Starting over is favored slightly because a fresh table keeps paying off after
/// the stretch that was measured.
const KEEP_BIAS: u64 = 1075;

/// What a decoder does with a frame's area once its delay has passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Disposal {
    /// Leave the frame in place under the next one.
    Keep = 1,
    /// Clear the frame's area to transparent.
    Background = 2,
}

/// One LZW-compressed image of a GIF.
#[derive(Debug)]
pub(crate) struct Frame {
    pub left: u16,
    pub top: u16,
    pub width: u16,
    pub height: u16,
    /// Display time in hundredths of a second.
    pub delay: u16,
    pub disposal: Disposal,
    pub transparent: Option<u8>,
    pub min_code_size: u8,
    pub lzw: Vec<u8>,
}

/// Palette indices of one frame in row order, ready for [`compress`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct Indices<'a> {
    pub pixels: &'a [u8],
    /// Empty, or one flag per pixel: `true` where the pixel may also be written as `transparent`
    /// because the canvas already shows its color.
    pub optional: &'a [bool],
    pub transparent: u8,
}

/// Encode a still image as a single-frame GIF with an exact color table, matching what the
/// `gif` crate decodes to: fully transparent pixels become the transparent index and every other
/// pixel is opaque. Returns `None` when the image needs more than 256 table entries or is larger
/// than GIF allows.
pub(crate) fn encode_still(img: &RgbaImage) -> Option<Vec<u8>> {
    let width = u16::try_from(img.width()).ok().filter(|&w| w > 0)?;
    let height = u16::try_from(img.height()).ok().filter(|&h| h > 0)?;
    let (pixels, _) = img.as_raw().as_chunks::<4>();

    let mut palette: Vec<u8> = Vec::new();
    let mut lookup = ColorIndex::default();
    let mut transparent = None;
    let mut indices: Vec<u8> = Vec::with_capacity(pixels.len());
    let mut last: Option<([u8; 4], u8)> = None;
    for &px in pixels {
        if let Some((prev, index)) = last
            && prev == px
        {
            indices.push(index);
            continue;
        }
        let key = if px[3] == 0 {
            [0; 4]
        } else {
            [px[0], px[1], px[2], 255]
        };
        let index = if let Some(index) = lookup.get(key) {
            index
        } else {
            let count = palette.len() / 3;
            if count == 256 {
                return None;
            }
            lookup.insert(key, count as u8);
            palette.extend_from_slice(&px[..3]);
            if px[3] == 0 {
                transparent = Some(count as u8);
            }
            count as u8
        };
        last = Some((px, index));
        indices.push(index);
    }

    let (min_code_size, lzw) = compress(&Indices {
        pixels: &indices,
        optional: &[],
        transparent: 0,
    });
    let frame = Frame {
        left: 0,
        top: 0,
        width,
        height,
        delay: 0,
        disposal: Disposal::Keep,
        transparent,
        min_code_size,
        lzw,
    };
    Some(write_gif(width, height, &palette, None, &[frame]))
}

/// LZW-compress `indices` and return the minimum code size with the packed code stream.
pub(crate) fn compress(indices: &Indices<'_>) -> (u8, Vec<u8>) {
    let n = indices.pixels.len();
    let mut max_index = indices.pixels.iter().copied().max().unwrap_or(0);
    if !indices.optional.is_empty() {
        max_index = max_index.max(indices.transparent);
    }
    let min_code_size = (8 - max_index.leading_zeros() as u8).max(2);
    let clear = 1u16 << min_code_size;

    let mut out = BitWriter {
        bytes: Vec::with_capacity(n / 4 + 16),
        acc: 0,
        bits: 0,
        widths: Widths::new(min_code_size),
    };
    let mut table = Table::new(min_code_size);
    let mut fresh = Table::new(min_code_size);
    let mut codes: Vec<u16> = Vec::new();
    let mut fresh_codes: Vec<u16> = Vec::new();

    out.clear(clear);
    let (mut pos, _) = indices.parse(&mut table, 0, n, &mut codes);
    out.codes(&codes);
    while pos < n {
        fresh.reset();
        fresh_codes.clear();
        let (fresh_end, fresh_bits) = indices.parse(&mut fresh, pos, n, &mut fresh_codes);
        codes.clear();
        let (kept_end, kept_bits) = indices.parse(&mut table, pos, fresh_end, &mut codes);
        let fresh_cost = (fresh_bits + u64::from(MAX_WIDTH)) * (kept_end - pos) as u64 * 1024;
        let kept_cost = kept_bits * (fresh_end - pos) as u64 * KEEP_BIAS;
        if fresh_cost <= kept_cost {
            out.clear(clear);
            out.codes(&fresh_codes);
            std::mem::swap(&mut table, &mut fresh);
            pos = fresh_end;
        } else {
            out.codes(&codes);
            pos = kept_end;
        }
    }
    out.put(clear + 1);
    (min_code_size, out.finish())
}

/// Assemble a GIF file from compressed frames. `palette` is the global color table as RGB
/// triples; `loops` adds the looping extension (`0` repeats forever) and is `None` for stills.
pub(crate) fn write_gif(
    width: u16,
    height: u16,
    palette: &[u8],
    loops: Option<u16>,
    frames: &[Frame],
) -> Vec<u8> {
    let colors = (palette.len() / 3).max(2);
    let table_bits = (colors - 1).bit_width().max(1) as u8;
    let lzw_len: usize = frames.iter().map(|f| f.lzw.len()).sum();
    let mut out = Vec::with_capacity(lzw_len + lzw_len / 255 + frames.len() * 24 + 800);

    out.extend_from_slice(b"GIF89a");
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&[0x80 | (table_bits - 1) << 4 | (table_bits - 1), 0, 0]);
    out.extend_from_slice(palette);
    out.resize(13 + (3usize << table_bits), 0);

    if let Some(loops) = loops {
        out.extend_from_slice(&[0x21, 0xFF, 11]);
        out.extend_from_slice(b"NETSCAPE2.0");
        out.extend_from_slice(&[3, 1]);
        out.extend_from_slice(&loops.to_le_bytes());
        out.push(0);
    }
    for frame in frames {
        if loops.is_some() || frame.transparent.is_some() {
            let flags = (frame.disposal as u8) << 2 | u8::from(frame.transparent.is_some());
            out.extend_from_slice(&[0x21, 0xF9, 4, flags]);
            out.extend_from_slice(&frame.delay.to_le_bytes());
            out.extend_from_slice(&[frame.transparent.unwrap_or(0), 0]);
        }
        out.push(0x2C);
        for field in [frame.left, frame.top, frame.width, frame.height] {
            out.extend_from_slice(&field.to_le_bytes());
        }
        out.extend_from_slice(&[0, frame.min_code_size]);
        for block in frame.lzw.chunks(255) {
            out.push(block.len() as u8);
            out.extend_from_slice(block);
        }
        out.push(0);
    }
    out.push(0x3B);
    out
}

/// Width of the next code, tracked the way a decoder grows its table.
#[derive(Debug, Clone, Copy)]
struct Widths {
    min_code_size: u8,
    /// Code the next table entry will get; [`MAX_CODES`] once the table is full.
    next: u16,
    width: u8,
}

impl Widths {
    fn new(min_code_size: u8) -> Self {
        Self {
            min_code_size,
            next: (1 << min_code_size) + 2,
            width: min_code_size + 1,
        }
    }

    fn is_full(self) -> bool {
        self.next == MAX_CODES
    }

    /// Width to write the next code with; also accounts for the table entry that code creates.
    fn step(&mut self) -> u8 {
        let width = self.width;
        if self.next < MAX_CODES {
            if self.next >= 1 << self.width {
                self.width += 1;
            }
            self.next += 1;
        }
        width
    }
}

#[derive(Debug)]
struct BitWriter {
    bytes: Vec<u8>,
    acc: u64,
    bits: u32,
    widths: Widths,
}

impl BitWriter {
    fn put(&mut self, code: u16) {
        self.push(code, self.widths.width);
    }

    fn push(&mut self, code: u16, width: u8) {
        self.acc |= u64::from(code) << self.bits;
        self.bits += u32::from(width);
        while self.bits >= 8 {
            self.bytes.push(self.acc as u8);
            self.acc >>= 8;
            self.bits -= 8;
        }
    }

    fn clear(&mut self, clear: u16) {
        self.put(clear);
        self.widths = Widths::new(self.widths.min_code_size);
    }

    fn codes(&mut self, codes: &[u16]) {
        for &code in codes {
            let width = self.widths.step();
            self.push(code, width);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.bits > 0 {
            self.bytes.push(self.acc as u8);
        }
        self.bytes
    }
}

/// The LZW code table, mapping (prefix code, next index) to the code of the longer string.
#[derive(Debug)]
struct Table {
    /// For alphabets of at most [`DENSE_MAX_CODE_SIZE`] bits: the child code at
    /// `prefix << min_code_size | index`, or 0 when absent (no child code is ever 0).
    children: Vec<u16>,
    /// For larger alphabets: open-addressing hash slots holding `key << 12 | code`.
    slots: Vec<u32>,
    widths: Widths,
    /// The entry the last written code creates, as (prefix code, entry code). Its final index is
    /// the first index of the next match, so it joins the table once that is known.
    pending: Option<(u16, u16)>,
}

impl Table {
    fn new(min_code_size: u8) -> Self {
        let dense = min_code_size <= DENSE_MAX_CODE_SIZE;
        Self {
            children: if dense {
                vec![0; usize::from(MAX_CODES) << min_code_size]
            } else {
                Vec::new()
            },
            slots: if dense {
                Vec::new()
            } else {
                vec![EMPTY; 1 << HASH_BITS]
            },
            widths: Widths::new(min_code_size),
            pending: None,
        }
    }

    fn reset(&mut self) {
        self.children.fill(0);
        self.slots.fill(EMPTY);
        self.widths = Widths::new(self.widths.min_code_size);
        self.pending = None;
    }

    fn slot(key: u32) -> usize {
        (key.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
    }

    #[inline]
    fn get(&self, prefix: u16, index: u8) -> Option<u16> {
        if !self.children.is_empty() {
            let child = self.children
                [usize::from(prefix) << self.widths.min_code_size | usize::from(index)];
            return (child != 0).then_some(child);
        }
        let key = u32::from(prefix) << 8 | u32::from(index);
        let mask = self.slots.len() - 1;
        let mut i = Self::slot(key);
        loop {
            let slot = self.slots[i];
            if slot == EMPTY {
                return None;
            }
            if slot >> 12 == key {
                return Some((slot & 0xFFF) as u16);
            }
            i = (i + 1) & mask;
        }
    }

    #[inline]
    fn insert(&mut self, prefix: u16, index: u8, code: u16) {
        if !self.children.is_empty() {
            self.children[usize::from(prefix) << self.widths.min_code_size | usize::from(index)] =
                code;
            return;
        }
        let key = u32::from(prefix) << 8 | u32::from(index);
        let mask = self.slots.len() - 1;
        let mut i = Self::slot(key);
        while self.slots[i] != EMPTY {
            i = (i + 1) & mask;
        }
        self.slots[i] = key << 12 | u32::from(code);
    }

    fn resolve_pending(&mut self, first: u8) {
        if let Some((prefix, entry)) = self.pending.take() {
            self.insert(prefix, first, entry);
        }
    }
}

impl Indices<'_> {
    /// Turn pixels from `pos` on into codes until `stop` is reached or a growing table fills.
    /// Returns the position after the last code and the bits those codes take.
    fn parse(
        &self,
        table: &mut Table,
        mut pos: usize,
        stop: usize,
        codes: &mut Vec<u16>,
    ) -> (usize, u64) {
        let mut bits = 0u64;
        while pos < stop {
            let (code, len) = if self.optional.is_empty() {
                self.longest_match(table, pos)
            } else {
                self.longest_optional_match(table, pos)
            };
            pos += len;
            codes.push(code);
            if table.widths.is_full() {
                bits += u64::from(MAX_WIDTH);
                continue;
            }
            let entry = table.widths.next;
            bits += u64::from(table.widths.step());
            table.pending = Some((code, entry));
            if table.widths.is_full() {
                break;
            }
        }
        (pos, bits)
    }

    fn longest_match(&self, table: &mut Table, pos: usize) -> (u16, usize) {
        let first = self.pixels[pos];
        table.resolve_pending(first);
        let mut code = u16::from(first);
        let mut len = 1;
        while let Some(&index) = self.pixels.get(pos + len) {
            let Some(longer) = table.get(code, index) else {
                break;
            };
            code = longer;
            len += 1;
        }
        (code, len)
    }

    /// The values pixel `i` may take: its color, then the transparent index where allowed.
    fn choices(&self, i: usize) -> ([u8; 2], usize) {
        (
            [self.pixels[i], self.transparent],
            1 + usize::from(self.optional[i]),
        )
    }

    /// Longest table match when some pixels have two allowed values. Every candidate is a table
    /// entry, so up to [`BEAM`] of them are followed side by side until none can grow.
    fn longest_optional_match(&self, table: &mut Table, pos: usize) -> (u16, usize) {
        let pending = table.pending;
        let (firsts, count) = self.choices(pos);
        let mut alive = [(0u16, 0u8); BEAM];
        let mut alive_len = count;
        for (state, &first) in alive.iter_mut().zip(&firsts[..count]) {
            *state = (u16::from(first), first);
        }
        let mut len = 1;
        while pos + len < self.pixels.len() {
            let (choices, count) = self.choices(pos + len);
            let mut longer = [(0u16, 0u8); BEAM];
            let mut longer_len = 0;
            'alive: for &(code, first) in &alive[..alive_len] {
                for &index in &choices[..count] {
                    let next = table.get(code, index).or_else(|| {
                        pending
                            .filter(|&(prefix, _)| prefix == code && index == first)
                            .map(|(_, entry)| entry)
                    });
                    if let Some(next) = next {
                        longer[longer_len] = (next, first);
                        longer_len += 1;
                        if longer_len == BEAM {
                            break 'alive;
                        }
                    }
                }
            }
            if longer_len == 0 {
                break;
            }
            alive = longer;
            alive_len = longer_len;
            len += 1;
        }
        let (code, first) = alive[0];
        table.resolve_pending(first);
        (code, len)
    }
}

#[cfg(test)]
mod tests {
    use image::codecs::gif::GifDecoder;
    use image::{AnimationDecoder, Rgba};

    use super::*;

    fn decode(bytes: &[u8]) -> Vec<RgbaImage> {
        GifDecoder::new(std::io::Cursor::new(bytes))
            .unwrap()
            .into_frames()
            .map(|frame| frame.unwrap().into_buffer())
            .collect()
    }

    fn xorshift(seed: u32) -> impl FnMut() -> u32 {
        let mut state = seed;
        move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        }
    }

    fn assert_still_round_trips(img: &RgbaImage) {
        let bytes = encode_still(img).expect("image fits one color table");
        let decoded = decode(&bytes);
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].dimensions(), img.dimensions());
        for (i, (d, s)) in decoded[0].pixels().zip(img.pixels()).enumerate() {
            if s[3] == 0 {
                assert_eq!(d[3], 0, "pixel {i} should be transparent");
            } else {
                assert_eq!(d.0, [s[0], s[1], s[2], 255], "pixel {i}");
            }
        }
    }

    #[test]
    fn still_gifs_round_trip_through_kept_and_cleared_tables() {
        let mut next = xorshift(1);
        let texture_change = RgbaImage::from_fn(700, 500, |x, y| {
            let ink = if y < 250 {
                (x / 3 + y / 2) % 5 < 2
            } else {
                next().is_multiple_of(3)
            };
            if ink {
                Rgba([0, 0, 0, 255])
            } else {
                Rgba([255, 255, 255, 255])
            }
        });
        assert_still_round_trips(&texture_change);

        let mut next = xorshift(7);
        let all_indices = RgbaImage::from_fn(400, 300, |x, _| {
            let v = if x % 7 == 0 { 255 } else { next() as u8 };
            Rgba([v, v / 2, 255 - v, 255])
        });
        assert_still_round_trips(&all_indices);

        let mut next = xorshift(3);
        let with_alpha = RgbaImage::from_fn(90, 70, |x, y| match (x + y) % 9 {
            0 => Rgba([x as u8, y as u8, 7, 0]),
            1 => Rgba([200, 10, 10, 100]),
            _ => Rgba([(next() % 254) as u8, 50, 60, 255]),
        });
        assert_still_round_trips(&with_alpha);

        assert_still_round_trips(&RgbaImage::from_pixel(1, 1, Rgba([9, 8, 7, 255])));
    }

    #[test]
    fn still_gif_with_too_many_colors_is_left_to_the_quantizer() {
        let img = RgbaImage::from_fn(300, 1, |x, _| Rgba([x as u8, (x >> 8) as u8, 0, 255]));
        assert!(encode_still(&img).is_none());
    }

    #[test]
    fn optional_pixels_decode_as_their_color_or_transparent() {
        let mut next = xorshift(11);
        let (width, height) = (160u16, 120u16);
        let n = usize::from(width) * usize::from(height);
        let forced = |i: usize| (i / 160).is_multiple_of(3) && i % 160 < 60;
        let pixels: Vec<u8> = (0..n)
            .map(|i| if forced(i) { 3 } else { (next() % 3) as u8 })
            .collect();
        let optional: Vec<bool> = (0..n)
            .map(|i| !forced(i) && i % 160 < 120 && i % 7 != 0)
            .collect();
        let (min_code_size, lzw) = compress(&Indices {
            pixels: &pixels,
            optional: &optional,
            transparent: 3,
        });
        let frame = Frame {
            left: 0,
            top: 0,
            width,
            height,
            delay: 10,
            disposal: Disposal::Keep,
            transparent: Some(3),
            min_code_size,
            lzw,
        };
        let palette = [10, 20, 30, 200, 100, 0, 0, 90, 250, 0, 0, 0];
        let decoded = decode(&write_gif(width, height, &palette, Some(0), &[frame]));
        let mut chosen = 0;
        for (i, px) in decoded[0].pixels().enumerate() {
            if px[3] == 0 {
                assert!(forced(i) || optional[i], "pixel {i} must keep its color");
                chosen += usize::from(optional[i]);
            } else {
                let c = usize::from(pixels[i]) * 3;
                assert_eq!(px.0, [palette[c], palette[c + 1], palette[c + 2], 255]);
            }
        }
        assert!(chosen > 0, "transparent never continued a match");
    }
}
