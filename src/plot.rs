//! Pen-plotter output: paper layout, multi-pen ink separation, and plotter SVG / G-code encoders.
//!
//! Page coordinates are millimetres with the origin at the paper's top-left corner and y pointing
//! down (the SVG convention). G-code converts them to machine coordinates with [`Origin`].

use std::fmt::Write as _;
use std::path::Path;
use std::str::FromStr;

use crate::par::prelude::*;
use clap::ValueEnum;
use image::RgbaImage;
use serde::Serialize;

use crate::color::Rgb;
use crate::stipple::DensityMap;

/// Plotter file format written by `dith plot`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlotFormat {
    /// G-code toolpath in millimetres with configurable pen-up/pen-down commands.
    Gcode,
    /// Physical-size SVG (mm units), one Inkscape layer and one path per pen, dots in tour order.
    Svg,
}

impl PlotFormat {
    /// Every plot format in listing order.
    pub const ALL: &'static [Self] = &[Self::Gcode, Self::Svg];

    /// Canonical CLI slug.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Gcode => "gcode",
            Self::Svg => "svg",
        }
    }

    /// One-line description for `list` / `capabilities`.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Gcode => {
                "dith plot: G-code toolpath (mm, G90) with configurable pen-up/down commands, feeds, origin, and a settings header"
            }
            Self::Svg => {
                "dith plot: physical-size plotter SVG in mm, one Inkscape layer and path per pen, dots in tour order"
            }
        }
    }

    /// Infer the format from an output extension (`.svg`, `.gcode`, `.nc`, `.ngc`, `.gc`, `.g`).
    #[must_use]
    pub fn infer_from_path(path: &Path) -> Option<Self> {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("svg") => Some(Self::Svg),
            Some("gcode" | "nc" | "ngc" | "gc" | "g") => Some(Self::Gcode),
            _ => None,
        }
    }
}

/// Named paper sizes as `(id, width_mm, height_mm)` in portrait orientation.
pub const PAPER_SIZES: &[(&str, f64, f64)] = &[
    ("a2", 420.0, 594.0),
    ("a3", 297.0, 420.0),
    ("a4", 210.0, 297.0),
    ("a5", 148.0, 210.0),
    ("a6", 105.0, 148.0),
    ("letter", 215.9, 279.4),
    ("legal", 215.9, 355.6),
    ("tabloid", 279.4, 431.8),
];

/// A sheet of paper in millimetres (as named; orientation is applied by [`plan_layout`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Paper {
    pub name: String,
    pub width_mm: f64,
    pub height_mm: f64,
}

impl FromStr for Paper {
    type Err = String;

    /// Parse a named size (`a4`, `letter`, ...) or `WxH` with an optional `mm`, `cm`, or `in` unit.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let key = s.trim().to_ascii_lowercase();
        if let Some(&(id, w, h)) = PAPER_SIZES.iter().find(|(id, ..)| *id == key) {
            return Ok(Self {
                name: id.to_string(),
                width_mm: w,
                height_mm: h,
            });
        }
        let (body, unit) = if let Some(b) = key.strip_suffix("mm") {
            (b, 1.0)
        } else if let Some(b) = key.strip_suffix("cm") {
            (b, 10.0)
        } else if let Some(b) = key.strip_suffix("in") {
            (b, 25.4)
        } else {
            (key.as_str(), 1.0)
        };
        let invalid = || {
            format!(
                "'{s}' is not a paper size: use {} or WxH[mm|cm|in] like 300x200mm",
                PAPER_SIZES
                    .iter()
                    .map(|(id, ..)| *id)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let (w, h) = body.split_once('x').ok_or_else(invalid)?;
        let w: f64 = w.trim().parse().map_err(|_| invalid())?;
        let h: f64 = h.trim().parse().map_err(|_| invalid())?;
        let (w, h) = (w * unit, h * unit);
        if !(w.is_finite()
            && h.is_finite()
            && (10.0..=5000.0).contains(&w)
            && (10.0..=5000.0).contains(&h))
        {
            return Err(format!(
                "paper '{s}' must be between 10mm and 5000mm per side"
            ));
        }
        Ok(Self {
            name: format!("{}x{}mm", fmt_trim(w, 2), fmt_trim(h, 2)),
            width_mm: w,
            height_mm: h,
        })
    }
}

/// Sheet orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Orientation {
    /// Landscape for wide sources, portrait otherwise.
    #[default]
    Auto,
    Portrait,
    Landscape,
}

