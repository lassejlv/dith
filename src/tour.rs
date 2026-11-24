//! Pen-up travel optimisation for plotter toolpaths.
//!
//! A greedy nearest-neighbour tour (uniform bucket grid, so each step is a local ring search) is
//! refined with 2-opt restricted to each node's nearest neighbours and driven by a don't-look-bit
//! queue. The tour is a closed cycle through the pen's home position, so it models the plotter
//! leaving home, visiting every dot once, and parking again. Everything is sequential and
//! tie-broken by index, so the order is deterministic.

use std::collections::VecDeque;
use std::time::Duration;

use web_time::Instant;

use crate::par::prelude::*;

/// Neighbour-list size for 2-opt candidate moves.
const NEIGHBOURS: usize = 10;

/// Tuning knobs for [`plan_tour`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TourOptions {
    /// Run 2-opt after the nearest-neighbour pass.
    pub two_opt: bool,
    /// Stop 2-opt after this much wall time (`None` = run until no improving move is left).
    pub time_limit: Option<Duration>,
}

impl Default for TourOptions {
    fn default() -> Self {
        Self {
            two_opt: true,
            time_limit: Some(Duration::from_secs(10)),
        }
    }
}

/// A visiting order plus travel statistics (all lengths are closed: home -> dots -> home).
#[derive(Debug, Clone, PartialEq)]
pub struct Tour {
    /// Indices into the input points in visiting order; every index appears exactly once.
    pub order: Vec<usize>,
    /// Travel when dots are visited in input order.
    pub unordered_length: f64,
    /// Travel after the nearest-neighbour pass.
    pub nearest_length: f64,
    /// Travel after 2-opt (equals `nearest_length` when 2-opt is off).
    pub length: f64,
    /// Improving 2-opt moves applied.
    pub moves: usize,
    /// Whether 2-opt stopped at the time limit instead of a local optimum.
    pub capped: bool,
}

/// Closed travel length of visiting `points` in `order`, starting and ending at `home`.
#[must_use]
pub fn closed_length(points: &[[f64; 2]], home: [f64; 2], order: &[usize]) -> f64 {
    let mut prev = home;
    let mut total = 0.0;
    for &i in order {
        total += dist(prev, points[i]);
        prev = points[i];
    }
    total + dist(prev, home)
}

