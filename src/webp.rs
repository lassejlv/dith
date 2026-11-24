//! Compact lossless WebP (VP8L) encoding.
//!
//! Dithered output almost always uses 256 or fewer colors, but `image`'s WebP encoder has no
//! color-indexing transform and no LZ77, so it stores such images as full ARGB. Palette images are
//! written here instead: a color-indexing transform that bundles 8, 4, or 2 pixels per symbol for
//! palettes of up to 2, 4, or 16 colors, cost-optimized LZ77 backward references (including the
//! short 2D neighborhood distance codes), and canonical prefix codes. Images with more than 256
//! colors fall back to `image`'s encoder.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ops::Range;
use std::sync::LazyLock;

use crate::par::prelude::*;
use anyhow::{Context, Result, ensure};
use image::codecs::webp::WebPEncoder;
use image::{ExtendedColorType, ImageEncoder, RgbaImage};

use crate::output::PngLayout;

/// Largest width or height a VP8L bitstream can describe (14-bit fields).
const MAX_DIMENSION: u32 = 1 << 14;
/// Longest LZ77 copy the 24 length prefix codes can express.
const MAX_COPY_LENGTH: usize = 4096;
/// Largest distance code the 40 distance prefix codes can express.
const MAX_DISTANCE_CODE: usize = 1 << 20;
/// Distance codes above the 120 neighborhood codes are plain scan-line distances offset by 120.
const NEIGHBORHOOD_CODES: usize = 120;
const NUM_LITERALS: usize = 256;
const NUM_LENGTH_CODES: usize = 24;
const NUM_DISTANCE_CODES: usize = 40;
const MAX_CODE_LENGTH: u32 = 15;
const MAX_CODE_LENGTH_CODE_LENGTH: u32 = 7;
const CODE_LENGTH_ORDER: [usize; 19] = [
    17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
];
/// `(dx, dy)` offsets of distance codes 1..=120, from the VP8L specification.
#[rustfmt::skip]
const NEIGHBORHOOD: [(i8, i8); NEIGHBORHOOD_CODES] = [
    (0, 1),  (1, 0),  (1, 1),  (-1, 1), (0, 2),  (2, 0),  (1, 2),
    (-1, 2), (2, 1),  (-2, 1), (2, 2),  (-2, 2), (0, 3),  (3, 0),
    (1, 3),  (-1, 3), (3, 1),  (-3, 1), (2, 3),  (-2, 3), (3, 2),
    (-3, 2), (0, 4),  (4, 0),  (1, 4),  (-1, 4), (4, 1),  (-4, 1),
    (3, 3),  (-3, 3), (2, 4),  (-2, 4), (4, 2),  (-4, 2), (0, 5),
    (3, 4),  (-3, 4), (4, 3),  (-4, 3), (5, 0),  (1, 5),  (-1, 5),
    (5, 1),  (-5, 1), (2, 5),  (-2, 5), (5, 2),  (-5, 2), (4, 4),
    (-4, 4), (3, 5),  (-3, 5), (5, 3),  (-5, 3), (0, 6),  (6, 0),
    (1, 6),  (-1, 6), (6, 1),  (-6, 1), (2, 6),  (-2, 6), (6, 2),
    (-6, 2), (4, 5),  (-4, 5), (5, 4),  (-5, 4), (3, 6),  (-3, 6),
    (6, 3),  (-6, 3), (0, 7),  (7, 0),  (1, 7),  (-1, 7), (5, 5),
    (-5, 5), (7, 1),  (-7, 1), (4, 6),  (-4, 6), (6, 4),  (-6, 4),
    (2, 7),  (-2, 7), (7, 2),  (-7, 2), (3, 7),  (-3, 7), (7, 3),
    (-7, 3), (5, 6),  (-5, 6), (6, 5),  (-6, 5), (8, 0),  (4, 7),
    (-4, 7), (7, 4),  (-7, 4), (8, 1),  (8, 2),  (6, 6),  (-6, 6),
    (8, 3),  (5, 7),  (-5, 7), (7, 5),  (-7, 5), (8, 4),  (6, 7),
    (-6, 7), (7, 6),  (-7, 6), (8, 5),  (7, 7),  (-7, 7), (8, 6),
    (8, 7),
];
/// Most bits of the hash-chain head table; small streams use about one slot per symbol.
const HASH_BITS: u32 = 18;
const NO_POSITION: u32 = u32::MAX;
/// Neighborhood distance codes whose matches are tracked at every position, down to length 1;
/// the hash chain finds matches of 3 or more at all other distances.
const TRACKED_CODES: usize = 24;
/// Hash-chain candidates examined per position.
const CHAIN_LIMIT: usize = 64;
/// A match at least this long is taken outright, without pricing the positions it covers.
const LONG_MATCH: usize = 256;
/// Copy lengths up to this are all priced individually; longer ones only at prefix-code boundaries.
const EXACT_LENGTHS: usize = 32;
/// Optimal-parse passes over sub-images, each priced with the statistics of the previous one.
const PARSE_PASSES: usize = 2;
/// Farthest distance, in symbols, the hash chains look back. Distant short matches cost about as
/// much as the literals they replace, and skipping them keeps the chain walk short.
const WINDOW: usize = 1 << 16;
const _: () = assert!(WINDOW + NEIGHBORHOOD_CODES <= MAX_DISTANCE_CODE);
/// Symbols per independently parsed segment. Segments are searched and parsed in parallel; copies
/// may reach back into earlier segments but never run past the end of their own. The size is
/// fixed, so output never depends on the number of threads.
const SEGMENT: usize = 1 << 15;

/// Encode `img` as a lossless WebP file.
///
/// Images with 256 or fewer distinct RGBA colors use a palette-aware VP8L encoder; others use
/// `image`'s lossless WebP encoder. Every pixel, alpha included, round-trips exactly.
///
/// # Errors
/// Returns an error if the image is larger than 16384x16384 pixels or encoding fails.
pub fn encode_lossless(img: &RgbaImage) -> Result<Vec<u8>> {
    let data = encode_vp8l(img)?;
    let mut out = Vec::with_capacity(data.len() + 21);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(b"WEBP");
    push_chunk(&mut out, *b"VP8L", &data)?;
    finish_riff(out)
}

/// One frame of an animated WebP: a VP8L bitstream placed on the canvas.
#[derive(Debug, Clone)]
pub struct AnimationFrame {
    /// Left edge on the canvas; must be even.
    pub x: u32,
    /// Top edge on the canvas; must be even.
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Display time in milliseconds (at most 2^24 - 1).
    pub duration_ms: u32,
    /// VP8L bitstream from [`encode_vp8l`].
    pub bitstream: Vec<u8>,
}

