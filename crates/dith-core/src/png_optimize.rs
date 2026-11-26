//! Maximum lossless PNG/APNG compression behind `--optimize`.
//!
//! Indexed palettes are tried in a few orders and every image stream (the `IDAT` image and each
//! APNG `fdAT` frame) is re-filtered with the best of several PNG filter strategies. Candidates
//! are ranked with the fast setting of this crate's DEFLATE encoder; the winners are compressed
//! with its slow setting and with Zopfli, and the smaller stream is kept. All other chunks are
//! kept as is.
//!
//! Packed indexed scanlines (1, 2 or 4 bits per pixel) are only tried unfiltered: byte-wise PNG
//! filters scramble packed dither patterns and never win there.

use std::io::Read;

use anyhow::{Context, Result, bail, ensure};
use flate2::read::ZlibDecoder;
use flate2::{Compress, Crc, FlushCompress};
use rayon::prelude::*;

use crate::deflate::{Effort, zlib_compress};
use crate::huffman::entropy_q8;

pub(crate) const SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

/// Largest chunk payload allowed by the PNG specification.
const MAX_CHUNK_LEN: usize = i32::MAX as usize;

/// How many of the best filter strategies, ranked by a fast trial compression, are compressed at
/// full effort.
const FINALISTS: usize = 2;

/// Rows of already filtered output the `Brute` strategy compresses along with each candidate row.
const BRUTE_CONTEXT_ROWS: usize = 7;

/// How a filter type is chosen for each scanline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Strategy {
    /// One PNG filter type (0 = None, 1 = Sub, 2 = Up, 3 = Average, 4 = Paeth) for every row.
    Fixed(u8),
    /// Per row, the filter with the smallest sum of absolute signed residuals.
    MinSum,
    /// Per row, the filter whose residual bytes have the lowest Shannon entropy.
    Entropy,
    /// Per row, the filter whose residuals contain the fewest distinct byte pairs.
    Bigrams,
    /// Per row, the filter whose residual byte pairs have the lowest Shannon entropy.
    BigEnt,
    /// Per row, the filter that DEFLATEs smallest together with the preceding rows.
    Brute,
}

const STRATEGIES: [Strategy; 10] = [
    Strategy::Fixed(0),
    Strategy::Fixed(1),
    Strategy::Fixed(2),
    Strategy::Fixed(3),
    Strategy::Fixed(4),
    Strategy::MinSum,
    Strategy::Entropy,
    Strategy::Bigrams,
    Strategy::BigEnt,
    Strategy::Brute,
];

#[derive(Clone)]
struct Chunk<'a> {
    kind: [u8; 4],
    data: &'a [u8],
}

enum Segment<'a> {
    Verbatim(Chunk<'a>),
    /// `PLTE` or `tRNS` of an indexed image, rewritten in the chosen palette order.
    Palette([u8; 4]),
    /// Unfiltered scanlines of one `IDAT` image or `fdAT` frame.
    Image {
        kind: [u8; 4],
        width: usize,
        raw: Vec<u8>,
    },
}

