//! Weighted Voronoi stippling (Secord 2002) on a raster density grid.
//!
//! Points are seeded by rejection sampling on the density field, then relaxed with Lloyd's
//! algorithm using density-weighted centroids. Centroids are weighted by `density^1.5`: a weighted
//! centroidal Voronoi tessellation settles at a point density proportional to `weight^(d / (d + 2))`
//! (Gersho), which in 2D is `sqrt(weight)`, so plain density weights flatten local contrast and
//! wash out small features such as eyes. Squaring restores proportionality in the limit but, on a
//! pixel raster at stipple scale, overshoots and carves empty halos beside dark edges; `1.5` keeps
//! the relaxed point density within about 10% of the ink ratio for features a few dots wide. Every iteration assigns each inked pixel to its
//! nearest site in a single parallel raster pass: pixels are processed in small blocks, each block
//! gathers the few sites that can be nearest to any of its pixels from a uniform site grid, and
//! partial centroid sums are merged in a fixed order so results are byte-identical regardless of
//! thread count.

use rayon::prelude::*;

use crate::palette::SplitMix64;

/// Ink density field in `[0.0, 1.0]` per raster pixel (`0.0` = bare paper, `1.0` = solid ink).
#[derive(Debug, Clone)]
pub struct DensityMap {
    width: usize,
    height: usize,
    data: Vec<f32>,
}

impl DensityMap {
    /// Wrap a row-major density buffer; values are clamped to `[0.0, 1.0]`.
    ///
    /// # Panics
    /// Panics if `data.len() != width * height`.
    #[must_use]
    pub fn new(width: usize, height: usize, mut data: Vec<f32>) -> Self {
        assert_eq!(data.len(), width * height, "density buffer size mismatch");
        for v in &mut data {
            *v = if v.is_finite() {
                v.clamp(0.0, 1.0)
            } else {
                0.0
            };
        }
        Self {
            width,
            height,
            data,
        }
    }

    /// Raster width in pixels.
    #[must_use]
    pub const fn width(&self) -> usize {
        self.width
    }

    /// Raster height in pixels.
    #[must_use]
    pub const fn height(&self) -> usize {
        self.height
    }

    /// Row-major density values.
    #[must_use]
    pub fn data(&self) -> &[f32] {
        &self.data
    }

    /// Total ink mass (sum of densities).
    #[must_use]
    pub fn mass(&self) -> f64 {
        self.data.iter().map(|&v| f64::from(v)).sum()
    }

    /// Mean density over the whole raster.
    #[must_use]
    pub fn mean(&self) -> f64 {
        if self.data.is_empty() {
            0.0
        } else {
            self.mass() / self.data.len() as f64
        }
    }
}

/// Tuning knobs for [`stipple`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StippleOptions {
    /// Number of stipple points to place.
    pub points: usize,
    /// Maximum Lloyd relaxation iterations.
    pub max_iterations: u32,
    /// Stop early once the mean site movement in one iteration drops below this (raster pixels).
    pub tolerance: f64,
    /// Seed for the deterministic rejection sampler.
    pub seed: u64,
}

impl Default for StippleOptions {
    fn default() -> Self {
        Self {
            points: 5000,
            max_iterations: 50,
            tolerance: 0.02,
            seed: 42,
        }
    }
}

/// One relaxed stipple in raster pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stipple {
    pub x: f64,
    pub y: f64,
    /// Mean ink density over the stipple's Voronoi cell in `[0.0, 1.0]` (drives variable dot size).
    pub darkness: f32,
}

/// Result of [`stipple`].
#[derive(Debug, Clone)]
pub struct StippleResult {
    pub stipples: Vec<Stipple>,
    /// Lloyd iterations actually run.
    pub iterations: u32,
    /// Whether the mean movement fell below the tolerance before `max_iterations`.
    pub converged: bool,
    /// Mean site movement during the last iteration, in raster pixels.
    pub final_movement: f64,
    /// Sites dropped because their Voronoi cell ended up with no ink.
    pub dropped: usize,
}