/// Write an animated WebP file: a `VP8X` header (flagging alpha when any frame uses it), an
/// `ANIM` chunk with a transparent background and `loops` (`0` = infinite), and one `ANMF` chunk
/// per frame. Frames neither blend nor dispose: each replaces its rectangle of the canvas the
/// previous frames left behind.
///
/// # Errors
/// Returns an error if the canvas or a frame exceeds WebP's limits, a frame lies outside the
/// canvas or at an odd offset, or the file would exceed 4 GiB.
pub fn encode_animation(
    canvas: (u32, u32),
    loops: u16,
    frames: &[AnimationFrame],
) -> Result<Vec<u8>> {
    let (width, height) = canvas;
    ensure!(
        (1..=MAX_DIMENSION).contains(&width) && (1..=MAX_DIMENSION).contains(&height),
        "WebP supports at most {MAX_DIMENSION}x{MAX_DIMENSION} pixels, got {width}x{height}"
    );
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(b"WEBP");

    let has_alpha = frames
        .iter()
        .any(|f| f.bitstream.get(4).is_some_and(|&b| b & 0x10 != 0));
    let mut vp8x = vec![if has_alpha { 0x12 } else { 0x02 }, 0, 0, 0];
    vp8x.extend_from_slice(&u24(width - 1));
    vp8x.extend_from_slice(&u24(height - 1));
    push_chunk(&mut out, *b"VP8X", &vp8x)?;

    let mut anim = vec![0u8; 4];
    anim.extend_from_slice(&loops.to_le_bytes());
    push_chunk(&mut out, *b"ANIM", &anim)?;

    for frame in frames {
        ensure!(
            frame.x % 2 == 0 && frame.y % 2 == 0,
            "WebP frame offsets must be even, got ({}, {})",
            frame.x,
            frame.y
        );
        ensure!(
            frame.width >= 1
                && frame.height >= 1
                && frame.x + frame.width <= width
                && frame.y + frame.height <= height,
            "WebP frame {}x{} at ({}, {}) does not fit the {width}x{height} canvas",
            frame.width,
            frame.height,
            frame.x,
            frame.y
        );
        let mut anmf = Vec::with_capacity(frame.bitstream.len() + 25);
        anmf.extend_from_slice(&u24(frame.x / 2));
        anmf.extend_from_slice(&u24(frame.y / 2));
        anmf.extend_from_slice(&u24(frame.width - 1));
        anmf.extend_from_slice(&u24(frame.height - 1));
        anmf.extend_from_slice(&u24(frame.duration_ms.min(0xFF_FFFF)));
        anmf.push(0x02);
        push_chunk(&mut anmf, *b"VP8L", &frame.bitstream)?;
        push_chunk(&mut out, *b"ANMF", &anmf)?;
    }
    finish_riff(out)
}

fn u24(value: u32) -> [u8; 3] {
    let [a, b, c, _] = value.to_le_bytes();
    [a, b, c]
}

/// Append a RIFF chunk, padded to an even length.
fn push_chunk(out: &mut Vec<u8>, fourcc: [u8; 4], payload: &[u8]) -> Result<()> {
    let len = u32::try_from(payload.len()).context("WebP chunk exceeds 4 GiB")?;
    out.extend_from_slice(&fourcc);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        out.push(0);
    }
    Ok(())
}

/// Fill in the RIFF size of a file built from `RIFF`, a placeholder size, `WEBP`, and chunks.
fn finish_riff(mut out: Vec<u8>) -> Result<Vec<u8>> {
    let size = u32::try_from(out.len() - 8).context("WebP file exceeds 4 GiB")?;
    out[4..8].copy_from_slice(&size.to_le_bytes());
    Ok(out)
}

/// Encode `img` as a bare VP8L bitstream, the payload of a `VP8L` chunk.
///
/// The LZ77 search runs on a rayon worker, so its many short parallel steps never wait for a
/// thread to wake up.
///
/// # Errors
/// Returns an error if the image is larger than 16384x16384 pixels or encoding fails.
pub fn encode_vp8l(img: &RgbaImage) -> Result<Vec<u8>> {
    let PngLayout::Indexed(palette) = PngLayout::for_images([img]) else {
        let mut file = Vec::new();
        WebPEncoder::new_lossless(&mut file)
            .write_image(
                img.as_raw(),
                img.width(),
                img.height(),
                ExtendedColorType::Rgba8,
            )
            .context("encoding webp output")?;
        return vp8l_payload(&file).context("image's WebP encoder wrote no VP8L chunk");
    };
    let (width, height) = img.dimensions();
    ensure!(
        width <= MAX_DIMENSION && height <= MAX_DIMENSION,
        "WebP supports at most {MAX_DIMENSION}x{MAX_DIMENSION} pixels, got {width}x{height}"
    );

    let width_bits: u32 = match palette.len() {
        0..=2 => 3,
        3..=4 => 2,
        5..=16 => 1,
        _ => 0,
    };
    let (indices, xsize) = bundle_indices(img, &palette, width_bits);

    let mut w = BitWriter::default();
    w.put(0x2f, 8);
    w.put(width - 1, 14);
    w.put(height - 1, 14);
    w.put(u32::from(palette.iter().any(|c| c[3] < 255)), 1);
    w.put(0, 3);

    w.put(1, 1);
    w.put(3, 2);
    w.put(palette.len() as u32 - 1, 8);
    let mut previous = [0u8; 4];
    let table: Vec<Token> = palette
        .iter()
        .map(|&color| {
            let delta = std::array::from_fn(|i| color[i].wrapping_sub(previous[i]));
            previous = color;
            Token::Literal(delta)
        })
        .collect();
    w.put(0, 1);
    write_entropy_coded_image(&mut w, &table);

    w.put(0, 1);
    w.put(0, 1);
    let (tokens, meta) = crate::par::on_worker(|| main_image(&indices, xsize, height as usize));
    write_main_image(&mut w, &tokens, meta.as_ref(), xsize);
    Ok(w.finish())
}

/// The payload of the first `VP8L` chunk of a WebP file.
fn vp8l_payload(file: &[u8]) -> Option<Vec<u8>> {
    let mut rest = file.get(12..)?;
    while rest.len() >= 8 {
        let len = u32::from_le_bytes(rest[4..8].try_into().ok()?) as usize;
        let payload = rest.get(8..8 + len)?;
        if &rest[..4] == b"VP8L" {
            return Some(payload.to_vec());
        }
        rest = rest.get(8 + len + len % 2..)?;
    }
    None
}

/// Replace every pixel by its palette index and pack `8 >> width_bits`-bit indices
/// `1 << width_bits` to a symbol, least significant bits first. Returns the symbols and their
/// row width.
fn bundle_indices(img: &RgbaImage, palette: &[[u8; 4]], width_bits: u32) -> (Vec<u8>, usize) {
    let mut lookup: Vec<(u32, u8)> = (0u8..)
        .zip(palette)
        .map(|(i, &c)| (u32::from_le_bytes(c), i))
        .collect();
    lookup.sort_unstable();
    let mut last = lookup[0];
    let width = img.width() as usize;
    let xsize = width.div_ceil(1 << width_bits);
    let bits_per_index = 8 >> width_bits;
    let lane_mask = (1 << width_bits) - 1;
    let mut symbols = vec![0u8; xsize * img.height() as usize];
    let (pixels, _) = img.as_raw().as_chunks::<4>();
    for (row, out_row) in pixels.chunks(width).zip(symbols.chunks_mut(xsize)) {
        for (x, &px) in row.iter().enumerate() {
            let key = u32::from_le_bytes(px);
            if key != last.0 {
                let slot = lookup.partition_point(|&(k, _)| k < key);
                last = lookup[slot];
            }
            out_row[x >> width_bits] |= last.1 << ((x & lane_mask) * bits_per_index);
        }
    }
    (symbols, xsize)
}

/// One coded element of an entropy-coded image.
#[derive(Debug, Clone, Copy)]
enum Token {
    /// A literal pixel as `[r, g, b, a]`.
    Literal([u8; 4]),
    /// Copy `length` pixels starting `code`'s distance back.
    Copy { length: usize, code: usize },
}

/// Split a length or distance code (`>= 1`) into its prefix symbol, extra-bit count, and extra bits.
fn prefix_encode(value: usize) -> (usize, u32, u32) {
    let v = value - 1;
    if v < 4 {
        return (v, 0, 0);
    }
    let high = v.ilog2();
    let extra_bits = high - 1;
    let second = (v >> extra_bits) & 1;
    (
        2 * high as usize + second,
        extra_bits,
        (v & ((1 << extra_bits) - 1)) as u32,
    )
}

/// Symbol counts of the five prefix codes of one entropy-coded image.
#[derive(Debug)]
struct Histograms {
    green: Vec<u32>,
    red: Vec<u32>,
    blue: Vec<u32>,
    alpha: Vec<u32>,
    distance: Vec<u32>,
    extra_bits: u64,
}

impl Histograms {
    fn new(tokens: &[Token]) -> Self {
        let mut h = Self::empty();
        for &token in tokens {
            h.add(token);
        }
        h
    }

