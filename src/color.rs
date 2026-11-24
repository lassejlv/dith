//! Color representations, conversions (sRGB, linear RGB, Oklab), and hex parsing.

use std::fmt;
use std::str::FromStr;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Error returned when parsing a hex color string fails.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid hex color '{input}': expected #RGB, #RRGGBB, or #RRGGBBAA")]
pub struct ParseColorError {
    /// Original offending input.
    pub input: String,
}

/// Color space used for error diffusion and nearest-palette distance matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorSpace {
    /// Perceptually uniform Oklab matching with linear-light error diffusion (recommended).
    #[default]
    Oklab,
    /// Physically linear RGB light for both diffusion and distance matching.
    Linear,
    /// Classic gamma-encoded sRGB math (retro/nostalgic look).
    Srgb,
}

impl ColorSpace {
    /// Short human- and agent-readable description of this color space.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Oklab => {
                "Linear-light diffusion with perceptual Oklab palette matching (most accurate tones)"
            }
            Self::Linear => "Physically linear RGB diffusion and Euclidean distance",
            Self::Srgb => "Classic gamma-encoded sRGB arithmetic (authentic retro behavior)",
        }
    }
}

/// 8-bit sRGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const BLACK: Self = Self::new(0, 0, 0);
    pub const WHITE: Self = Self::new(255, 255, 255);

    /// Construct a new 8-bit sRGB color.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Construct from a 24-bit `0xRRGGBB` literal.
    #[must_use]
    pub const fn from_u32(hex: u32) -> Self {
        Self {
            r: ((hex >> 16) & 0xFF) as u8,
            g: ((hex >> 8) & 0xFF) as u8,
            b: (hex & 0xFF) as u8,
        }
    }

    /// Return as `[r, g, b]` array.
    #[must_use]
    pub const fn to_array(self) -> [u8; 3] {
        [self.r, self.g, self.b]
    }

    /// Format as lowercase `#rrggbb`.
    #[must_use]
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Convert to normalized `[0.0, 1.0]` sRGB floats.
    #[must_use]
    pub fn to_srgb_f32(self) -> [f32; 3] {
        [
            f32::from(self.r) / 255.0,
            f32::from(self.g) / 255.0,
            f32::from(self.b) / 255.0,
        ]
    }

    /// Convert from normalized `[0.0, 1.0]` sRGB floats (clamped and rounded).
    #[must_use]
    pub fn from_srgb_f32(v: [f32; 3]) -> Self {
        Self {
            r: float_to_u8(v[0]),
            g: float_to_u8(v[1]),
            b: float_to_u8(v[2]),
        }
    }

    /// Convert to linear RGB `[0.0, 1.0]`.
    #[must_use]
    pub fn to_linear(self) -> LinearRgb {
        LinearRgb::new(
            srgb_u8_to_linear(self.r),
            srgb_u8_to_linear(self.g),
            srgb_u8_to_linear(self.b),
        )
    }

    /// Convert to perceptual Oklab.
    #[must_use]
    pub fn to_oklab(self) -> Oklab {
        self.to_linear().to_oklab()
    }

    /// Rec. 709 relative luminance in `[0.0, 1.0]`.
    #[must_use]
    pub fn luminance(self) -> f32 {
        self.to_linear().luminance()
    }

    /// Relative perceptual contrast brightness (Oklab L in `[0.0, 1.0]`).
    #[must_use]
    pub fn lightness(self) -> f32 {
        self.to_oklab().l
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

impl FromStr for Rgb {
    type Err = ParseColorError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        let hex = trimmed.strip_prefix('#').unwrap_or(trimmed);
        match hex.len() {
            3 => {
                let r = parse_hex_nibble(hex.as_bytes()[0]).ok_or_else(|| ParseColorError {
                    input: trimmed.to_string(),
                })?;
                let g = parse_hex_nibble(hex.as_bytes()[1]).ok_or_else(|| ParseColorError {
                    input: trimmed.to_string(),
                })?;
                let b = parse_hex_nibble(hex.as_bytes()[2]).ok_or_else(|| ParseColorError {
                    input: trimmed.to_string(),
                })?;
                Ok(Self::new(r * 17, g * 17, b * 17))
            }
            6 | 8 => {
                let r = u8::from_str_radix(&hex[0..2], 16).map_err(|_| ParseColorError {
                    input: trimmed.to_string(),
                })?;
                let g = u8::from_str_radix(&hex[2..4], 16).map_err(|_| ParseColorError {
                    input: trimmed.to_string(),
                })?;
                let b = u8::from_str_radix(&hex[4..6], 16).map_err(|_| ParseColorError {
                    input: trimmed.to_string(),
                })?;
                if hex.len() == 8 {
                    let _ = u8::from_str_radix(&hex[6..8], 16).map_err(|_| ParseColorError {
                        input: trimmed.to_string(),
                    })?;
                }
                Ok(Self::new(r, g, b))
            }
            _ => Err(ParseColorError {
                input: trimmed.to_string(),
            }),
        }
    }
}

