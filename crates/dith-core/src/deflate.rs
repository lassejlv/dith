//! Near-optimal DEFLATE compression for PNG image data.
//!
//! Dithered scanlines are mostly literals with a few useful matches, and their statistics change
//! with the tone of the image. Level 9 of a greedy compressor loses several percent on such data
//! because it takes matches that cost more than the literals they replace and cannot place block
//! boundaries. This encoder finds candidate matches once, then chooses literals and matches by
//! minimum estimated bit cost, splits the stream into blocks with their own Huffman codes, and
//! refines the costs from each block's real statistics.
//!
//! All cost arithmetic is fixed point and blocks are compressed independently of the thread
//! count, so the output is identical on every platform.

use rayon::prelude::*;

use crate::huffman::{
    BitWriter, ONE_BIT, canonical_codes, code_length_tokens, entropy_q8, huffman_lengths,
    symbol_costs_q8,
};

const WINDOW: usize = 1 << 15;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const NUM_LITLEN: usize = 286;
const NUM_DIST: usize = 30;
const END_OF_BLOCK: usize = 256;
const MAX_CODE_LENGTH: u32 = 15;
const MAX_CODE_LENGTH_CODE_LENGTH: u32 = 7;
const CODE_LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];
const HASH3_BITS: u32 = 15;
const HASH4_BITS: u32 = 16;
const NO_POSITION: u32 = u32::MAX;
/// Input bytes searched for matches as one unit of parallel work.
const SECTION: usize = 1 << 16;
/// Matches are always tried at 1 to this many scanlines back.
const TRACKED_ROWS: usize = 8;
/// Short distances that are always tried: the periods of flat and ordered-dither rows.
const TRACKED_NEAR: [usize; 4] = [1, 2, 4, 8];
/// A match at least this long is taken outright, without pricing the positions it covers.
const NICE_MATCH: usize = 128;
/// Input bytes per block before blocks with similar statistics are merged.
const CHUNK: usize = 4096;
/// Most positions priced in one dynamic-programming run, so costs fit in 32 bits.
const SEGMENT: usize = 1 << 17;
/// Assumed cost of a length or distance symbol before any statistics exist.
const DEFAULT_LENGTH_COST: u32 = 6 * ONE_BIT;
const DEFAULT_DISTANCE_COST: u32 = 5 * ONE_BIT;
/// Cap on the estimated cost of one symbol.
const MAX_SYMBOL_COST: u32 = 16 * ONE_BIT;

/// How much work to spend on the search.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Effort {
    /// Hash-chain candidates examined per position.
    pub chain_limit: usize,
    /// Cost-model refinement passes after the first parse.
    pub refinements: usize,
}

impl Effort {
    /// Fast enough for default output.
    pub(crate) const DEFAULT: Self = Self {
        chain_limit: 32,
        refinements: 1,
    };
    /// Cheap enough to rank many `--optimize` candidates before the finalists are compressed.
    pub(crate) const TRIAL: Self = Self {
        chain_limit: 8,
        refinements: 0,
    };
    /// Slower, for `--optimize`.
    pub(crate) const HIGH: Self = Self {
        chain_limit: 1024,
        refinements: 30,
    };
}

/// Compress `data` into a zlib stream.
///
/// `stride` is the distance between vertically adjacent samples (the scanline length including
/// its filter byte), or 0 when the data has no such structure.
pub(crate) fn zlib_compress(data: &[u8], stride: usize, effort: Effort) -> Vec<u8> {
    let mut w = BitWriter::with_capacity(data.len() / 2 + 64);
    w.put(0x78, 8);
    w.put(0xDA, 8);
    deflate(&mut w, data, stride, effort);
    w.align();
    w.put_bytes(&adler32(data).to_be_bytes());
    w.finish()
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    b << 16 | a
}

/// Split a match length (`3..=258`) into its symbol, extra-bit count and extra bits.
fn length_code(length: usize) -> (usize, u32, u32) {
    let v = length - MIN_MATCH;
    if v < 8 {
        return (257 + v, 0, 0);
    }
    if length == MAX_MATCH {
        return (285, 0, 0);
    }
    let high = v.ilog2();
    let extra_bits = high - 2;
    (
        257 + 4 * (high as usize - 1) + ((v >> extra_bits) & 3),
        extra_bits,
        (v & ((1 << extra_bits) - 1)) as u32,
    )
}

