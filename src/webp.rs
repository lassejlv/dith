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
/// Cost-model refinement passes of the optimal parse.
const PARSE_PASSES: usize = 3;

/// Encode `img` as a lossless WebP file.
///
/// Images with 256 or fewer distinct RGBA colors use a palette-aware VP8L encoder; others use
/// `image`'s lossless WebP encoder. Every pixel, alpha included, round-trips exactly.
///
/// # Errors
/// Returns an error if the image is larger than 16384x16384 pixels or encoding fails.
pub fn encode_lossless(img: &RgbaImage) -> Result<Vec<u8>> {
    let PngLayout::Indexed(palette) = PngLayout::for_images([img]) else {
        let mut out = Vec::new();
        WebPEncoder::new_lossless(&mut out)
            .write_image(
                img.as_raw(),
                img.width(),
                img.height(),
                ExtendedColorType::Rgba8,
            )
            .context("encoding webp output")?;
        return Ok(out);
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
    w.put(0, 1);
    write_entropy_coded_image(&mut w, &backward_references(&indices, xsize));

    let data = w.finish();
    let chunk_len = u32::try_from(data.len()).context("WebP bitstream exceeds 4 GiB")?;
    let mut out = Vec::with_capacity(data.len() + 21);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(12 + chunk_len + chunk_len % 2).to_le_bytes());
    out.extend_from_slice(b"WEBPVP8L");
    out.extend_from_slice(&chunk_len.to_le_bytes());
    out.extend_from_slice(&data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
    Ok(out)
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
        let mut h = Self {
            green: vec![0; NUM_LITERALS + NUM_LENGTH_CODES],
            red: vec![0; NUM_LITERALS],
            blue: vec![0; NUM_LITERALS],
            alpha: vec![0; NUM_LITERALS],
            distance: vec![0; NUM_DISTANCE_CODES],
            extra_bits: 0,
        };
        for &token in tokens {
            match token {
                Token::Literal([r, g, b, a]) => {
                    h.green[usize::from(g)] += 1;
                    h.red[usize::from(r)] += 1;
                    h.blue[usize::from(b)] += 1;
                    h.alpha[usize::from(a)] += 1;
                }
                Token::Copy { length, code } => {
                    let (length_symbol, length_extra, _) = prefix_encode(length);
                    let (distance_symbol, distance_extra, _) = prefix_encode(code);
                    h.green[NUM_LITERALS + length_symbol] += 1;
                    h.distance[distance_symbol] += 1;
                    h.extra_bits += u64::from(length_extra + distance_extra);
                }
            }
        }
        h
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
        self.extra_bits
            + self
                .all()
                .iter()
                .zip(self.codes())
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
    let [green, red, blue, alpha, distance] = Histograms::new(tokens).codes();
    for code in [&green, &red, &blue, &alpha, &distance] {
        code.write_header(w);
    }
    for &token in tokens {
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
        let green = symbol_costs(green);
        let length_prefix = &green[NUM_LITERALS..];
        let length = (0..=MAX_COPY_LENGTH)
            .map(|l| {
                let (symbol, extra_bits, _) = prefix_encode(l.max(1));
                length_prefix[symbol] + extra_bits as f32
            })
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

/// Entropy estimate in bits per symbol; unused symbols get a finite, pessimistic cost.
fn symbol_costs(histogram: &[u32]) -> Vec<f32> {
    let total = histogram.iter().map(|&c| c as f32).sum::<f32>().max(1.0);
    histogram
        .iter()
        .map(|&c| (f64::from(total / (c as f32).max(0.4)).log2() as f32).max(0.0))
        .collect()
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
    /// Hash-chain matches longer than every tracked neighborhood match, as `(length, code)`;
    /// those starting at position `i` are `chain_matches[chain_starts[i]..chain_starts[i + 1]]`.
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
        let mut longest_tracked = vec![0u16; n];
        let mut runs = vec![0usize; tracked.len()];
        for i in (0..n).rev() {
            let symbol = Some(&symbols[i]);
            let mut longest = 0;
            for (run, &(distance, _)) in runs.iter_mut().zip(&tracked) {
                let matches = usize::from(symbols.get(i.wrapping_sub(distance)) == symbol);
                *run = (*run + 1).min(MAX_COPY_LENGTH) * matches;
                longest = longest.max(*run);
            }
            longest_tracked[i] = longest as u16;
        }

        let mut head = vec![NO_POSITION; 1 << HASH_BITS];
        let mut chain = vec![NO_POSITION; n];
        let mut chain_matches = Vec::new();
        let mut chain_starts = Vec::with_capacity(n + 1);
        for i in 0..n {
            chain_starts.push(chain_matches.len() as u32);
            if i + 3 > n {
                continue;
            }
            let key = u32::from(symbols[i]) << 16
                | u32::from(symbols[i + 1]) << 8
                | u32::from(symbols[i + 2]);
            let hash = (key.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize;
            chain[i] = head[hash];
            head[hash] = i as u32;

            let limit = (n - i).min(MAX_COPY_LENGTH).min(LONG_MATCH);
            let mut longest = usize::from(longest_tracked[i]);
            let mut j = chain[i];
            for _ in 0..CHAIN_LIMIT {
                if j == NO_POSITION || longest >= limit {
                    break;
                }
                let start = j as usize;
                let distance = i - start;
                if distance + NEIGHBORHOOD_CODES > MAX_DISTANCE_CODE {
                    break;
                }
                let code = match neighbor_codes.get(distance) {
                    Some(&code) if code != 0 => usize::from(code),
                    _ => distance + NEIGHBORHOOD_CODES,
                };
                if code > TRACKED_CODES && symbols[start + longest] == symbols[i + longest] {
                    let length = symbols[i..i + (n - i).min(MAX_COPY_LENGTH)]
                        .iter()
                        .zip(&symbols[start..])
                        .take_while(|(a, b)| a == b)
                        .count();
                    if length > longest && length >= 3 {
                        longest = length;
                        chain_matches.push((length as u16, code as u32));
                    }
                }
                j = chain[start];
            }
        }
        chain_starts.push(chain_matches.len() as u32);
        Self {
            symbols,
            tracked,
            groups,
            chain_matches,
            chain_starts,
        }
    }

    /// Cheapest parse under `model`, found by dynamic programming over positions in order.
    ///
    /// A position whose longest match reaches `LONG_MATCH` takes that match outright, skipping the
    /// positions it covers.
    fn parse(&self, model: &CostModel, length_steps: &[usize]) -> Vec<Token> {
        let s = self.symbols;
        let n = s.len();
        let group_costs: Vec<f32> = self
            .groups
            .iter()
            .map(|group| model.distance_cost(self.tracked[group.start].1))
            .collect();
        let mut group_order: Vec<usize> = (0..self.groups.len()).collect();
        group_order.sort_by(|&a, &b| group_costs[a].total_cmp(&group_costs[b]));
        let mut match_ends = vec![0usize; self.tracked.len()];
        let mut group_longest = vec![0usize; self.groups.len()];

        let mut cost = vec![f64::INFINITY; n + 1];
        cost[0] = 0.0;
        let mut steps = vec![(0u16, 0u32); n + 1];
        let mut candidates: Vec<(f32, usize, usize)> = Vec::new();
        let mut i = 0;
        while i < n {
            for (longest, group) in group_longest.iter_mut().zip(&self.groups) {
                let mut best = 0;
                for (k, end) in group.clone().zip(&mut match_ends[group.clone()]) {
                    let distance = self.tracked[k].0;
                    if *end <= i {
                        *end = i;
                        if distance <= i {
                            while *end < n && s[*end] == s[*end - distance] {
                                *end += 1;
                            }
                        }
                    }
                    best = best.max((*end - i).min(MAX_COPY_LENGTH) << 8 | k);
                }
                *longest = best;
            }
            candidates.clear();
            let mut longest = 0;
            for &g in &group_order {
                let length = group_longest[g] >> 8;
                if length > longest {
                    longest = length;
                    let code = self.tracked[group_longest[g] & 0xff].1;
                    candidates.push((group_costs[g], length, code));
                }
            }

            let chained = &self.chain_matches
                [self.chain_starts[i] as usize..self.chain_starts[i + 1] as usize];
            if !chained.is_empty() {
                for &(length, code) in chained {
                    let code = code as usize;
                    candidates.push((model.distance_cost(code), usize::from(length), code));
                }
                candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
            }

            let here = cost[i];
            let literal = here + f64::from(model.literal[usize::from(s[i])]);
            if literal < cost[i + 1] {
                cost[i + 1] = literal;
                steps[i + 1] = (0, 0);
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
                    if c < cost[i + l] {
                        cost[i + l] = c;
                        steps[i + l] = (l as u16, code as u32);
                    }
                }
                covered = length;
            }
            i += if covered >= LONG_MATCH { covered } else { 1 };
        }

        let mut tokens = Vec::new();
        let mut end = n;
        while end > 0 {
            let (length, code) = steps[end];
            if length == 0 {
                end -= 1;
                tokens.push(Token::Literal([0, s[end], 0, 255]));
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
}

/// Tokens for the bundled index stream: the smallest of several optimal parses, each priced with
/// the statistics of the previous one.
fn backward_references(symbols: &[u8], xsize: usize) -> Vec<Token> {
    let matcher = Matcher::new(symbols, xsize);
    let length_steps: Vec<usize> = (1..=MAX_COPY_LENGTH)
        .filter(|&l| {
            l <= EXACT_LENGTHS
                || l == MAX_COPY_LENGTH
                || prefix_encode(l).0 != prefix_encode(l + 1).0
        })
        .collect();
    let mut model = CostModel::initial(symbols);
    let mut best: Option<(u64, Vec<Token>)> = None;
    for _ in 0..PARSE_PASSES {
        let tokens = matcher.parse(&model, &length_steps);
        let histograms = Histograms::new(&tokens);
        let bits = histograms.coded_bits();
        model = CostModel::new(&histograms.green, &histograms.distance);
        if best.as_ref().is_none_or(|(best_bits, _)| bits < *best_bits) {
            best = Some((bits, tokens));
        }
    }
    best.map_or_else(Vec::new, |(_, tokens)| tokens)
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
