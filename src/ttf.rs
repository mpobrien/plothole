//! TTF font → single-stroke path pipeline.
//!
//! Pipeline per glyph:
//!   outline (ttf-parser) → raster fill (tiny-skia) → threshold →
//!   Zhang-Suen thinning → chain vectorisation → Douglas-Peucker → Path<f64>

use std::collections::HashSet;

use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Transform};
use ttf_parser::{Face, GlyphId, OutlineBuilder};

use crate::font::{Path, Vec2d};


// ── Public type ───────────────────────────────────────────────────────────────

pub struct TtfFont {
    data: Vec<u8>,
    face_index: u32,
}

impl TtfFont {
    pub fn from_file(path: &str, face_index: u32) -> Result<Self, Box<dyn std::error::Error>> {
        let data = std::fs::read(path)?;
        // Validate up-front so callers get a clear error.
        Face::parse(&data, face_index).map_err(|e| format!("invalid TTF face {face_index}: {e}"))?;
        Ok(Self { data, face_index })
    }

    /// Returns one `Vec<Path<f64>>` per character, with cursor-positioned x
    /// coordinates — the same shape that the Hershey `text_to_paths` returns.
    ///
    /// `axes` is a list of `(tag, value)` pairs for variable font axis overrides,
    /// e.g. `[("wght", 700.0)]`.  Unknown tags are silently ignored.
    pub fn text_to_paths(&self, text: &str, raster_px: f32, dp_epsilon: f64, axes: &[(String, f32)]) -> Vec<Vec<Path<f64>>> {
        let mut face = Face::parse(&self.data, self.face_index).unwrap();
        for (tag, value) in axes {
            let bytes = tag.as_bytes();
            if bytes.len() == 4 {
                let t = ttf_parser::Tag::from_bytes(bytes.try_into().unwrap());
                face.set_variation(t, *value);
            }
        }
        let scale = raster_px / face.units_per_em() as f32;

        let mut result = vec![];
        let mut cursor_x = 0.0f64;

        for ch in text.chars() {
            let Some(glyph_id) = face.glyph_index(ch) else {
                result.push(vec![]);
                continue;
            };

            let advance_px = face.glyph_hor_advance(glyph_id)
                .unwrap_or(0) as f64 * scale as f64;

            let paths = rasterize_and_extract(&face, glyph_id, scale, cursor_x, dp_epsilon);
            result.push(paths);
            cursor_x += advance_px;
        }

        result
    }
}

// ── Outline → raster ──────────────────────────────────────────────────────────

/// Implements `ttf_parser::OutlineBuilder`, applying scale + Y-flip inline so
/// the resulting path is in tiny-skia screen coordinates.
struct SkiaBuilder {
    pb: PathBuilder,
    scale: f32,
    tx: f32, // x translation: shifts glyph bbox left edge to `padding`
    ty: f32, // y translation: after Y-flip, shifts bbox top edge to `padding`
}

impl SkiaBuilder {
    #[inline] fn sx(&self, x: f32) -> f32 {  x * self.scale + self.tx }
    #[inline] fn sy(&self, y: f32) -> f32 { -y * self.scale + self.ty }
}

impl OutlineBuilder for SkiaBuilder {
    fn move_to(&mut self, x: f32, y: f32) {
        self.pb.move_to(self.sx(x), self.sy(y));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.pb.line_to(self.sx(x), self.sy(y));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.pb.quad_to(self.sx(x1), self.sy(y1), self.sx(x), self.sy(y));
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.pb.cubic_to(
            self.sx(x1), self.sy(y1),
            self.sx(x2), self.sy(y2),
            self.sx(x),  self.sy(y),
        );
    }
    fn close(&mut self) { self.pb.close(); }
}