    fn empty() -> Self {
        Self {
            green: vec![0; NUM_LITERALS + NUM_LENGTH_CODES],
            red: vec![0; NUM_LITERALS],
            blue: vec![0; NUM_LITERALS],
            alpha: vec![0; NUM_LITERALS],
            distance: vec![0; NUM_DISTANCE_CODES],
            extra_bits: 0,
        }
    }

    fn add(&mut self, token: Token) {
        match token {
            Token::Literal([r, g, b, a]) => {
                self.green[usize::from(g)] += 1;
                self.red[usize::from(r)] += 1;
                self.blue[usize::from(b)] += 1;
                self.alpha[usize::from(a)] += 1;
            }
            Token::Copy { length, code } => {
                let (length_symbol, length_extra, _) = prefix_encode(length);
                let (distance_symbol, distance_extra, _) = prefix_encode(code);
                self.green[NUM_LITERALS + length_symbol] += 1;
                self.distance[distance_symbol] += 1;
                self.extra_bits += u64::from(length_extra + distance_extra);
            }
        }
    }

    fn all(&self) -> [&Vec<u32>; 5] {
        [
            &self.green,
            &self.red,
            &self.blue,
            &self.alpha,
            &self.distance,
        ]
    }

    fn codes(&self) -> [PrefixCode; 5] {
        self.all()
            .map(|histogram| PrefixCode::new(histogram, MAX_CODE_LENGTH))
    }

    /// Size of the coded symbols in bits, excluding the prefix code headers.
    fn coded_bits(&self) -> u64 {
        self.coded_bits_with(&self.codes())
    }

    /// Size of the coded symbols under `codes` in bits, excluding the prefix code headers.
    fn coded_bits_with(&self, codes: &[PrefixCode; 5]) -> u64 {
        self.extra_bits
            + self
                .all()
                .iter()
                .zip(codes)
                .map(|(histogram, code)| {
                    histogram
                        .iter()
                        .zip(&code.widths)
                        .map(|(&count, &width)| u64::from(count) * u64::from(width))
                        .sum::<u64>()
                })
                .sum::<u64>()
    }
}

/// Write the five prefix codes for `tokens` followed by the tokens themselves.
fn write_entropy_coded_image(w: &mut BitWriter, tokens: &[Token]) {
    let codes = Histograms::new(tokens).codes();
    for code in &codes {
        code.write_header(w);
    }
    for &token in tokens {
        write_token(w, &codes, token);
    }
}

/// Write one token with the green, red, blue, alpha, and distance codes `codes`.
fn write_token(w: &mut BitWriter, codes: &[PrefixCode; 5], token: Token) {
    let [green, red, blue, alpha, distance] = codes;
    match token {
        Token::Literal([r, g, b, a]) => {
            green.write(w, usize::from(g));
            red.write(w, usize::from(r));
            blue.write(w, usize::from(b));
            alpha.write(w, usize::from(a));
        }
        Token::Copy { length, code } => {
            let (symbol, extra_bits, extra) = prefix_encode(length);
            green.write(w, NUM_LITERALS + symbol);
            w.put(extra, extra_bits);
            let (symbol, extra_bits, extra) = prefix_encode(code);
            distance.write(w, symbol);
            w.put(extra, extra_bits);
        }
    }
}

/// Position of the first symbol of every token.
fn token_starts(tokens: &[Token]) -> impl Iterator<Item = usize> {
    tokens.iter().scan(0, |position, token| {
        let start = *position;
        *position += match *token {
            Token::Literal(_) => 1,
            Token::Copy { length, .. } => length,
        };
        Some(start)
    })
}

/// Bits taken by the headers of `codes`.
fn header_bits(codes: &[PrefixCode; 5]) -> u64 {
    let mut w = BitWriter::default();
    for code in codes {
        code.write_header(&mut w);
    }
    w.len_bits()
}

/// Bits taken by `tokens` written as an entropy-coded image with its own codes.
fn entropy_coded_bits(tokens: &[Token]) -> u64 {
    let histograms = Histograms::new(tokens);
    let codes = histograms.codes();
    header_bits(&codes) + histograms.coded_bits_with(&codes)
}

/// Prefix code groups assigned to square tiles of the main image (VP8L meta prefix codes).
#[derive(Debug)]
struct MetaCodes {
    /// Tiles are `1 << bits` symbols wide and tall.
    bits: u32,
    tiles_x: usize,
    /// Group of every tile, row by row, numbered densely from 0.
    tile_groups: Vec<u8>,
}

impl MetaCodes {
    /// Group of the tile holding symbol `position`.
    fn group_at(&self, position: usize, xsize: usize) -> u8 {
        let (x, y) = (position % xsize, position / xsize);
        self.tile_groups[(y >> self.bits) * self.tiles_x + (x >> self.bits)]
    }

    /// Group of each token, from the tile holding its first symbol.
    fn token_groups(&self, tokens: &[Token], xsize: usize) -> Vec<u8> {
        token_starts(tokens)
            .map(|start| self.group_at(start, xsize))
            .collect()
    }

    /// Histograms of every group.
    fn histograms(&self, tokens: &[Token], groups: &[u8]) -> Vec<Histograms> {
        let count = usize::from(self.tile_groups.iter().copied().max().unwrap_or(0)) + 1;
        let mut histograms: Vec<Histograms> = (0..count).map(|_| Histograms::empty()).collect();
        for (&token, &group) in tokens.iter().zip(groups) {
            histograms[usize::from(group)].add(token);
        }
        histograms
    }

    /// Exact size in bits of the meta prefix code image, every group's codes, and `tokens`.
    fn total_bits(&self, tokens: &[Token], xsize: usize) -> u64 {
        let groups = self.token_groups(tokens, xsize);
        5 + entropy_coded_bits(&self.image_tokens())
            + self
                .histograms(tokens, &groups)
                .iter()
                .map(|h| {
                    let codes = h.codes();
                    header_bits(&codes) + h.coded_bits_with(&codes)
                })
                .sum::<u64>()
    }

    /// Tokens of the meta prefix code image: one pixel per tile, its group in green.
    fn image_tokens(&self) -> Vec<Token> {
        backward_references(&self.tile_groups, self.tiles_x)
    }
}

/// Write the main image's codes and tokens: a single set of five prefix codes, or meta prefix
/// codes giving each tile one of several sets when that is smaller.
fn write_main_image(w: &mut BitWriter, tokens: &[Token], meta: Option<&MetaCodes>, xsize: usize) {
    let Some(meta) = meta else {
        w.put(0, 1);
        write_entropy_coded_image(w, tokens);
        return;
    };
    w.put(1, 1);
    w.put(meta.bits - 2, 3);
    w.put(0, 1);
    write_entropy_coded_image(w, &meta.image_tokens());
    let groups = meta.token_groups(tokens, xsize);
    let codes: Vec<[PrefixCode; 5]> = meta
        .histograms(tokens, &groups)
        .iter()
        .map(Histograms::codes)
        .collect();
    for group in &codes {
        for code in group {
            code.write_header(w);
        }
    }
    for (&token, &group) in tokens.iter().zip(&groups) {
        write_token(w, &codes[usize::from(group)], token);
    }
}

/// Most tiles a meta prefix code candidate may have, bounding the clustering work.
const MAX_TILES: usize = 4096;
/// Tile sizes tried, from the smallest one that respects `MAX_TILES`.
const TILE_SIZES_TRIED: usize = 2;
/// Most prefix code groups a meta prefix code image may use.
const MAX_GROUPS: usize = 256;
/// Tile-group pairs priced per parallel clustering job; fewer go to one thread.
const TILES_PER_JOB: usize = 1 << 12;
/// Assignment rounds per clustering step.
const CLUSTER_ROUNDS: usize = 3;
/// Symbols of a tile histogram: green (literals and lengths), then distances.
const CLUSTER_SYMBOLS: usize = NUM_LITERALS + NUM_LENGTH_CODES + NUM_DISTANCE_CODES;