/// Errors from [`stipple`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StippleError {
    #[error("the image has no ink to stipple (every pixel is paper-colored)")]
    Blank,
    #[error("rejection sampling could not place {requested} points ({placed} placed)")]
    Sampling { requested: usize, placed: usize },
}

/// Place `opts.points` stipples on `density` with weighted Voronoi stippling.
///
/// # Errors
/// Returns [`StippleError::Blank`] when the density map has no ink.
pub fn stipple(density: &DensityMap, opts: &StippleOptions) -> Result<StippleResult, StippleError> {
    let n = opts.points;
    if n == 0 {
        return Ok(StippleResult {
            stipples: Vec::new(),
            iterations: 0,
            converged: true,
            final_movement: 0.0,
            dropped: 0,
        });
    }
    let max_density = density.data.iter().copied().fold(0.0_f32, f32::max);
    let mass = density.mass();
    if max_density <= 0.0 || mass <= 0.0 {
        return Err(StippleError::Blank);
    }

    let mut sites = seed_sites(density, n, max_density, opts.seed)?;
    let raster = Raster::new(density, n, mass, f64::from(max_density));

    let mut iterations = 0;
    let mut converged = false;
    let mut final_movement = 0.0;
    let mut sums = vec![[0.0_f64; 3]; n];
    while iterations < opts.max_iterations {
        iterations += 1;
        raster.accumulate(&sites, &mut sums, false);
        let mut moved = 0.0;
        for (site, acc) in sites.iter_mut().zip(&sums) {
            if acc[0] > 0.0 {
                let nx = acc[1] / acc[0];
                let ny = acc[2] / acc[0];
                moved += ((nx - site[0]).powi(2) + (ny - site[1]).powi(2)).sqrt();
                *site = [nx, ny];
            }
        }
        final_movement = moved / n as f64;
        if final_movement < opts.tolerance {
            converged = true;
            break;
        }
    }

    raster.accumulate(&sites, &mut sums, true);
    let mut stipples = Vec::with_capacity(n);
    for (site, acc) in sites.iter().zip(&sums) {
        if acc[0] > 0.0 && acc[1] > 0.0 {
            stipples.push(Stipple {
                x: site[0],
                y: site[1],
                darkness: (acc[0] / acc[1]).clamp(0.0, 1.0) as f32,
            });
        }
    }
    let dropped = n - stipples.len();
    Ok(StippleResult {
        stipples,
        iterations,
        converged,
        final_movement,
        dropped,
    })
}

/// Deterministic rejection sampling: accept a uniform candidate with probability `density / max`.
fn seed_sites(
    density: &DensityMap,
    n: usize,
    max_density: f32,
    seed: u64,
) -> Result<Vec<[f64; 2]>, StippleError> {
    let w = density.width as f64;
    let h = density.height as f64;
    let expected = n as f64 * f64::from(max_density) / density.mean().max(1e-12);
    let limit = (expected * 16.0 + 10_000.0).min(4.0e9) as u64;
    let mut rng = SplitMix64::new(seed ^ 0xD1B5_4A32_D192_ED03);
    let mut sites = Vec::with_capacity(n);
    let mut attempts = 0_u64;
    while sites.len() < n {
        if attempts >= limit {
            return Err(StippleError::Sampling {
                requested: n,
                placed: sites.len(),
            });
        }
        attempts += 1;
        let x = rng.next_f64() * w;
        let y = rng.next_f64() * h;
        let px = (x as usize).min(density.width - 1);
        let py = (y as usize).min(density.height - 1);
        let d = density.data[py * density.width + px];
        if rng.next_f32() * max_density < d {
            sites.push([x, y]);
        }
    }
    Ok(sites)
}