fn rasterize_and_extract(
    face: &Face<'_>,
    glyph_id: GlyphId,
    scale: f32,
    cursor_x: f64,
    dp_epsilon: f64,
) -> Vec<Path<f64>> {
    let bbox = match face.glyph_bounding_box(glyph_id) {
        Some(b) => b,
        None    => return vec![],
    };

    // Pixel-space bounding box + a small border so the skeleton never touches
    // the pixmap edge (Zhang-Suen ignores the outermost ring of pixels).
    let pad = 4i32;
    let x_min_px = (bbox.x_min as f32 * scale).floor() as i32;
    let y_min_px = (bbox.y_min as f32 * scale).floor() as i32;
    let x_max_px = (bbox.x_max as f32 * scale).ceil()  as i32;
    let y_max_px = (bbox.y_max as f32 * scale).ceil()  as i32;
    let w = ((x_max_px - x_min_px) + 2 * pad) as u32;
    let h = ((y_max_px - y_min_px) + 2 * pad) as u32;
    if w == 0 || h == 0 { return vec![]; }

    // Transform so that font origin (bbox.x_min, bbox.y_max) → screen (pad, pad).
    //   screen_x =  font_x * scale  + tx       tx = -x_min_px + pad
    //   screen_y = -font_y * scale  + ty       ty =  y_max_px + pad
    let tx = -x_min_px as f32 + pad as f32;
    let ty =  y_max_px as f32 + pad as f32;

    let mut builder = SkiaBuilder { pb: PathBuilder::new(), scale, tx, ty };
    if face.outline_glyph(glyph_id, &mut builder).is_none() { return vec![]; }
    let skia_path = match builder.pb.finish() { Some(p) => p, None => return vec![] };

    let mut pixmap = match Pixmap::new(w, h) { Some(p) => p, None => return vec![] };
    let mut paint = Paint::default();
    paint.set_color_rgba8(0, 0, 0, 255);
    // EvenOdd correctly hollows out counters (the inside of 'O', 'A', etc.)
    // so the skeleton follows the ink strokes rather than cutting through holes.
    pixmap.fill_path(&skia_path, &paint, FillRule::EvenOdd, Transform::identity(), None);

    // Threshold alpha channel → binary grid.
    let raw = pixmap.data();
    let mut grid = vec![vec![false; w as usize]; h as usize];
    for y in 0..h as usize {
        for x in 0..w as usize {
            grid[y][x] = raw[(y * w as usize + x) * 4 + 3] > 127;
        }
    }

    zhang_suen(&mut grid);
    let chains = vectorize(&grid);

    // Inverse transform: screen pixel → output coordinates.
    //   out_x = cursor_x + (screen_x - pad) + x_min_px   (0 = pen origin)
    //   out_y = y_max_px - (screen_y - pad)               (0 = baseline, +up)
    chains.into_iter()
        .map(|chain| {
            let simplified = douglas_peucker(&chain, dp_epsilon);
            let mut path = Path::empty();
            for (sx, sy) in simplified {
                let out_x = cursor_x + (sx - pad as f64) + x_min_px as f64;
                // Match the Hershey Y convention: Y increases downward,
                // baseline at 0, ascenders at negative Y.
                // screen sy=pad → top of glyph → out_y = -y_max_px
                // screen sy=y_max_px+pad → baseline → out_y = 0
                let out_y = (sy - pad as f64) - y_max_px as f64;
                path.push(Vec2d { x: out_x, y: out_y });
            }
            path
        })
        .filter(|p| p.points().len() >= 2)
        .collect()
}

// ── Zhang-Suen thinning ───────────────────────────────────────────────────────
//
// Standard two-sub-iteration algorithm.  Neighbours P2..P9 are ordered
// clockwise starting from North:
//
//   P9 P2 P3
//   P8  · P4
//   P7 P6 P5

fn zhang_suen(grid: &mut Vec<Vec<bool>>) {
    let h = grid.len();
    if h < 3 { return; }
    let w = grid[0].len();
    if w < 3 { return; }

    loop {
        let mut changed = false;
        for sub in 0..2usize {
            let mut to_remove = vec![];
            for y in 1..h - 1 {
                for x in 1..w - 1 {
                    if !grid[y][x] { continue; }

                    let p = [
                        grid[y - 1][x],     // P2 N
                        grid[y - 1][x + 1], // P3 NE
                        grid[y][x + 1],     // P4 E
                        grid[y + 1][x + 1], // P5 SE
                        grid[y + 1][x],     // P6 S
                        grid[y + 1][x - 1], // P7 SW
                        grid[y][x - 1],     // P8 W
                        grid[y - 1][x - 1], // P9 NW
                    ];

                    // B(P1): number of non-zero neighbours
                    let b: u8 = p.iter().map(|&v| v as u8).sum();
                    if b < 2 || b > 6 { continue; }

                    // A(P1): number of 0→1 transitions in the cyclic sequence
                    let a = (0..8).filter(|&i| !p[i] && p[(i + 1) % 8]).count();
                    if a != 1 { continue; }

                    // Sub-iteration conditions
                    let (c1, c2) = if sub == 0 {
                        (p[0] && p[2] && p[4], p[2] && p[4] && p[6])
                    } else {
                        (p[0] && p[2] && p[6], p[0] && p[4] && p[6])
                    };

                    // Delete if both products are zero
                    if !c1 && !c2 {
                        to_remove.push((y, x));
                    }
                }
            }
            if !to_remove.is_empty() {
                changed = true;
                for (y, x) in to_remove {
                    grid[y][x] = false;
                }
            }
        }
        if !changed { break; }
    }
}

// ── Vectorisation ─────────────────────────────────────────────────────────────