/// Split a match distance (`1..=32768`) into its symbol, extra-bit count and extra bits.
fn distance_code(distance: usize) -> (usize, u32, u32) {
    let v = distance - 1;
    if v < 4 {
        return (v, 0, 0);
    }
    let high = v.ilog2();
    let extra_bits = high - 1;
    (
        2 * high as usize + ((v >> extra_bits) & 1),
        extra_bits,
        (v & ((1 << extra_bits) - 1)) as u32,
    )
}

/// A literal byte (`distance == 0`, the byte in `length`) or a match.
#[derive(Debug, Clone, Copy)]
struct Token {
    length: u16,
    distance: u16,
}

/// Candidate matches at every position, found once and shared by every parse.
///
/// Each position lists the nearest match of every length the hash chains found, plus matches at
/// whole multiples of the scanline stride. The input is searched in fixed sections so the work
/// can run in parallel without changing the result.
struct Matches {
    sections: Vec<Section>,
}

struct Section {
    starts: Vec<u32>,
    list: Vec<Token>,
}

impl Matches {
    fn find(data: &[u8], stride: usize, chain_limit: usize) -> Self {
        let starts: Vec<usize> = (0..data.len()).step_by(SECTION).collect();
        Self {
            sections: starts
                .into_par_iter()
                .map(|start| Section::find(data, start, stride, chain_limit))
                .collect(),
        }
    }

    fn at(&self, position: usize) -> &[Token] {
        let section = &self.sections[position / SECTION];
        let i = position % SECTION;
        &section.list[section.starts[i] as usize..section.starts[i + 1] as usize]
    }
}

impl Section {
    fn find(data: &[u8], start: usize, stride: usize, chain_limit: usize) -> Self {
        let n = data.len();
        let end = (start + SECTION).min(n);
        let mut starts = Vec::with_capacity(end - start + 1);
        let mut list: Vec<Token> = Vec::with_capacity(end - start);
        let mut head3 = vec![NO_POSITION; 1 << HASH3_BITS];
        let mut head4 = vec![NO_POSITION; 1 << HASH4_BITS];
        let mut chain = vec![NO_POSITION; WINDOW];
        let insert = |i: usize, head3: &mut [u32], head4: &mut [u32], chain: &mut [u32]| {
            if i + 4 <= n {
                head3[hash3(data, i)] = i as u32;
                let h4 = hash4(data, i);
                chain[i % WINDOW] = head4[h4];
                head4[h4] = i as u32;
            }
        };
        for i in start.saturating_sub(WINDOW)..start {
            insert(i, &mut head3, &mut head4, &mut chain);
        }
        let mut tracked: Vec<usize> = TRACKED_NEAR
            .into_iter()
            .chain((1..=TRACKED_ROWS).map(|rows| rows * stride))
            .filter(|&d| d > 0 && d <= WINDOW)
            .collect();
        tracked.sort_unstable();
        tracked.dedup();
        let mut tracked_ends = vec![0usize; tracked.len()];

        let mut i = start;
        while i < end {
            starts.push(list.len() as u32);
            if i + MIN_MATCH > n {
                i += 1;
                continue;
            }
            let limit = (n - i).min(MAX_MATCH);
            let first = list.len();
            let mut longest = MIN_MATCH - 1;
            for (&distance, run_end) in tracked.iter().zip(&mut tracked_ends) {
                if distance > i {
                    break;
                }
                if *run_end <= i {
                    *run_end = i + match_length(data, i - distance, i, limit);
                    if *run_end == i + MAX_MATCH {
                        while *run_end < n && data[*run_end] == data[*run_end - distance] {
                            *run_end += 1;
                        }
                    }
                }
                let length = (*run_end - i).min(limit);
                if length > longest {
                    longest = length;
                    list.push(Token {
                        length: length as u16,
                        distance: distance as u16,
                    });
                }
            }
            if longest < NICE_MATCH {
                let tracked_count = list.len() - first;
                let mut longest = MIN_MATCH - 1;
                let push = |list: &mut Vec<Token>, length: usize, distance: usize| {
                    let dominated = list[first..first + tracked_count].iter().any(|t| {
                        usize::from(t.distance) <= distance && usize::from(t.length) >= length
                    });
                    if !dominated {
                        list.push(Token {
                            length: length as u16,
                            distance: distance as u16,
                        });
                    }
                };
                let h3 = hash3(data, i);
                let candidate = head3[h3];
                if candidate != NO_POSITION && i - candidate as usize <= WINDOW {
                    let length = match_length(data, candidate as usize, i, limit);
                    if length >= MIN_MATCH {
                        longest = length;
                        push(&mut list, length, i - candidate as usize);
                    }
                }
                if i + 4 <= n {
                    let mut candidate = head4[hash4(data, i)];
                    for _ in 0..chain_limit {
                        if candidate == NO_POSITION || longest >= limit {
                            break;
                        }
                        let at = candidate as usize;
                        if i - at > WINDOW {
                            break;
                        }
                        if data[at + longest] == data[i + longest] {
                            let length = match_length(data, at, i, limit);
                            if length > longest {
                                longest = length;
                                push(&mut list, length, i - at);
                            }
                        }
                        let next = chain[at % WINDOW];
                        if next >= candidate {
                            break;
                        }
                        candidate = next;
                    }
                }
            }
            insert(i, &mut head3, &mut head4, &mut chain);
            i += 1;

            let Some(nice) = list[first..]
                .iter()
                .filter(|t| usize::from(t.length) >= NICE_MATCH)
                .max_by_key(|t| (t.length, std::cmp::Reverse(t.distance)))
                .copied()
            else {
                continue;
            };
            let covered = (i - 1 + usize::from(nice.length)).min(end);
            let mut remaining = nice.length - 1;
            while i < covered {
                starts.push(list.len() as u32);
                list.push(Token {
                    length: remaining,
                    distance: nice.distance,
                });
                insert(i, &mut head3, &mut head4, &mut chain);
                remaining -= 1;
                i += 1;
            }
        }
        starts.push(list.len() as u32);
        Self { starts, list }
    }
}