/// Order `points` to minimise pen-up travel from and back to `home`.
#[must_use]
pub fn plan_tour(points: &[[f64; 2]], home: [f64; 2], opts: &TourOptions) -> Tour {
    let identity: Vec<usize> = (0..points.len()).collect();
    let unordered_length = closed_length(points, home, &identity);
    if points.len() < 2 {
        return Tour {
            order: identity,
            unordered_length,
            nearest_length: unordered_length,
            length: unordered_length,
            moves: 0,
            capped: false,
        };
    }

    // Node 0 is home; node i + 1 is points[i].
    let mut nodes = Vec::with_capacity(points.len() + 1);
    nodes.push(home);
    nodes.extend_from_slice(points);
    let grid = Grid::new(&nodes);

    let mut cycle = nearest_neighbour(&nodes, &grid);
    let nearest_length = cycle_length(&nodes, &cycle);
    let (moves, capped) = if opts.two_opt {
        two_opt(&nodes, &grid, &mut cycle, opts.time_limit)
    } else {
        (0, false)
    };
    let length = cycle_length(&nodes, &cycle);

    let start = cycle.iter().position(|&v| v == 0).unwrap_or(0);
    let order = cycle[start + 1..]
        .iter()
        .chain(&cycle[..start])
        .map(|&v| v as usize - 1)
        .collect();
    Tour {
        order,
        unordered_length,
        nearest_length,
        length,
        moves,
        capped,
    }
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

fn cycle_length(nodes: &[[f64; 2]], cycle: &[u32]) -> f64 {
    let mut total = 0.0;
    for (i, &v) in cycle.iter().enumerate() {
        let next = cycle[(i + 1) % cycle.len()];
        total += dist(nodes[v as usize], nodes[next as usize]);
    }
    total
}

/// Uniform bucket grid over node positions (about two nodes per cell).
struct Grid {
    min: [f64; 2],
    cell: f64,
    cols: usize,
    rows: usize,
    buckets: Vec<Vec<u32>>,
}

impl Grid {
    fn new(nodes: &[[f64; 2]]) -> Self {
        let mut min = [f64::INFINITY; 2];
        let mut max = [f64::NEG_INFINITY; 2];
        for p in nodes {
            for k in 0..2 {
                min[k] = min[k].min(p[k]);
                max[k] = max[k].max(p[k]);
            }
        }
        let w = (max[0] - min[0]).max(1e-9);
        let h = (max[1] - min[1]).max(1e-9);
        let cell = (w * h * 2.0 / nodes.len() as f64).sqrt().max(1e-9);
        let cols = ((w / cell) as usize + 1).min(4096);
        let rows = ((h / cell) as usize + 1).min(4096);
        let cell = (w / cols as f64).max(h / rows as f64).max(1e-9) * 1.000_001;
        let mut grid = Self {
            min,
            cell,
            cols,
            rows,
            buckets: vec![Vec::new(); cols * rows],
        };
        for (i, &p) in nodes.iter().enumerate() {
            let c = grid.cell_index(p);
            grid.buckets[c].push(i as u32);
        }
        grid
    }

    fn coords(&self, p: [f64; 2]) -> (isize, isize) {
        let cx = (((p[0] - self.min[0]) / self.cell) as usize).min(self.cols - 1);
        let cy = (((p[1] - self.min[1]) / self.cell) as usize).min(self.rows - 1);
        (cx as isize, cy as isize)
    }

    fn cell_index(&self, p: [f64; 2]) -> usize {
        let (cx, cy) = self.coords(p);
        cy as usize * self.cols + cx as usize
    }

    /// Visit every cell on Chebyshev ring `ring` around `(gx, gy)`.
    fn ring(&self, gx: isize, gy: isize, ring: isize, mut visit: impl FnMut(usize)) {
        for cy in (gy - ring)..=(gy + ring) {
            if cy < 0 || cy >= self.rows as isize {
                continue;
            }
            let step = if cy == gy - ring || cy == gy + ring {
                1
            } else {
                (2 * ring).max(1)
            };
            let mut cx = gx - ring;
            while cx <= gx + ring {
                if cx >= 0 && cx < self.cols as isize {
                    visit(cy as usize * self.cols + cx as usize);
                }
                cx += step;
            }
        }
    }

    fn max_ring(&self) -> isize {
        self.cols.max(self.rows) as isize
    }

    /// The `NEIGHBOURS` nearest other nodes of `v`, closest first.
    fn neighbours(&self, nodes: &[[f64; 2]], v: usize) -> Vec<u32> {
        let p = nodes[v];
        let (gx, gy) = self.coords(p);
        let k = NEIGHBOURS.min(nodes.len() - 1);
        let mut best: Vec<(f64, u32)> = Vec::with_capacity(k + 1);
        for ring in 0..=self.max_ring() {
            self.ring(gx, gy, ring, |c| {
                for &u in &self.buckets[c] {
                    if u as usize == v {
                        continue;
                    }
                    let d = dist(p, nodes[u as usize]);
                    if best.len() == k && (d, u) >= best[k - 1] {
                        continue;
                    }
                    let at = best.partition_point(|e| *e < (d, u));
                    best.insert(at, (d, u));
                    best.truncate(k);
                }
            });
            let reach = ring as f64 * self.cell;
            if best.len() == k && best[k - 1].0 <= reach {
                break;
            }
        }
        best.into_iter().map(|(_, u)| u).collect()
    }
}

/// Greedy tour from home (node 0), always stepping to the closest unvisited node.
fn nearest_neighbour(nodes: &[[f64; 2]], grid: &Grid) -> Vec<u32> {
    let mut buckets = grid.buckets.clone();
    let home_cell = grid.cell_index(nodes[0]);
    buckets[home_cell].retain(|&v| v != 0);
    let mut cycle = Vec::with_capacity(nodes.len());
    cycle.push(0_u32);
    let mut cur = nodes[0];
    for _ in 1..nodes.len() {
        let (gx, gy) = grid.coords(cur);
        let mut best: Option<(f64, u32, usize, usize)> = None;
        for ring in 0..=grid.max_ring() {
            grid.ring(gx, gy, ring, |c| {
                for (slot, &u) in buckets[c].iter().enumerate() {
                    let d = dist(cur, nodes[u as usize]);
                    if best.is_none_or(|(bd, bu, _, _)| (d, u) < (bd, bu)) {
                        best = Some((d, u, c, slot));
                    }
                }
            });
            let reach = ring as f64 * grid.cell;
            if best.is_some_and(|(bd, ..)| bd <= reach) {
                break;
            }
        }
        let Some((_, u, c, slot)) = best else { break };
        buckets[c].swap_remove(slot);
        cycle.push(u);
        cur = nodes[u as usize];
    }
    cycle
}

/// Neighbour-list 2-opt with don't-look bits. Returns (moves applied, hit time limit).
fn two_opt(
    nodes: &[[f64; 2]],
    grid: &Grid,
    cycle: &mut [u32],
    time_limit: Option<Duration>,
) -> (usize, bool) {
    let m = cycle.len();
    if m < 4 {
        return (0, false);
    }
    let started = Instant::now();
    let neighbours: Vec<Vec<u32>> = (0..m)
        .into_par_iter()
        .map(|v| grid.neighbours(nodes, v))
        .collect();
    let mut pos = vec![0_usize; m];
    for (i, &v) in cycle.iter().enumerate() {
        pos[v as usize] = i;
    }
    let mut queue: VecDeque<u32> = cycle.iter().copied().collect();
    let mut queued = vec![true; m];
    let mut moves = 0;
    let mut popped = 0_usize;
    let d = |a: u32, b: u32| dist(nodes[a as usize], nodes[b as usize]);

    while let Some(a) = queue.pop_front() {
        queued[a as usize] = false;
        popped += 1;
        if popped.is_multiple_of(256)
            && let Some(limit) = time_limit
            && started.elapsed() >= limit
        {
            return (moves, true);
        }
        'improve: loop {
            for forward in [true, false] {
                let pa = pos[a as usize];
                let b = if forward {
                    cycle[(pa + 1) % m]
                } else {
                    cycle[(pa + m - 1) % m]
                };
                let dab = d(a, b);
                for &c in &neighbours[a as usize] {
                    let dac = d(a, c);
                    if dac >= dab {
                        break;
                    }
                    let pc = pos[c as usize];
                    let e = if forward {
                        cycle[(pc + 1) % m]
                    } else {
                        cycle[(pc + m - 1) % m]
                    };
                    if c == b || e == a {
                        continue;
                    }
                    let delta = dac + d(b, e) - dab - d(c, e);
                    if delta < -1e-9 {
                        let (from, to) = if forward {
                            (pos[b as usize], pc)
                        } else {
                            (pa, pos[e as usize])
                        };
                        reverse(cycle, &mut pos, from, to);
                        moves += 1;
                        for v in [a, b, c, e] {
                            if !queued[v as usize] {
                                queued[v as usize] = true;
                                queue.push_back(v);
                            }
                        }
                        continue 'improve;
                    }
                }
            }
            break;
        }
    }
    (moves, false)
}

