//! Compact lossless WebP (VP8L) encoding.
//!
//! Dithered output almost always uses 256 or fewer colors, but `image`'s WebP encoder has no
//! color-indexing transform and no LZ77, so it stores such images as full ARGB. Palette images are
//! written here instead: a color-indexing transform that bundles 8, 4, or 2 pixels per symbol for
//! palettes of up to 2, 4, or 16 colors, cost-optimized LZ77 backward references (including the
//! short 2D neighborhood distance codes), and canonical prefix codes that follow the image: it is
//! cut into square tiles, tiles with similar statistics share one set of prefix codes, and an
//! entropy image records which set each tile uses. Images with more than 256 colors fall back to
//! `image`'s encoder.
//!
//! Matches are searched and parsed on fixed sections of the image in parallel, and every cost is a
//! fixed-point integer, so the output is identical on every platform and thread count.

use std::ops::Range;

use anyhow::{Context, Result, ensure};
use image::codecs::webp::WebPEncoder;
use image::{ExtendedColorType, ImageEncoder, RgbaImage};
use rayon::prelude::*;

use crate::huffman::{
    BitWriter, ONE_BIT, canonical_codes, code_length_tokens, entropy_q8, huffman_lengths,
    symbol_costs_q8,
};
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
const NUM_GREEN: usize = NUM_LITERALS + NUM_LENGTH_CODES;
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
/// How far back the hash chains reach, in symbols.
const HASH_WINDOW: usize = 1 << 18;
/// A match at least this long is taken outright, without pricing the positions it covers.
const LONG_MATCH: usize = 256;
/// Copy lengths up to this are all priced individually; longer ones only at prefix-code boundaries.
const EXACT_LENGTHS: usize = 32;
/// Parses after the first, each priced with the statistics of the one before.
const PARSE_PASSES: usize = 3;
/// Symbols searched or parsed as one unit of parallel work. Copies never cross a section end.
const SECTION: usize = 1 << 16;
/// Most tiles the entropy image may have.
const MAX_TILES: usize = 16384;
/// Tile sizes tried, smallest allowed first.
const TILE_SIZES: usize = 2;
/// Most prefix code groups the clustering starts from before merging.
const MAX_GROUPS: usize = 48;
/// Reassignment rounds of the tile clustering.
const CLUSTER_ROUNDS: usize = 6;
/// Cap on the estimated cost of one symbol.
const MAX_SYMBOL_COST: u32 = 24 * ONE_BIT;
/// Estimated cost of the red, blue and alpha prefix codes every group writes.
const GROUP_OVERHEAD: u64 = 24 * ONE_BIT as u64;

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

    let mut w = BitWriter::with_capacity(indices.len() / 2 + 64);
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
    write_main_image(&mut w, &indices, xsize);

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
    let width = img.width() as usize;
    let xsize = width.div_ceil(1 << width_bits);
    let bits_per_index = 8 >> width_bits;
    let mut symbols = vec![0u8; xsize * img.height() as usize];
    let indices = crate::output::palette_indices(img, palette);
    for (row, out_row) in indices.chunks(width).zip(symbols.chunks_mut(xsize)) {
        for (group, symbol) in row.chunks(1 << width_bits).zip(out_row) {
            for (k, &index) in group.iter().enumerate() {
                *symbol |= index << (k * bits_per_index);
            }
        }
    }
    (symbols, xsize)
}

/// One coded element of an entropy-coded image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    /// A literal pixel as `[r, g, b, a]`.
    Literal([u8; 4]),
    /// Copy `length` pixels starting `code`'s distance back.
    Copy { length: usize, code: usize },
}

impl Token {
    /// Number of pixels the token covers.
    fn len(self) -> usize {
        match self {
            Self::Literal(_) => 1,
            Self::Copy { length, .. } => length,
        }
    }
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

/// Symbol counts of the five prefix codes of one entropy-coded image or prefix code group.
#[derive(Debug, Clone)]
struct Histograms {
    green: Vec<u32>,
    red: Vec<u32>,
    blue: Vec<u32>,
    alpha: Vec<u32>,
    distance: Vec<u32>,
    extra_bits: u64,
}

impl Histograms {
    fn empty() -> Self {
        Self {
            green: vec![0; NUM_GREEN],
            red: vec![0; NUM_LITERALS],
            blue: vec![0; NUM_LITERALS],
            alpha: vec![0; NUM_LITERALS],
            distance: vec![0; NUM_DISTANCE_CODES],
            extra_bits: 0,
        }
    }