fn hash3(data: &[u8], i: usize) -> usize {
    let key = u32::from(data[i]) | u32::from(data[i + 1]) << 8 | u32::from(data[i + 2]) << 16;
    (key.wrapping_mul(0x9E37_79B1) >> (32 - HASH3_BITS)) as usize
}

fn hash4(data: &[u8], i: usize) -> usize {
    let key = u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
    (key.wrapping_mul(0x9E37_79B1) >> (32 - HASH4_BITS)) as usize
}

/// Length of the common prefix of `data[a..]` and `data[b..]`, at most `limit` (`a < b`).
fn match_length(data: &[u8], a: usize, b: usize, limit: usize) -> usize {
    let x = &data[a..a + limit];
    let y = &data[b..b + limit];
    let mut length = 0;
    for (cx, cy) in x.as_chunks::<8>().0.iter().zip(y.as_chunks::<8>().0) {
        let diff = u64::from_le_bytes(*cx) ^ u64::from_le_bytes(*cy);
        if diff != 0 {
            return length + (diff.trailing_zeros() / 8) as usize;
        }
        length += 8;
    }
    length
        + x[length..]
            .iter()
            .zip(&y[length..])
            .take_while(|(p, q)| p == q)
            .count()
}

/// Symbol counts of one block.
#[derive(Clone)]
struct Histogram {
    litlen: [u32; NUM_LITLEN],
    distance: [u32; NUM_DIST],
}

impl Histogram {
    fn new(tokens: &[Token]) -> Self {
        let mut h = Self {
            litlen: [0; NUM_LITLEN],
            distance: [0; NUM_DIST],
        };
        for token in tokens {
            if token.distance == 0 {
                h.litlen[usize::from(token.length)] += 1;
            } else {
                h.litlen[length_code(usize::from(token.length)).0] += 1;
                h.distance[distance_code(usize::from(token.distance)).0] += 1;
            }
        }
        h.litlen[END_OF_BLOCK] = 1;
        h
    }

    fn merged(&self, other: &Self) -> Self {
        let mut h = self.clone();
        for (a, b) in h.litlen.iter_mut().zip(&other.litlen) {
            *a += b;
        }
        for (a, b) in h.distance.iter_mut().zip(&other.distance) {
            *a += b;
        }
        h.litlen[END_OF_BLOCK] = 1;
        h
    }

