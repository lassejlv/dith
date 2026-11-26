//! Bit output, canonical prefix codes and fixed-point entropy estimates shared by the DEFLATE and
//! VP8L encoders. Everything here is integer arithmetic, so results are identical on every
//! platform.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// Fractional bits of the fixed-point costs returned by [`log2_q8`] and friends.
pub(crate) const COST_SHIFT: u32 = 8;
/// One bit in fixed-point cost units.
pub(crate) const ONE_BIT: u32 = 1 << COST_SHIFT;

/// `round(log2(1 + i / 256) * 256)` for `i` in `0..=256`.
const LOG2_FRACTION: [u16; 257] = log2_fraction_table();

const fn log2_fraction_table() -> [u16; 257] {
    let mut table = [0u16; 257];
    let mut i = 1;
    while i < 256 {
        let mut x: u64 = (256 + i as u64) << 22;
        let mut result: u64 = 0;
        let mut bit = 0;
        while bit < 16 {
            x = (x * x) >> 30;
            result <<= 1;
            if x >= 2 << 30 {
                x >>= 1;
                result |= 1;
            }
            bit += 1;
        }
        table[i] = ((result + 128) >> 8) as u16;
        i += 1;
    }
    table[256] = 256;
    table
}

/// `log2(value)` in 1/256 bit units (0 for a zero `value`).
pub(crate) fn log2_q8(value: u64) -> u32 {
    if value < 2 {
        return 0;
    }
    let high = value.ilog2();
    let mantissa = if high >= 8 {
        value >> (high - 8)
    } else {
        value << (8 - high)
    };
    (high << COST_SHIFT) + u32::from(LOG2_FRACTION[(mantissa - 256) as usize])
}

/// Shannon entropy of `histogram` in 1/256 bit units: the cost of coding every counted symbol
/// with an ideal code.
pub(crate) fn entropy_q8(histogram: &[u32]) -> u64 {
    let mut total = 0u64;
    let mut weighted = 0u64;
    for &count in histogram {
        if count > 0 {
            total += u64::from(count);
            weighted += u64::from(count) * u64::from(log2_q8(u64::from(count)));
        }
    }
    (total * u64::from(log2_q8(total))).saturating_sub(weighted)
}

/// Estimated cost of each symbol in 1/256 bit units from its frequency, clamped to
/// `min_cost..=max_cost`; unused symbols cost one bit more than a symbol seen once.
pub(crate) fn symbol_costs_q8(histogram: &[u32], min_cost: u32, max_cost: u32) -> Vec<u32> {
    let total: u64 = histogram.iter().map(|&c| u64::from(c)).sum();
    let log_total = log2_q8(total.max(1));
    histogram
        .iter()
        .map(|&count| {
            let cost = if count == 0 {
                log_total + ONE_BIT
            } else {
                log_total.saturating_sub(log2_q8(u64::from(count)))
            };
            cost.clamp(min_cost, max_cost)
        })
        .collect()
}

/// Least-significant-bit-first bit writer.
#[derive(Debug, Default)]
pub(crate) struct BitWriter {
    bytes: Vec<u8>,
    pending: u64,
    pending_bits: u32,
}

impl BitWriter {
    pub(crate) fn with_capacity(bytes: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(bytes),
            pending: 0,
            pending_bits: 0,
        }
    }

    pub(crate) fn put(&mut self, value: u32, bits: u32) {
        debug_assert!(bits <= 32 && u64::from(value) < 1u64 << bits);
        self.pending |= u64::from(value) << self.pending_bits;
        self.pending_bits += bits;
        if self.pending_bits >= 32 {
            self.bytes
                .extend_from_slice(&(self.pending as u32).to_le_bytes());
            self.pending >>= 32;
            self.pending_bits -= 32;
        }
    }

    /// Pad with zero bits to the next byte boundary.
    pub(crate) fn align(&mut self) {
        while self.pending_bits >= 8 {
            self.bytes.push(self.pending as u8);
            self.pending >>= 8;
            self.pending_bits -= 8;
        }
        if self.pending_bits > 0 {
            self.bytes.push(self.pending as u8);
            self.pending = 0;
            self.pending_bits = 0;
        }
    }

    /// Append whole bytes; the writer must be byte aligned.
    pub(crate) fn put_bytes(&mut self, bytes: &[u8]) {
        debug_assert_eq!(self.pending_bits, 0);
        self.bytes.extend_from_slice(bytes);
    }

    pub(crate) fn finish(mut self) -> Vec<u8> {
        self.align();
        self.bytes
    }
}