fn black_neighbors(grid: &[Vec<bool>], y: usize, x: usize) -> Vec<(usize, usize)> {
    let h = grid.len() as i32;
    let w = grid[0].len() as i32;
    let mut out = vec![];
    for dy in -1i32..=1 {
        for dx in -1i32..=1 {
            if dy == 0 && dx == 0 { continue; }
            let (ny, nx) = (y as i32 + dy, x as i32 + dx);
            if ny >= 0 && nx >= 0 && ny < h && nx < w && grid[ny as usize][nx as usize] {
                out.push((ny as usize, nx as usize));
            }
        }
    }
    out
}

/// Walk all chains in the skeleton using edge-based traversal.
///
/// Endpoints (degree 1) are used as preferred starting points so their chains
/// are captured first; remaining unvisited edges at junctions and loops are
/// picked up in a second pass.
fn vectorize(grid: &[Vec<bool>]) -> Vec<Vec<(f64, f64)>> {
    let h = grid.len();
    if h == 0 { return vec![]; }
    let w = grid[0].len();

    // Pre-compute degree map to avoid re-scanning neighbours repeatedly.
    let mut deg = vec![vec![0u8; w]; h];
    for y in 0..h {
        for x in 0..w {
            if grid[y][x] {
                deg[y][x] = black_neighbors(grid, y, x).len() as u8;
            }
        }
    }

    // Encode an undirected edge as a sorted pair of flat pixel indices.
    let enc  = |y: usize, x: usize| (y * w + x) as u32;
    let edge = |a: (usize, usize), b: (usize, usize)| {
        let (ea, eb) = (enc(a.0, a.1), enc(b.0, b.1));
        if ea < eb { (ea, eb) } else { (eb, ea) }
    };

    // Endpoints first, then everything else (handles loops and junctions).
    let mut starts: Vec<(usize, usize)> = (0..h).flat_map(|y| (0..w).map(move |x| (y, x)))
        .filter(|&(y, x)| grid[y][x] && deg[y][x] == 1)
        .collect();
    let rest: Vec<(usize, usize)> = (0..h).flat_map(|y| (0..w).map(move |x| (y, x)))
        .filter(|&(y, x)| grid[y][x] && deg[y][x] != 1)
        .collect();
    starts.extend(rest);

    let mut visited_edges: HashSet<(u32, u32)> = HashSet::new();
    let mut chains: Vec<Vec<(f64, f64)>> = vec![];

    for start in starts {
        for first in black_neighbors(grid, start.0, start.1) {
            if visited_edges.contains(&edge(start, first)) { continue; }

            let mut chain = vec![(start.1 as f64, start.0 as f64)];
            let mut prev = start;
            let mut cur  = first;

            loop {
                visited_edges.insert(edge(prev, cur));
                chain.push((cur.1 as f64, cur.0 as f64));

                // Stop at endpoints or junctions; only chain through degree-2 pixels.
                if deg[cur.0][cur.1] != 2 { break; }

                let next = black_neighbors(grid, cur.0, cur.1)
                    .into_iter()
                    .find(|&n| n != prev && !visited_edges.contains(&edge(cur, n)));

                match next {
                    Some(n) => { prev = cur; cur = n; }
                    None    => break,
                }
            }

            if chain.len() >= 2 {
                chains.push(chain);
            }
        }
    }

    chains
}

// ── Douglas-Peucker simplification ────────────────────────────────────────────

fn seg_dist(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len_sq = dx * dx + dy * dy;
    if len_sq < 1e-12 {
        return ((p.0 - a.0).powi(2) + (p.1 - a.1).powi(2)).sqrt();
    }
    let t = ((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len_sq;
    let proj = (a.0 + t.clamp(0.0, 1.0) * dx, a.1 + t.clamp(0.0, 1.0) * dy);
    ((p.0 - proj.0).powi(2) + (p.1 - proj.1).powi(2)).sqrt()
}

fn douglas_peucker(pts: &[(f64, f64)], eps: f64) -> Vec<(f64, f64)> {
    if pts.len() < 3 { return pts.to_vec(); }
    let (first, last) = (pts[0], *pts.last().unwrap());
    let (mut max_d, mut max_i) = (0.0f64, 0);
    for (i, &p) in pts[1..pts.len() - 1].iter().enumerate() {
        let d = seg_dist(p, first, last);
        if d > max_d { max_d = d; max_i = i + 1; }
    }
    if max_d > eps {
        let mut left = douglas_peucker(&pts[..=max_i], eps);
        let right    = douglas_peucker(&pts[max_i..],  eps);
        left.pop(); // remove the duplicated junction point
        left.extend(right);
        left
    } else {
        vec![first, last]
    }
}