/// The smallest meta prefix code layout for `tokens` (`None` when one set of codes for the
/// whole image is smallest) and the exact size in bits it codes the image in.
///
/// For each tile size, tiles are clustered by their symbol
/// statistics (k-means under an entropy cost, seeded at the tiles the current groups code worst),
/// doubling the group count while the exact coded size keeps shrinking noticeably.
fn plan_meta_codes(tokens: &[Token], xsize: usize, ysize: usize) -> (u64, Option<MetaCodes>) {
    let single = 1 + entropy_coded_bits(tokens);
    let palette_literals = tokens
        .iter()
        .all(|t| matches!(t, Token::Copy { .. } | Token::Literal([0, _, 0, 255])));
    if !palette_literals {
        return (single, None);
    }
    let mut best = (single, None);
    let extra_bits = Histograms::new(tokens).extra_bits;
    let tile_sizes = (2..=9u32)
        .filter(|&bits| xsize.div_ceil(1 << bits) * ysize.div_ceil(1 << bits) <= MAX_TILES)
        .take(TILE_SIZES_TRIED);
    for bits in tile_sizes {
        let tiles_x = xsize.div_ceil(1 << bits);
        let tiles = tiles_x * ysize.div_ceil(1 << bits);
        if tiles < 2 {
            break;
        }
        let mut counts = vec![[0u32; CLUSTER_SYMBOLS]; tiles];
        for (&token, start) in tokens.iter().zip(token_starts(tokens)) {
            let (x, y) = (start % xsize, start / xsize);
            let tile = &mut counts[(y >> bits) * tiles_x + (x >> bits)];
            match token {
                Token::Literal([_, g, _, _]) => tile[usize::from(g)] += 1,
                Token::Copy { length, code } => {
                    tile[NUM_LITERALS + prefix_encode(length).0] += 1;
                    tile[NUM_LITERALS + NUM_LENGTH_CODES + prefix_encode(code).0] += 1;
                }
            }
        }
        let mut clustering = Clustering::new(&counts);
        let mut previous = single;
        while clustering.groups() < MAX_GROUPS.min(tiles) {
            clustering.grow((clustering.groups() * 2).min(MAX_GROUPS));
            clustering.refine();
            let meta = MetaCodes {
                bits,
                tiles_x,
                tile_groups: clustering.dense_assignment(),
            };
            let size = 5
                + extra_bits
                + entropy_coded_bits(&meta.image_tokens())
                + clustering.counts.iter().map(group_bits).sum::<u64>();
            let worthwhile = size + size / 400 < previous;
            previous = previous.min(size);
            if size < best.0 {
                best = (size, Some(meta));
            }
            if !worthwhile {
                break;
            }
        }
    }
    best
}

/// Exact bits of one group's five prefix codes and its symbols (extra bits excluded), from
/// counts laid out like `CLUSTER_SYMBOLS` for palette literals `[0, index, 0, 255]`.
fn group_bits(counts: &[u32; CLUSTER_SYMBOLS]) -> u64 {
    let (green, distance) = counts.split_at(NUM_LITERALS + NUM_LENGTH_CODES);
    let literals: u32 = green[..NUM_LITERALS].iter().sum();
    let mut zero = vec![0; NUM_LITERALS];
    zero[0] = literals;
    let mut opaque = vec![0; NUM_LITERALS];
    opaque[255] = literals;
    let histograms = Histograms {
        green: green.to_vec(),
        red: zero.clone(),
        blue: zero,
        alpha: opaque,
        distance: distance.to_vec(),
        extra_bits: 0,
    };
    let codes = histograms.codes();
    header_bits(&codes) + histograms.coded_bits_with(&codes)
}

/// k-means clustering of tile histograms under an entropy cost.
#[derive(Debug)]
struct Clustering {
    /// Nonzero `(symbol, count)` entries of every tile.
    tiles: Vec<Vec<(u16, u32)>>,
    /// Bits each tile would take under codes fitted to it alone.
    own_bits: Vec<f32>,
    assignment: Vec<usize>,
    /// Bits each tile takes under its group's costs.
    assigned_bits: Vec<f32>,
    /// Symbol counts of every group.
    counts: Vec<[u32; CLUSTER_SYMBOLS]>,
    /// Estimated bits per symbol of every group.
    costs: Vec<Vec<f32>>,
}

impl Clustering {
    fn new(counts: &[[u32; CLUSTER_SYMBOLS]]) -> Self {
        let tiles: Vec<Vec<(u16, u32)>> = counts
            .iter()
            .map(|tile| {
                (0u16..)
                    .zip(tile)
                    .filter(|&(_, &c)| c > 0)
                    .map(|(s, &c)| (s, c))
                    .collect()
            })
            .collect();
        let own_bits = counts
            .iter()
            .zip(&tiles)
            .map(|(tile, entries)| {
                let costs = entropy_costs(tile);
                entries
                    .iter()
                    .fold(0.0, |sum, &(s, c)| sum + costs[usize::from(s)] * c as f32)
            })
            .collect();
        let mut clustering = Self {
            assignment: vec![0; tiles.len()],
            assigned_bits: vec![0.0; tiles.len()],
            tiles,
            own_bits,
            counts: Vec::new(),
            costs: Vec::new(),
        };
        clustering.update();
        clustering
    }

    fn groups(&self) -> usize {
        self.costs.len()
    }

    /// Bits of `tile` under the costs of group `group`.
    fn bits(&self, tile: usize, group: usize) -> f32 {
        let costs = &self.costs[group];
        self.tiles[tile]
            .iter()
            .fold(0.0, |sum, &(s, c)| sum + costs[usize::from(s)] * c as f32)
    }

    /// Recompute group costs from the current assignment, dropping empty groups.
    fn update(&mut self) {
        let mut counts: Vec<[u32; CLUSTER_SYMBOLS]> = Vec::new();
        let mut renumber: Vec<Option<usize>> =
            vec![None; self.assignment.iter().max().map_or(1, |&g| g + 1)];
        for (tile, group) in self.tiles.iter().zip(&mut self.assignment) {
            let dense = *renumber[*group].get_or_insert_with(|| {
                counts.push([0; CLUSTER_SYMBOLS]);
                counts.len() - 1
            });
            *group = dense;
            for &(s, c) in tile {
                counts[dense][usize::from(s)] += c;
            }
        }
        self.costs = counts.iter().map(entropy_costs).collect();
        self.counts = counts;
        self.assigned_bits = (0..self.tiles.len())
            .map(|t| self.bits(t, self.assignment[t]))
            .collect();
    }

    /// Add groups seeded at the tiles that lose the most bits under their current group until
    /// there are `target` groups.
    fn grow(&mut self, target: usize) {
        let mut current = self.assigned_bits.clone();
        while self.groups() < target {
            let Some((seed, loss)) = current
                .iter()
                .zip(&self.own_bits)
                .map(|(&bits, &own)| bits - own)
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)))
            else {
                return;
            };
            if loss <= 0.0 {
                return;
            }
            let mut counts = [0u32; CLUSTER_SYMBOLS];
            for &(s, c) in &self.tiles[seed] {
                counts[usize::from(s)] = c;
            }
            self.costs.push(entropy_costs(&counts));
            self.counts.push(counts);
            let group = self.groups() - 1;
            self.assignment[seed] = group;
            for (t, bits) in current.iter_mut().enumerate() {
                *bits = bits.min(self.bits(t, group));
            }
            current[seed] = self.own_bits[seed];
        }
    }

    /// Alternate assigning every tile to its cheapest group and refitting group costs.
    fn refine(&mut self) {
        for _ in 0..CLUSTER_ROUNDS {
            let assignment: Vec<usize> = (0..self.tiles.len())
                .into_par_iter()
                .with_min_len(TILES_PER_JOB / self.groups())
                .map(|t| {
                    (0..self.groups())
                        .map(|g| (self.bits(t, g), g))
                        .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)))
                        .map_or(0, |(_, g)| g)
                })
                .collect();
            let changed = assignment != self.assignment;
            self.assignment = assignment;
            self.update();
            if !changed {
                return;
            }
        }
    }

    /// Group of every tile, numbered densely from 0 in order of first use.
    fn dense_assignment(&self) -> Vec<u8> {
        let mut renumber = vec![None; self.groups()];
        let mut next = 0u8;
        self.assignment
            .iter()
            .map(|&g| {
                *renumber[g].get_or_insert_with(|| {
                    next += 1;
                    next - 1
                })
            })
            .collect()
    }
}