    /// Estimated size in 1/256 bit units of a block with these counts, Huffman tables included.
    fn estimated_cost(&self) -> u64 {
        let used = self.litlen.iter().filter(|&&c| c > 0).count()
            + self.distance.iter().filter(|&&c| c > 0).count();
        entropy_q8(&self.litlen)
            + entropy_q8(&self.distance)
            + u64::from(ONE_BIT) * (60 + 4 * used as u64)
    }
}

/// Estimated bit costs, in 1/256 bit units, used to choose between literals and matches.
struct CostModel {
    literal: [u32; 256],
    /// Cost of a match length including its extra bits, indexed by length.
    length: [u32; MAX_MATCH + 1],
    /// Cost of a distance symbol including its extra bits.
    distance: [u32; NUM_DIST],
}

impl CostModel {
    /// Literal costs from the byte frequencies of `bytes`, with flat guesses for matches.
    fn initial(bytes: &[u8]) -> Self {
        let mut counts = [0u32; 256];
        for &b in bytes {
            counts[usize::from(b)] += 1;
        }
        let literal = symbol_costs_q8(&counts, ONE_BIT, MAX_SYMBOL_COST);
        Self::from_symbol_costs(
            &literal,
            &[DEFAULT_LENGTH_COST; NUM_LITLEN - 257],
            &[DEFAULT_DISTANCE_COST; NUM_DIST],
        )
    }

    fn from_histogram(histogram: &Histogram) -> Self {
        let litlen = symbol_costs_q8(&histogram.litlen, ONE_BIT, MAX_SYMBOL_COST);
        let distance = symbol_costs_q8(&histogram.distance, ONE_BIT, MAX_SYMBOL_COST);
        Self::from_symbol_costs(&litlen[..256], &litlen[257..], &distance)
    }

    fn from_symbol_costs(
        literal: &[u32],
        length_symbols: &[u32],
        distance_symbols: &[u32],
    ) -> Self {
        let mut model = Self {
            literal: [0; 256],
            length: [0; MAX_MATCH + 1],
            distance: [0; NUM_DIST],
        };
        model.literal.copy_from_slice(literal);
        for (length, cost) in model.length.iter_mut().enumerate().skip(MIN_MATCH) {
            let (symbol, extra_bits, _) = length_code(length);
            *cost = length_symbols[symbol - 257] + extra_bits * ONE_BIT;
        }
        for (symbol, cost) in model.distance.iter_mut().enumerate() {
            let extra_bits = (symbol as u32 / 2).saturating_sub(1);
            *cost = distance_symbols[symbol] + extra_bits * ONE_BIT;
        }
        model
    }
}

/// Cheapest tokens for `data[start..end]` under `model`.
fn parse(
    data: &[u8],
    matches: &Matches,
    model: &CostModel,
    start: usize,
    end: usize,
) -> Vec<Token> {
    let mut tokens = Vec::with_capacity((end - start) / 2);
    let mut cost = Vec::new();
    let mut steps = Vec::new();
    let mut candidates: Vec<(u32, Token)> = Vec::new();
    let mut segment_start = start;
    while segment_start < end {
        let segment_end = (segment_start + SEGMENT).min(end);
        let len = segment_end - segment_start;
        cost.clear();
        cost.resize(len + 1, u32::MAX);
        steps.clear();
        steps.resize(
            len + 1,
            Token {
                length: 0,
                distance: 0,
            },
        );
        cost[0] = 0;
        let mut i = 0;
        while i < len {
            let position = segment_start + i;
            let here = cost[i];
            let byte = data[position];
            let literal = here + model.literal[usize::from(byte)];
            if literal < cost[i + 1] {
                cost[i + 1] = literal;
                steps[i + 1] = Token {
                    length: u16::from(byte),
                    distance: 0,
                };
            }
            candidates.clear();
            for candidate in matches.at(position) {
                let cost = model.distance[distance_code(usize::from(candidate.distance)).0];
                let at = candidates.partition_point(|&(c, _)| c <= cost);
                candidates.insert(at, (cost, *candidate));
            }
            let mut next_length = MIN_MATCH;
            let mut longest = 0;
            for &(distance_cost, candidate) in &candidates {
                let length = usize::from(candidate.length).min(len - i);
                if length < next_length {
                    continue;
                }
                let base = here + distance_cost;
                for (l, (slot, step)) in cost[i + next_length..=i + length]
                    .iter_mut()
                    .zip(&mut steps[i + next_length..=i + length])
                    .enumerate()
                {
                    let c = base + model.length[next_length + l];
                    if c < *slot {
                        *slot = c;
                        *step = Token {
                            length: (next_length + l) as u16,
                            distance: candidate.distance,
                        };
                    }
                }
                next_length = length + 1;
                longest = length;
            }
            i += if longest >= NICE_MATCH { longest } else { 1 };
        }

        let first = tokens.len();
        let mut at = len;
        while at > 0 {
            let step = steps[at];
            at -= if step.distance == 0 {
                1
            } else {
                usize::from(step.length)
            };
            tokens.push(step);
        }
        tokens[first..].reverse();
        segment_start = segment_end;
    }
    tokens
}