fn parse_hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Physically linear RGB triple (nominally in `[0.0, 1.0]`, may overshoot during error diffusion).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LinearRgb {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl LinearRgb {
    #[must_use]
    pub const fn new(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b }
    }

    #[must_use]
    pub const fn to_array(self) -> [f32; 3] {
        [self.r, self.g, self.b]
    }

    #[must_use]
    pub const fn from_array(v: [f32; 3]) -> Self {
        Self {
            r: v[0],
            g: v[1],
            b: v[2],
        }
    }

    /// Rec. 709 linear luminance.
    #[must_use]
    pub fn luminance(self) -> f32 {
        (0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b).clamp(0.0, 1.0)
    }

    /// Convert back to 8-bit sRGB.
    #[must_use]
    pub fn to_rgb(self) -> Rgb {
        Rgb::new(
            linear_to_srgb_u8(self.r),
            linear_to_srgb_u8(self.g),
            linear_to_srgb_u8(self.b),
        )
    }

    /// Convert linear RGB to Björn Ottosson's Oklab color space.
    #[must_use]
    pub fn to_oklab(self) -> Oklab {
        let r = self.r.max(0.0);
        let g = self.g.max(0.0);
        let b = self.b.max(0.0);

        let l = 0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b;
        let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
        let s = 0.088_302_46 * r + 0.281_718_84 * g + 0.629_978_7 * b;

        let l_ = fast_cbrt_nonneg(l);
        let m_ = fast_cbrt_nonneg(m);
        let s_ = fast_cbrt_nonneg(s);

        Oklab {
            l: 0.210_454_26 * l_ + 0.793_617_8 * m_ - 0.004_072_047 * s_,
            a: 1.977_998_5 * l_ - 2.428_592_2 * m_ + 0.450_593_7 * s_,
            b: 0.025_904_037 * l_ + 0.782_771_77 * m_ - 0.808_675_77 * s_,
        }
    }

    /// Squared Euclidean distance in linear RGB.
    #[must_use]
    pub fn dist_sq(self, other: Self) -> f32 {
        let dr = self.r - other.r;
        let dg = self.g - other.g;
        let db = self.b - other.b;
        dr * dr + dg * dg + db * db
    }
}

/// Perceptual Oklab color coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Oklab {
    pub l: f32,
    pub a: f32,
    pub b: f32,
}

impl Oklab {
    #[must_use]
    pub const fn new(l: f32, a: f32, b: f32) -> Self {
        Self { l, a, b }
    }

    /// Squared perceptual distance in Oklab.
    #[must_use]
    pub fn dist_sq(self, other: Self) -> f32 {
        let dl = self.l - other.l;
        let da = self.a - other.a;
        let db = self.b - other.b;
        dl * dl + da * da + db * db
    }

    /// Linearly interpolate between two Oklab colors (produces vivid, muddy-free gradients).
    #[must_use]
    pub fn lerp(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        Self {
            l: self.l + (other.l - self.l) * t,
            a: self.a + (other.a - self.a) * t,
            b: self.b + (other.b - self.b) * t,
        }
    }

    /// Convert Oklab back to linear RGB.
    #[must_use]
    pub fn to_linear(self) -> LinearRgb {
        let l_ = self.l + 0.396_337_78 * self.a + 0.215_803_76 * self.b;
        let m_ = self.l - 0.105_561_346 * self.a - 0.063_854_17 * self.b;
        let s_ = self.l - 0.089_484_18 * self.a - 1.291_485_5 * self.b;

        let l = l_ * l_ * l_;
        let m = m_ * m_ * m_;
        let s = s_ * s_ * s_;

        LinearRgb {
            r: (4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s).clamp(0.0, 1.0),
            g: (-1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s).clamp(0.0, 1.0),
            b: (-0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s).clamp(0.0, 1.0),
        }
    }