/// Entropy-coded bits per symbol for counts laid out like `CLUSTER_SYMBOLS`, green and distance
/// symbols priced separately.
fn entropy_costs(counts: &[u32; CLUSTER_SYMBOLS]) -> Vec<f32> {
    let (green, distance) = counts.split_at(NUM_LITERALS + NUM_LENGTH_CODES);
    let mut costs = symbol_costs(green);
    costs.extend(symbol_costs(distance));
    costs
}

/// A canonical prefix code.
#[derive(Debug)]
struct PrefixCode {
    /// Code lengths as transmitted; a lone symbol has length 1.
    lengths: Vec<u8>,
    /// Bits actually written per symbol; a lone symbol takes none.
    widths: Vec<u8>,
    /// Bit-reversed codes, ready for least-significant-bit-first output.
    codes: Vec<u16>,
}

impl PrefixCode {
    fn new(histogram: &[u32], max_length: u32) -> Self {
        let lengths = huffman_lengths(histogram, max_length);
        let mut codes = vec![0u16; lengths.len()];
        if lengths.iter().filter(|&&l| l > 0).count() < 2 {
            let widths = vec![0; lengths.len()];
            return Self {
                lengths,
                widths,
                codes,
            };
        }
        let mut count = [0u16; 16];
        for &length in &lengths {
            count[usize::from(length)] += 1;
        }
        count[0] = 0;
        let mut next = [0u16; 16];
        for length in 1..16 {
            next[length] = (next[length - 1] + count[length - 1]) << 1;
        }
        for (code, &length) in codes.iter_mut().zip(&lengths) {
            if length > 0 {
                let canonical = next[usize::from(length)];
                next[usize::from(length)] += 1;
                *code = canonical.reverse_bits() >> (16 - length);
            }
        }
        Self {
            widths: lengths.clone(),
            lengths,
            codes,
        }
    }

    fn write(&self, w: &mut BitWriter, symbol: usize) {
        w.put(
            u32::from(self.codes[symbol]),
            u32::from(self.widths[symbol]),
        );
    }

    fn write_header(&self, w: &mut BitWriter) {
        let used: Vec<usize> = (0..self.lengths.len())
            .filter(|&s| self.lengths[s] > 0)
            .collect();
        if used.len() <= 2 && used.iter().all(|&s| s < NUM_LITERALS) {
            w.put(1, 1);
            w.put(used.len() as u32 - 1, 1);
            if used[0] < 2 {
                w.put(0, 1);
                w.put(used[0] as u32, 1);
            } else {
                w.put(1, 1);
                w.put(used[0] as u32, 8);
            }
            if let Some(&second) = used.get(1) {
                w.put(second as u32, 8);
            }
            return;
        }

        let tokens = code_length_tokens(&self.lengths);
        let trailing_zeros = tokens
            .iter()
            .rev()
            .take_while(|&&(symbol, _)| matches!(symbol, 0 | 17 | 18))
            .count();
        let written = (tokens.len() - trailing_zeros).max(2).min(tokens.len());
        let mut histogram = [0u32; 19];
        for &(symbol, _) in &tokens[..written] {
            histogram[usize::from(symbol)] += 1;
        }
        let code_length_code = Self::new(&histogram, MAX_CODE_LENGTH_CODE_LENGTH);
        let num_code_lengths = CODE_LENGTH_ORDER
            .iter()
            .rposition(|&s| code_length_code.lengths[s] > 0)
            .map_or(0, |i| i + 1)
            .max(4);

        w.put(0, 1);
        w.put(num_code_lengths as u32 - 4, 4);
        for &symbol in &CODE_LENGTH_ORDER[..num_code_lengths] {
            w.put(u32::from(code_length_code.lengths[symbol]), 3);
        }
        if written < tokens.len() {
            let max_symbol = (written - 2) as u32;
            let bit_pairs = max_symbol.checked_ilog2().unwrap_or(0) / 2 + 1;
            w.put(1, 1);
            w.put(bit_pairs - 1, 3);
            w.put(max_symbol, bit_pairs * 2);
        } else {
            w.put(0, 1);
        }
        for &(symbol, extra) in &tokens[..written] {
            code_length_code.write(w, usize::from(symbol));
            match symbol {
                16 => w.put(u32::from(extra), 2),
                17 => w.put(u32::from(extra), 3),
                18 => w.put(u32::from(extra), 7),
                _ => {}
            }
        }
    }
}

/// Run-length tokens `(symbol, extra bits)` for a list of code lengths: 0..=15 literal, 16 repeats
/// the previous length 3..=6 times, 17 and 18 emit 3..=10 and 11..=138 zeros.
fn code_length_tokens(lengths: &[u8]) -> Vec<(u8, u8)> {
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < lengths.len() {
        let value = lengths[i];
        let run = lengths[i..].iter().take_while(|&&l| l == value).count();
        i += run;
        let mut remaining = run;
        if value == 0 {
            while remaining >= 11 {
                let n = remaining.min(138);
                tokens.push((18, (n - 11) as u8));
                remaining -= n;
            }
            if remaining >= 3 {
                tokens.push((17, (remaining - 3) as u8));
                remaining = 0;
            }
        } else {
            tokens.push((value, 0));
            remaining -= 1;
            while remaining >= 3 {
                let n = remaining.min(6);
                tokens.push((16, (n - 3) as u8));
                remaining -= n;
            }
        }
        tokens.extend(std::iter::repeat_n((value, 0), remaining));
    }
    tokens
}

/// Huffman code lengths for `histogram`, limited to `max_length` bits.
///
/// Fewer than two used symbols yield a single symbol of length 1 (symbol 0 when nothing is used).
/// Over-long codes are fixed by raising the smallest counts until the tree is shallow enough.
fn huffman_lengths(histogram: &[u32], max_length: u32) -> Vec<u8> {
    let mut lengths = vec![0u8; histogram.len()];
    let used: Vec<usize> = (0..histogram.len()).filter(|&s| histogram[s] > 0).collect();
    if used.len() < 2 {
        lengths[used.first().copied().unwrap_or(0)] = 1;
        return lengths;
    }
    let mut floor = 1u64;
    loop {
        let mut heap: BinaryHeap<Reverse<(u64, usize)>> = used
            .iter()
            .enumerate()
            .map(|(node, &s)| Reverse((u64::from(histogram[s]).max(floor), node)))
            .collect();
        let mut parent = vec![0usize; 2 * used.len() - 1];
        let mut next = used.len();
        while let (Some(Reverse((wa, a))), Some(Reverse((wb, b)))) = (heap.pop(), heap.pop()) {
            parent[a] = next;
            parent[b] = next;
            heap.push(Reverse((wa + wb, next)));
            next += 1;
        }
        let mut depth = vec![0u32; next];
        for node in (0..next - 1).rev() {
            depth[node] = depth[parent[node]] + 1;
        }
        if depth[..used.len()].iter().all(|&d| d <= max_length) {
            for (&symbol, &d) in used.iter().zip(&depth) {
                lengths[symbol] = d as u8;
            }
            return lengths;
        }
        floor *= 2;
    }
}

/// Estimated bit costs used by the optimal LZ77 parse.
#[derive(Debug)]
struct CostModel {
    literal: Vec<f32>,
    /// Cost of a copy length, indexed by length (`0..=MAX_COPY_LENGTH`).
    length: Vec<f32>,
    distance: Vec<f32>,
}

impl CostModel {
    /// Literal costs from the symbol frequencies, with every length and distance prefix assumed
    /// moderately likely so the first parse explores backward references.
    fn initial(symbols: &[u8]) -> Self {
        let mut green = vec![0u32; NUM_LITERALS + NUM_LENGTH_CODES];
        for &s in symbols {
            green[usize::from(s)] += 1;
        }
        let guess = (symbols.len() / 64).max(1) as u32;
        green[NUM_LITERALS..].fill(guess);
        Self::new(&green, &[1; NUM_DISTANCE_CODES])
    }