/// One DEFLATE block: the input range it covers and the smallest tokens found for it so far.
struct Block {
    start: usize,
    end: usize,
    tokens: Vec<Token>,
    histogram: Histogram,
    /// Exact size of the block as it would be written.
    bits: u64,
    /// Statistics of the latest parse when it was not the smallest; the next parse is priced
    /// with these.
    latest: Option<(Histogram, u64)>,
    settled: bool,
}

impl Block {
    fn new(start: usize, end: usize, tokens: Vec<Token>) -> Self {
        let histogram = Histogram::new(&tokens);
        let bits = block_bits(&histogram, end - start);
        Self {
            start,
            end,
            tokens,
            histogram,
            bits,
            latest: None,
            settled: false,
        }
    }

    /// Parse again with costs taken from the previous parse, keeping the result when it is the
    /// smallest so far. The block is settled once a parse reproduces the size of the one before.
    fn refine(&mut self, data: &[u8], matches: &Matches) {
        if self.settled {
            return;
        }
        let (statistics, previous_bits) = match &self.latest {
            Some((histogram, bits)) => (histogram, *bits),
            None => (&self.histogram, self.bits),
        };
        let model = CostModel::from_histogram(statistics);
        let tokens = parse(data, matches, &model, self.start, self.end);
        let histogram = Histogram::new(&tokens);
        let bits = block_bits(&histogram, self.end - self.start);
        self.settled = bits == previous_bits;
        if bits < self.bits {
            self.tokens = tokens;
            self.histogram = histogram;
            self.bits = bits;
            self.latest = None;
        } else {
            self.latest = Some((histogram, bits));
        }
    }
}

fn deflate(w: &mut BitWriter, data: &[u8], stride: usize, effort: Effort) {
    if data.is_empty() {
        w.put(0b011, 3);
        w.put(0, 7);
        return;
    }
    let matches = Matches::find(data, stride, effort.chain_limit);
    let ranges: Vec<(usize, usize)> = (0..data.len())
        .step_by(CHUNK)
        .map(|start| (start, (start + CHUNK).min(data.len())))
        .collect();
    let mut blocks: Vec<Block> = ranges
        .into_par_iter()
        .map(|(start, end)| {
            let model = CostModel::initial(&data[start..end]);
            Block::new(start, end, parse(data, &matches, &model, start, end))
        })
        .collect();
    for _ in 0..effort.refinements {
        blocks = merge_blocks(blocks);
        if blocks.iter().all(|block| block.settled) {
            break;
        }
        blocks
            .par_iter_mut()
            .for_each(|block| block.refine(data, &matches));
    }
    let blocks = merge_blocks(blocks);
    let last = blocks.len() - 1;
    for (i, block) in blocks.iter().enumerate() {
        write_block(w, data, block, i == last);
    }
}