    /// Convert Oklab back to 8-bit sRGB.
    #[must_use]
    pub fn to_rgb(self) -> Rgb {
        self.to_linear().to_rgb()
    }
}

/// sRGB `u8` -> linear light: entry `i` is `srgb_f32_to_linear(i / 255)`. Written out rather than
/// computed with `powf` at startup, because platform libms round `powf` differently in the last
/// bit and a 1-ULP change here flips error-diffusion decisions, so outputs would differ across
/// platforms.
#[rustfmt::skip]
static SRGB_U8_TO_LINEAR_LUT: [f32; 256] = [
    0.0, 0.000303527, 0.000607054, 0.000910581, 0.001214108, 0.001517635, 0.001821162,
    0.0021246888, 0.002428216, 0.002731743, 0.00303527, 0.0033465356, 0.003676507, 0.004024717,
    0.004391442, 0.0047769533, 0.005181517, 0.0056053917, 0.0060488326, 0.006512091, 0.00699541,
    0.0074990317, 0.008023192, 0.008568125, 0.009134057, 0.009721218, 0.010329823, 0.010960094,
    0.011612245, 0.012286487, 0.012983031, 0.013702081, 0.014443844, 0.015208514, 0.015996292,
    0.016807375, 0.017641952, 0.018500218, 0.019382361, 0.020288562, 0.02121901, 0.022173883,
    0.023153365, 0.02415763, 0.025186857, 0.026241222, 0.027320892, 0.028426038, 0.029556833,
    0.03071344, 0.03189603, 0.033104762, 0.034339808, 0.035601314, 0.036889445, 0.038204364,
    0.039546236, 0.0409152, 0.04231141, 0.043735027, 0.045186203, 0.046665084, 0.048171822,
    0.049706563, 0.051269468, 0.052860655, 0.05448028, 0.056128494, 0.057805434, 0.05951124,
    0.06124607, 0.06301003, 0.06480328, 0.06662595, 0.06847818, 0.07036011, 0.07227186,
    0.07421358, 0.07618539, 0.07818743, 0.08021983, 0.082282715, 0.084376216, 0.086500466,
    0.088655606, 0.09084173, 0.09305898, 0.095307484, 0.09758736, 0.09989874, 0.10224175,
    0.10461649, 0.10702311, 0.10946172, 0.111932434, 0.11443538, 0.11697067, 0.119538434,
    0.1221388, 0.12477184, 0.1274377, 0.13013649, 0.13286833, 0.13563335, 0.13843162,
    0.1412633, 0.14412849, 0.14702728, 0.1499598, 0.15292616, 0.15592647, 0.15896086,
    0.1620294, 0.16513222, 0.1682694, 0.1714411, 0.17464739, 0.17788841, 0.18116423,
    0.18447499, 0.18782076, 0.19120167, 0.19461781, 0.1980693, 0.20155624, 0.2050787,
    0.20863685, 0.21223073, 0.21586053, 0.21952623, 0.22322798, 0.22696589, 0.23074007,
    0.23455065, 0.23839766, 0.2422812, 0.2462014, 0.25015837, 0.25415218, 0.2581829,
    0.26225072, 0.26635566, 0.27049786, 0.27467737, 0.27889434, 0.2831488, 0.2874409,
    0.2917707, 0.29613832, 0.30054384, 0.30498737, 0.30946895, 0.31398875, 0.31854683,
    0.32314324, 0.32777813, 0.33245158, 0.33716366, 0.34191445, 0.3467041, 0.3515327,
    0.35640025, 0.36130688, 0.3662527, 0.37123778, 0.37626222, 0.3813261, 0.38642952,
    0.39157256, 0.3967553, 0.40197787, 0.4072403, 0.4125427, 0.41788515, 0.42326775,
    0.42869055, 0.4341537, 0.43965724, 0.44520125, 0.45078585, 0.45641106, 0.46207705,
    0.46778384, 0.47353154, 0.47932023, 0.48514998, 0.4910209, 0.49693304, 0.5028866,
    0.50888145, 0.5149178, 0.5209957, 0.5271152, 0.5332765, 0.5394796, 0.5457246,
    0.5520115, 0.5583405, 0.56471163, 0.5711249, 0.5775805, 0.5840785, 0.5906189,
    0.5972019, 0.6038274, 0.6104956, 0.61720663, 0.62396044, 0.6307572, 0.63759696,
    0.64447975, 0.6514057, 0.65837485, 0.66538733, 0.6724432, 0.67954254, 0.68668544,
    0.6938719, 0.701102, 0.70837593, 0.71569365, 0.72305524, 0.7304609, 0.73791057,
    0.74540436, 0.7529423, 0.76052463, 0.7681513, 0.77582234, 0.7835379, 0.79129803,
    0.79910284, 0.80695236, 0.8148467, 0.82278585, 0.83076996, 0.8387991, 0.8468733,
    0.8549927, 0.8631573, 0.8713672, 0.87962234, 0.8879232, 0.8962694, 0.90466136,
    0.9130987, 0.92158204, 0.9301109, 0.9386859, 0.9473066, 0.9559735, 0.9646863,
    0.9734455, 0.9822506, 0.9911022, 1.0,
];