/// Static per-raster acceleration data shared by every Lloyd iteration.
struct Raster<'a> {
    density: &'a DensityMap,
    block: usize,
    blocks_x: usize,
    blocks_y: usize,
    /// Whether each block holds any ink (blank blocks are skipped while relaxing).
    inked: Vec<bool>,
    cell: f64,
}

impl<'a> Raster<'a> {
    fn new(density: &'a DensityMap, n: usize, mass: f64, max_density: f64) -> Self {
        // Site spacing in the darkest regions: every site owns roughly `mass / n` of ink.
        let spacing = (mass / (n as f64 * max_density)).sqrt();
        let block = (spacing.round() as usize).clamp(2, 16);
        let blocks_x = density.width.div_ceil(block);
        let blocks_y = density.height.div_ceil(block);
        let mut inked = vec![false; blocks_x * blocks_y];
        for y in 0..density.height {
            let row = &density.data[y * density.width..(y + 1) * density.width];
            for (x, &d) in row.iter().enumerate() {
                if d > 0.0 {
                    inked[(y / block) * blocks_x + x / block] = true;
                }
            }
        }
        let area = (density.width * density.height) as f64;
        let cell = (spacing * 1.5).max((area / 4.0e6).sqrt()).max(1.0);
        Self {
            density,
            block,
            blocks_x,
            blocks_y,
            inked,
            cell,
        }
    }

    /// Assign every pixel to its nearest site and accumulate per-site sums into `out`.
    ///
    /// Relaxing (`stats == false`): `[w, w * x, w * y]` with `w = density^1.5` over inked pixels only.
    /// Final stats (`stats == true`): `[mass, pixel_count, 0]` over every pixel.
    fn accumulate(&self, sites: &[[f64; 2]], out: &mut [[f64; 3]], stats: bool) {
        let grid = SiteGrid::new(sites, self.density.width, self.density.height, self.cell);
        let rows: Vec<Vec<(u32, [f64; 3])>> = (0..self.blocks_y)
            .into_par_iter()
            .map(|by| self.block_row(by, &grid, stats))
            .collect();
        out.fill([0.0; 3]);
        for row in rows {
            for (idx, acc) in row {
                let slot = &mut out[idx as usize];
                slot[0] += acc[0];
                slot[1] += acc[1];
                slot[2] += acc[2];
            }
        }
    }

    fn block_row(&self, by: usize, grid: &SiteGrid, stats: bool) -> Vec<(u32, [f64; 3])> {
        let w = self.density.width;
        let h = self.density.height;
        let mut out = Vec::new();
        let mut candidates: Vec<(f64, u32)> = Vec::new();
        let mut local: Vec<[f64; 3]> = Vec::new();
        for bx in 0..self.blocks_x {
            if !stats && !self.inked[by * self.blocks_x + bx] {
                continue;
            }
            let x0 = bx * self.block;
            let y0 = by * self.block;
            let x1 = (x0 + self.block).min(w);
            let y1 = (y0 + self.block).min(h);
            let cx = (x0 + x1) as f64 * 0.5;
            let cy = (y0 + y1) as f64 * 0.5;
            let half_diag = 0.5 * (((x1 - x0).pow(2) + (y1 - y0).pow(2)) as f64).sqrt();

            // Any site nearest to a pixel in the block lies within d0 + 2 * half_diag of the centre.
            let d0 = grid.nearest_distance(cx, cy);
            grid.within(cx, cy, d0 + 2.0 * half_diag, &mut candidates);
            candidates.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            local.clear();
            local.resize(candidates.len(), [0.0; 3]);

            for y in y0..y1 {
                let py = y as f64 + 0.5;
                let row = &self.density.data[y * w..(y + 1) * w];
                for (x, &d) in row.iter().enumerate().take(x1).skip(x0) {
                    if d <= 0.0 && !stats {
                        continue;
                    }
                    let px = x as f64 + 0.5;
                    let mut best = f64::INFINITY;
                    let mut best_i = 0;
                    for (i, &(dc, idx)) in candidates.iter().enumerate() {
                        let lower = dc - half_diag;
                        if lower > 0.0 && lower * lower > best {
                            break;
                        }
                        let s = grid.sites[idx as usize];
                        let dist = (s[0] - px).powi(2) + (s[1] - py).powi(2);
                        if dist < best {
                            best = dist;
                            best_i = i;
                        }
                    }
                    let m = f64::from(d);
                    let acc = &mut local[best_i];
                    if stats {
                        acc[0] += m;
                        acc[1] += 1.0;
                    } else {
                        let wgt = m * m.sqrt();
                        acc[0] += wgt;
                        acc[1] += wgt * px;
                        acc[2] += wgt * py;
                    }
                }
            }
            for (&(_, idx), acc) in candidates.iter().zip(&local) {
                if acc[0] > 0.0 || acc[1] > 0.0 {
                    out.push((idx, *acc));
                }
            }
        }
        out
    }
}