/// Huffman code lengths for `histogram`, limited to `max_length` bits.
///
/// Fewer than two used symbols yield a single symbol of length 1 (symbol 0 when nothing is used).
/// Over-long codes are fixed by raising the smallest counts until the tree is shallow enough.
pub(crate) fn huffman_lengths(histogram: &[u32], max_length: u32) -> Vec<u8> {
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

/// Canonical codes for `lengths` (at most 15 bits each), bit-reversed so they can be written
/// least significant bit first.
pub(crate) fn canonical_codes(lengths: &[u8]) -> Vec<u16> {
    let mut count = [0u16; 16];
    for &length in lengths {
        count[usize::from(length)] += 1;
    }
    count[0] = 0;
    let mut next = [0u16; 16];
    for length in 1..16 {
        next[length] = (next[length - 1] + count[length - 1]) << 1;
    }
    lengths
        .iter()
        .map(|&length| {
            if length == 0 {
                return 0;
            }
            let canonical = next[usize::from(length)];
            next[usize::from(length)] += 1;
            canonical.reverse_bits() >> (16 - length)
        })
        .collect()
}

/// Run-length tokens `(symbol, extra bits)` for a list of code lengths: 0..=15 literal, 16 repeats
/// the previous length 3..=6 times, 17 and 18 emit 3..=10 and 11..=138 zeros.
pub(crate) fn code_length_tokens(lengths: &[u8]) -> Vec<(u8, u8)> {
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn fixed_point_log2_tracks_the_real_logarithm() {
        assert_eq!(log2_q8(0), 0);
        assert_eq!(log2_q8(1), 0);
        assert_eq!(log2_q8(2), 256);
        assert_eq!(log2_q8(1 << 40), 40 * 256);
        for value in [3u64, 5, 7, 100, 255, 257, 1000, 65_537, 123_456_789] {
            let exact = (value as f64).log2() * 256.0;
            let got = f64::from(log2_q8(value));
            assert!((exact - got).abs() < 2.5, "log2({value}): {got} vs {exact}");
        }
    }

    #[test]
    fn entropy_of_a_uniform_histogram_is_log2_of_its_size() {
        assert_eq!(entropy_q8(&[4, 4, 4, 4]), 16 * 2 * 256);
        assert_eq!(entropy_q8(&[9, 0, 0]), 0);
    }

    #[test]
    fn canonical_codes_are_prefix_free() {
        let lengths = [2u8, 1, 3, 3, 0];
        let codes = canonical_codes(&lengths);
        assert_eq!(codes, [0b01, 0b0, 0b011, 0b111, 0]);
    }

    #[test]
    fn bit_writer_packs_least_significant_bits_first() {
        let mut w = BitWriter::default();
        w.put(0b1, 1);
        w.put(0b10, 2);
        w.put(0xABCD, 16);
        w.put(0x1234_5678, 32);
        w.align();
        w.put_bytes(&[0xEE]);
        let bytes = w.finish();
        let mut value = 0u128;
        for (i, &b) in bytes.iter().enumerate() {
            value |= u128::from(b) << (8 * i);
        }
        assert_eq!(value & 1, 1);
        assert_eq!((value >> 1) & 0b11, 0b10);
        assert_eq!((value >> 3) & 0xFFFF, 0xABCD);
        assert_eq!((value >> 19) & 0xFFFF_FFFF, 0x1234_5678);
        assert_eq!(bytes.len(), 8);
        assert_eq!(bytes[7], 0xEE);
    }
}