/// Merge neighbouring blocks, cheapest merge first, while one shared set of Huffman codes is
/// estimated to be smaller than two.
fn merge_blocks(blocks: Vec<Block>) -> Vec<Block> {
    let mut slots: Vec<Option<Block>> = blocks.into_iter().map(Some).collect();
    let n = slots.len();
    let mut costs: Vec<u64> = slots
        .iter()
        .map(|b| b.as_ref().map_or(0, |b| b.histogram.estimated_cost()))
        .collect();
    let mut next: Vec<usize> = (1..=n).collect();
    let mut previous: Vec<usize> = (0..n).map(|i| i.wrapping_sub(1)).collect();
    let gain = |slots: &[Option<Block>], costs: &[u64], a: usize, b: usize| -> Option<(u64, u64)> {
        let (left, right) = (slots[a].as_ref()?, slots[b].as_ref()?);
        let merged = left.histogram.merged(&right.histogram).estimated_cost();
        (merged < costs[a] + costs[b]).then(|| (costs[a] + costs[b] - merged, merged))
    };
    let mut heap = std::collections::BinaryHeap::new();
    for a in 0..n.saturating_sub(1) {
        if let Some((saved, merged)) = gain(&slots, &costs, a, a + 1) {
            heap.push((
                saved,
                std::cmp::Reverse(a),
                a + 1,
                merged,
                costs[a],
                costs[a + 1],
            ));
        }
    }
    while let Some((_, std::cmp::Reverse(a), b, merged, cost_a, cost_b)) = heap.pop() {
        if slots[a].is_none() || slots[b].is_none() || next[a] != b {
            continue;
        }
        if costs[a] != cost_a || costs[b] != cost_b {
            continue;
        }
        let (Some(mut left), Some(right)) = (slots[a].take(), slots[b].take()) else {
            continue;
        };
        left.end = right.end;
        left.histogram = left.histogram.merged(&right.histogram);
        left.tokens.extend_from_slice(&right.tokens);
        left.bits = block_bits(&left.histogram, left.end - left.start);
        left.latest = None;
        left.settled = false;
        slots[a] = Some(left);
        costs[a] = merged;
        next[a] = next[b];
        if next[b] < n {
            previous[next[b]] = a;
        }
        if previous[a] != usize::MAX
            && let Some((saved, merged)) = gain(&slots, &costs, previous[a], a)
        {
            let p = previous[a];
            heap.push((saved, std::cmp::Reverse(p), a, merged, costs[p], costs[a]));
        }
        if next[a] < n
            && let Some((saved, merged)) = gain(&slots, &costs, a, next[a])
        {
            let q = next[a];
            heap.push((saved, std::cmp::Reverse(a), q, merged, costs[a], costs[q]));
        }
    }
    slots.into_iter().flatten().collect()
}

/// The Huffman codes of a dynamic block and the size of its header.
struct DynamicCodes {
    litlen_lengths: Vec<u8>,
    distance_lengths: Vec<u8>,
    code_length_lengths: Vec<u8>,
    code_length_tokens: Vec<(u8, u8)>,
    num_code_lengths: usize,
    header_bits: u64,
}

impl DynamicCodes {
    fn new(histogram: &Histogram) -> Self {
        let mut litlen_lengths = huffman_lengths(&histogram.litlen, MAX_CODE_LENGTH);
        let mut distance_lengths = huffman_lengths(&histogram.distance, MAX_CODE_LENGTH);
        // Some decoders reject a block with fewer than two distance codes.
        for symbol in 0..2 {
            if distance_lengths.iter().filter(|&&l| l > 0).count() < 2
                && distance_lengths[symbol] == 0
            {
                distance_lengths[symbol] = 1;
            }
        }
        let num_litlen = litlen_lengths
            .iter()
            .rposition(|&l| l > 0)
            .map_or(257, |i| (i + 1).max(257));
        let num_distance = distance_lengths
            .iter()
            .rposition(|&l| l > 0)
            .map_or(1, |i| i + 1);
        litlen_lengths.truncate(num_litlen);
        distance_lengths.truncate(num_distance);
        let combined: Vec<u8> = litlen_lengths
            .iter()
            .chain(&distance_lengths)
            .copied()
            .collect();
        let code_length_tokens = code_length_tokens(&combined);
        let mut counts = [0u32; 19];
        for &(symbol, _) in &code_length_tokens {
            counts[usize::from(symbol)] += 1;
        }
        let code_length_lengths = huffman_lengths(&counts, MAX_CODE_LENGTH_CODE_LENGTH);
        let num_code_lengths = CODE_LENGTH_ORDER
            .iter()
            .rposition(|&s| code_length_lengths[s] > 0)
            .map_or(4, |i| (i + 1).max(4));
        let mut header_bits = 5 + 5 + 4 + 3 * num_code_lengths as u64;
        for &(symbol, _) in &code_length_tokens {
            header_bits += u64::from(code_length_lengths[usize::from(symbol)])
                + match symbol {
                    16 => 2,
                    17 => 3,
                    18 => 7,
                    _ => 0,
                };
        }
        Self {
            litlen_lengths,
            distance_lengths,
            code_length_lengths,
            code_length_tokens,
            num_code_lengths,
            header_bits,
        }
    }