/// Uniform bucket grid over site positions for nearest / radius queries.
struct SiteGrid<'a> {
    sites: &'a [[f64; 2]],
    cell: f64,
    cols: usize,
    rows: usize,
    starts: Vec<u32>,
    items: Vec<u32>,
}

impl<'a> SiteGrid<'a> {
    fn new(sites: &'a [[f64; 2]], width: usize, height: usize, cell: f64) -> Self {
        let cols = ((width as f64 / cell).ceil() as usize).max(1);
        let rows = ((height as f64 / cell).ceil() as usize).max(1);
        let cell_of = |s: &[f64; 2]| {
            let cx = ((s[0] / cell).max(0.0) as usize).min(cols - 1);
            let cy = ((s[1] / cell).max(0.0) as usize).min(rows - 1);
            cy * cols + cx
        };
        let mut starts = vec![0_u32; cols * rows + 1];
        for s in sites {
            starts[cell_of(s) + 1] += 1;
        }
        for i in 1..starts.len() {
            starts[i] += starts[i - 1];
        }
        let mut fill = starts.clone();
        let mut items = vec![0_u32; sites.len()];
        for (i, s) in sites.iter().enumerate() {
            let c = cell_of(s);
            items[fill[c] as usize] = i as u32;
            fill[c] += 1;
        }
        Self {
            sites,
            cell,
            cols,
            rows,
            starts,
            items,
        }
    }

    fn bucket(&self, cx: usize, cy: usize) -> &[u32] {
        let c = cy * self.cols + cx;
        &self.items[self.starts[c] as usize..self.starts[c + 1] as usize]
    }

    /// Distance from `(x, y)` to the nearest site, by expanding rings of grid cells.
    fn nearest_distance(&self, x: f64, y: f64) -> f64 {
        let gx = ((x / self.cell) as usize).min(self.cols - 1) as isize;
        let gy = ((y / self.cell) as usize).min(self.rows - 1) as isize;
        let max_ring = self.cols.max(self.rows) as isize;
        let mut best = f64::INFINITY;
        for ring in 0..=max_ring {
            for cy in (gy - ring)..=(gy + ring) {
                if cy < 0 || cy >= self.rows as isize {
                    continue;
                }
                let on_edge = cy == gy - ring || cy == gy + ring;
                let step = if on_edge { 1 } else { (2 * ring).max(1) };
                let mut cx = gx - ring;
                while cx <= gx + ring {
                    if cx >= 0 && cx < self.cols as isize {
                        for &i in self.bucket(cx as usize, cy as usize) {
                            let s = self.sites[i as usize];
                            let d = (s[0] - x).powi(2) + (s[1] - y).powi(2);
                            best = best.min(d);
                        }
                    }
                    cx += step;
                }
            }
            let reach = ring as f64 * self.cell;
            if best.is_finite() && best <= reach * reach {
                break;
            }
        }
        best.sqrt()
    }