/// How the source maps onto the drawable area inside the margins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlotFit {
    /// Fit the whole source inside the drawable area, centred.
    #[default]
    Contain,
    /// Fill the drawable area, centre-cropping the source.
    Cover,
}

/// Where G-code machine `X0 Y0` sits on the sheet (also the pen's home and parking spot).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    /// Bottom-left corner, +Y up (most GRBL/Marlin plotters).
    #[default]
    BottomLeft,
    /// Top-left corner, +Y down (AxiDraw-style, matches SVG).
    TopLeft,
    /// Sheet centre, +Y up.
    Center,
}

impl Origin {
    /// Home position in page coordinates for a sheet of `width` x `height` mm.
    #[must_use]
    pub fn home(self, width: f64, height: f64) -> [f64; 2] {
        match self {
            Self::BottomLeft => [0.0, height],
            Self::TopLeft => [0.0, 0.0],
            Self::Center => [width * 0.5, height * 0.5],
        }
    }

    /// Convert a page point to machine coordinates.
    #[must_use]
    pub fn to_machine(self, p: [f64; 2], width: f64, height: f64) -> [f64; 2] {
        match self {
            Self::BottomLeft => [p[0], height - p[1]],
            Self::TopLeft => p,
            Self::Center => [p[0] - width * 0.5, height * 0.5 - p[1]],
        }
    }
}

/// How each stipple is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DotShape {
    /// Pen down and up in place: a dot the width of the pen (fastest).
    #[default]
    Point,
    /// A small circle whose outer diameter is the dot size.
    Circle,
    /// A short horizontal dash whose length is the dot size.
    Stroke,
}

/// Resolved page geometry.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PageLayout {
    pub width_mm: f64,
    pub height_mm: f64,
    /// Resolved orientation (`portrait` or `landscape`).
    pub orientation: Orientation,
    /// Area inside the margins as `[x, y, w, h]` mm.
    pub drawable_mm: [f64; 4],
    /// Area the source raster maps onto as `[x, y, w, h]` mm.
    pub plot_area_mm: [f64; 4],
}

/// Place a source of aspect `source_aspect` (width / height) on `paper`.
///
/// # Errors
/// Returns a message when the margins leave no drawable area.
pub fn plan_layout(
    paper: &Paper,
    orientation: Orientation,
    margin_mm: f64,
    fit: PlotFit,
    source_aspect: f64,
) -> Result<PageLayout, String> {
    let short = paper.width_mm.min(paper.height_mm);
    let long = paper.width_mm.max(paper.height_mm);
    let resolved = match orientation {
        Orientation::Auto if source_aspect > 1.0 => Orientation::Landscape,
        Orientation::Auto => Orientation::Portrait,
        other => other,
    };
    let (width_mm, height_mm) = if resolved == Orientation::Landscape {
        (long, short)
    } else {
        (short, long)
    };
    let dw = width_mm - 2.0 * margin_mm;
    let dh = height_mm - 2.0 * margin_mm;
    if dw < 1.0 || dh < 1.0 {
        return Err(format!(
            "margin {margin_mm}mm leaves no drawable area on a {width_mm}x{height_mm}mm sheet"
        ));
    }
    let drawable_mm = [margin_mm, margin_mm, dw, dh];
    let plot_area_mm = match fit {
        PlotFit::Cover => drawable_mm,
        PlotFit::Contain => {
            let aspect = source_aspect.max(1e-6);
            let (w, h) = if aspect > dw / dh {
                (dw, dw / aspect)
            } else {
                (dh * aspect, dh)
            };
            [margin_mm + (dw - w) * 0.5, margin_mm + (dh - h) * 0.5, w, h]
        }
    };
    Ok(PageLayout {
        width_mm,
        height_mm,
        orientation: resolved,
        drawable_mm,
        plot_area_mm,
    })
}