    fn write_header(&self, w: &mut BitWriter) {
        w.put(self.litlen_lengths.len() as u32 - 257, 5);
        w.put(self.distance_lengths.len() as u32 - 1, 5);
        w.put(self.num_code_lengths as u32 - 4, 4);
        for &symbol in &CODE_LENGTH_ORDER[..self.num_code_lengths] {
            w.put(u32::from(self.code_length_lengths[symbol]), 3);
        }
        let codes = canonical_codes(&self.code_length_lengths);
        for &(symbol, extra) in &self.code_length_tokens {
            let s = usize::from(symbol);
            w.put(u32::from(codes[s]), u32::from(self.code_length_lengths[s]));
            match symbol {
                16 => w.put(u32::from(extra), 2),
                17 => w.put(u32::from(extra), 3),
                18 => w.put(u32::from(extra), 7),
                _ => {}
            }
        }
    }
}

/// Bits needed for the symbols of `histogram` with the given code lengths, extra bits included.
fn coded_bits(histogram: &Histogram, litlen_lengths: &[u8], distance_lengths: &[u8]) -> u64 {
    let mut bits = 0u64;
    for (symbol, &count) in histogram.litlen.iter().enumerate() {
        if count > 0 {
            let extra = if symbol > 264 && symbol < 285 {
                (symbol as u64 - 261) / 4
            } else {
                0
            };
            bits += u64::from(count) * (u64::from(litlen_lengths[symbol]) + extra);
        }
    }
    for (symbol, &count) in histogram.distance.iter().enumerate() {
        if count > 0 {
            let extra = (symbol as u64 / 2).saturating_sub(1);
            bits += u64::from(count) * (u64::from(distance_lengths[symbol]) + extra);
        }
    }
    bits
}

fn fixed_lengths() -> (Vec<u8>, Vec<u8>) {
    let litlen = (0..288)
        .map(|s| match s {
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        })
        .collect();
    (litlen, vec![5; 30])
}

/// Exact size in bits of a block of `len` input bytes with the given symbol counts, written as
/// the smallest of a stored, fixed-code or dynamic-code block.
fn block_bits(histogram: &Histogram, len: usize) -> u64 {
    let dynamic = DynamicCodes::new(histogram);
    let dynamic_bits = 3
        + dynamic.header_bits
        + coded_bits(
            histogram,
            &dynamic.litlen_lengths,
            &dynamic.distance_lengths,
        );
    let (fixed_litlen, fixed_distance) = fixed_lengths();
    let fixed_bits = 3 + coded_bits(histogram, &fixed_litlen, &fixed_distance);
    let stored_bits = len.div_ceil(65535).max(1) as u64 * 40 + len as u64 * 8;
    dynamic_bits.min(fixed_bits).min(stored_bits)
}

