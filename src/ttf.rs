//! TTF font → single-stroke path pipeline.
//!
//! For each glyph the TTF Bézier contours are sampled into dense polylines,
//! then converted to a centerline using one of two strategies:
//!
//! • **1 contour** (I, T, S, C, …)
//!   *Fold-point algorithm*: find the two points farthest apart on the closed
//!   loop (the "fold points" at each end of the stroke), split there into two
//!   halves, and average corresponding resampled points → open path.
//!
//! • **2+ contours** (O, D, P, B, …)
//!   *Midpoint-loop algorithm*: pair the outer and inner contours, align their
//!   parametrisations (trying both forward and reversed inner) and average →
//!   closed loop path.
//!
//! Both results are simplified with Douglas-Peucker before returning.

use ttf_parser::{Face, GlyphId, OutlineBuilder};

use crate::font::{Path, Vec2d};

// ── Public API ─────────────────────────────────────────────────────────────────

pub struct TtfFont {
    data:       Vec<u8>,
    face_index: u32,
}

impl TtfFont {
    pub fn from_file(path: &str, face_index: u32) -> Result<Self, Box<dyn std::error::Error>> {
        let data = std::fs::read(path)?;
        Face::parse(&data, face_index)
            .map_err(|e| format!("invalid TTF face {face_index}: {e}"))?;
        Ok(Self { data, face_index })
    }

    pub fn from_bytes(data: &[u8], face_index: u32) -> Result<Self, Box<dyn std::error::Error>> {
        let data = data.to_vec();
        Face::parse(&data, face_index)
            .map_err(|e| format!("invalid TTF face {face_index}: {e}"))?;
        Ok(Self { data, face_index })
    }

    /// Returns one `Vec<Path<f64>>` per character in cursor-offset coordinates.
    ///
    /// `em_size` controls the output scale: 1 em = `em_size` output units.
    /// `dp_epsilon` is the Douglas-Peucker tolerance in those same units.
    /// `axes` are variable-font axis overrides, e.g. `[("wght", 700.0)]`.
    pub fn text_to_paths(
        &self,
        text:       &str,
        em_size:    f32,
        dp_epsilon: f64,
        axes:       &[(String, f32)],
    ) -> Vec<Vec<Path<f64>>> {
        let mut face = Face::parse(&self.data, self.face_index).unwrap();
        for (tag, value) in axes {
            let bytes = tag.as_bytes();
            if bytes.len() == 4 {
                let t = ttf_parser::Tag::from_bytes(bytes.try_into().unwrap());
                face.set_variation(t, *value);
            }
        }
        let scale = em_size as f64 / face.units_per_em() as f64;
        let line_height =
            (face.ascender() as f64 - face.descender() as f64) * scale * 1.2;

        let mut result   = vec![];
        let mut cursor_x = 0.0f64;
        let mut line_y   = 0.0f64;

        for ch in text.chars() {
            if ch == '\n' {
                cursor_x = 0.0;
                line_y  += line_height;
                continue;
            }
            let Some(glyph_id) = face.glyph_index(ch) else {
                result.push(vec![]);
                continue;
            };
            let advance = face.glyph_hor_advance(glyph_id).unwrap_or(0) as f64 * scale;
            result.push(extract_paths(&face, glyph_id, scale, cursor_x, line_y, dp_epsilon));
            cursor_x += advance;
        }
        result
    }
}

// ── Contour sampler ────────────────────────────────────────────────────────────

/// Walks ttf-parser outline commands, sampling every Bézier segment into
/// a dense polyline.  Coordinates are transformed to output space:
///   out_x = cursor_x + font_x * scale
///   out_y = line_y   - font_y * scale   (Y-flipped; baseline at 0, up = −y)
struct ContourSampler {
    contours: Vec<Vec<(f64, f64)>>,
    current:  Vec<(f64, f64)>,
    scale:    f64,
    cursor_x: f64,
    line_y:   f64,
    cur:      (f64, f64), // current pen in font units
}

impl ContourSampler {
    fn new(scale: f64, cursor_x: f64, line_y: f64) -> Self {
        Self { contours: vec![], current: vec![], scale, cursor_x, line_y, cur: (0.0, 0.0) }
    }

    fn out(&self, fx: f64, fy: f64) -> (f64, f64) {
        (self.cursor_x + fx * self.scale, self.line_y - fy * self.scale)
    }

    fn emit(&mut self, fx: f64, fy: f64) {
        let p = self.out(fx, fy);
        self.current.push(p);
    }
}