    fn new(green: &[u32], distance: &[u32]) -> Self {
        /// Prefix symbol and extra-bit count of every copy length.
        static LENGTH_PREFIXES: LazyLock<Vec<(usize, f32)>> = LazyLock::new(|| {
            (0..=MAX_COPY_LENGTH)
                .map(|l| {
                    let (symbol, extra_bits, _) = prefix_encode(l.max(1));
                    (symbol, extra_bits as f32)
                })
                .collect()
        });
        let green = symbol_costs(green);
        let length_prefix = &green[NUM_LITERALS..];
        let length = LENGTH_PREFIXES
            .iter()
            .map(|&(symbol, extra_bits)| length_prefix[symbol] + extra_bits)
            .collect();
        Self {
            literal: green[..NUM_LITERALS].to_vec(),
            length,
            distance: symbol_costs(distance),
        }
    }

    fn distance_cost(&self, code: usize) -> f32 {
        let (symbol, extra_bits, _) = prefix_encode(code);
        self.distance[symbol] + extra_bits as f32
    }
}

/// Entropy estimate in bits per symbol; unused symbols are priced as if seen 0.4 times.
fn symbol_costs(histogram: &[u32]) -> Vec<f32> {
    /// `log2(1 / 0.4)`.
    const UNSEEN: f64 = 1.321_928_094_887_362_3;
    let total = log2_count(histogram.iter().map(|&c| u64::from(c)).sum::<u64>().max(1));
    histogram
        .iter()
        .map(|&c| {
            let bits = if c == 0 {
                total + UNSEEN
            } else {
                total - log2_count(u64::from(c))
            };
            bits.max(0.0) as f32
        })
        .collect()
}

/// `log2(x)` for `x >= 1`, cached for small counts.
fn log2_count(x: u64) -> f64 {
    static SMALL: LazyLock<Vec<f64>> =
        LazyLock::new(|| (0..1 << 12).map(|x| log2(x.max(1))).collect());
    SMALL.get(x as usize).copied().unwrap_or_else(|| log2(x))
}

/// `log2(x)` for `x >= 1` from correctly rounded IEEE-754 operations only, so it gives the same
/// bits on every platform (libm's `log2` may differ in the last place).
fn log2(x: u64) -> f64 {
    let exponent = x.ilog2();
    let mut mantissa = x as f64 / f64::from_bits(u64::from(1023 + exponent) << 52);
    let mut result = f64::from(exponent);
    let mut bit = 0.5;
    for _ in 0..40 {
        mantissa *= mantissa;
        if mantissa >= 2.0 {
            mantissa *= 0.5;
            result += bit;
        }
        bit *= 0.5;
    }
    result
}

/// LZ77 match finder over a symbol stream laid out in rows of `xsize`.
#[derive(Debug)]
struct Matcher<'a> {
    symbols: &'a [u8],
    /// `(distance, code)` of the neighborhood distances tracked at every position, by ascending
    /// code.
    tracked: Vec<(usize, usize)>,
    /// Ranges of `tracked` sharing a distance prefix symbol, and therefore a cost.
    groups: Vec<Range<usize>>,
    segments: Vec<Segment>,
    xsize: usize,
}

/// Hash-chain matches of one parse segment.
#[derive(Debug)]
struct Segment {
    range: Range<usize>,
    /// Matches longer than every tracked neighborhood match, as `(length, code)`; those starting
    /// at `range.start + k` are `chain_matches[chain_starts[k]..chain_starts[k + 1]]`.
    chain_matches: Vec<(u16, u32)>,
    chain_starts: Vec<u32>,
}

impl<'a> Matcher<'a> {
    fn new(symbols: &'a [u8], xsize: usize) -> Self {
        let mut neighbor_codes = vec![0u8; 8 * xsize + 9];
        let mut tracked = Vec::new();
        for (code, &(dx, dy)) in (1u8..).zip(&NEIGHBORHOOD) {
            let distance = (isize::from(dx) + isize::from(dy) * xsize as isize).max(1) as usize;
            if neighbor_codes[distance] == 0 {
                neighbor_codes[distance] = code;
                if usize::from(code) <= TRACKED_CODES {
                    tracked.push((distance, usize::from(code)));
                }
            }
        }

        let mut groups: Vec<Range<usize>> = Vec::new();
        for (k, &(_, code)) in tracked.iter().enumerate() {
            match groups.last_mut() {
                Some(group) if prefix_encode(tracked[group.start].1).0 == prefix_encode(code).0 => {
                    group.end = k + 1;
                }
                _ => groups.push(k..k + 1),
            }
        }

        let n = symbols.len();
        let hash_bits = n.next_power_of_two().ilog2().clamp(8, HASH_BITS);
        let mut head = vec![NO_POSITION; 1 << hash_bits];
        let mut chain = vec![NO_POSITION; n];
        for (i, window) in symbols.windows(3).enumerate() {
            let key = u32::from(window[0]) << 16 | u32::from(window[1]) << 8 | u32::from(window[2]);
            let hash = (key.wrapping_mul(0x9E37_79B1) >> (32 - hash_bits)) as usize;
            chain[i] = head[hash];
            head[hash] = i as u32;
        }

        let segments = (0..n.div_ceil(SEGMENT))
            .into_par_iter()
            .map(|k| {
                let range = k * SEGMENT..((k + 1) * SEGMENT).min(n);
                search_segment(symbols, &tracked, &neighbor_codes, &chain, range)
            })
            .collect();
        Self {
            symbols,
            tracked,
            groups,
            segments,
            xsize,
        }
    }

    /// Distance costs of the tracked groups under `model` and the groups in ascending cost.
    fn price(&self, model: CostModel) -> Pricing {
        let group_costs: Vec<f32> = self
            .groups
            .iter()
            .map(|group| model.distance_cost(self.tracked[group.start].1))
            .collect();
        let mut group_order: Vec<usize> = (0..self.groups.len()).collect();
        group_order.sort_by(|&a, &b| group_costs[a].total_cmp(&group_costs[b]));
        Pricing {
            model,
            group_costs,
            group_order,
        }
    }