/// Reverse the cyclic segment from position `i` forward to position `j` (or, equivalently and
/// cheaper, its complement).
fn reverse(cycle: &mut [u32], pos: &mut [usize], mut i: usize, mut j: usize) {
    let m = cycle.len();
    let mut len = (j + m - i) % m + 1;
    if 2 * len > m {
        let (ni, nj) = ((j + 1) % m, (i + m - 1) % m);
        i = ni;
        j = nj;
        len = m - len;
    }
    for _ in 0..len / 2 {
        cycle.swap(i, j);
        pos[cycle[i] as usize] = i;
        pos[cycle[j] as usize] = j;
        i = (i + 1) % m;
        j = (j + m - 1) % m;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::SplitMix64;

    fn random_points(n: usize, seed: u64) -> Vec<[f64; 2]> {
        let mut rng = SplitMix64::new(seed);
        (0..n)
            .map(|_| [rng.next_f64() * 200.0, rng.next_f64() * 280.0])
            .collect()
    }

    fn assert_permutation(order: &[usize], n: usize) {
        let mut seen = vec![false; n];
        for &i in order {
            assert!(!seen[i], "index {i} visited twice");
            seen[i] = true;
        }
        assert_eq!(order.len(), n);
        assert!(seen.iter().all(|&s| s));
    }

    #[test]
    fn tour_visits_every_point_exactly_once_and_shortens_travel() {
        let pts = random_points(3000, 9);
        let tour = plan_tour(&pts, [0.0, 280.0], &TourOptions::default());
        assert_permutation(&tour.order, pts.len());
        assert!(!tour.capped);
        assert!(tour.nearest_length < tour.unordered_length * 0.1);
        assert!(tour.length < tour.nearest_length * 0.95, "{tour:?}");
        let recomputed = closed_length(&pts, [0.0, 280.0], &tour.order);
        assert!((recomputed - tour.length).abs() < 1e-6);
    }

    #[test]
    fn tour_is_deterministic() {
        let pts = random_points(1500, 4);
        let a = plan_tour(&pts, [0.0, 0.0], &TourOptions::default());
        let b = plan_tour(&pts, [0.0, 0.0], &TourOptions::default());
        assert_eq!(a, b);
    }

    #[test]
    fn nearest_neighbour_only_and_tiny_inputs() {
        let pts = random_points(50, 1);
        let opts = TourOptions {
            two_opt: false,
            time_limit: None,
        };
        let tour = plan_tour(&pts, [0.0, 0.0], &opts);
        assert_permutation(&tour.order, 50);
        assert!((tour.length - tour.nearest_length).abs() < 1e-12);
        for n in 0..5 {
            let pts = random_points(n, 2);
            let tour = plan_tour(&pts, [5.0, 5.0], &TourOptions::default());
            assert_permutation(&tour.order, n);
        }
        let dup = vec![[3.0, 3.0]; 20];
        assert_permutation(
            &plan_tour(&dup, [3.0, 3.0], &TourOptions::default()).order,
            20,
        );
    }

    #[test]
    fn two_opt_untangles_a_crossed_square() {
        let pts = vec![[0.0, 10.0], [10.0, 0.0], [10.0, 10.0], [0.0, 0.0]];
        let identity = [0, 1, 2, 3];
        let crossed = closed_length(&pts, [0.0, 0.0], &identity);
        let tour = plan_tour(&pts, [0.0, 0.0], &TourOptions::default());
        assert!(tour.length < crossed);
        assert!((tour.length - 40.0).abs() < 1e-9, "{}", tour.length);
    }
}