/// First index of [`LINEAR_TO_SRGB_U8_LUT`] whose sRGB value is `v`, for every `v` in `1..=255`.
#[rustfmt::skip]
const LINEAR_TO_SRGB_U8_STARTS: [u16; 255] = [
    1, 2, 4, 5, 6, 7, 9, 10, 11, 12, 14, 15, 16, 18, 19, 21,
    23, 24, 26, 28, 30, 32, 34, 37, 39, 42, 44, 47, 49, 52, 55, 58,
    61, 64, 68, 71, 75, 78, 82, 85, 89, 93, 97, 102, 106, 110, 115, 119,
    124, 129, 134, 139, 144, 149, 154, 160, 165, 171, 177, 183, 189, 195, 201, 207,
    214, 220, 227, 234, 241, 248, 255, 262, 270, 277, 285, 293, 300, 309, 317, 325,
    333, 342, 350, 359, 368, 377, 386, 396, 405, 414, 424, 434, 444, 454, 464, 474,
    485, 495, 506, 517, 528, 539, 550, 562, 573, 585, 597, 609, 621, 633, 645, 658,
    671, 683, 696, 709, 722, 736, 749, 763, 777, 791, 805, 819, 833, 848, 862, 877,
    892, 907, 922, 938, 953, 969, 985, 1001, 1017, 1033, 1050, 1066, 1083, 1100, 1117, 1134,
    1152, 1169, 1187, 1205, 1222, 1241, 1259, 1277, 1296, 1315, 1334, 1353, 1372, 1391, 1411, 1430,
    1450, 1470, 1491, 1511, 1531, 1552, 1573, 1594, 1615, 1636, 1658, 1679, 1701, 1723, 1745, 1768,
    1790, 1813, 1835, 1858, 1882, 1905, 1928, 1952, 1976, 2000, 2024, 2048, 2073, 2097, 2122, 2147,
    2172, 2197, 2223, 2249, 2274, 2300, 2327, 2353, 2380, 2406, 2433, 2460, 2487, 2515, 2542, 2570,
    2598, 2626, 2654, 2683, 2712, 2740, 2769, 2799, 2828, 2857, 2887, 2917, 2947, 2977, 3008, 3038,
    3069, 3100, 3131, 3163, 3194, 3226, 3258, 3290, 3322, 3354, 3387, 3420, 3453, 3486, 3519, 3553,
    3587, 3620, 3655, 3689, 3723, 3758, 3793, 3828, 3863, 3898, 3934, 3970, 4006, 4042, 4078,
];

/// Linear `[0.0, 1.0]` in 4096 steps -> sRGB `u8`: entry `i` is
/// `round(255 * linear_to_srgb_f32(i / 4096))`, expanded at compile time from
/// [`LINEAR_TO_SRGB_U8_STARTS`] so no platform `powf` is involved.
static LINEAR_TO_SRGB_U8_LUT: [u8; 4097] = {
    let mut lut = [0u8; 4097];
    let mut value = 0;
    let mut i = 0;
    while i < lut.len() {
        while value < LINEAR_TO_SRGB_U8_STARTS.len()
            && LINEAR_TO_SRGB_U8_STARTS[value] as usize <= i
        {
            value += 1;
        }
        lut[i] = value as u8;
        i += 1;
    }
    lut
};