    fn new(tokens: &[Token]) -> Self {
        let mut h = Self::empty();
        for &token in tokens {
            h.add(token);
        }
        h
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
    let codes = Histograms::new(tokens).codes();
    for code in &codes {
        code.write_header(w);
    }
    for &token in tokens {
        write_token(w, &codes, token);
    }
}

fn write_token(
    w: &mut BitWriter,
    [green, red, blue, alpha, distance]: &[PrefixCode; 5],
    token: Token,
) {
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
        if lengths.iter().filter(|&&l| l > 0).count() < 2 {
            return Self {
                widths: vec![0; lengths.len()],
                codes: vec![0; lengths.len()],
                lengths,
            };
        }
        Self {
            widths: lengths.clone(),
            codes: canonical_codes(&lengths),
            lengths,
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

/// Estimated bit costs, in 1/256 bit units, used by the optimal LZ77 parse.
#[derive(Debug)]
struct CostModel {
    literal: Vec<u32>,
    /// Cost of each length prefix symbol.
    length_prefix: Vec<u32>,
    distance: Vec<u32>,
}

/// Prefix symbol and extra-bit count of every copy length (`0..=MAX_COPY_LENGTH`, 0 as 1).
static LENGTH_CODES: [(u8, u8); MAX_COPY_LENGTH + 1] = length_codes();

const fn length_codes() -> [(u8, u8); MAX_COPY_LENGTH + 1] {
    let mut table = [(0u8, 0u8); MAX_COPY_LENGTH + 1];
    let mut length = 2;
    while length <= MAX_COPY_LENGTH {
        let v = length - 1;
        table[length] = if v < 4 {
            (v as u8, 0)
        } else {
            let high = v.ilog2();
            let extra_bits = high - 1;
            (
                (2 * high as usize + ((v >> extra_bits) & 1)) as u8,
                extra_bits as u8,
            )
        };
        length += 1;
    }
    table
}

impl CostModel {
    /// Literal costs from the symbol frequencies, with every length and distance prefix assumed
    /// moderately likely so the first parse explores backward references.
    fn initial(symbols: &[u8]) -> Self {
        let mut green = vec![0u32; NUM_GREEN];
        for &s in symbols {
            green[usize::from(s)] += 1;
        }
        let guess = (symbols.len() / 64).max(1) as u32;
        green[NUM_LITERALS..].fill(guess);
        Self::new(&green, &[1; NUM_DISTANCE_CODES])
    }

    fn new(green: &[u32], distance: &[u32]) -> Self {
        let green = symbol_costs_q8(green, 0, MAX_SYMBOL_COST);
        Self {
            literal: green[..NUM_LITERALS].to_vec(),
            length_prefix: green[NUM_LITERALS..].to_vec(),
            distance: symbol_costs_q8(distance, 0, MAX_SYMBOL_COST),
        }
    }

    fn length_cost(&self, length: usize) -> u32 {
        let (symbol, extra_bits) = LENGTH_CODES[length];
        self.length_prefix[usize::from(symbol)] + u32::from(extra_bits) * ONE_BIT
    }

    fn distance_cost(&self, code: usize) -> u32 {
        let (symbol, extra_bits, _) = prefix_encode(code);
        self.distance[symbol] + extra_bits * ONE_BIT
    }
}

/// A cost model together with the costs of the tracked distance groups, cheapest group first.
#[derive(Debug)]
struct PricedModel {
    model: CostModel,
    group_costs: Vec<u32>,
    group_order: Vec<usize>,
}

/// Which set of prefix codes each part of the image uses.
#[derive(Debug, Clone)]
struct Grouping {
    /// Tiles are `1 << bits` symbols square.
    bits: u32,
    tiles_x: usize,
    /// Group of every tile, in raster order.
    tile_group: Vec<u8>,
    groups: usize,
}

impl Grouping {
    fn single() -> Self {
        Self {
            bits: 0,
            tiles_x: 1,
            tile_group: vec![0],
            groups: 1,
        }
    }

    fn group_at(&self, position: usize, xsize: usize) -> usize {
        if self.groups == 1 {
            return 0;
        }
        let (x, y) = (position % xsize, position / xsize);
        usize::from(self.tile_group[(y >> self.bits) * self.tiles_x + (x >> self.bits)])
    }

    /// Symbol counts of every group for `tokens` laid out in rows of `xsize`.
    fn histograms(&self, tokens: &[Token], xsize: usize) -> Vec<Histograms> {
        let mut histograms = vec![Histograms::empty(); self.groups];
        let mut position = 0;
        for &token in tokens {
            histograms[self.group_at(position, xsize)].add(token);
            position += token.len();
        }
        histograms
    }
}

/// LZ77 match finder over a symbol stream laid out in rows of `xsize`.
#[derive(Debug)]
struct Matcher<'a> {
    symbols: &'a [u8],
    xsize: usize,
    /// `(distance, code)` of the neighborhood distances tracked at every position, by ascending
    /// code.
    tracked: Vec<(usize, usize)>,
    /// Ranges of `tracked` sharing a distance prefix symbol, and therefore a cost.
    groups: Vec<Range<usize>>,
    /// Hash-chain matches of every section.
    sections: Vec<ChainMatches>,
}

/// Hash-chain matches longer than every tracked neighborhood match, as `(length, code)`; those
/// starting at the section's position `i` are `matches[starts[i]..starts[i + 1]]`.
#[derive(Debug)]
struct ChainMatches {
    starts: Vec<u32>,
    matches: Vec<(u16, u32)>,
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

        let starts: Vec<usize> = (0..symbols.len()).step_by(SECTION).collect();
        let sections = starts
            .into_par_iter()
            .map(|start| ChainMatches::find(symbols, start, &neighbor_codes, &tracked))
            .collect();
        Self {
            symbols,
            xsize,
            tracked,
            groups,
            sections,
        }
    }

    fn chained(&self, position: usize) -> &[(u16, u32)] {
        let section = &self.sections[position / SECTION];
        let i = position % SECTION;
        &section.matches[section.starts[i] as usize..section.starts[i + 1] as usize]
    }

    fn price(&self, model: CostModel) -> PricedModel {
        let group_costs: Vec<u32> = self
            .groups
            .iter()
            .map(|group| model.distance_cost(self.tracked[group.start].1))
            .collect();
        let mut group_order: Vec<usize> = (0..self.groups.len()).collect();
        group_order.sort_by_key(|&g| group_costs[g]);
        PricedModel {
            model,
            group_costs,
            group_order,
        }
    }

    /// Cheapest parse under the cost model of each position's group, found section by section.
    fn parse(
        &self,
        models: &[PricedModel],
        grouping: &Grouping,
        length_steps: &[usize],
    ) -> Vec<Token> {
        let n = self.symbols.len();
        let starts: Vec<usize> = (0..n).step_by(SECTION).collect();
        starts
            .into_par_iter()
            .map(|start| {
                self.parse_section(
                    start..(start + SECTION).min(n),
                    models,
                    grouping,
                    length_steps,
                )
            })
            .collect::<Vec<_>>()
            .into_iter()
            .fold(Vec::with_capacity(n / 2), |mut tokens, section| {
                let mut section = section.into_iter();
                if let (
                    Some(Token::Copy { length, code }),
                    Some(Token::Copy {
                        length: next,
                        code: next_code,
                    }),
                ) = (tokens.last_mut(), section.clone().next())
                    && *code == next_code
                    && *length + next <= MAX_COPY_LENGTH
                {
                    *length += next;
                    section.next();
                }
                tokens.extend(section);
                tokens
            })
    }

    /// Dynamic programming over the positions of `range` in order. A position whose longest
    /// match reaches `LONG_MATCH` takes that match outright, skipping the positions it covers.
    fn parse_section(
        &self,
        range: Range<usize>,
        models: &[PricedModel],
        grouping: &Grouping,
        length_steps: &[usize],
    ) -> Vec<Token> {
        let s = self.symbols;
        let (start, end) = (range.start, range.end);
        let len = end - start;
        let mut match_ends = vec![0usize; self.tracked.len()];
        let mut group_longest = vec![0usize; self.groups.len()];

        let mut cost = vec![u32::MAX; len + 1];
        cost[0] = 0;
        let mut steps = vec![(0u16, 0u32); len + 1];
        let mut candidates: Vec<(u32, usize, usize)> = Vec::new();
        let mut i = 0;
        while i < len {
            let position = start + i;
            let priced = &models[grouping.group_at(position, self.xsize)];
            let model = &priced.model;
            for (longest, group) in group_longest.iter_mut().zip(&self.groups) {
                let mut best = 0;
                for (k, match_end) in group.clone().zip(&mut match_ends[group.clone()]) {
                    let distance = self.tracked[k].0;
                    if *match_end <= position {
                        *match_end = position;
                        if distance <= position {
                            while *match_end < end && s[*match_end] == s[*match_end - distance] {
                                *match_end += 1;
                            }
                        }
                    }
                    best = best.max((*match_end - position).min(MAX_COPY_LENGTH) << 8 | k);
                }
                *longest = best;
            }
            candidates.clear();
            let mut longest = 0;
            for &g in &priced.group_order {
                let length = group_longest[g] >> 8;
                if length > longest {
                    longest = length;
                    let code = self.tracked[group_longest[g] & 0xff].1;
                    candidates.push((priced.group_costs[g], length, code));
                }
            }

            let chained = self.chained(position);
            if !chained.is_empty() {
                for &(length, code) in chained {
                    let code = code as usize;
                    let length = usize::from(length).min(len - i);
                    candidates.push((model.distance_cost(code), length, code));
                }
                candidates.sort_by_key(|c| c.0);
            }

            let here = cost[i];
            let literal = here + model.literal[usize::from(s[position])];
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
                    let c = here + distance_cost + model.length_cost(l);
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
        let mut at = len;
        while at > 0 {
            let (length, code) = steps[at];
            if length == 0 {
                at -= 1;
                tokens.push(Token::Literal([0, s[start + at], 0, 255]));
            } else {
                at -= usize::from(length);
                tokens.push(Token::Copy {
                    length: usize::from(length),
                    code: code as usize,
                });
            }
        }
        tokens.reverse();
        tokens
    }

    /// The smallest of several parses with one set of prefix codes, each priced with the
    /// statistics of the previous one.
    fn single_code_parse(&self, length_steps: &[usize]) -> Vec<Token> {
        let single = Grouping::single();
        let mut model = CostModel::initial(self.symbols);
        let mut best: Option<(u64, Vec<Token>)> = None;
        for _ in 0..PARSE_PASSES {
            let tokens = self.parse(&[self.price(model)], &single, length_steps);
            let histograms = Histograms::new(&tokens);
            let bits = histograms.coded_bits();
            model = CostModel::new(&histograms.green, &histograms.distance);
            if best.as_ref().is_none_or(|(best_bits, _)| bits < *best_bits) {
                best = Some((bits, tokens));
            }
        }
        best.map_or_else(Vec::new, |(_, tokens)| tokens)
    }
}

impl ChainMatches {
    /// Hash-chain matches for the positions of the section starting at `start`, searching back at
    /// most `HASH_WINDOW` symbols.
    fn find(
        symbols: &[u8],
        start: usize,
        neighbor_codes: &[u8],
        tracked: &[(usize, usize)],
    ) -> Self {
        let n = symbols.len();
        let end = (start + SECTION).min(n);

        let mut longest_tracked = vec![0u16; end - start];
        let mut runs = vec![0usize; tracked.len()];
        for i in (start..(end + MAX_COPY_LENGTH).min(n)).rev() {
            let symbol = symbols[i];
            let mut longest = 0;
            for (run, &(distance, _)) in runs.iter_mut().zip(tracked) {
                let matches = usize::from(i >= distance && symbols[i - distance] == symbol);
                *run = (*run + 1).min(MAX_COPY_LENGTH) * matches;
                longest = longest.max(*run);
            }
            if i < end {
                longest_tracked[i - start] = longest as u16;
            }
        }

        let hash = |i: usize| {
            let key = u32::from(symbols[i]) << 16
                | u32::from(symbols[i + 1]) << 8
                | u32::from(symbols[i + 2]);
            (key.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
        };
        let base = start.saturating_sub(HASH_WINDOW);
        let mut head = vec![NO_POSITION; 1 << HASH_BITS];
        let mut chain = vec![NO_POSITION; end - base];
        for i in base..start.min(n.saturating_sub(2)) {
            let h = hash(i);
            chain[i - base] = head[h];
            head[h] = i as u32;
        }

        let mut matches = Vec::new();
        let mut starts = Vec::with_capacity(end - start + 1);
        for i in start..end {
            starts.push(matches.len() as u32);
            if i + 3 > n {
                continue;
            }
            let h = hash(i);
            chain[i - base] = head[h];
            head[h] = i as u32;

            let limit = (n - i).min(MAX_COPY_LENGTH).min(LONG_MATCH);
            let mut longest = usize::from(longest_tracked[i - start]);
            let mut j = chain[i - base];
            for _ in 0..CHAIN_LIMIT {
                if j == NO_POSITION || longest >= limit || (j as usize) < base {
                    break;
                }
                let at = j as usize;
                let distance = i - at;
                if distance + NEIGHBORHOOD_CODES > MAX_DISTANCE_CODE {
                    break;
                }
                let code = match neighbor_codes.get(distance) {
                    Some(&code) if code != 0 => usize::from(code),
                    _ => distance + NEIGHBORHOOD_CODES,
                };
                if code > TRACKED_CODES && symbols[at + longest] == symbols[i + longest] {
                    let length = symbols[i..i + (n - i).min(MAX_COPY_LENGTH)]
                        .iter()
                        .zip(&symbols[at..])
                        .take_while(|(a, b)| a == b)
                        .count();
                    if length > longest && length >= 3 {
                        longest = length;
                        matches.push((length as u16, code as u32));
                    }
                }
                j = chain[at - base];
            }
        }
        starts.push(matches.len() as u32);
        Self { starts, matches }
    }
}

fn length_steps() -> Vec<usize> {
    (1..=MAX_COPY_LENGTH)
        .filter(|&l| {
            l <= EXACT_LENGTHS
                || l == MAX_COPY_LENGTH
                || prefix_encode(l).0 != prefix_encode(l + 1).0
        })
        .collect()
}

/// Tokens for a small sub-image (the entropy image) with one set of prefix codes.
fn backward_references(symbols: &[u8], xsize: usize) -> Vec<Token> {
    Matcher::new(symbols, xsize).single_code_parse(&length_steps())
}

/// Write the bundled index image with the smallest of several parses. The first parse uses one
/// cost model for the whole image; its tokens decide whether tiles clustered into groups, each
/// with its own prefix codes, beat one set of codes. Every later parse is priced with the
/// statistics of each position's group in the previous one, and the tiles are clustered again.
fn write_main_image(w: &mut BitWriter, symbols: &[u8], xsize: usize) {
    let ysize = symbols.len() / xsize;
    let matcher = Matcher::new(symbols, xsize);
    let steps = length_steps();
    let single = Grouping::single();
    let mut tokens = matcher.parse(
        &[matcher.price(CostModel::initial(symbols))],
        &single,
        &steps,
    );
    let mut best_bits = spatial_bits(&tokens, &single, xsize);
    let mut best = (single, tokens.clone());
    let mut grouping = choose_grouping(&tokens, xsize, ysize);
    for pass in 0..=PARSE_PASSES {
        if pass > 0 {
            let models: Vec<PricedModel> = grouping
                .histograms(&tokens, xsize)
                .iter()
                .map(|h| matcher.price(CostModel::new(&h.green, &h.distance)))
                .collect();
            tokens = matcher.parse(&models, &grouping, &steps);
            grouping = if grouping.groups > 1 {
                [
                    cluster(&tokens, xsize, ysize, grouping.bits),
                    Grouping::single(),
                ]
                .into_iter()
                .min_by_key(|grouping| spatial_bits(&tokens, grouping, xsize))
                .unwrap_or_else(Grouping::single)
            } else {
                choose_grouping(&tokens, xsize, ysize)
            };
        }
        if grouping.groups > 1 || pass > 0 {
            let bits = spatial_bits(&tokens, &grouping, xsize);
            if bits < best_bits {
                best_bits = bits;
                best = (grouping.clone(), tokens.clone());
            }
        }
    }
    write_spatial(w, &best.1, &best.0, xsize);
}

/// The tile size and clustering that codes `tokens` smallest, or one group for the whole image
/// when that is smaller.
fn choose_grouping(tokens: &[Token], xsize: usize, ysize: usize) -> Grouping {
    (2..=9)
        .filter(|&bits| xsize.div_ceil(1 << bits) * ysize.div_ceil(1 << bits) <= MAX_TILES)
        .take(TILE_SIZES)
        .map(|bits| cluster(tokens, xsize, ysize, bits))
        .chain(std::iter::once(Grouping::single()))
        .min_by_key(|grouping| spatial_bits(tokens, grouping, xsize))
        .unwrap_or_else(Grouping::single)
}

/// Exact size in bits of the image written by [`write_spatial`].
fn spatial_bits(tokens: &[Token], grouping: &Grouping, xsize: usize) -> u64 {
    let mut w = BitWriter::default();
    write_spatial(&mut w, tokens, grouping, xsize);
    w.bit_len()
}

/// Write a spatially coded image: no color cache, the entropy image when there are several
/// groups, every group's five prefix codes, then the tokens.
fn write_spatial(w: &mut BitWriter, tokens: &[Token], grouping: &Grouping, xsize: usize) {
    w.put(0, 1);
    if grouping.groups > 1 {
        w.put(1, 1);
        w.put(grouping.bits - 2, 3);
        w.put(0, 1);
        write_entropy_coded_image(
            w,
            &backward_references(&grouping.tile_group, grouping.tiles_x),
        );
    } else {
        w.put(0, 1);
    }
    let codes: Vec<[PrefixCode; 5]> = grouping
        .histograms(tokens, xsize)
        .iter()
        .map(Histograms::codes)
        .collect();
    for group in &codes {
        for code in group {
            code.write_header(w);
        }
    }
    let mut position = 0;
    for &token in tokens {
        write_token(w, &codes[grouping.group_at(position, xsize)], token);
        position += token.len();
    }
}

/// Symbols counted per tile when clustering: green (literals and lengths), then distances.
const TILE_SYMBOLS: usize = NUM_GREEN + NUM_DISTANCE_CODES;

/// Estimated cost in 1/256 bits of coding a histogram of `TILE_SYMBOLS` counts with its own
/// prefix codes, headers included.
fn group_cost(histogram: &[u32]) -> u64 {
    let (green, distance) = histogram.split_at(NUM_GREEN);
    [green, distance]
        .iter()
        .map(|part| {
            let used = part.iter().filter(|&&c| c > 0).count() as u64;
            let header = if used <= 2 { 12 } else { 40 + 3 * used };
            entropy_q8(part) + header * u64::from(ONE_BIT)
        })
        .sum::<u64>()
        + GROUP_OVERHEAD
}

/// Symbol costs of a group histogram, for assigning tiles to the cheapest group.
fn group_symbol_costs(histogram: &[u32]) -> Vec<u32> {
    let (green, distance) = histogram.split_at(NUM_GREEN);
    let mut costs = symbol_costs_q8(green, 0, MAX_SYMBOL_COST);
    costs.extend(symbol_costs_q8(distance, 0, MAX_SYMBOL_COST));
    costs
}

/// Cluster the tiles of side `1 << bits` into groups with similar statistics.
///
/// Seeds are picked farthest first (the tile that the current groups code worst), refined by
/// reassigning every tile to its cheapest group, then merged greedily while one group is
/// estimated cheaper than two.
fn cluster(tokens: &[Token], xsize: usize, ysize: usize, bits: u32) -> Grouping {
    let tiles_x = xsize.div_ceil(1 << bits);
    let tiles = tiles_x * ysize.div_ceil(1 << bits);
    let mut counts = vec![0u32; tiles * TILE_SYMBOLS];
    let mut position = 0;
    for &token in tokens {
        let (x, y) = (position % xsize, position / xsize);
        let row = &mut counts[((y >> bits) * tiles_x + (x >> bits)) * TILE_SYMBOLS..];
        match token {
            Token::Literal([_, g, _, _]) => row[usize::from(g)] += 1,
            Token::Copy { length, code } => {
                row[NUM_LITERALS + prefix_encode(length).0] += 1;
                row[NUM_GREEN + prefix_encode(code).0] += 1;
            }
        }
        position += token.len();
    }
    let occupied: Vec<usize> = (0..tiles)
        .filter(|&t| {
            counts[t * TILE_SYMBOLS..(t + 1) * TILE_SYMBOLS]
                .iter()
                .any(|&c| c > 0)
        })
        .collect();
    let sparse: Vec<Vec<(u16, u32)>> = occupied
        .iter()
        .map(|&t| {
            counts[t * TILE_SYMBOLS..(t + 1) * TILE_SYMBOLS]
                .iter()
                .enumerate()
                .filter(|&(_, &c)| c > 0)
                .map(|(s, &c)| (s as u16, c))
                .collect()
        })
        .collect();
    let tile_cost = |entries: &[(u16, u32)], costs: &[u32]| -> u64 {
        entries
            .iter()
            .map(|&(s, c)| u64::from(c) * u64::from(costs[usize::from(s)]))
            .sum()
    };

    let mut total = vec![0u32; TILE_SYMBOLS];
    for entries in &sparse {
        for &(s, c) in entries {
            total[usize::from(s)] += c;
        }
    }
    let mut centers = vec![group_symbol_costs(&total)];
    let mut best: Vec<u64> = sparse.iter().map(|e| tile_cost(e, &centers[0])).collect();
    let own: Vec<u64> = sparse
        .iter()
        .map(|entries| {
            let mut histogram = vec![0u32; TILE_SYMBOLS];
            for &(s, c) in entries {
                histogram[usize::from(s)] = c;
            }
            tile_cost(entries, &group_symbol_costs(&histogram))
        })
        .collect();
    while centers.len() < MAX_GROUPS.min(sparse.len()) {
        let Some((seed, gain)) = (0..sparse.len())
            .map(|t| (t, best[t].saturating_sub(own[t])))
            .max_by_key(|&(t, gain)| (gain, std::cmp::Reverse(t)))
        else {
            break;
        };
        if gain == 0 {
            break;
        }
        let mut histogram = vec![0u32; TILE_SYMBOLS];
        for &(s, c) in &sparse[seed] {
            histogram[usize::from(s)] = c;
        }
        let costs = group_symbol_costs(&histogram);
        for (b, entries) in best.iter_mut().zip(&sparse) {
            *b = (*b).min(tile_cost(entries, &costs));
        }
        centers.push(costs);
    }

    let mut assignment = vec![0usize; sparse.len()];
    let mut histograms: Vec<Vec<u32>> = Vec::new();
    for _ in 0..CLUSTER_ROUNDS {
        assignment = sparse
            .par_iter()
            .map(|entries| {
                (0..centers.len())
                    .min_by_key(|&c| (tile_cost(entries, &centers[c]), c))
                    .unwrap_or(0)
            })
            .collect();
        histograms = vec![vec![0u32; TILE_SYMBOLS]; centers.len()];
        for (entries, &g) in sparse.iter().zip(&assignment) {
            for &(s, c) in entries {
                histograms[g][usize::from(s)] += c;
            }
        }
        let mut renumber = vec![usize::MAX; centers.len()];
        let mut kept = Vec::new();
        for (g, histogram) in histograms.into_iter().enumerate() {
            if histogram.iter().any(|&c| c > 0) {
                renumber[g] = kept.len();
                kept.push(histogram);
            }
        }
        for g in &mut assignment {
            *g = renumber[*g];
        }
        histograms = kept;
        centers = histograms.iter().map(|h| group_symbol_costs(h)).collect();
    }

    let mut costs: Vec<u64> = histograms.iter().map(|h| group_cost(h)).collect();
    let merged =
        |a: &[u32], b: &[u32]| -> Vec<u32> { a.iter().zip(b).map(|(x, y)| x + y).collect() };
    let k = histograms.len();
    let mut alive = vec![true; k];
    let mut pair_cost = vec![vec![0u64; k]; k];
    for a in 0..k {
        for b in a + 1..k {
            pair_cost[a][b] = group_cost(&merged(&histograms[a], &histograms[b]));
        }
    }
    let mut target: Vec<usize> = (0..k).collect();
    loop {
        let mut choice = None;
        for a in (0..k).filter(|&a| alive[a]) {
            for b in (a + 1..k).filter(|&b| alive[b]) {
                let separate = costs[a] + costs[b];
                if pair_cost[a][b] < separate {
                    let saving = separate - pair_cost[a][b];
                    if choice.is_none_or(|(best_saving, _, _)| saving > best_saving) {
                        choice = Some((saving, a, b));
                    }
                }
            }
        }
        let Some((_, a, b)) = choice else {
            break;
        };
        histograms[a] = merged(&histograms[a], &histograms[b]);
        costs[a] = pair_cost[a][b];
        alive[b] = false;
        for t in &mut target {
            if *t == b {
                *t = a;
            }
        }
        for c in (0..k).filter(|&c| alive[c] && c != a) {
            let (lo, hi) = (a.min(c), a.max(c));
            pair_cost[lo][hi] = group_cost(&merged(&histograms[a], &histograms[c]));
        }
    }

    let mut renumber = vec![usize::MAX; k];
    let mut groups = 0;
    let mut tile_group = vec![0u8; tiles];
    let mut occupied_iter = occupied.iter().zip(&assignment).peekable();
    let mut previous = 0u8;
    for (t, slot) in tile_group.iter_mut().enumerate() {
        if let Some(&(&tile, &g)) = occupied_iter.peek()
            && tile == t
        {
            occupied_iter.next();
            let g = target[g];
            if renumber[g] == usize::MAX {
                renumber[g] = groups;
                groups += 1;
            }
            previous = renumber[g] as u8;
        }
        *slot = previous;
    }
    if groups < 2 {
        return Grouping::single();
    }
    Grouping {
        bits,
        tiles_x,
        tile_group,
        groups,
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
    fn regions_with_different_textures_get_their_own_prefix_codes() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let colors = [[0, 0, 0, 255], [255, 255, 255, 255], [200, 30, 30, 255]];
        let img = RgbaImage::from_fn(640, 480, |x, y| {
            let roll = next() % 16;
            let index = match (x < 320, y < 240) {
                (true, true) => usize::from(roll < 2),
                (false, true) => usize::from(roll < 8),
                (true, false) => 2 * usize::from(roll < 5),
                (false, false) => 1 + usize::from(roll < 14),
            };
            Rgba(colors[index])
        });
        round_trip(&img);

        let palette: Vec<[u8; 4]> = colors.to_vec();
        let (symbols, xsize) = bundle_indices(&img, &palette, 2);
        let matcher = Matcher::new(&symbols, xsize);
        let tokens = matcher.parse(
            &[matcher.price(CostModel::initial(&symbols))],
            &Grouping::single(),
            &length_steps(),
        );
        let grouping = choose_grouping(&tokens, xsize, symbols.len() / xsize);
        assert!(grouping.groups >= 4, "{} groups", grouping.groups);
        assert!(
            spatial_bits(&tokens, &grouping, xsize)
                < spatial_bits(&tokens, &Grouping::single(), xsize)
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
    fn length_table_matches_prefix_encoding() {
        for (length, &entry) in LENGTH_CODES.iter().enumerate().skip(1) {
            let (symbol, extra_bits, _) = prefix_encode(length);
            assert_eq!(entry, (symbol as u8, extra_bits as u8), "{length}");
        }
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