    /// Cheapest parse of `segment`, found by dynamic programming over positions in order. Each
    /// position is priced with `prices[0]`, or with the prices of its tile's group under `meta`.
    ///
    /// A position whose longest match reaches `LONG_MATCH` takes that match outright, skipping the
    /// positions it covers.
    fn parse(
        &self,
        segment: &Segment,
        prices: &[Pricing],
        meta: Option<&MetaCodes>,
        length_steps: &[usize],
    ) -> Vec<Token> {
        let s = self.symbols;
        let Range { start, end: n } = segment.range;
        let mut match_ends = vec![0usize; self.tracked.len()];
        let mut active = 0u32;
        let mut group_of = vec![0usize; self.tracked.len()];
        for (g, group) in self.groups.iter().enumerate() {
            group_of[group.clone()].fill(g);
        }
        let mut group_longest = vec![0usize; self.groups.len()];

        let len = n - start;
        let mut cost = vec![f64::INFINITY; len + 1];
        cost[0] = 0.0;
        let mut steps = vec![(0u16, 0u32); len + 1];
        let mut candidates: Vec<(f32, usize, usize)> = Vec::new();
        let mut i = start;
        while i < n {
            let pricing = &prices[meta.map_or(0, |meta| usize::from(meta.group_at(i, self.xsize)))];
            let model = &pricing.model;
            let mut live = active;
            while live != 0 {
                let k = live.trailing_zeros() as usize;
                live &= live - 1;
                if match_ends[k] <= i {
                    active &= !(1 << k);
                }
            }
            let symbol = s[i];
            let mut starting = 0u32;
            for (k, &(distance, _)) in self.tracked.iter().enumerate() {
                let fresh = distance <= i && s[i - distance] == symbol;
                starting |= u32::from(fresh) << k;
            }
            starting &= !active;
            active |= starting;
            while starting != 0 {
                let k = starting.trailing_zeros() as usize;
                starting &= starting - 1;
                let distance = self.tracked[k].0;
                let end = &mut match_ends[k];
                *end = i + 1;
                while *end < n && s[*end] == s[*end - distance] {
                    *end += 1;
                }
            }
            group_longest.fill(0);
            let mut live = active;
            while live != 0 {
                let k = live.trailing_zeros() as usize;
                live &= live - 1;
                let length = match_ends[k] - i;
                let best = &mut group_longest[group_of[k]];
                *best = (*best).max(length.min(MAX_COPY_LENGTH) << 8 | k);
            }
            candidates.clear();
            let mut longest = 0;
            for &g in &pricing.group_order {
                let length = group_longest[g] >> 8;
                if length > longest {
                    longest = length;
                    let code = self.tracked[group_longest[g] & 0xff].1;
                    candidates.push((pricing.group_costs[g], length, code));
                }
            }

            let k = i - start;
            let chained = &segment.chain_matches
                [segment.chain_starts[k] as usize..segment.chain_starts[k + 1] as usize];
            if !chained.is_empty() {
                for &(length, code) in chained {
                    let code = code as usize;
                    candidates.push((model.distance_cost(code), usize::from(length), code));
                }
                candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
            }

            let here = cost[k];
            let literal = here + f64::from(model.literal[usize::from(s[i])]);
            if literal < cost[k + 1] {
                cost[k + 1] = literal;
                steps[k + 1] = (0, 0);
            }
            let mut covered = 0;
            for &(distance_cost, length, code) in &candidates {
                if length <= covered {
                    continue;
                }
                let first = length_steps.partition_point(|&l| l <= covered);
                for &l in length_steps[first..]
                    .iter()
                    .take_while(|&&l| l < length)
                    .chain(std::iter::once(&length))
                {
                    let c = here + f64::from(distance_cost + model.length[l]);
                    if c < cost[k + l] {
                        cost[k + l] = c;
                        steps[k + l] = (l as u16, code as u32);
                    }
                }
                covered = length;
            }
            i += if covered >= LONG_MATCH { covered } else { 1 };
        }

        let mut tokens = Vec::new();
        let mut end = len;
        while end > 0 {
            let (length, code) = steps[end];
            if length == 0 {
                end -= 1;
                tokens.push(Token::Literal([0, s[start + end], 0, 255]));
            } else {
                end -= usize::from(length);
                tokens.push(Token::Copy {
                    length: usize::from(length),
                    code: code as usize,
                });
            }
        }
        tokens.reverse();
        tokens
    }

    /// Parse every segment in parallel and join the tokens.
    fn parse_all(&self, prices: &[Pricing], meta: Option<&MetaCodes>) -> Vec<Token> {
        let length_steps: Vec<usize> = (1..=MAX_COPY_LENGTH)
            .filter(|&l| {
                l <= EXACT_LENGTHS
                    || l == MAX_COPY_LENGTH
                    || prefix_encode(l).0 != prefix_encode(l + 1).0
            })
            .collect();
        self.segments
            .par_iter()
            .map(|segment| self.parse(segment, prices, meta, &length_steps))
            .collect::<Vec<_>>()
            .concat()
    }
}

/// A cost model with the derived costs of the tracked distance groups.
#[derive(Debug)]
struct Pricing {
    model: CostModel,
    group_costs: Vec<f32>,
    /// Tracked groups by ascending distance cost.
    group_order: Vec<usize>,
}

/// Hash-chain matches at the positions of `range` that beat every tracked neighborhood match,
/// none running past the end of `range`.
fn search_segment(
    symbols: &[u8],
    tracked: &[(usize, usize)],
    neighbor_codes: &[u8],
    chain: &[u32],
    range: Range<usize>,
) -> Segment {
    let Range { start, end: n } = range;
    let mut longest_tracked = vec![0u16; n - start];
    let mut runs = vec![0usize; tracked.len()];
    for i in (start..n).rev() {
        let symbol = Some(&symbols[i]);
        let mut longest = 0;
        for (run, &(distance, _)) in runs.iter_mut().zip(tracked) {
            let matches = usize::from(symbols.get(i.wrapping_sub(distance)) == symbol);
            *run = (*run + 1).min(MAX_COPY_LENGTH) * matches;
            longest = longest.max(*run);
        }
        longest_tracked[i - start] = longest as u16;
    }

    let mut chain_matches = Vec::new();
    let mut chain_starts = Vec::with_capacity(n - start + 1);
    for i in start..n {
        chain_starts.push(chain_matches.len() as u32);
        let max_length = (n - i).min(MAX_COPY_LENGTH);
        let limit = max_length.min(LONG_MATCH);
        let mut longest = usize::from(longest_tracked[i - start]);
        let mut j = chain[i];
        for _ in 0..CHAIN_LIMIT {
            if j == NO_POSITION || longest >= limit {
                break;
            }
            let candidate = j as usize;
            let distance = i - candidate;
            if distance > WINDOW {
                break;
            }
            let code = match neighbor_codes.get(distance) {
                Some(&code) if code != 0 => usize::from(code),
                _ => distance + NEIGHBORHOOD_CODES,
            };
            if code > TRACKED_CODES && symbols[candidate + longest] == symbols[i + longest] {
                let length = symbols[i..i + max_length]
                    .iter()
                    .zip(&symbols[candidate..])
                    .take_while(|(a, b)| a == b)
                    .count();
                if length > longest && length >= 3 {
                    longest = length;
                    chain_matches.push((length as u16, code as u32));
                }
            }
            j = chain[candidate];
        }
    }
    chain_starts.push(chain_matches.len() as u32);
    Segment {
        range,
        chain_matches,
        chain_starts,
    }
}

/// Tokens for a symbol stream laid out in rows of `xsize`: the smallest of several optimal parses,
/// each priced with the statistics of the previous one.
fn backward_references(symbols: &[u8], xsize: usize) -> Vec<Token> {
    let matcher = Matcher::new(symbols, xsize);
    let mut pricing = matcher.price(CostModel::initial(symbols));
    let mut best: Option<(u64, Vec<Token>)> = None;
    for _ in 0..PARSE_PASSES {
        let tokens = matcher.parse_all(std::slice::from_ref(&pricing), None);
        let histograms = Histograms::new(&tokens);
        let bits = histograms.coded_bits();
        pricing = matcher.price(CostModel::new(&histograms.green, &histograms.distance));
        if best.as_ref().is_none_or(|(best_bits, _)| bits < *best_bits) {
            best = Some((bits, tokens));
        }
    }
    best.map_or_else(Vec::new, |(_, tokens)| tokens)
}

/// Tokens and meta prefix codes for the main image, `symbols` laid out in rows of `xsize`.
///
/// A first parse priced with global statistics plans the meta prefix codes; the second parse
/// prices every position with the statistics of its tile's group (or globally again when one set
/// of codes is smallest), and the smaller of the two parses is kept.
fn main_image(symbols: &[u8], xsize: usize, ysize: usize) -> (Vec<Token>, Option<MetaCodes>) {
    let matcher = Matcher::new(symbols, xsize);
    let tokens = matcher.parse_all(&[matcher.price(CostModel::initial(symbols))], None);
    let (bits, meta) = plan_meta_codes(&tokens, xsize, ysize);
    let histograms = match &meta {
        Some(meta) => meta.histograms(&tokens, &meta.token_groups(&tokens, xsize)),
        None => vec![Histograms::new(&tokens)],
    };
    let prices: Vec<Pricing> = histograms
        .iter()
        .map(|h| matcher.price(CostModel::new(&h.green, &h.distance)))
        .collect();
    let refined = matcher.parse_all(&prices, meta.as_ref());
    let refined_bits = meta.as_ref().map_or_else(
        || 1 + entropy_coded_bits(&refined),
        |meta| meta.total_bits(&refined, xsize),
    );
    (if refined_bits < bits { refined } else { tokens }, meta)
}