/// Raster dimensions for an area of `w` x `h` mm whose longest side spans `resolution` pixels.
#[must_use]
pub fn raster_size(w: f64, h: f64, resolution: u32) -> (u32, u32) {
    let res = f64::from(resolution.max(1));
    if w >= h {
        (resolution.max(1), ((res * h / w).round() as u32).max(1))
    } else {
        (((res * w / h).round() as u32).max(1), resolution.max(1))
    }
}

/// Split an image into one ink density map per pen.
///
/// Each pixel is modelled as paper tinted by exactly one pen: in Oklab it is projected onto every
/// paper-to-pen line, the pen whose tint reproduces it best wins, and the projection (`0` = paper,
/// `1` = solid pen) becomes that pen's density. With a single black pen on white this is `1 - L`.
/// Alpha blends toward paper; densities below `cutoff` count as paper.
#[must_use]
pub fn separate_inks(img: &RgbaImage, paper: Rgb, pens: &[Rgb], cutoff: f32) -> Vec<DensityMap> {
    let w = img.width() as usize;
    let h = img.height() as usize;
    let p0 = paper.to_oklab();
    let axes: Vec<([f32; 3], f32)> = pens
        .iter()
        .map(|pen| {
            let q = pen.to_oklab();
            let dir = [q.l - p0.l, q.a - p0.a, q.b - p0.b];
            let len2 = dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2];
            (dir, len2.max(1e-9))
        })
        .collect();
    let assigned: Vec<(u8, f32)> = img
        .as_raw()
        .par_chunks_exact(4)
        .map(|px| {
            let o = Rgb::new(px[0], px[1], px[2]).to_oklab();
            let v = [o.l - p0.l, o.a - p0.a, o.b - p0.b];
            let mut best = (0_u8, 0.0_f32, f32::INFINITY);
            for (k, (dir, len2)) in axes.iter().enumerate() {
                let t = ((v[0] * dir[0] + v[1] * dir[1] + v[2] * dir[2]) / len2).clamp(0.0, 1.0);
                let err = (v[0] - t * dir[0]).powi(2)
                    + (v[1] - t * dir[1]).powi(2)
                    + (v[2] - t * dir[2]).powi(2);
                if err < best.2 {
                    best = (k as u8, t, err);
                }
            }
            let t = best.1 * f32::from(px[3]) / 255.0;
            (best.0, if t < cutoff { 0.0 } else { t })
        })
        .collect();
    (0..pens.len())
        .map(|k| {
            let data = assigned
                .iter()
                .map(|&(pen, t)| if pen as usize == k { t } else { 0.0 })
                .collect();
            DensityMap::new(w, h, data)
        })
        .collect()
}

/// Add Sobel edge strength to a density map: `d + weight * |∇d| / 4`, clamped to `[0, 1]`.
///
/// Stippling spends dots by area, so thin features (eyelids, lips, hairlines) get few. Edge emphasis
/// puts extra ink along them. Pixels with zero density (paper) stay zero.
#[must_use]
pub fn emphasize_edges(map: &DensityMap, weight: f32) -> DensityMap {
    let w = map.width();
    let h = map.height();
    let d = map.data();
    let weight = f64::from(weight) * 0.25;
    let at = |x: usize, y: usize, dx: isize, dy: isize| {
        let xc = x.saturating_add_signed(dx).min(w - 1);
        let yc = y.saturating_add_signed(dy).min(h - 1);
        f64::from(d[yc * w + xc])
    };
    let mut out = vec![0.0_f32; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, slot) in row.iter_mut().enumerate() {
            let v = f64::from(d[y * w + x]);
            if v <= 0.0 {
                continue;
            }
            let gx = at(x, y, 1, -1) + 2.0 * at(x, y, 1, 0) + at(x, y, 1, 1)
                - at(x, y, -1, -1)
                - 2.0 * at(x, y, -1, 0)
                - at(x, y, -1, 1);
            let gy = at(x, y, -1, 1) + 2.0 * at(x, y, 0, 1) + at(x, y, 1, 1)
                - at(x, y, -1, -1)
                - 2.0 * at(x, y, 0, -1)
                - at(x, y, 1, -1);
            *slot = (v + weight * (gx * gx + gy * gy).sqrt()).min(1.0) as f32;
        }
    });
    DensityMap::new(w, h, out)
}