/// Write `block` as the smallest of a stored, fixed-code or dynamic-code block.
fn write_block(w: &mut BitWriter, data: &[u8], block: &Block, last: bool) {
    let bytes = &data[block.start..block.end];
    let dynamic = DynamicCodes::new(&block.histogram);
    let dynamic_bits = 3
        + dynamic.header_bits
        + coded_bits(
            &block.histogram,
            &dynamic.litlen_lengths,
            &dynamic.distance_lengths,
        );
    let (fixed_litlen, fixed_distance) = fixed_lengths();
    let fixed_bits = 3 + coded_bits(&block.histogram, &fixed_litlen, &fixed_distance);
    let stored_bits = bytes.len().div_ceil(65535).max(1) as u64 * 40 + bytes.len() as u64 * 8;

    if stored_bits <= dynamic_bits.min(fixed_bits) {
        let parts = bytes.len().div_ceil(65535).max(1);
        for (i, part) in bytes.chunks(65535).enumerate() {
            w.put(u32::from(last && i + 1 == parts), 1);
            w.put(0, 2);
            w.align();
            let len = part.len() as u16;
            w.put_bytes(&len.to_le_bytes());
            w.put_bytes(&(!len).to_le_bytes());
            w.put_bytes(part);
        }
        return;
    }
    w.put(u32::from(last), 1);
    let (litlen_lengths, distance_lengths) = if fixed_bits <= dynamic_bits {
        w.put(1, 2);
        (fixed_litlen, fixed_distance)
    } else {
        w.put(2, 2);
        dynamic.write_header(w);
        (dynamic.litlen_lengths, dynamic.distance_lengths)
    };
    let litlen_codes = canonical_codes(&litlen_lengths);
    let distance_codes = canonical_codes(&distance_lengths);
    for token in &block.tokens {
        let length = usize::from(token.length);
        if token.distance == 0 {
            w.put(
                u32::from(litlen_codes[length]),
                u32::from(litlen_lengths[length]),
            );
            continue;
        }
        let (symbol, extra_bits, extra) = length_code(length);
        w.put(
            u32::from(litlen_codes[symbol]) | extra << litlen_lengths[symbol],
            u32::from(litlen_lengths[symbol]) + extra_bits,
        );
        let (symbol, extra_bits, extra) = distance_code(usize::from(token.distance));
        w.put(
            u32::from(distance_codes[symbol]) | extra << distance_lengths[symbol],
            u32::from(distance_lengths[symbol]) + extra_bits,
        );
    }
    w.put(
        u32::from(litlen_codes[END_OF_BLOCK]),
        u32::from(litlen_lengths[END_OF_BLOCK]),
    );
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    fn round_trip(data: &[u8], effort: Effort) -> usize {
        let compressed = zlib_compress(data, 977, effort);
        let mut decoded = Vec::new();
        flate2::read::ZlibDecoder::new(compressed.as_slice())
            .read_to_end(&mut decoded)
            .unwrap();
        assert!(decoded == data, "{} bytes did not round-trip", data.len());
        compressed.len()
    }

    fn noise(len: usize, seed: u64, mask: u8) -> Vec<u8> {
        let mut state = seed | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 24) as u8 & mask
            })
            .collect()
    }

    #[test]
    fn length_and_distance_codes_match_the_deflate_tables() {
        assert_eq!(length_code(3), (257, 0, 0));
        assert_eq!(length_code(10), (264, 0, 0));
        assert_eq!(length_code(11), (265, 1, 0));
        assert_eq!(length_code(12), (265, 1, 1));
        assert_eq!(length_code(19), (269, 2, 0));
        assert_eq!(length_code(130), (280, 4, 15));
        assert_eq!(length_code(257), (284, 5, 30));
        assert_eq!(length_code(258), (285, 0, 0));
        assert_eq!(distance_code(1), (0, 0, 0));
        assert_eq!(distance_code(4), (3, 0, 0));
        assert_eq!(distance_code(5), (4, 1, 0));
        assert_eq!(distance_code(8), (5, 1, 1));
        assert_eq!(distance_code(24577), (29, 13, 0));
        assert_eq!(distance_code(32768), (29, 13, 8191));
    }

    #[test]
    fn every_kind_of_input_round_trips() {
        for effort in [Effort::DEFAULT, Effort::HIGH] {
            round_trip(&[], effort);
            round_trip(&[7], effort);
            round_trip(&[1, 2], effort);
            round_trip(b"abcabcabcabcabcabcabcabcabcabc", effort);
            round_trip(&vec![0u8; 100_000], effort);
            round_trip(&noise(70_000, 3, 0xFF), effort);
            round_trip(&noise(40_000, 5, 0x11), effort);
            let mut periodic = noise(977, 9, 0xFF).repeat(60);
            periodic.extend(noise(5000, 11, 0x0F));
            periodic.extend(vec![0xAA; 3000]);
            round_trip(&periodic, effort);
        }
    }

    #[test]
    fn incompressible_data_is_stored_and_repetitive_data_shrinks() {
        let random = noise(200_000, 21, 0xFF);
        assert!(round_trip(&random, Effort::DEFAULT) < random.len() + 64);
        assert!(round_trip(&vec![5u8; 200_000], Effort::DEFAULT) < 400);
        let periodic = noise(1500, 33, 0xFF).repeat(40);
        assert!(round_trip(&periodic, Effort::DEFAULT) < 2500);
    }

    #[test]
    fn matches_never_reach_beyond_the_window() {
        let mut data = noise(300, 41, 0xFF);
        data.extend(vec![0u8; WINDOW + 500]);
        data.extend(noise(300, 41, 0xFF));
        round_trip(&data, Effort::HIGH);
    }
}