/// Least-significant-bit-first bit writer.
#[derive(Debug, Default)]
struct BitWriter {
    bytes: Vec<u8>,
    pending: u64,
    pending_bits: u32,
}

impl BitWriter {
    fn put(&mut self, value: u32, bits: u32) {
        debug_assert!(bits <= 32 && u64::from(value) < 1u64 << bits);
        self.pending |= u64::from(value) << self.pending_bits;
        self.pending_bits += bits;
        while self.pending_bits >= 8 {
            self.bytes.push(self.pending as u8);
            self.pending >>= 8;
            self.pending_bits -= 8;
        }
    }

    fn len_bits(&self) -> u64 {
        self.bytes.len() as u64 * 8 + u64::from(self.pending_bits)
    }

    fn finish(mut self) -> Vec<u8> {
        if self.pending_bits > 0 {
            self.bytes.push(self.pending as u8);
        }
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn round_trip(img: &RgbaImage) -> Vec<u8> {
        let bytes = encode_lossless(img).unwrap();
        let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::WebP)
            .unwrap()
            .to_rgba8();
        assert!(
            decoded == *img,
            "{}x{} image did not round-trip",
            img.width(),
            img.height()
        );
        bytes
    }

    /// Deterministic dither-like image using `colors` palette entries, some of them translucent.
    fn palette_image(width: u32, height: u32, colors: usize, seed: u64) -> RgbaImage {
        let mut state = seed | 1;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let palette: Vec<[u8; 4]> = (0..colors)
            .map(|i| {
                let r = next();
                let alpha = [255, 0, 128][i % 3];
                [r as u8, (r >> 8) as u8, (r >> 16) as u8, alpha]
            })
            .collect();
        RgbaImage::from_fn(width, height, |x, y| {
            let ordered = (x / 3 + 5 * y + (x * y) % 7) as usize;
            let noise = usize::from(next() % 5 == 0);
            Rgba(palette[(ordered + noise) % colors])
        })
    }

    #[test]
    fn every_palette_size_and_odd_width_round_trips() {
        for colors in [1, 2, 3, 4, 5, 16, 17, 256] {
            for (width, height) in [(1, 1), (1, 9), (9, 1), (3, 5), (7, 3), (13, 11), (64, 33)] {
                round_trip(&palette_image(width, height, colors, 7 + colors as u64));
            }
        }
    }

    #[test]
    fn long_runs_and_repeats_round_trip() {
        let img = RgbaImage::from_fn(700, 300, |x, y| {
            let shade = if y < 100 {
                0
            } else if (x / 4 + y) % 2 == 0 {
                1
            } else {
                2
            };
            Rgba([shade * 90, 40, 200 - shade * 60, 255])
        });
        round_trip(&img);
    }

    #[test]
    fn transparent_pixels_keep_their_color() {
        let img = RgbaImage::from_fn(9, 4, |x, y| Rgba([x as u8 * 20, y as u8 * 50, 7, 0]));
        round_trip(&img);
    }

    #[test]
    fn more_than_256_colors_fall_back_losslessly() {
        let img = RgbaImage::from_fn(20, 20, |x, y| Rgba([x as u8 * 12, y as u8 * 12, 9, 255]));
        round_trip(&img);
    }

    #[test]
    fn dithered_output_is_far_smaller_than_argb_webp() {
        let img = palette_image(256, 256, 2, 99);
        let mut argb = Vec::new();
        WebPEncoder::new_lossless(&mut argb)
            .write_image(img.as_raw(), 256, 256, ExtendedColorType::Rgba8)
            .unwrap();
        let indexed = round_trip(&img);
        assert!(
            indexed.len() * 3 < argb.len(),
            "indexed {} vs ARGB {} bytes",
            indexed.len(),
            argb.len()
        );
    }

    #[test]
    fn regions_with_different_statistics_get_meta_prefix_codes() {
        let img = RgbaImage::from_fn(768, 512, |x, y| {
            let density = [2, 3, 5, 9, 17, 40][(x / 128) as usize] + (y / 256) * 7;
            let hash = (x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA6B)) >> 9;
            let on = hash.wrapping_mul(0xC2B2_AE35) % density == 0;
            Rgba(if on {
                [250, 250, 240, 255]
            } else {
                [10, 10, 20, 255]
            })
        });
        let bytes = round_trip(&img);
        let palette = [[10, 10, 20, 255], [250, 250, 240, 255]];
        let (indices, xsize) = bundle_indices(&img, &palette, 3);
        let (tokens, meta) = main_image(&indices, xsize, 512);
        let meta = meta.expect("regions should get their own prefix codes");
        assert!(meta.tile_groups.iter().any(|&g| g > 0));
        let mut single = BitWriter::default();
        write_entropy_coded_image(&mut single, &tokens);
        assert!(
            bytes.len() * 8 < single.len_bits() as usize,
            "meta codes should be smaller"
        );
    }

    #[test]
    fn encoding_is_deterministic_across_thread_counts() {
        let img = palette_image(1500, 300, 9, 5);
        let reference = encode_lossless(&img).unwrap();
        for threads in [1, 3] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            assert_eq!(pool.install(|| encode_lossless(&img).unwrap()), reference);
        }
    }

    #[test]
    fn portable_log2_matches_exact_values() {
        for exponent in 0..64 {
            assert_eq!(log2(1 << exponent), f64::from(exponent));
        }
        for (x, expected) in [
            (3, 1.584_962_500_721_156),
            (10, std::f64::consts::LOG2_10),
            (1_000_003, 19.931_572_897_402_805),
        ] {
            assert!((log2(x) - expected).abs() < 1e-9, "log2({x}) = {}", log2(x));
            assert_eq!(log2_count(x), log2(x));
        }
    }

    #[test]
    fn rejects_images_beyond_vp8l_limits() {
        assert!(encode_lossless(&RgbaImage::new(MAX_DIMENSION + 1, 1)).is_err());
    }

    #[test]
    fn indices_bundle_least_significant_first() {
        let black = Rgba([0, 0, 0, 255]);
        let white = Rgba([255, 255, 255, 255]);
        let row = [
            white, black, white, white, black, black, black, black, white,
        ];
        let img = RgbaImage::from_fn(9, 1, |x, _| row[x as usize]);
        let (symbols, xsize) = bundle_indices(&img, &[black.0, white.0], 3);
        assert_eq!((symbols, xsize), (vec![0b0000_1101, 0b1], 2));
    }

    #[test]
    fn prefix_encoding_inverts_the_spec_decoder() {
        for value in 1..=(MAX_DISTANCE_CODE) {
            let (symbol, extra_bits, extra) = prefix_encode(value);
            let decoded = if symbol < 4 {
                symbol + 1
            } else {
                let bits = (symbol - 2) >> 1;
                assert_eq!(bits as u32, extra_bits);
                ((2 + (symbol & 1)) << bits) + extra as usize + 1
            };
            assert_eq!(decoded, value);
        }
        assert_eq!(prefix_encode(MAX_COPY_LENGTH).0, NUM_LENGTH_CODES - 1);
        assert_eq!(prefix_encode(MAX_DISTANCE_CODE).0, NUM_DISTANCE_CODES - 1);
    }

    #[test]
    fn huffman_lengths_are_complete_and_limited() {
        let mut fibonacci = vec![1u32, 1];
        while fibonacci.len() < 40 {
            fibonacci.push(fibonacci[fibonacci.len() - 1] + fibonacci[fibonacci.len() - 2]);
        }
        for limit in [7, 15] {
            let lengths = huffman_lengths(&fibonacci, limit);
            assert!(lengths.iter().all(|&l| (1..=limit).contains(&u32::from(l))));
            let kraft: f64 = lengths.iter().map(|&l| 0.5f64.powi(i32::from(l))).sum();
            assert!((kraft - 1.0).abs() < 1e-12, "kraft sum {kraft}");
        }
    }
}