/// One dot on the page, in page millimetres.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlotDot {
    pub x: f64,
    pub y: f64,
    /// Outer ink diameter in mm (ignored for [`DotShape::Point`]).
    pub diameter: f64,
}

/// All dots for one pen, already in drawing order.
#[derive(Debug, Clone, PartialEq)]
pub struct PenPlot {
    pub color: Rgb,
    pub dots: Vec<PlotDot>,
}

/// Geometry of a dot as actually drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Mark {
    Point,
    Circle(f64),
    Dash(f64),
}

/// Smallest pen motion worth drawing; anything shorter is plotted as a point.
const MIN_MOTION_MM: f64 = 0.05;

fn mark(dot: &PlotDot, shape: DotShape, pen_width: f64) -> Mark {
    let inner = dot.diameter - pen_width;
    match shape {
        DotShape::Circle if inner * 0.5 >= MIN_MOTION_MM => Mark::Circle(inner * 0.5),
        DotShape::Stroke if inner >= MIN_MOTION_MM => Mark::Dash(inner),
        _ => Mark::Point,
    }
}

/// Pen-down drawing length of a dot in mm.
#[must_use]
pub fn draw_length(dot: &PlotDot, shape: DotShape, pen_width: f64) -> f64 {
    match mark(dot, shape, pen_width) {
        Mark::Point => 0.0,
        Mark::Circle(r) => std::f64::consts::TAU * r,
        Mark::Dash(l) => l,
    }
}

/// Everything an encoder needs about one plot.
#[derive(Debug, Clone)]
pub struct PlotDocument<'a> {
    pub layout: &'a PageLayout,
    pub pens: &'a [PenPlot],
    pub shape: DotShape,
    pub pen_width_mm: f64,
    /// Free-form settings lines written as comments at the top of the file.
    pub header: &'a [String],
}

/// Plotter SVG: physical size in mm, one Inkscape layer with one stroked path per pen.
#[must_use]
pub fn encode_plot_svg(doc: &PlotDocument) -> String {
    let w = doc.layout.width_mm;
    let h = doc.layout.height_mm;
    let dots: usize = doc.pens.iter().map(|p| p.dots.len()).sum();
    let mut out = String::with_capacity(256 + dots * 24);
    let _ = writeln!(
        out,
        r#"<?xml version="1.0" encoding="UTF-8" standalone="no"?>"#
    );
    let _ = writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape" version="1.1" width="{}mm" height="{}mm" viewBox="0 0 {} {}">"#,
        fmt_trim(w, 3),
        fmt_trim(h, 3),
        fmt_trim(w, 3),
        fmt_trim(h, 3)
    );
    for line in doc.header {
        let _ = writeln!(out, "<!-- {} -->", line.replace("--", "- -"));
    }
    for (k, pen) in doc.pens.iter().enumerate() {
        let hex = pen.color.to_hex();
        let _ = writeln!(
            out,
            r#"<g inkscape:groupmode="layer" inkscape:label="{} pen {hex}" id="pen-{}">"#,
            k + 1,
            k + 1
        );
        let _ = write!(
            out,
            r#"<path fill="none" stroke="{hex}" stroke-width="{}" stroke-linecap="round" stroke-linejoin="round" d=""#,
            fmt_trim(doc.pen_width_mm, 3)
        );
        for (i, dot) in pen.dots.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            match mark(dot, doc.shape, doc.pen_width_mm) {
                Mark::Point => {
                    let _ = write!(out, "M{} {}h0", fmt_trim(dot.x, 2), fmt_trim(dot.y, 2));
                }
                Mark::Circle(r) => {
                    let rs = fmt_trim(r, 3);
                    let _ = write!(
                        out,
                        "M{} {}a{rs} {rs} 0 1 0 {} 0a{rs} {rs} 0 1 0 {} 0",
                        fmt_trim(dot.x + r, 2),
                        fmt_trim(dot.y, 2),
                        fmt_trim(-2.0 * r, 3),
                        fmt_trim(2.0 * r, 3)
                    );
                }
                Mark::Dash(l) => {
                    let _ = write!(
                        out,
                        "M{} {}h{}",
                        fmt_trim(dot.x - l * 0.5, 2),
                        fmt_trim(dot.y, 2),
                        fmt_trim(l, 3)
                    );
                }
            }
        }
        out.push_str("\"/>\n</g>\n");
    }
    out.push_str("</svg>\n");
    out
}