impl OutlineBuilder for ContourSampler {
    fn move_to(&mut self, x: f32, y: f32) {
        if !self.current.is_empty() {
            self.contours.push(std::mem::take(&mut self.current));
        }
        self.cur = (x as f64, y as f64);
        self.emit(self.cur.0, self.cur.1);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.cur = (x as f64, y as f64);
        self.emit(self.cur.0, self.cur.1);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (p0, p1, p2) = (self.cur, (x1 as f64, y1 as f64), (x as f64, y as f64));
        for i in 1..=10u32 {
            let t = i as f64 / 10.0;
            let u = 1.0 - t;
            self.emit(u*u*p0.0 + 2.0*u*t*p1.0 + t*t*p2.0,
                      u*u*p0.1 + 2.0*u*t*p1.1 + t*t*p2.1);
        }
        self.cur = p2;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (p0, p1, p2, p3) = (self.cur,
            (x1 as f64, y1 as f64), (x2 as f64, y2 as f64), (x as f64, y as f64));
        for i in 1..=12u32 {
            let t = i as f64 / 12.0;
            let u = 1.0 - t;
            let (u2, t2) = (u*u, t*t);
            self.emit(u2*u*p0.0 + 3.0*u2*t*p1.0 + 3.0*u*t2*p2.0 + t2*t*p3.0,
                      u2*u*p0.1 + 3.0*u2*t*p1.1 + 3.0*u*t2*p2.1 + t2*t*p3.1);
        }
        self.cur = p3;
    }

    fn close(&mut self) {
        if !self.current.is_empty() {
            self.contours.push(std::mem::take(&mut self.current));
        }
    }
}

// ── Geometry primitives ────────────────────────────────────────────────────────

#[inline]
fn dist2(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    dx*dx + dy*dy
}

/// Signed area of a polygon (shoelace formula).  Sign depends on orientation.
/// We only use the absolute value for ranking outer vs inner contours.
fn signed_area(pts: &[(f64, f64)]) -> f64 {
    let n = pts.len();
    if n < 3 { return 0.0; }
    (0..n).map(|i| {
        let j = (i + 1) % n;
        pts[i].0 * pts[j].1 - pts[j].0 * pts[i].1
    }).sum::<f64>() * 0.5
}

/// Resample `pts` to exactly `n` uniformly arc-length-spaced points.
fn resample(pts: &[(f64, f64)], n: usize) -> Vec<(f64, f64)> {
    if pts.len() < 2 || n < 2 { return pts.to_vec(); }
    let mut cum = vec![0.0f64];
    for w in pts.windows(2) {
        cum.push(cum.last().unwrap() + dist2(w[0], w[1]).sqrt());
    }
    let total = *cum.last().unwrap();
    if total < 1e-10 { return vec![pts[0]; n]; }
    let mut out = Vec::with_capacity(n);
    let mut j = 0usize;
    for i in 0..n {
        let target = total * i as f64 / (n - 1) as f64;
        while j + 1 < cum.len() - 1 && cum[j + 1] <= target { j += 1; }
        let seg = cum[j + 1] - cum[j];
        let t   = if seg < 1e-10 { 0.0 } else { (target - cum[j]) / seg };
        out.push((pts[j].0 + t * (pts[j+1].0 - pts[j].0),
                  pts[j].1 + t * (pts[j+1].1 - pts[j].1)));
    }
    out
}

// ── Fold-point centerline ──────────────────────────────────────────────────────
//
// For a single closed contour representing an elongated stroke (e.g. 'I'):
//
//   fold A ──── right side ────▶ fold B
//      ▲                             │
//      │ left side (reversed)        │
//      └─────────────────────────────┘
//
// 1. Find the two farthest-apart points on the contour (two-pass O(n) heuristic).
// 2. Split at those indices into half A and half B.
// 3. Reverse half B so both halves run A → B.
// 4. Resample to equal length and average → centerline open path.

fn fold_centerline(pts: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let n = pts.len();
    if n < 4 { return pts.to_vec(); }

    // Pass 1: farthest from pts[0]
    let bi = (1..n).max_by(|&i, &j| {
        dist2(pts[0], pts[i]).partial_cmp(&dist2(pts[0], pts[j])).unwrap()
    }).unwrap_or(n / 2);

    // Pass 2: farthest from pts[bi]
    let bj = (0..n).filter(|&j| j != bi)
        .max_by(|&i, &j| {
            dist2(pts[bi], pts[i]).partial_cmp(&dist2(pts[bi], pts[j])).unwrap()
        }).unwrap_or(0);

    let (bi, bj) = if bi < bj { (bi, bj) } else { (bj, bi) };

    // Half A: pts[bi ..= bj]
    let half_a: Vec<_> = pts[bi..=bj].to_vec();

    // Half B: pts[bj ..] + pts[.. =bi], reversed → also runs bi → bj
    let mut half_b: Vec<_> = pts[bj..].iter()
        .chain(pts[..=bi].iter())
        .cloned()
        .collect();
    half_b.reverse();

    let m  = half_a.len().max(half_b.len()).max(2);
    let ra = resample(&half_a, m);
    let rb = resample(&half_b, m);

    ra.iter().zip(rb.iter())
        .map(|(&a, &b)| ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5))
        .collect()
}