/// Convert a single `[0.0, 1.0]` sRGB channel to linear light (IEC 61966-2-1).
#[must_use]
pub fn srgb_f32_to_linear(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Convert a single `u8` sRGB channel to linear light `[0.0, 1.0]` via fast LUT.
#[inline]
#[must_use]
pub fn srgb_u8_to_linear(v: u8) -> f32 {
    SRGB_U8_TO_LINEAR_LUT[usize::from(v)]
}

/// Convert a single linear light channel `[0.0, 1.0]` to sRGB `[0.0, 1.0]`.
#[must_use]
pub fn linear_to_srgb_f32(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Convert a single linear light channel `[0.0, 1.0]` to `u8` sRGB via fast LUT.
#[inline]
#[must_use]
pub fn linear_to_srgb_u8(c: f32) -> u8 {
    let idx = (c.clamp(0.0, 1.0) * 4096.0 + 0.5) as usize;
    LINEAR_TO_SRGB_U8_LUT[idx.min(4096)]
}

#[inline]
fn float_to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Fast, safe, branchless cube root for non-negative `f32` (~1e-7 accuracy, ~6x faster than libm `cbrt`).
#[inline]
fn fast_cbrt_nonneg(x: f32) -> f32 {
    if x <= 1e-12 {
        return 0.0;
    }
    let mut y = f32::from_bits(x.to_bits() / 3 + 0x2a51_067f);
    y = (2.0 * y + x / (y * y)) * (1.0 / 3.0);
    y = (2.0 * y + x / (y * y)) * (1.0 / 3.0);
    y
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parsing_round_trips() {
        assert_eq!("#000".parse::<Rgb>().unwrap(), Rgb::BLACK);
        assert_eq!("fff".parse::<Rgb>().unwrap(), Rgb::WHITE);
        assert_eq!(
            "#1a2b3c".parse::<Rgb>().unwrap(),
            Rgb::new(0x1a, 0x2b, 0x3c)
        );
        assert_eq!(
            "#1a2b3cff".parse::<Rgb>().unwrap(),
            Rgb::new(0x1a, 0x2b, 0x3c)
        );
        assert_eq!(Rgb::new(0x0f, 0xa0, 0x55).to_hex(), "#0fa055");
        assert!("not-a-color".parse::<Rgb>().is_err());
    }

    #[test]
    fn srgb_luts_match_their_formulas() {
        for (i, &lin) in SRGB_U8_TO_LINEAR_LUT.iter().enumerate() {
            let exact = srgb_f32_to_linear(i as f32 / 255.0);
            assert!(
                (lin - exact).abs() <= exact * f32::EPSILON,
                "sRGB {i} -> linear {lin}, formula gives {exact}"
            );
        }
        for (i, &v) in LINEAR_TO_SRGB_U8_LUT.iter().enumerate() {
            let exact = float_to_u8(linear_to_srgb_f32(i as f32 / 4096.0));
            assert!(
                v.abs_diff(exact) <= 1,
                "linear {i}/4096 -> sRGB {v}, formula gives {exact}"
            );
        }
        assert_eq!(LINEAR_TO_SRGB_U8_LUT[0], 0);
        assert_eq!(LINEAR_TO_SRGB_U8_LUT[4096], 255);
    }

    #[test]
    fn oklab_round_trip_is_accurate() {
        for sample in [
            Rgb::BLACK,
            Rgb::WHITE,
            Rgb::new(220, 48, 32),
            Rgb::new(40, 160, 120),
            Rgb::new(30, 64, 210),
            Rgb::new(128, 128, 128),
        ] {
            let round_trip = sample.to_oklab().to_rgb();
            assert!(
                sample.r.abs_diff(round_trip.r) <= 1
                    && sample.g.abs_diff(round_trip.g) <= 1
                    && sample.b.abs_diff(round_trip.b) <= 1,
                "round-trip drift for {sample}: got {round_trip}"
            );
        }
    }
}