/// Machine settings for [`encode_gcode`].
#[derive(Debug, Clone, PartialEq)]
pub struct MachineOptions {
    /// Lines that raise the pen (e.g. `M5`, `G0 Z3`).
    pub pen_up: Vec<String>,
    /// Lines that lower the pen (e.g. `M3 S30`, `G1 Z0 F1000`).
    pub pen_down: Vec<String>,
    /// Lines emitted at home before every pen after the first (e.g. `M0` to pause).
    pub pen_change: Vec<String>,
    /// Pause after every pen-up / pen-down, in seconds (`G4 P<s>`; `0` = none).
    pub dwell_s: f64,
    /// Drawing feed rate in mm/min.
    pub draw_feed: f64,
    /// Travel feed rate in mm/min.
    pub travel_feed: f64,
    pub origin: Origin,
}

/// G-code toolpath in absolute millimetres; every pen starts and parks at the machine origin.
#[must_use]
pub fn encode_gcode(doc: &PlotDocument, machine: &MachineOptions) -> String {
    let w = doc.layout.width_mm;
    let h = doc.layout.height_mm;
    let dots: usize = doc.pens.iter().map(|p| p.dots.len()).sum();
    let mut out = String::with_capacity(512 + dots * 64);
    let draw_f = fmt_trim(machine.draw_feed, 1);
    let travel_f = fmt_trim(machine.travel_feed, 1);
    let dwell = (machine.dwell_s > 0.0).then(|| format!("G4 P{}", fmt_trim(machine.dwell_s, 3)));
    let xy = |p: [f64; 2]| {
        let m = machine.origin.to_machine(p, w, h);
        format!("X{} Y{}", fmt_trim(m[0], 3), fmt_trim(m[1], 3))
    };
    let lines = |out: &mut String, cmds: &[String]| {
        for c in cmds {
            out.push_str(c);
            out.push('\n');
        }
        if let Some(d) = &dwell {
            out.push_str(d);
            out.push('\n');
        }
    };

    for line in doc.header {
        let _ = writeln!(out, "; {line}");
    }
    out.push_str("G21 ; millimetres\nG90 ; absolute coordinates\n");
    lines(&mut out, &machine.pen_up);
    for (k, pen) in doc.pens.iter().enumerate() {
        let _ = writeln!(
            out,
            "; pen {}/{} {} ({} dots)",
            k + 1,
            doc.pens.len(),
            pen.color.to_hex(),
            pen.dots.len()
        );
        if k > 0 {
            for c in &machine.pen_change {
                out.push_str(c);
                out.push('\n');
            }
        }
        for dot in &pen.dots {
            let m = mark(dot, doc.shape, doc.pen_width_mm);
            let start = match m {
                Mark::Point => [dot.x, dot.y],
                Mark::Circle(r) => [dot.x + r, dot.y],
                Mark::Dash(l) => [dot.x - l * 0.5, dot.y],
            };
            let _ = writeln!(out, "G0 {} F{travel_f}", xy(start));
            lines(&mut out, &machine.pen_down);
            match m {
                Mark::Point => {}
                Mark::Circle(r) => {
                    let _ = writeln!(out, "G2 {} I{} J0 F{draw_f}", xy(start), fmt_trim(-r, 3));
                }
                Mark::Dash(l) => {
                    let _ = writeln!(out, "G1 {} F{draw_f}", xy([dot.x + l * 0.5, dot.y]));
                }
            }
            lines(&mut out, &machine.pen_up);
        }
        let _ = writeln!(out, "G0 X0 Y0 F{travel_f}");
    }
    out.push_str("M2\n");
    out
}