    /// Collect `(distance, index)` of every site within `radius` of `(x, y)`.
    fn within(&self, x: f64, y: f64, radius: f64, out: &mut Vec<(f64, u32)>) {
        out.clear();
        let r2 = radius * radius;
        let lo_x = (((x - radius) / self.cell).floor().max(0.0) as usize).min(self.cols - 1);
        let hi_x = (((x + radius) / self.cell).floor().max(0.0) as usize).min(self.cols - 1);
        let lo_y = (((y - radius) / self.cell).floor().max(0.0) as usize).min(self.rows - 1);
        let hi_y = (((y + radius) / self.cell).floor().max(0.0) as usize).min(self.rows - 1);
        for cy in lo_y..=hi_y {
            for cx in lo_x..=hi_x {
                for &i in self.bucket(cx, cy) {
                    let s = self.sites[i as usize];
                    let d = (s[0] - x).powi(2) + (s[1] - y).powi(2);
                    if d <= r2 {
                        out.push((d.sqrt(), i));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: usize, h: usize) -> DensityMap {
        let data = (0..w * h)
            .map(|i| ((i % w) as f32 + 0.5) / w as f32)
            .collect();
        DensityMap::new(w, h, data)
    }

    fn brute_force_nearest(sites: &[[f64; 2]], x: f64, y: f64) -> f64 {
        sites
            .iter()
            .map(|s| (s[0] - x).powi(2) + (s[1] - y).powi(2))
            .fold(f64::INFINITY, f64::min)
    }

    #[test]
    fn stippling_is_deterministic_and_places_requested_points() {
        let density = gradient(160, 120);
        let opts = StippleOptions {
            points: 700,
            max_iterations: 20,
            tolerance: 0.0,
            seed: 7,
        };
        let a = stipple(&density, &opts).unwrap();
        let b = stipple(&density, &opts).unwrap();
        assert_eq!(a.stipples, b.stipples);
        assert_eq!(a.stipples.len() + a.dropped, 700);
        assert_eq!(a.dropped, 0);
        assert_eq!(a.iterations, 20);
        let other = stipple(&density, &StippleOptions { seed: 8, ..opts }).unwrap();
        assert_ne!(a.stipples, other.stipples);
    }

    #[test]
    fn results_do_not_depend_on_thread_count() {
        let density = gradient(200, 90);
        let opts = StippleOptions {
            points: 900,
            max_iterations: 8,
            tolerance: 0.0,
            seed: 3,
        };
        let single = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap()
            .install(|| stipple(&density, &opts).unwrap());
        let many = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap()
            .install(|| stipple(&density, &opts).unwrap());
        assert_eq!(single.stipples, many.stipples);
    }

    #[test]
    fn lloyd_relaxation_converges_on_a_gradient() {
        let density = gradient(128, 128);
        let opts = StippleOptions {
            points: 400,
            max_iterations: 200,
            tolerance: 0.01,
            seed: 11,
        };
        let res = stipple(&density, &opts).unwrap();
        assert!(res.converged, "movement {}", res.final_movement);
        assert!(res.iterations < 200);

        // Point density follows ink density: the dark right half holds ~3x the light left half.
        let right = res.stipples.iter().filter(|s| s.x >= 64.0).count() as f64;
        let left = res.stipples.len() as f64 - right;
        let ratio = right / left;
        assert!((2.4..3.6).contains(&ratio), "right/left ratio {ratio}");

        // Mean cell darkness tracks the gradient value at each site.
        for s in &res.stipples {
            assert!((f64::from(s.darkness) - s.x / 128.0).abs() < 0.12, "{s:?}");
        }

        // A relaxed site sits at the weighted centroid of its own cell: one more pass barely moves it.
        let sites: Vec<[f64; 2]> = res.stipples.iter().map(|s| [s.x, s.y]).collect();
        let raster = Raster::new(&density, sites.len(), density.mass(), 1.0);
        let mut sums = vec![[0.0; 3]; sites.len()];
        raster.accumulate(&sites, &mut sums, false);
        let mean_shift = sites
            .iter()
            .zip(&sums)
            .map(|(s, a)| ((a[1] / a[0] - s[0]).powi(2) + (a[2] / a[0] - s[1]).powi(2)).sqrt())
            .sum::<f64>()
            / sites.len() as f64;
        assert!(mean_shift < 0.02, "mean centroid shift {mean_shift}");
    }

    #[test]
    fn relaxed_point_density_stays_proportional_to_ink() {
        // Stripes of ink 0.25 / 1.0, 16 px wide (about three dot spacings). Ink says 4:1; plain
        // density weights relax to about 3:1 (flattening toward sqrt, 2:1), squared ones to 5.5:1.
        let (w, h) = (128, 128);
        let data = (0..w * h)
            .map(|i| if (i % w) / 16 % 2 == 0 { 0.25 } else { 1.0 })
            .collect();
        let density = DensityMap::new(w, h, data);
        let opts = StippleOptions {
            points: 800,
            max_iterations: 100,
            tolerance: 0.0,
            seed: 2,
        };
        let res = stipple(&density, &opts).unwrap();
        let dark = res
            .stipples
            .iter()
            .filter(|s| (s.x as usize) / 16 % 2 == 1)
            .count() as f64;
        let light = res.stipples.len() as f64 - dark;
        let ratio = dark / light;
        assert!((3.6..5.0).contains(&ratio), "dark/light ratio {ratio}");
    }

    #[test]
    fn raster_pass_matches_brute_force_assignment() {
        let (w, h) = (97, 61);
        let mut rng = SplitMix64::new(21);
        let data: Vec<f32> = (0..w * h)
            .map(|i| if i % 7 == 0 { 0.0 } else { rng.next_f32() })
            .collect();
        let density = DensityMap::new(w, h, data);
        let sites: Vec<[f64; 2]> = (0..150)
            .map(|_| [rng.next_f64() * w as f64, rng.next_f64() * h as f64])
            .collect();
        let raster = Raster::new(&density, sites.len(), density.mass(), 1.0);
        let mut fast = vec![[0.0; 3]; sites.len()];
        raster.accumulate(&sites, &mut fast, false);
        let mut slow = vec![[0.0_f64; 3]; sites.len()];
        for y in 0..h {
            for x in 0..w {
                let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                let best = (0..sites.len())
                    .min_by(|&a, &b| {
                        let da = (sites[a][0] - px).powi(2) + (sites[a][1] - py).powi(2);
                        let db = (sites[b][0] - px).powi(2) + (sites[b][1] - py).powi(2);
                        da.total_cmp(&db)
                    })
                    .unwrap();
                let d = f64::from(density.data[y * w + x]);
                let m = d * d.sqrt();
                slow[best][0] += m;
                slow[best][1] += m * px;
                slow[best][2] += m * py;
            }
        }
        for (f, s) in fast.iter().zip(&slow) {
            for k in 0..3 {
                assert!(
                    (f[k] - s[k]).abs() < 1e-6 * (1.0 + s[k].abs()),
                    "{f:?} vs {s:?}"
                );
            }
        }
    }

    #[test]
    fn grid_queries_match_brute_force() {
        let mut rng = SplitMix64::new(5);
        let sites: Vec<[f64; 2]> = (0..300)
            .map(|_| [rng.next_f64() * 90.0, rng.next_f64() * 40.0])
            .collect();
        let grid = SiteGrid::new(&sites, 90, 40, 3.7);
        for _ in 0..500 {
            let x = rng.next_f64() * 90.0;
            let y = rng.next_f64() * 40.0;
            let expected = brute_force_nearest(&sites, x, y).sqrt();
            assert!((grid.nearest_distance(x, y) - expected).abs() < 1e-12);
        }
    }

    #[test]
    fn blank_density_is_rejected() {
        let density = DensityMap::new(8, 8, vec![0.0; 64]);
        assert_eq!(
            stipple(&density, &StippleOptions::default()).unwrap_err(),
            StippleError::Blank
        );
    }
}