// ── Midpoint loop ──────────────────────────────────────────────────────────────
//
// For a glyph with an outer contour and an inner counter (O, D, P, …):
//
// Both contours are resampled to N points.  The inner contour's rotational
// offset (and whether to reverse it) is found by minimising total squared
// distance to the outer.  Then each pair of corresponding points is averaged.

fn midpoint_loop(outer: &[(f64, f64)], inner: &[(f64, f64)]) -> Vec<(f64, f64)> {
    const N: usize = 256;
    let outer_r   = resample(outer, N);
    let inner_r   = resample(inner, N);
    let inner_rev: Vec<_> = inner_r.iter().rev().cloned().collect();

    // Return (best_offset, total_cost) for aligning `candidate` to outer_r.
    let best_fit = |candidate: &[(f64, f64)]| -> (usize, f64) {
        (0..N)
            .map(|k| {
                let cost: f64 = outer_r.iter().enumerate()
                    .map(|(i, &o)| dist2(o, candidate[(i + k) % N]))
                    .sum();
                (k, cost)
            })
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .unwrap()
    };

    let (off_fwd, cost_fwd) = best_fit(&inner_r);
    let (off_rev, cost_rev) = best_fit(&inner_rev);

    let (inner_use, best_off) = if cost_fwd <= cost_rev {
        (inner_r.as_slice(),   off_fwd)
    } else {
        (inner_rev.as_slice(), off_rev)
    };

    outer_r.iter().enumerate()
        .map(|(i, &o)| {
            let inn = inner_use[(i + best_off) % N];
            ((o.0 + inn.0) * 0.5, (o.1 + inn.1) * 0.5)
        })
        .collect()
}

// ── Douglas-Peucker simplification ────────────────────────────────────────────

fn seg_dist(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len_sq = dx*dx + dy*dy;
    if len_sq < 1e-12 { return dist2(p, a).sqrt(); }
    let t    = ((p.0 - a.0)*dx + (p.1 - a.1)*dy) / len_sq;
    let proj = (a.0 + t.clamp(0.0, 1.0)*dx, a.1 + t.clamp(0.0, 1.0)*dy);
    dist2(p, proj).sqrt()
}

fn douglas_peucker(pts: &[(f64, f64)], eps: f64) -> Vec<(f64, f64)> {
    if pts.len() < 3 { return pts.to_vec(); }
    let (first, last) = (pts[0], *pts.last().unwrap());
    let (mut max_d, mut max_i) = (0.0f64, 0usize);
    for (i, &p) in pts[1..pts.len()-1].iter().enumerate() {
        let d = seg_dist(p, first, last);
        if d > max_d { max_d = d; max_i = i + 1; }
    }
    if max_d > eps {
        let mut left = douglas_peucker(&pts[..=max_i], eps);
        let right    = douglas_peucker(&pts[max_i..],  eps);
        left.pop();
        left.extend(right);
        left
    } else {
        vec![first, last]
    }
}

// ── Per-glyph extraction ───────────────────────────────────────────────────────

fn extract_paths(
    face:       &Face<'_>,
    glyph_id:   GlyphId,
    scale:      f64,
    cursor_x:   f64,
    line_y:     f64,
    dp_epsilon: f64,
) -> Vec<Path<f64>> {
    let mut sampler = ContourSampler::new(scale, cursor_x, line_y);
    if face.outline_glyph(glyph_id, &mut sampler).is_none() { return vec![]; }
    // Flush any trailing contour that did not receive an explicit close().
    if !sampler.current.is_empty() {
        sampler.contours.push(std::mem::take(&mut sampler.current));
    }
    let mut contours = sampler.contours;
    if contours.is_empty() { return vec![]; }

    // Largest absolute area first → outer boundary comes before inner counters.
    contours.sort_by(|a, b| {
        signed_area(b).abs().partial_cmp(&signed_area(a).abs()).unwrap()
    });

    let raw: Vec<(f64, f64)> = if contours.len() == 1 {
        // ── Single contour: stroke with two ends ──────────────────────────────
        fold_centerline(&contours[0])
    } else {
        // ── Multiple contours: outer ring + inner counter ─────────────────────
        // Compute the arc-midpoint loop between the outer and the largest inner.
        let mut mid = midpoint_loop(&contours[0], &contours[1]);
        // Close the loop.
        if let Some(&first) = mid.first() { mid.push(first); }
        mid
    };

    if raw.len() < 2 { return vec![]; }
    let simplified = douglas_peucker(&raw, dp_epsilon);
    if simplified.len() < 2 { return vec![]; }

    let mut path = Path::empty();
    for (x, y) in simplified { path.push(Vec2d { x, y }); }
    vec![path]
}
