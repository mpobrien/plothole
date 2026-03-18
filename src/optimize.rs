/// The two endpoints of a pen-down path — the only information the optimizer
/// needs; it never looks at the interior points.
pub struct PathEndpoints {
    pub start: (f64, f64),
    pub end: (f64, f64),
}

/// How to traverse one path in the optimized ordering.
pub struct PathOrder {
    /// Index into the original `paths` slice.
    pub index: usize,
    /// When `true`, traverse the path end → start instead of start → end.
    pub reversed: bool,
}

/// Implemented by any algorithm that reorders pen-down paths to minimise
/// total pen-up travel distance.
///
/// `exit_target`: if `Some`, the optimizer additionally minimises the distance
/// from the group's exit point to that target (i.e. the start of the next
/// group), so the pen ends up as close as possible to the next character.
pub trait PathOptimizer {
    fn optimize(&self, paths: &[PathEndpoints], start: (f64, f64), exit_target: Option<(f64, f64)>) -> Vec<PathOrder>;
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn dist(a: (f64, f64), b: (f64, f64)) -> f64 {
    ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt()
}

/// Total pen-up travel distance for a given ordering, starting from `start`.
pub fn penup_distance(paths: &[PathEndpoints], order: &[PathOrder], start: (f64, f64)) -> f64 {
    let mut total = 0.0;
    let mut pen = start;
    for step in order {
        let entry = if step.reversed { paths[step.index].end } else { paths[step.index].start };
        let exit  = if step.reversed { paths[step.index].start } else { paths[step.index].end };
        total += dist(pen, entry);
        pen = exit;
    }
    total
}

// ── NearestNeighbor ──────────────────────────────────────────────────────────

/// Greedy nearest-neighbour: at each step move to the closest unvisited path
/// endpoint, choosing the better entry direction automatically.
///
/// O(n²) time. Typically 15–25 % above optimal, but fast enough for any n.
pub struct NearestNeighbor;

impl PathOptimizer for NearestNeighbor {
    fn optimize(&self, paths: &[PathEndpoints], start: (f64, f64), exit_target: Option<(f64, f64)>) -> Vec<PathOrder> {
        let n = paths.len();
        let mut visited  = vec![false; n];
        let mut result   = Vec::with_capacity(n);
        let mut pen      = start;
        let mut remaining = n;

        for _ in 0..n {
            remaining -= 1;
            let is_last = remaining == 0;

            let mut best_cost = f64::INFINITY;
            let mut best_i    = 0;
            let mut best_rev  = false;

            for (i, p) in paths.iter().enumerate() {
                if visited[i] { continue; }
                // On the last step, include the cost of reaching the exit target
                // from whichever endpoint we exit through.
                let exit_cost = |exit: (f64, f64)| -> f64 {
                    if is_last { exit_target.map_or(0.0, |t| dist(exit, t)) } else { 0.0 }
                };
                let cost_fwd = dist(pen, p.start) + exit_cost(p.end);
                let cost_rev = dist(pen, p.end)   + exit_cost(p.start);
                if cost_fwd < best_cost { best_cost = cost_fwd; best_i = i; best_rev = false; }
                if cost_rev < best_cost { best_cost = cost_rev; best_i = i; best_rev = true;  }
            }

            visited[best_i] = true;
            pen = if best_rev { paths[best_i].start } else { paths[best_i].end };
            result.push(PathOrder { index: best_i, reversed: best_rev });
        }

        result
    }
}

// ── HeldKarp ─────────────────────────────────────────────────────────────────

/// Maximum number of paths HeldKarp will accept.  Beyond this the memory
/// (O(n · 2ⁿ)) becomes impractical: at n = 20 the DP table alone is ~335 MB.
pub const HELD_KARP_LIMIT: usize = 20;

/// Exact Held-Karp dynamic programming.
///
/// Finds the ordering **and per-path direction** that minimises total pen-up
/// travel from `start`.
///
/// - Time:   O(n² · 2ⁿ)
/// - Memory: O(n · 2ⁿ)  (≈ 17 MB at n = 16, ≈ 335 MB at n = 20)
///
/// Panics if `paths.len() > HELD_KARP_LIMIT`.  For larger inputs use
/// [`NearestNeighbor`] (or another heuristic) instead.
pub struct HeldKarp;

impl PathOptimizer for HeldKarp {
    fn optimize(&self, paths: &[PathEndpoints], start: (f64, f64), exit_target: Option<(f64, f64)>) -> Vec<PathOrder> {
        let n = paths.len();
        assert!(
            n <= HELD_KARP_LIMIT,
            "HeldKarp: {n} paths exceeds the limit of {HELD_KARP_LIMIT}. \
             Use NearestNeighbor for larger inputs."
        );

        if n == 0 { return vec![]; }
        if n == 1 {
            let reversed = dist(start, paths[0].end) < dist(start, paths[0].start);
            return vec![PathOrder { index: 0, reversed }];
        }

        let masks     = 1usize << n;
        let full_mask = masks - 1;

        // Flat DP table.
        // Index: mask * n * 2  +  i * 2  +  dir
        //   dir = 0 → traversed forward  (exited at paths[i].end)
        //   dir = 1 → traversed backward (exited at paths[i].start)
        let mut dp = vec![f64::INFINITY; masks * n * 2];

        // Parent pointers for reconstruction.
        // prev_mask is always `mask ^ (1 << i)`, so we only store (prev_i, prev_dir).
        let mut par_i   = vec![0u8; masks * n * 2];
        let mut par_dir = vec![0u8; masks * n * 2];

        let idx = |mask: usize, i: usize, dir: usize| mask * n * 2 + i * 2 + dir;

        // ── base cases ───────────────────────────────────────────────────────
        for i in 0..n {
            dp[idx(1 << i, i, 0)] = dist(start, paths[i].start);
            dp[idx(1 << i, i, 1)] = dist(start, paths[i].end);
        }

        // ── forward pass ─────────────────────────────────────────────────────
        for mask in 1..masks {
            for i in 0..n {
                if mask & (1 << i) == 0 { continue; }
                for dir in 0..2usize {
                    let cost = dp[idx(mask, i, dir)];
                    if cost == f64::INFINITY { continue; }

                    let exit = if dir == 0 { paths[i].end } else { paths[i].start };

                    for j in 0..n {
                        if mask & (1 << j) != 0 { continue; }
                        let new_mask = mask | (1 << j);

                        for new_dir in 0..2usize {
                            let entry    = if new_dir == 0 { paths[j].start } else { paths[j].end };
                            let new_cost = cost + dist(exit, entry);
                            let slot     = idx(new_mask, j, new_dir);
                            if new_cost < dp[slot] {
                                dp[slot]      = new_cost;
                                par_i[slot]   = i as u8;
                                par_dir[slot] = dir as u8;
                            }
                        }
                    }
                }
            }
        }

        // ── find the best terminal state ──────────────────────────────────────
        // Include the cost of reaching the next group's entry (if known), so
        // the optimizer chooses an exit that minimises total travel including
        // the pen-up move to the next character.
        let (mut best_i, mut best_dir) = (0, 0);
        let mut best_cost = f64::INFINITY;
        for i in 0..n {
            for dir in 0..2usize {
                let exit = if dir == 0 { paths[i].end } else { paths[i].start };
                let c = dp[idx(full_mask, i, dir)]
                    + exit_target.map_or(0.0, |t| dist(exit, t));
                if c < best_cost { best_cost = c; best_i = i; best_dir = dir; }
            }
        }

        // ── reconstruct by following parent pointers ──────────────────────────
        let mut result = Vec::with_capacity(n);
        let mut mask   = full_mask;
        let mut i      = best_i;
        let mut dir    = best_dir;

        loop {
            result.push(PathOrder { index: i, reversed: dir == 1 });
            let prev_mask = mask ^ (1 << i);
            if prev_mask == 0 { break; }
            let slot = idx(mask, i, dir);
            let pi   = par_i[slot]   as usize;
            let pd   = par_dir[slot] as usize;
            mask = prev_mask;
            i    = pi;
            dir  = pd;
        }

        result.reverse();
        result
    }
}