/// Recompress a non-interlaced PNG or APNG whose image data was written unfiltered (as the `png`
/// crate does with `Compression::NoCompression`) into the smallest stream this module can find.
///
/// # Errors
/// Returns an error if the input is not such a PNG or recompression fails.
pub(crate) fn recompress(stored: &[u8]) -> Result<Vec<u8>> {
    let chunks = parse_chunks(
        stored
            .strip_prefix(&SIGNATURE)
            .context("not a PNG stream")?,
    )?;
    let ihdr = chunks
        .first()
        .filter(|c| c.kind == *b"IHDR" && c.data.len() == 13)
        .context("PNG stream does not start with a valid IHDR chunk")?
        .data;
    ensure!(ihdr[12] == 0, "interlaced PNG streams are not supported");
    let indexed = ihdr[9] == 3;
    let channels: usize = match ihdr[9] {
        0 | 3 => 1,
        4 => 2,
        2 => 3,
        6 => 4,
        other => bail!("unknown PNG color type {other}"),
    };
    let depth = usize::from(ihdr[8]);
    let bits_per_pixel = channels * depth;

    let mut segments: Vec<Segment> = Vec::new();
    let mut palette: Vec<[u8; 4]> = Vec::new();
    let mut zlib = Vec::new();
    let mut width = be_u32(&ihdr[0..4]) as usize;
    for (i, chunk) in chunks.iter().enumerate() {
        match &chunk.kind {
            b"IDAT" => zlib.extend_from_slice(chunk.data),
            b"fdAT" => zlib.extend_from_slice(chunk.data.get(4..).context("truncated fdAT chunk")?),
            b"fcTL" => {
                ensure!(chunk.data.len() == 26, "invalid fcTL chunk");
                width = be_u32(&chunk.data[4..8]) as usize;
            }
            b"PLTE" if indexed => {
                palette = chunk
                    .data
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|c| [c[0], c[1], c[2], 255])
                    .collect();
            }
            b"tRNS" if indexed => {
                for (entry, &alpha) in palette.iter_mut().zip(chunk.data) {
                    entry[3] = alpha;
                }
            }
            _ => {}
        }
        match &chunk.kind {
            b"IDAT" | b"fdAT" => {
                if chunks
                    .get(i + 1)
                    .is_some_and(|next| next.kind == chunk.kind)
                {
                    continue;
                }
                segments.push(Segment::Image {
                    kind: chunk.kind,
                    width,
                    raw: unfiltered_scanlines(&std::mem::take(&mut zlib), width, bits_per_pixel)?,
                });
            }
            b"PLTE" | b"tRNS" if indexed => segments.push(Segment::Palette(chunk.kind)),
            _ => segments.push(Segment::Verbatim(chunk.clone())),
        }
    }

    let frames: Vec<(usize, &[u8])> = segments
        .iter()
        .filter_map(|s| match s {
            Segment::Image { width, raw, .. } => Some((*width, raw.as_slice())),
            _ => None,
        })
        .collect();
    let candidates = if indexed {
        palette_orders(&palette, &frames, depth)
            .into_iter()
            .map(|order| {
                let mut new_index = vec![0u8; palette.len()];
                for (new, &old) in order.iter().enumerate() {
                    new_index[old] = new as u8;
                }
                let remapped: Vec<Vec<u8>> = frames
                    .iter()
                    .map(|&(w, raw)| {
                        let indices: Vec<u8> = unpack(raw, w, depth)
                            .iter()
                            .map(|&i| new_index[usize::from(i)])
                            .collect();
                        pack(&indices, w, depth)
                    })
                    .collect();
                (order.iter().map(|&old| palette[old]).collect(), remapped)
            })
            .collect::<Vec<(Vec<[u8; 4]>, _)>>()
    } else {
        vec![(
            palette,
            frames.iter().map(|&(_, raw)| raw.to_vec()).collect(),
        )]
    };

    let bpp = bits_per_pixel.div_ceil(8);
    let strategies: &[Strategy] = match (indexed, depth) {
        (true, 1 | 2 | 4) => &STRATEGIES[..1],
        (true, _) => &STRATEGIES[..STRATEGIES.len() - 1],
        (false, _) => &STRATEGIES,
    };
    let (palette, finalists) = candidates
        .into_par_iter()
        .map(|(palette, images)| {
            let finalists: Vec<Vec<Trial>> = images
                .par_iter()
                .zip(frames.par_iter())
                .map(|(raw, &(width, _))| {
                    trial_filters(raw, (width * bits_per_pixel).div_ceil(8), bpp, strategies)
                })
                .collect();
            let total: usize = finalists.iter().map(|f| f[0].size).sum();
            (total, palette, finalists)
        })
        .collect::<Vec<_>>()
        .into_iter()
        .min_by_key(|(total, _, _)| *total)
        .map(|(_, palette, finalists)| (palette, finalists))
        .context("PNG stream has no image data")?;

    let mut streams = finalists
        .into_par_iter()
        .map(|trials| {
            trials
                .into_par_iter()
                .enumerate()
                .flat_map(|(rank, trial)| {
                    let mut streams = vec![Ok(zlib_compress(
                        &trial.filtered,
                        trial.stride,
                        Effort::HIGH,
                    ))];
                    if rank == 0 {
                        let mut out = Vec::new();
                        streams.push(
                            zopfli::compress(
                                zopfli::Options::default(),
                                zopfli::Format::Zlib,
                                trial.filtered.as_slice(),
                                &mut out,
                            )
                            .map(|()| out),
                        );
                    }
                    streams
                })
                .collect::<std::io::Result<Vec<_>>>()
                .map(|s| s.into_iter().min_by_key(Vec::len).unwrap_or_default())
        })
        .collect::<std::io::Result<Vec<_>>>()
        .context("compressing PNG image data with Zopfli")?
        .into_iter();

    let mut out = SIGNATURE.to_vec();
    let mut sequence = 0u32;
    for segment in segments {
        match segment {
            Segment::Verbatim(chunk) if chunk.kind == *b"fcTL" => {
                let mut data = chunk.data.to_vec();
                data[..4].copy_from_slice(&sequence.to_be_bytes());
                sequence += 1;
                write_chunk(&mut out, chunk.kind, &data);
            }
            Segment::Verbatim(chunk) => write_chunk(&mut out, chunk.kind, chunk.data),
            Segment::Palette(kind) if kind == *b"PLTE" => {
                let data: Vec<u8> = palette.iter().flat_map(|c| [c[0], c[1], c[2]]).collect();
                write_chunk(&mut out, kind, &data);
            }
            Segment::Palette(kind) => {
                let data: Vec<u8> = palette
                    .iter()
                    .map(|c| c[3])
                    .take_while(|&a| a < 255)
                    .collect();
                write_chunk(&mut out, kind, &data);
            }
            Segment::Image { kind, .. } => {
                let zlib = streams.next().context("missing recompressed PNG frame")?;
                if kind == *b"fdAT" {
                    for part in zlib.chunks(MAX_CHUNK_LEN - 4) {
                        let mut data = Vec::with_capacity(4 + part.len());
                        data.extend_from_slice(&sequence.to_be_bytes());
                        data.extend_from_slice(part);
                        sequence += 1;
                        write_chunk(&mut out, kind, &data);
                    }
                } else {
                    for part in zlib.chunks(MAX_CHUNK_LEN) {
                        write_chunk(&mut out, kind, part);
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Palette orders worth trying, as lists of old indices: the original order, most frequent first,
/// and darkest first. Translucent entries stay ahead of opaque ones so `tRNS` stays short.
fn palette_orders(palette: &[[u8; 4]], frames: &[(usize, &[u8])], depth: usize) -> Vec<Vec<usize>> {
    let mut counts = vec![0u64; palette.len()];
    for &(width, raw) in frames {
        for i in unpack(raw, width, depth) {
            if let Some(count) = counts.get_mut(usize::from(i)) {
                *count += 1;
            }
        }
    }
    let identity: Vec<usize> = (0..palette.len()).collect();
    let mut by_count = identity.clone();
    by_count.sort_by_key(|&i| (palette[i][3] == 255, std::cmp::Reverse(counts[i])));
    let mut by_luma = identity.clone();
    by_luma.sort_by_key(|&i| {
        let [r, g, b, a] = palette[i].map(u32::from);
        (a == 255, 2126 * r + 7152 * g + 722 * b)
    });
    let mut orders = vec![identity];
    for order in [by_count, by_luma] {
        if !orders.contains(&order) {
            orders.push(order);
        }
    }
    orders
}

/// Unpack `depth`-bit palette indices (1, 2, 4, or 8) from packed scanlines, one byte per pixel.
fn unpack(raw: &[u8], width: usize, depth: usize) -> Vec<u8> {
    let per_byte = 8 / depth;
    let mask = ((1u16 << depth) - 1) as u8;
    let row_len = width.div_ceil(per_byte);
    raw.chunks_exact(row_len)
        .flat_map(|row| {
            (0..width).map(move |x| (row[x / per_byte] >> (8 - depth * (x % per_byte + 1))) & mask)
        })
        .collect()
}

/// Pack one-byte-per-pixel palette indices into `depth`-bit scanlines (the inverse of [`unpack`]).
fn pack(indices: &[u8], width: usize, depth: usize) -> Vec<u8> {
    let per_byte = 8 / depth;
    let row_len = width.div_ceil(per_byte);
    let mut out = vec![0u8; row_len * (indices.len() / width)];
    for (row, out_row) in indices
        .chunks_exact(width)
        .zip(out.chunks_exact_mut(row_len))
    {
        for (x, &i) in row.iter().enumerate() {
            out_row[x / per_byte] |= i << (8 - depth * (x % per_byte + 1));
        }
    }
    out
}

/// Inflate one zlib image stream of unfiltered scanlines and strip the filter type bytes.
fn unfiltered_scanlines(zlib: &[u8], width: usize, bits_per_pixel: usize) -> Result<Vec<u8>> {
    let mut scanlines = Vec::new();
    ZlibDecoder::new(zlib)
        .read_to_end(&mut scanlines)
        .context("inflating PNG image data")?;
    let row_len = (width * bits_per_pixel).div_ceil(8);
    ensure!(
        scanlines.len() % (row_len + 1) == 0,
        "PNG image data does not match its frame width {width}"
    );
    let mut raw = Vec::with_capacity(scanlines.len());
    for line in scanlines.chunks_exact(row_len + 1) {
        ensure!(line[0] == 0, "expected unfiltered PNG scanlines");
        raw.extend_from_slice(&line[1..]);
    }
    Ok(raw)
}

/// One filtered candidate for an image stream.
struct Trial {
    /// Compressed size at the fast trial effort.
    size: usize,
    stride: usize,
    filtered: Vec<u8>,
}

/// Filter `raw` with each of `strategies` and return the [`FINALISTS`] distinct results that
/// compress smallest at a fast trial effort, smallest first.
fn trial_filters(raw: &[u8], row_len: usize, bpp: usize, strategies: &[Strategy]) -> Vec<Trial> {
    let stride = row_len + 1;
    let mut trials: Vec<Trial> = strategies
        .par_iter()
        .map(|&strategy| {
            let filtered = apply_strategy(strategy, raw, row_len, bpp);
            Trial {
                size: zlib_compress(&filtered, stride, Effort::DEFAULT).len(),
                stride,
                filtered,
            }
        })
        .collect();
    trials.sort_by_key(|trial| trial.size);
    trials.dedup_by(|a, b| a.filtered == b.filtered);
    trials.truncate(FINALISTS);
    trials
}

/// Filter every `row_len`-byte scanline of `raw`, prefixing each with its filter type byte.
fn apply_strategy(strategy: Strategy, raw: &[u8], row_len: usize, bpp: usize) -> Vec<u8> {
    let zero_row = vec![0u8; row_len];
    let mut prev = zero_row.as_slice();
    let mut out = Vec::with_capacity(raw.len() + raw.len() / row_len);
    let mut scorer = RowScorer::new(strategy);
    let mut candidate = Vec::with_capacity(row_len + 1);
    let mut best = Vec::with_capacity(row_len + 1);
    for row in raw.chunks_exact(row_len) {
        if let Strategy::Fixed(kind) = strategy {
            filter_row(kind, bpp, prev, row, &mut out);
        } else {
            let context = &out[out.len().saturating_sub(BRUTE_CONTEXT_ROWS * (row_len + 1))..];
            let mut best_score = u64::MAX;
            for kind in 0..5 {
                candidate.clear();
                filter_row(kind, bpp, prev, row, &mut candidate);
                let score = scorer.score(context, &candidate);
                if score < best_score {
                    best_score = score;
                    std::mem::swap(&mut best, &mut candidate);
                }
            }
            out.extend_from_slice(&best);
        }
        prev = row;
    }
    out
}

/// Scores candidate filtered rows for the adaptive strategies (lower compresses better).
struct RowScorer {
    strategy: Strategy,
    deflate: Compress,
    buffer: Vec<u8>,
    pair_counts: Vec<u32>,
    touched: Vec<usize>,
}

impl RowScorer {
    fn new(strategy: Strategy) -> Self {
        Self {
            strategy,
            deflate: Compress::new(flate2::Compression::new(5), false),
            buffer: Vec::new(),
            pair_counts: if matches!(strategy, Strategy::Bigrams | Strategy::BigEnt) {
                vec![0; 1 << 16]
            } else {
                Vec::new()
            },
            touched: Vec::new(),
        }
    }

    /// Score `candidate` (a filter type byte plus residuals) following the filtered `context` rows.
    fn score(&mut self, context: &[u8], candidate: &[u8]) -> u64 {
        let residuals = &candidate[1..];
        match self.strategy {
            Strategy::Fixed(_) => 0,
            Strategy::MinSum => residuals
                .iter()
                .map(|&b| u64::from(b.cast_signed().unsigned_abs()))
                .sum::<u64>(),
            Strategy::Entropy => {
                let mut counts = [0u32; 256];
                for &b in residuals {
                    counts[usize::from(b)] += 1;
                }
                entropy_q8(&counts)
            }
            Strategy::Bigrams | Strategy::BigEnt => {
                for pair in residuals.windows(2) {
                    let index = usize::from(pair[0]) << 8 | usize::from(pair[1]);
                    if self.pair_counts[index] == 0 {
                        self.touched.push(index);
                    }
                    self.pair_counts[index] += 1;
                }
                let score = if self.strategy == Strategy::Bigrams {
                    self.touched.len() as u64
                } else {
                    let counts: Vec<u32> =
                        self.touched.iter().map(|&i| self.pair_counts[i]).collect();
                    entropy_q8(&counts)
                };
                for &i in &self.touched {
                    self.pair_counts[i] = 0;
                }
                self.touched.clear();
                score
            }
            Strategy::Brute => {
                self.buffer.clear();
                self.buffer.extend_from_slice(context);
                self.buffer.extend_from_slice(candidate);
                let mut compressed = Vec::with_capacity(self.buffer.len() * 2 + 64);
                self.deflate.reset();
                match self.deflate.compress_vec(
                    &self.buffer,
                    &mut compressed,
                    FlushCompress::Finish,
                ) {
                    Ok(_) => compressed.len() as u64,
                    Err(_) => u64::MAX,
                }
            }
        }
    }
}

/// Append the PNG filter type byte and the filtered bytes of `row` (with `prev` as the row above).
fn filter_row(kind: u8, bpp: usize, prev: &[u8], row: &[u8], out: &mut Vec<u8>) {
    out.push(kind);
    out.extend(row.iter().enumerate().map(|(i, &x)| {
        let a = if i >= bpp { row[i - bpp] } else { 0 };
        let b = prev[i];
        let c = if i >= bpp { prev[i - bpp] } else { 0 };
        let predicted = match kind {
            0 => 0,
            1 => a,
            2 => b,
            3 => a.midpoint(b),
            _ => paeth(a, b, c),
        };
        x.wrapping_sub(predicted)
    }));
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let (pa, pb, pc) = (
        (p - i16::from(a)).abs(),
        (p - i16::from(b)).abs(),
        (p - i16::from(c)).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn parse_chunks(mut body: &[u8]) -> Result<Vec<Chunk<'_>>> {
    let mut chunks = Vec::new();
    while !body.is_empty() {
        ensure!(body.len() >= 12, "truncated PNG chunk header");
        let len = be_u32(&body[0..4]) as usize;
        ensure!(body.len() >= 12 + len, "truncated PNG chunk");
        chunks.push(Chunk {
            kind: [body[4], body[5], body[6], body[7]],
            data: &body[8..8 + len],
        });
        body = &body[12 + len..];
    }
    Ok(chunks)
}

pub(crate) fn write_chunk(out: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(&kind);
    out.extend_from_slice(data);
    let mut crc = Crc::new();
    crc.update(&out[start..]);
    out.extend_from_slice(&crc.sum().to_be_bytes());
}

fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}