/// Format `v` with at most `decimals` fractional digits, trimming trailing zeros (`-0` -> `0`).
#[must_use]
pub fn fmt_trim(v: f64, decimals: u32) -> String {
    let scale = 10_i64.pow(decimals);
    let q = (v * scale as f64).round() as i64;
    let mut s = String::new();
    if q < 0 {
        s.push('-');
    }
    let a = q.unsigned_abs();
    let scale = scale.unsigned_abs();
    let _ = write!(s, "{}", a / scale);
    let frac = a % scale;
    if frac != 0 {
        let digits = format!("{frac:0width$}", width = decimals as usize);
        s.push('.');
        s.push_str(digits.trim_end_matches('0'));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paper_sizes_parse() {
        let a4: Paper = "A4".parse().unwrap();
        assert_eq!((a4.width_mm, a4.height_mm), (210.0, 297.0));
        let custom: Paper = "11x17in".parse().unwrap();
        assert!((custom.width_mm - 279.4).abs() < 1e-9);
        assert_eq!(custom.name, "279.4x431.8mm");
        let mm: Paper = "300x200".parse().unwrap();
        assert_eq!(mm.name, "300x200mm");
        assert!("a9".parse::<Paper>().is_err());
        assert!("2x2mm".parse::<Paper>().is_err());
    }

    #[test]
    fn layout_contains_and_orients() {
        let a4: Paper = "a4".parse().unwrap();
        let wide = plan_layout(&a4, Orientation::Auto, 10.0, PlotFit::Contain, 2.0).unwrap();
        assert_eq!(wide.orientation, Orientation::Landscape);
        assert_eq!((wide.width_mm, wide.height_mm), (297.0, 210.0));
        let [x, y, w, h] = wide.plot_area_mm;
        assert!((w / h - 2.0).abs() < 1e-9);
        assert!((w - 277.0).abs() < 1e-9);
        assert!((x - 10.0).abs() < 1e-9 && (y - (210.0 - h) / 2.0).abs() < 1e-9);
        let cover = plan_layout(&a4, Orientation::Portrait, 15.0, PlotFit::Cover, 2.0).unwrap();
        assert_eq!(cover.plot_area_mm, [15.0, 15.0, 180.0, 267.0]);
        assert!(plan_layout(&a4, Orientation::Auto, 120.0, PlotFit::Contain, 1.0).is_err());
        assert_eq!(raster_size(180.0, 267.0, 1200), (809, 1200));
    }

    #[test]
    fn ink_separation_projects_onto_pens() {
        let mut img = RgbaImage::new(3, 1);
        img.put_pixel(0, 0, image::Rgba([255, 255, 255, 255]));
        img.put_pixel(1, 0, image::Rgba([0, 0, 0, 255]));
        img.put_pixel(2, 0, image::Rgba([230, 30, 30, 255]));
        let maps = separate_inks(&img, Rgb::WHITE, &[Rgb::BLACK, Rgb::new(230, 30, 30)], 0.0);
        assert_eq!(maps.len(), 2);
        assert_eq!(maps[0].data(), &[0.0, 1.0, 0.0]);
        assert!(maps[1].data()[2] > 0.99);
        assert_eq!(maps[1].data()[1], 0.0);
    }

    #[test]
    fn edge_emphasis_adds_ink_along_steps_only() {
        let data: Vec<f32> = (0..64).map(|i| if i % 8 < 4 { 0.2 } else { 0.6 }).collect();
        let mut with_paper = data.clone();
        with_paper[0] = 0.0;
        let out = emphasize_edges(&DensityMap::new(8, 8, with_paper), 1.0);
        let d = out.data();
        assert_eq!(d[0], 0.0);
        assert_eq!(d[10], 0.2);
        assert!(d[11] > 0.5 && d[12] > 0.6);
        assert_eq!(d[15], 0.6);
    }

    #[test]
    fn number_formatting_is_compact() {
        assert_eq!(fmt_trim(12.5, 3), "12.5");
        assert_eq!(fmt_trim(-0.0001, 3), "0");
        assert_eq!(fmt_trim(-3.25, 2), "-3.25");
        assert_eq!(fmt_trim(7.0, 2), "7");
        assert_eq!(fmt_trim(0.05, 3), "0.05");
    }
}
