//! 3D scene rendering with hidden-line removal for plotter output.
//!
//! Convex primitives (cube, sphere, cylinder, pyramid, n-prism) are tessellated
//! to triangle meshes. Edges are classified at construction time:
//!
//!   - Crease  : sharp dihedral angle (e.g. cube edges) — drawn whenever ≥1 adjacent face is front-facing
//!   - Smooth  : near-coplanar adjacents (e.g. sphere triangulation) — drawn only on silhouettes
//!   - Boundary: open mesh edges — drawn whenever the adjacent face is front-facing
//!
//! At render time:
//!   1. Project every vertex with the camera, classify each face front/back-facing.
//!   2. Per object, collect visible edges and build the 2D silhouette polygon.
//!   3. Sort objects by depth, clip each object's edges against the 2D silhouettes
//!      of all closer objects.
//!
//! Convex assumption keeps clipping simple: an object's silhouette is one closed
//! polygon, and self-occlusion is handled by the front/back-face test alone.

use std::collections::HashMap;

use crate::font::{Path, Vec2d};

// ── Built-in presets ───────────────────────────────────────────────────────

pub mod presets {
    use super::*;

    /// Render Hershey text into (x, y) paths in a unit-ish coordinate system,
    /// then normalise to `[0,1]²` with y-flipped so it reads upright on a face.
    fn hershey_text_unit_paths(text: &str, font_name: &str) -> Vec<Vec<(f64, f64)>> {
        let ft = match crate::hershey::fonts().get(&font_name.to_uppercase() as &str) {
            Some(f) => f,
            None    => return Vec::new(),
        };
        let line_height = {
            let mut min_y = i32::MAX;
            let mut max_y = i32::MIN;
            for glyph in ft.iter() {
                for p in &glyph.paths {
                    for pt in p.points() {
                        if pt.y < min_y { min_y = pt.y; }
                        if pt.y > max_y { max_y = pt.y; }
                    }
                }
            }
            if min_y > max_y { 32.0 } else { (max_y - min_y) as f64 * 1.2 }
        };
        let mut all: Vec<Vec<(f64, f64)>> = Vec::new();
        for (line_idx, line) in text.split('\n').enumerate() {
            let y_offset = line_idx as f64 * line_height;
            let mut x = 0i32;
            for ch in line.chars() {
                let index = (ch as usize).wrapping_sub(32);
                if index >= ft.len() { continue; }
                let glyph = &ft[index];
                for gp in &glyph.paths {
                    let pts: Vec<(f64, f64)> = gp.points().iter()
                        .map(|pt| ((x as f64) + (pt.x as f64) - (glyph.left as f64), pt.y as f64 + y_offset))
                        .collect();
                    if pts.len() >= 2 { all.push(pts); }
                }
                x += glyph.right - glyph.left;
            }
        }
        if all.is_empty() { return all; }
        let mut min_x = f64::INFINITY; let mut max_x = f64::NEG_INFINITY;
        let mut min_y = f64::INFINITY; let mut max_y = f64::NEG_INFINITY;
        for p in &all { for &(x, y) in p {
            if x < min_x { min_x = x; } if x > max_x { max_x = x; }
            if y < min_y { min_y = y; } if y > max_y { max_y = y; }
        }}
        let w = max_x - min_x;
        let h = max_y - min_y;
        let s = 0.85 / w.max(h);
        let cx = (min_x + max_x) * 0.5;
        let cy = (min_y + max_y) * 0.5;
        for p in all.iter_mut() { for pt in p.iter_mut() {
            pt.0 = 0.5 + (pt.0 - cx) * s;
            pt.1 = 0.5 - (pt.1 - cy) * s;
        }}
        all
    }

    /// Build a named preset scene. `swarm_count` is only used for the "swarm" preset.
    pub fn build(name: &str, swarm_count: usize) -> Result<Scene, String> {
        Ok(match name {
            "showcase" => Scene { objects: vec![
                cube         (Vec3::new(-3.5, -1.0, 0.0), 2.0),
                sphere       (Vec3::new(-1.0,  1.5, 0.5), 1.2, 16, 24),
                cylinder     (Vec3::new( 1.5, -0.5, 0.0), 1.0, 2.5, 32),
                pyramid      (Vec3::new( 4.0,  1.5, 0.0), 2.0, 2.5),
                prism        (Vec3::new( 0.5,  3.5, 0.0), 6,   1.2, 1.8),
                dodecahedron (Vec3::new(-3.0,  3.0, 0.5), 1.3),
            ]},
            "cubes" => Scene { objects: (0..8).map(|i| {
                let r = 1.5 + (i as f64) * 0.3;
                let a = i as f64 * 0.7;
                cube(Vec3::new(r * a.cos(), r * a.sin(), (i as f64 - 3.5) * 0.4), 1.2)
            }).collect() },
            "tower" => Scene { objects: vec![
                cube     (Vec3::new(0.0, 0.0, 0.0), 3.0),
                cylinder (Vec3::new(0.0, 0.0, 2.5), 1.0, 2.0, 32),
                sphere   (Vec3::new(0.0, 0.0, 4.5), 0.9, 16, 24),
            ]},
            "mixed" => Scene { objects: vec![
                cube    (Vec3::new(-2.0, 0.0, 0.0), 1.8),
                sphere  (Vec3::new( 0.0, 0.0, 0.5), 1.0, 14, 22),
                pyramid (Vec3::new( 2.5, 0.0, 0.0), 1.6, 2.0),
            ]},
            "shapes" => Scene { objects: vec![
                house        (Vec3::new(-3.5, -1.0, 0.0), 2.0),
                frustum      (Vec3::new(-1.0,  1.5, 0.5), 2.4, 1.0, 2.0),
                wedge        (Vec3::new( 1.5, -0.5, 0.0), 2.0),
                octahedron   (Vec3::new( 4.0,  1.5, 0.5), 1.4),
                dodecahedron (Vec3::new( 0.5,  3.5, 0.5), 1.4),
            ]},
            "frame" => Scene { objects: vec![
                // Frame at the front (closest to camera). Things behind should be visible
                // through the hole.
                frame  (Vec3::new( 0.0,  0.0,  2.0), 4.0, 2.0, 0.4),
                cube   (Vec3::new(-1.5, -0.5, -1.5), 1.5),
                sphere (Vec3::new( 1.0,  0.5, -1.0), 0.9, 16, 24),
            ]},
            "torus" => Scene { objects: vec![
                polytorus(Vec3::zero(), 2.5, 0.9, 12, 6),
            ]},
            "torus-stack" => Scene { objects: vec![
                polytorus(Vec3::new(0.0, 0.0, -1.5), 2.5, 0.7, 14, 6),
                polytorus(Vec3::new(0.0, 0.0,  0.0), 2.0, 0.6,  8, 5),
                polytorus(Vec3::new(0.0, 0.0,  1.5), 1.4, 0.5,  6, 4),
            ]},
            "knot" => Scene { objects: vec![
                // Trefoil: 2 wraps around the torus axis × 3 around the tube.
                // M=12 (cross-section subdivision) keeps lobe tips smooth — at lower M, the
                // polygonal facets become silhouettes whenever the tube points nearly along view.
                torus_knot(Vec3::zero(), 2.5, 0.7, 0.4, 2, 3, 90, 12),
            ]},
            "knot-5-2" => Scene { objects: vec![
                // Pentafoil / Solomon's-seal-style 5-fold knot.
                torus_knot(Vec3::zero(), 2.5, 0.6, 0.32, 2, 5, 130, 6),
            ]},
            "knot-tiny" => Scene { objects: vec![
                // Minimal trefoil for debugging self-occlusion: 24 path segments × 4 around
                // = 96 quads. Big enough to be a knot, small enough to inspect by hand.
                torus_knot(Vec3::zero(), 2.5, 0.7, 0.5, 2, 3, 24, 4),
            ]},
            "swarm" => {
                let n = swarm_count.max(1);
                // Scale extent with cube count^(1/3) so density stays roughly constant.
                let extent = (n as f64 / 1000.0).powf(1.0 / 3.0) * 28.0;
                let mut state: u64 = 0xC0FFEE_5EED_u64;
                let mut rand = || -> f64 {
                    state ^= state << 13; state ^= state >> 7; state ^= state << 17;
                    (state as f64) / (u64::MAX as f64)
                };
                let mut objs = Vec::with_capacity(n);
                for _ in 0..n {
                    let cx = (rand() - 0.5) * 2.0 * extent;
                    let cy = (rand() - 0.5) * 2.0 * extent;
                    let cz = (rand() - 0.5) * 2.0 * extent;
                    let size = 0.6 + rand() * 0.8;
                    let rx = rand() * 2.0 * std::f64::consts::PI;
                    let ry = rand() * 2.0 * std::f64::consts::PI;
                    let rz = rand() * 2.0 * std::f64::consts::PI;
                    let (sx, cx_) = rx.sin_cos();
                    let (sy, cy_) = ry.sin_cos();
                    let (sz, cz_) = rz.sin_cos();
                    let rotate = |p: Vec3| -> Vec3 {
                        let p1 = Vec3::new(p.x, cx_ * p.y - sx * p.z, sx * p.y + cx_ * p.z);
                        let p2 = Vec3::new(cy_ * p1.x + sy * p1.z, p1.y, -sy * p1.x + cy_ * p1.z);
                        Vec3::new(cz_ * p2.x - sz * p2.y, sz * p2.x + cz_ * p2.y, p2.z)
                    };
                    let h = size * 0.5;
                    let local = [
                        Vec3::new(-h,-h,-h), Vec3::new( h,-h,-h), Vec3::new( h, h,-h), Vec3::new(-h, h,-h),
                        Vec3::new(-h,-h, h), Vec3::new( h,-h, h), Vec3::new( h, h, h), Vec3::new(-h, h, h),
                    ];
                    let center = Vec3::new(cx, cy, cz);
                    let verts: Vec<Vec3> = local.iter().map(|&p| rotate(p).add(center)).collect();
                    let faces = vec![
                        [0,2,1],[0,3,2], [4,5,6],[4,6,7],
                        [0,1,5],[0,5,4], [2,3,7],[2,7,6],
                        [0,4,7],[0,7,3], [1,2,6],[1,6,5],
                    ];
                    objs.push(Mesh::new(verts, faces));
                }
                Scene { objects: objs }
            }
            "textured" => {
                let h = 1.0_f64;
                let v = [
                    Vec3::new(-h,-h,-h), Vec3::new( h,-h,-h), Vec3::new( h, h,-h), Vec3::new(-h, h,-h),
                    Vec3::new(-h,-h, h), Vec3::new( h,-h, h), Vec3::new( h, h, h), Vec3::new(-h, h, h),
                ];
                let cube_t = cube(Vec3::zero(), 2.0)
                    .with_texture(TexturedFace::from_quad(v[4], v[5], v[6], v[7], texture_grid(6, 6)))
                    .with_texture(TexturedFace::from_quad(v[1], v[2], v[6], v[5], texture_dots(5, 5, 0.06)))
                    .with_texture(TexturedFace::from_quad(v[3], v[7], v[6], v[2], texture_hatch(0.12, 45.0)));
                Scene { objects: vec![cube_t] }
            }
            "text-swarm" => {
                let n = swarm_count.max(1);
                let extent = (n as f64 / 1000.0).powf(1.0 / 3.0) * 28.0;
                let mut state: u64 = 0xC0FFEE_5EED_u64;
                let mut rand = || -> f64 {
                    state ^= state << 13; state ^= state >> 7; state ^= state << 17;
                    (state as f64) / (u64::MAX as f64)
                };
                // Precompute the 26 letter glyphs once and clone-attach them to each cube.
                let chars: Vec<char> = ('A'..='Z').collect();
                let letter_paths: Vec<Vec<Vec<(f64, f64)>>> = chars.iter()
                    .map(|c| hershey_text_unit_paths(&c.to_string(), "FUTURAL"))
                    .collect();
                let mut objs = Vec::with_capacity(n);
                for _ in 0..n {
                    let cx = (rand() - 0.5) * 2.0 * extent;
                    let cy = (rand() - 0.5) * 2.0 * extent;
                    let cz = (rand() - 0.5) * 2.0 * extent;
                    let size = 0.8 + rand() * 0.6;
                    let rx = rand() * 2.0 * std::f64::consts::PI;
                    let ry = rand() * 2.0 * std::f64::consts::PI;
                    let rz = rand() * 2.0 * std::f64::consts::PI;
                    let (sx, cx_) = rx.sin_cos();
                    let (sy, cy_) = ry.sin_cos();
                    let (sz, cz_) = rz.sin_cos();
                    let rotate = |p: Vec3| -> Vec3 {
                        let p1 = Vec3::new(p.x, cx_ * p.y - sx * p.z, sx * p.y + cx_ * p.z);
                        let p2 = Vec3::new(cy_ * p1.x + sy * p1.z, p1.y, -sy * p1.x + cy_ * p1.z);
                        Vec3::new(cz_ * p2.x - sz * p2.y, sz * p2.x + cz_ * p2.y, p2.z)
                    };
                    let h = size * 0.5;
                    let local = [
                        Vec3::new(-h,-h,-h), Vec3::new( h,-h,-h), Vec3::new( h, h,-h), Vec3::new(-h, h,-h),
                        Vec3::new(-h,-h, h), Vec3::new( h,-h, h), Vec3::new( h, h, h), Vec3::new(-h, h, h),
                    ];
                    let center = Vec3::new(cx, cy, cz);
                    let v: Vec<Vec3> = local.iter().map(|&p| rotate(p).add(center)).collect();
                    let faces = vec![
                        [0,2,1],[0,3,2], [4,5,6],[4,6,7],
                        [0,1,5],[0,5,4], [2,3,7],[2,7,6],
                        [0,4,7],[0,7,3], [1,2,6],[1,6,5],
                    ];
                    let li = (rand() * chars.len() as f64) as usize % chars.len();
                    let lp = letter_paths[li].clone();
                    let mesh = Mesh::new(v.clone(), faces)
                        .with_texture(TexturedFace::from_quad(v[4], v[5], v[6], v[7], lp.clone())) // +z
                        .with_texture(TexturedFace::from_quad(v[0], v[3], v[2], v[1], lp.clone())) // -z
                        .with_texture(TexturedFace::from_quad(v[1], v[2], v[6], v[5], lp.clone())) // +x
                        .with_texture(TexturedFace::from_quad(v[0], v[4], v[7], v[3], lp.clone())) // -x
                        .with_texture(TexturedFace::from_quad(v[3], v[7], v[6], v[2], lp.clone())) // +y
                        .with_texture(TexturedFace::from_quad(v[0], v[1], v[5], v[4], lp));        // -y
                    objs.push(mesh);
                }
                Scene { objects: objs }
            }
            "text" => {
                let h = 1.0_f64;
                let v = [
                    Vec3::new(-h,-h,-h), Vec3::new( h,-h,-h), Vec3::new( h, h,-h), Vec3::new(-h, h,-h),
                    Vec3::new(-h,-h, h), Vec3::new( h,-h, h), Vec3::new( h, h, h), Vec3::new(-h, h, h),
                ];
                let cube_t = cube(Vec3::zero(), 2.0)
                    .with_texture(TexturedFace::from_quad(v[4], v[5], v[6], v[7], hershey_text_unit_paths("HELLO", "FUTURAL")))
                    .with_texture(TexturedFace::from_quad(v[1], v[2], v[6], v[5], hershey_text_unit_paths("PLOT",  "ROWMANT")))
                    .with_texture(TexturedFace::from_quad(v[3], v[7], v[6], v[2], hershey_text_unit_paths("HOLE",  "GOTHGRT")));
                Scene { objects: vec![cube_t] }
            }
            other => return Err(format!("unknown preset \"{other}\"")),
        })
    }

    pub fn names() -> &'static [&'static str] {
        &["showcase", "cubes", "tower", "mixed", "shapes", "frame",
          "torus", "torus-stack", "knot", "knot-5-2", "knot-tiny",
          "swarm", "textured", "text", "text-swarm"]
    }
}

// ── 3D math ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vec3 {
    pub const fn new(x: f64, y: f64, z: f64) -> Self { Self { x, y, z } }
    pub const fn zero()                       -> Self { Self::new(0.0, 0.0, 0.0) }

    pub fn add(self, o: Self)  -> Self { Self::new(self.x + o.x, self.y + o.y, self.z + o.z) }
    pub fn sub(self, o: Self)  -> Self { Self::new(self.x - o.x, self.y - o.y, self.z - o.z) }
    pub fn scale(self, s: f64) -> Self { Self::new(self.x * s,   self.y * s,   self.z * s) }
    pub fn dot(self, o: Self)  -> f64  { self.x * o.x + self.y * o.y + self.z * o.z }
    pub fn cross(self, o: Self) -> Self {
        Self::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }
    pub fn length(self)    -> f64 { self.dot(self).sqrt() }
    pub fn normalize(self) -> Self {
        let l = self.length();
        if l < 1e-12 { self } else { self.scale(1.0 / l) }
    }
}

// ── Camera (orthographic) ──────────────────────────────────────────────────

pub struct Camera {
    pub eye:     Vec3,
    pub target:  Vec3,
    pub up:      Vec3,
    pub scale:   f64, // overall output-unit multiplier
    pub fov_deg: f64, // 0.0 = orthographic; >0 = perspective with this vertical FOV
    pub near:    f64, // segments are clipped at this view-space depth before projection
}

impl Camera {
    /// Returns (x_2d, y_2d, depth) in output coordinates. Larger depth = farther from camera.
    pub fn project(&self, p: Vec3) -> (f64, f64, f64) {
        let forward = self.target.sub(self.eye).normalize();
        let right   = forward.cross(self.up).normalize();
        let up      = right.cross(forward).normalize();

        let v = p.sub(self.eye);
        let x = v.dot(right);
        let y = v.dot(up);
        let z = v.dot(forward); // depth (positive = in front of camera)

        if self.fov_deg > 0.0 {
            // Perspective: divide by depth, scale by focal length f = 1 / tan(fov/2).
            let f  = 1.0 / (self.fov_deg.to_radians() * 0.5).tan();
            let zc = z.max(self.near); // clamp at near plane; segments are clipped before reaching here
            (x / zc * f * self.scale, -y / zc * f * self.scale, z)
        } else {
            (x * self.scale, -y * self.scale, z)
        }
    }

    /// Project a 3D segment, clipping it against the near plane first.
    /// Returns None if the entire segment is behind the near plane.
    pub fn project_segment(&self, a: Vec3, b: Vec3) -> Option<((f64, f64), (f64, f64))> {
        let forward = self.target.sub(self.eye).normalize();
        let za = a.sub(self.eye).dot(forward);
        let zb = b.sub(self.eye).dot(forward);
        let n  = self.near;
        let (a_clip, b_clip) = match (za >= n, zb >= n) {
            (false, false) => return None,
            (true, true)   => (a, b),
            (true, false)  => {
                let t = (n - za) / (zb - za);
                (a, a.add(b.sub(a).scale(t)))
            }
            (false, true)  => {
                let t = (n - za) / (zb - za);
                (a.add(b.sub(a).scale(t)), b)
            }
        };
        let (ax, ay, _) = self.project(a_clip);
        let (bx, by, _) = self.project(b_clip);
        Some(((ax, ay), (bx, by)))
    }
}

// ── Mesh + edge classification ─────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    Crease,
    Smooth,
    Boundary,
}

#[derive(Clone, Debug)]
pub struct Edge {
    pub a:     usize,
    pub b:     usize,
    pub kind:  EdgeKind,
    pub faces: [Option<usize>; 2],
}

#[derive(Clone, Debug)]
pub struct Mesh {
    pub vertices:        Vec<Vec3>,
    pub faces:           Vec<[usize; 3]>,
    pub edges:           Vec<Edge>,
    pub textured_faces:  Vec<TexturedFace>,
    /// When true, the renderer relies purely on front/back face culling for hidden-line
    /// removal within this object — correct only for convex shapes. Set to false for
    /// non-convex shapes so that per-triangle self-occlusion clipping kicks in.
    pub assume_convex:   bool,
}

/// A flat patch on the mesh that carries a 2D texture (line-art paths).
/// Texture coordinates are in `[0,1]²`; (0,0) maps to `origin`, u axis to (1,0), v axis to (0,1).
#[derive(Clone, Debug)]
pub struct TexturedFace {
    pub origin: Vec3,
    pub u_axis: Vec3,
    pub v_axis: Vec3,
    pub paths:  Vec<Vec<(f64, f64)>>,
}

impl TexturedFace {
    /// Build a textured face from four corner points in CCW order (viewed from outside).
    /// Origin = a, u_axis = b - a (along bottom edge), v_axis = d - a (along left edge).
    pub fn from_quad(a: Vec3, b: Vec3, _c: Vec3, d: Vec3, paths: Vec<Vec<(f64, f64)>>) -> Self {
        Self { origin: a, u_axis: b.sub(a), v_axis: d.sub(a), paths }
    }

    fn normal(&self) -> Vec3 { self.u_axis.cross(self.v_axis).normalize() }
    fn map(&self, uv: (f64, f64)) -> Vec3 {
        self.origin.add(self.u_axis.scale(uv.0)).add(self.v_axis.scale(uv.1))
    }
}

impl Mesh {
    pub fn new(vertices: Vec<Vec3>, faces: Vec<[usize; 3]>) -> Self {
        let edges = compute_edges(&vertices, &faces, /*crease threshold:*/ 0.6_f64.to_radians().cos());
        Self { vertices, faces, edges, textured_faces: Vec::new(), assume_convex: true }
    }

    /// Build with a custom crease angle threshold (in radians).
    pub fn with_crease_angle(vertices: Vec<Vec3>, faces: Vec<[usize; 3]>, crease_rad: f64) -> Self {
        let edges = compute_edges(&vertices, &faces, crease_rad.cos());
        Self { vertices, faces, edges, textured_faces: Vec::new(), assume_convex: true }
    }

    pub fn with_texture(mut self, face: TexturedFace) -> Self {
        self.textured_faces.push(face);
        self
    }

    /// Mark this mesh as non-convex; render-time self-occlusion (per-triangle clipping)
    /// will be applied. Slower per object but necessary for shapes with holes or concavities.
    pub fn with_self_occlusion(mut self) -> Self {
        self.assume_convex = false;
        self
    }

    /// Build a mesh from polygon faces. Each polygon is a list of vertex indices in
    /// CCW order viewed from outside. Polygons should be planar (small warps are
    /// tolerated). Fan-triangulation diagonals are removed from the edge list entirely
    /// — they're internal-only and should never be drawn, even at silhouette boundaries
    /// where a warped quad's two fan triangles might numerically split between front
    /// and back. Triangles themselves are kept so self-occlusion still works correctly.
    pub fn from_polygons(vertices: Vec<Vec3>, polygons: Vec<Vec<usize>>) -> Self {
        use std::collections::HashSet;
        let mut faces: Vec<[usize; 3]> = Vec::new();
        let mut fan_diagonals: HashSet<(usize, usize)> = HashSet::new();
        for poly in &polygons {
            if poly.len() < 3 { continue; }
            for i in 1..poly.len() - 1 {
                faces.push([poly[0], poly[i], poly[i + 1]]);
                if i >= 2 {
                    let (a, b) = (poly[0].min(poly[i]), poly[0].max(poly[i]));
                    fan_diagonals.insert((a, b));
                }
            }
        }
        let mut mesh = Self::new(vertices, faces);
        mesh.edges.retain(|e| {
            let key = (e.a.min(e.b), e.a.max(e.b));
            !fan_diagonals.contains(&key)
        });
        mesh
    }
}

/// A polygonal `(p, q)`-torus knot. The path winds around the torus's central axis `p` times
/// and around the tube cross-section `q` times. Trefoil = (2, 3); pentafoil = (2, 5);
/// gcd(p, q) > 1 yields one component of a torus link instead of a single connected knot.
///
/// Tube is built by sweeping an `n_around`-gon cross-section along the path with a
/// parallel-transport frame; a single uniform rotation is distributed along the loop to
/// close the holonomy gap so there's no visible seam at the wrap-around.
pub fn torus_knot(
    center:   Vec3,
    major_r:  f64,
    path_r:   f64,
    tube_r:   f64,
    p:        u32,
    q:        u32,
    n_along:  usize,
    n_around: usize,
) -> Mesh {
    let n  = n_along.max(8);
    let m  = n_around.max(3);
    let pf = p as f64;
    let qf = q as f64;

    // Sample the knot center path.
    let pts: Vec<Vec3> = (0..n).map(|i| {
        let t = 2.0 * std::f64::consts::PI * i as f64 / n as f64;
        let (sp, cp) = (pf * t).sin_cos();
        let (sq, cq) = (qf * t).sin_cos();
        let radial = major_r + path_r * cq;
        Vec3::new(radial * cp, radial * sp, path_r * sq)
    }).collect();

    // Tangents via central differences.
    let tangents: Vec<Vec3> = (0..n).map(|i| {
        let prev = pts[(i + n - 1) % n];
        let next = pts[(i + 1) % n];
        next.sub(prev).normalize()
    }).collect();

    // Parallel-transport an arbitrary normal along the path.
    let initial_normal = {
        let t    = tangents[0];
        let cand = if t.x.abs() < 0.9 { Vec3::new(1.0, 0.0, 0.0) } else { Vec3::new(0.0, 1.0, 0.0) };
        cand.sub(t.scale(t.dot(cand))).normalize()
    };
    let mut normals = Vec::with_capacity(n);
    normals.push(initial_normal);
    for i in 1..n {
        let prev_n = normals[i - 1];
        let t      = tangents[i];
        normals.push(prev_n.sub(t.scale(prev_n.dot(t))).normalize());
    }

    // Compute and distribute the holonomy so the last frame's wrap matches the first.
    let last_to_start = {
        let t = tangents[0];
        let proj = normals[n - 1].sub(t.scale(normals[n - 1].dot(t))).normalize();
        let n0   = normals[0];
        let dot  = proj.dot(n0).clamp(-1.0, 1.0);
        let mag  = dot.acos();
        let sign = t.dot(proj.cross(n0)).signum();
        if sign == 0.0 { mag } else { mag * sign }
    };
    for i in 0..n {
        let theta = -last_to_start * i as f64 / n as f64;
        let (s, c) = theta.sin_cos();
        let nrm = normals[i];
        let t   = tangents[i];
        let bin = t.cross(nrm).normalize();
        normals[i] = nrm.scale(c).add(bin.scale(s));
    }

    // Generate vertices: a polygonal cross-section ring at each path point.
    let mut v: Vec<Vec3> = Vec::with_capacity(n * m);
    for i in 0..n {
        let p_c   = pts[i];
        let t     = tangents[i];
        let nrm   = normals[i];
        let bin   = t.cross(nrm).normalize();
        for j in 0..m {
            let phi = 2.0 * std::f64::consts::PI * j as f64 / m as f64;
            let (sphi, cphi) = phi.sin_cos();
            v.push(center.add(p_c.add(nrm.scale(tube_r * cphi)).add(bin.scale(tube_r * sphi))));
        }
    }

    let idx = |i: usize, j: usize| (i % n) * m + (j % m);
    let mut polygons: Vec<Vec<usize>> = Vec::with_capacity(n * m);
    for i in 0..n {
        for j in 0..m {
            polygons.push(vec![idx(i, j), idx(i + 1, j), idx(i + 1, j + 1), idx(i, j + 1)]);
        }
    }
    Mesh::from_polygons(v, polygons).with_self_occlusion()
}

/// A polygonal torus (donut): `major_r` is the distance from the torus center to the tube
/// center, `minor_r` is the tube radius. `major_segs` and `minor_segs` control how many
/// flat polygon faces the donut is built from (low values = chunky/faceted look).
pub fn polytorus(center: Vec3, major_r: f64, minor_r: f64, major_segs: usize, minor_segs: usize) -> Mesh {
    let n = major_segs.max(3);
    let m = minor_segs.max(3);
    let mut v: Vec<Vec3> = Vec::with_capacity(n * m);
    for i in 0..n {
        let theta = 2.0 * std::f64::consts::PI * i as f64 / n as f64;
        let (st, ct) = theta.sin_cos();
        for j in 0..m {
            let phi = 2.0 * std::f64::consts::PI * j as f64 / m as f64;
            let (sp, cp) = phi.sin_cos();
            let radial = major_r + minor_r * cp;
            v.push(center.add(Vec3::new(radial * ct, radial * st, minor_r * sp)));
        }
    }
    let idx = |i: usize, j: usize| (i % n) * m + (j % m);
    let mut polygons: Vec<Vec<usize>> = Vec::with_capacity(n * m);
    for i in 0..n {
        for j in 0..m {
            polygons.push(vec![idx(i, j), idx(i + 1, j), idx(i + 1, j + 1), idx(i, j + 1)]);
        }
    }
    Mesh::from_polygons(v, polygons).with_self_occlusion()
}

/// A square picture-frame shape: outer square minus inner square hole, extruded by `depth`.
/// `outer` and `inner` are the side lengths of the two squares (inner < outer).
/// This is a non-convex shape with a topological hole — exercises multi-loop silhouette extraction.
pub fn frame(center: Vec3, outer: f64, inner: f64, depth: f64) -> Mesh {
    let o = outer * 0.5;
    let i = inner * 0.5;
    let h = depth * 0.5;
    let v: Vec<Vec3> = vec![
        // 0..3   outer SW, SE, NE, NW front (z = +h)
        Vec3::new(-o,-o, h), Vec3::new( o,-o, h), Vec3::new( o, o, h), Vec3::new(-o, o, h),
        // 4..7   inner SW, SE, NE, NW front
        Vec3::new(-i,-i, h), Vec3::new( i,-i, h), Vec3::new( i, i, h), Vec3::new(-i, i, h),
        // 8..11  outer SW, SE, NE, NW back  (z = -h)
        Vec3::new(-o,-o,-h), Vec3::new( o,-o,-h), Vec3::new( o, o,-h), Vec3::new(-o, o,-h),
        // 12..15 inner SW, SE, NE, NW back
        Vec3::new(-i,-i,-h), Vec3::new( i,-i,-h), Vec3::new( i, i,-h), Vec3::new(-i, i,-h),
    ].into_iter().map(|p| p.add(center)).collect();
    let mut polygons = Vec::with_capacity(16);
    for s in 0..4 {
        let s1 = (s + 1) % 4;
        // Front annulus strip — outward +z.
        polygons.push(vec![s, s1, 4 + s1, 4 + s]);
        // Back annulus strip  — outward -z (reversed winding).
        polygons.push(vec![8 + s1, 8 + s, 12 + s, 12 + s1]);
        // Outer side wall — outward radially outward.
        polygons.push(vec![s, 8 + s, 8 + s1, s1]);
        // Inner side wall (tunnel wall) — outward INTO the hole.
        polygons.push(vec![4 + s, 4 + s1, 12 + s1, 12 + s]);
    }
    Mesh::from_polygons(v, polygons).with_self_occlusion()
}

/// A simple house shape: a 2×2×1 box with a gabled roof on top.
/// Demonstrates non-rectangular faces (the gable walls are pentagons, the bottom and roof slopes are quads).
pub fn house(center: Vec3, size: f64) -> Mesh {
    let h = size * 0.5;
    let v = vec![
        Vec3::new(-h,-h,    0.0), Vec3::new( h,-h,    0.0), Vec3::new( h, h,    0.0), Vec3::new(-h, h,    0.0), // 0..3 base
        Vec3::new(-h,-h,      h), Vec3::new( h,-h,      h), Vec3::new( h, h,      h), Vec3::new(-h, h,      h), // 4..7 eaves
        Vec3::new(0.0,-h, 2.0*h), Vec3::new(0.0, h, 2.0*h),                                                       // 8..9 ridge ends
    ].into_iter().map(|p| p.add(center)).collect();
    let polygons = vec![
        vec![0, 3, 2, 1],          // bottom (-z)
        vec![1, 2, 6, 5],          // east wall (+x)
        vec![0, 4, 7, 3],          // west wall (-x)
        vec![0, 1, 5, 8, 4],       // south gable (-y)
        vec![2, 3, 7, 9, 6],       // north gable (+y)
        vec![5, 6, 9, 8],          // east roof slope
        vec![4, 8, 9, 7],          // west roof slope
    ];
    Mesh::from_polygons(v, polygons)
}

/// Truncated square pyramid (frustum). `base` and `top` are the side lengths of
/// the bottom and top squares; `height` is the vertical extent.
pub fn frustum(center: Vec3, base: f64, top: f64, height: f64) -> Mesh {
    let bh = base * 0.5;
    let th = top * 0.5;
    let zh = height * 0.5;
    let v = vec![
        Vec3::new(-bh,-bh,-zh), Vec3::new( bh,-bh,-zh), Vec3::new( bh, bh,-zh), Vec3::new(-bh, bh,-zh), // 0..3 bottom
        Vec3::new(-th,-th, zh), Vec3::new( th,-th, zh), Vec3::new( th, th, zh), Vec3::new(-th, th, zh), // 4..7 top
    ].into_iter().map(|p| p.add(center)).collect();
    let polygons = vec![
        vec![0, 3, 2, 1],     // bottom
        vec![4, 5, 6, 7],     // top
        vec![0, 1, 5, 4],     // -y
        vec![1, 2, 6, 5],     // +x
        vec![2, 3, 7, 6],     // +y
        vec![3, 0, 4, 7],     // -x
    ];
    Mesh::from_polygons(v, polygons)
}

/// A right-triangular prism (wedge) with hypotenuse running from +x bottom to -x top.
pub fn wedge(center: Vec3, size: f64) -> Mesh {
    let h = size * 0.5;
    let v = vec![
        Vec3::new(-h,-h,-h), Vec3::new( h,-h,-h), Vec3::new(-h,-h, h), // 0..2 south triangle
        Vec3::new(-h, h,-h), Vec3::new( h, h,-h), Vec3::new(-h, h, h), // 3..5 north triangle
    ].into_iter().map(|p| p.add(center)).collect();
    let polygons = vec![
        vec![0, 1, 2],          // south wall (-y)
        vec![4, 3, 5],          // north wall (+y)
        vec![0, 3, 4, 1],       // bottom (-z)
        vec![0, 2, 5, 3],       // back wall (-x)
        vec![1, 4, 5, 2],       // hypotenuse / slanted face
    ];
    Mesh::from_polygons(v, polygons)
}

/// Regular octahedron — 8 triangular faces, 6 vertices.
pub fn octahedron(center: Vec3, radius: f64) -> Mesh {
    let r = radius;
    let v = vec![
        Vec3::new( r, 0.0, 0.0), Vec3::new(-r, 0.0, 0.0),  // 0,1: ±x
        Vec3::new(0.0,  r, 0.0), Vec3::new(0.0, -r, 0.0),  // 2,3: ±y
        Vec3::new(0.0, 0.0,  r), Vec3::new(0.0, 0.0, -r),  // 4,5: ±z
    ].into_iter().map(|p| p.add(center)).collect();
    let polygons = vec![
        vec![4, 0, 2], vec![4, 2, 1], vec![4, 1, 3], vec![4, 3, 0],  // top half (around +z apex)
        vec![5, 2, 0], vec![5, 1, 2], vec![5, 3, 1], vec![5, 0, 3],  // bottom half (around -z apex)
    ];
    Mesh::from_polygons(v, polygons)
}

/// Regular dodecahedron with circumradius `radius` (distance from center to vertex).
/// Uses 12 pentagonal faces — a real workout for the polygon-face pipeline.
pub fn dodecahedron(center: Vec3, radius: f64) -> Mesh {
    let phi     = (1.0 + 5.0_f64.sqrt()) / 2.0;
    let inv_phi = 1.0 / phi;
    let s       = radius / 3.0_f64.sqrt(); // place cube vertices on a sphere of radius `radius`

    // 20 vertices: 8 cube corners + 12 "rectangle" vertices.
    let mut v: Vec<Vec3> = Vec::new();
    for &sx in &[ s, -s] { for &sy in &[ s, -s] { for &sz in &[ s, -s] {
        v.push(Vec3::new(sx, sy, sz));
    }}}
    for &y in &[ s * inv_phi, -s * inv_phi] {
        for &z in &[ s * phi, -s * phi] { v.push(Vec3::new(0.0, y, z)); }
    }
    for &x in &[ s * inv_phi, -s * inv_phi] {
        for &y in &[ s * phi, -s * phi] { v.push(Vec3::new(x, y, 0.0)); }
    }
    for &x in &[ s * phi, -s * phi] {
        for &z in &[ s * inv_phi, -s * inv_phi] { v.push(Vec3::new(x, 0.0, z)); }
    }

    // 12 face-normal directions (= dual icosahedron vertices).
    // For this vertex layout the faces actually point at (0,±φ,±1), (±φ,±1,0), (±1,0,±φ)
    // — y/z swapped from the more common icosahedron convention.
    let face_dirs: Vec<Vec3> = {
        let mut d = Vec::with_capacity(12);
        for &y in &[phi, -phi] { for &z in &[1.0, -1.0] { d.push(Vec3::new(0.0, y, z)); }}
        for &x in &[phi, -phi] { for &y in &[1.0, -1.0] { d.push(Vec3::new(x, y, 0.0)); }}
        for &x in &[1.0, -1.0] { for &z in &[phi, -phi] { d.push(Vec3::new(x, 0.0, z)); }}
        d
    };

    // For each face direction, pick the 5 vertices with highest dot product, then
    // sort them CCW by angle in a 2D basis on the face plane.
    let mut polygons: Vec<Vec<usize>> = Vec::with_capacity(12);
    for nd in &face_dirs {
        let n = nd.normalize();
        let mut by_dot: Vec<(usize, f64)> = (0..v.len()).map(|i| (i, v[i].dot(n))).collect();
        by_dot.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        let face: Vec<usize> = by_dot[..5].iter().map(|&(i, _)| i).collect();

        // CCW sort around centroid
        let centroid = face.iter().fold(Vec3::zero(), |acc, &i| acc.add(v[i])).scale(0.2);
        // pick any reference vector not parallel to n
        let raw_u = if n.x.abs() < 0.9 { Vec3::new(1.0, 0.0, 0.0) } else { Vec3::new(0.0, 1.0, 0.0) };
        let u = raw_u.sub(n.scale(raw_u.dot(n))).normalize();
        let v_axis = n.cross(u);
        let mut sorted: Vec<(usize, f64)> = face.iter().map(|&i| {
            let r = v[i].sub(centroid);
            (i, r.dot(v_axis).atan2(r.dot(u)))
        }).collect();
        sorted.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        polygons.push(sorted.into_iter().map(|(i, _)| i).collect());
    }

    let translated = v.into_iter().map(|p| p.add(center)).collect();
    Mesh::from_polygons(translated, polygons)
}

// ── Built-in texture generators (paths in unit-square coords) ──────────────

pub fn texture_grid(rows: usize, cols: usize) -> Vec<Vec<(f64, f64)>> {
    let mut paths = Vec::new();
    for r in 0..=rows {
        let y = r as f64 / rows as f64;
        paths.push(vec![(0.0, y), (1.0, y)]);
    }
    for c in 0..=cols {
        let x = c as f64 / cols as f64;
        paths.push(vec![(x, 0.0), (x, 1.0)]);
    }
    paths
}

pub fn texture_dots(rows: usize, cols: usize, radius: f64) -> Vec<Vec<(f64, f64)>> {
    let n_seg = 16;
    let mut paths = Vec::new();
    for r in 0..rows {
        for c in 0..cols {
            let cx = (c as f64 + 0.5) / cols as f64;
            let cy = (r as f64 + 0.5) / rows as f64;
            let mut path = Vec::with_capacity(n_seg + 1);
            for i in 0..=n_seg {
                let a = i as f64 * 2.0 * std::f64::consts::PI / n_seg as f64;
                path.push((cx + radius * a.cos(), cy + radius * a.sin()));
            }
            paths.push(path);
        }
    }
    paths
}

pub fn texture_hatch(spacing: f64, angle_deg: f64) -> Vec<Vec<(f64, f64)>> {
    let a = angle_deg.to_radians();
    let (s, c) = a.sin_cos();
    // Lines of slope (c, s); intercepts spaced by `spacing` along the perpendicular (-s, c).
    // Project unit square corners onto the perpendicular to find range of intercepts.
    let perp = (-s, c);
    let projs = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
        .iter().map(|(x, y)| x * perp.0 + y * perp.1).collect::<Vec<_>>();
    let lo = projs.iter().cloned().fold(f64::INFINITY, f64::min);
    let hi = projs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let mut paths = Vec::new();
    let mut t = (lo / spacing).ceil() * spacing;
    while t <= hi {
        // Line {(t * perp.0 + k * c, t * perp.1 + k * s) : k ∈ ℝ}.
        // Clip to unit square by finding entry/exit k values along x=0,1,y=0,1.
        let mut ks: Vec<f64> = Vec::new();
        let p0 = (t * perp.0, t * perp.1);
        if c.abs() > 1e-9 {
            ks.push((0.0 - p0.0) / c);
            ks.push((1.0 - p0.0) / c);
        }
        if s.abs() > 1e-9 {
            ks.push((0.0 - p0.1) / s);
            ks.push((1.0 - p0.1) / s);
        }
        ks.sort_by(|a, b| a.partial_cmp(b).unwrap());
        // For each consecutive pair, check if midpoint is inside the unit square.
        let mut best: Option<(f64, f64)> = None;
        for w in ks.windows(2) {
            let mid = (p0.0 + 0.5 * (w[0] + w[1]) * c, p0.1 + 0.5 * (w[0] + w[1]) * s);
            if mid.0 >= -1e-9 && mid.0 <= 1.0 + 1e-9 && mid.1 >= -1e-9 && mid.1 <= 1.0 + 1e-9 {
                best = Some((w[0], w[1]));
                break;
            }
        }
        if let Some((k0, k1)) = best {
            paths.push(vec![
                (p0.0 + k0 * c, p0.1 + k0 * s),
                (p0.0 + k1 * c, p0.1 + k1 * s),
            ]);
        }
        t += spacing;
    }
    paths
}

fn face_normal(verts: &[Vec3], face: &[usize; 3]) -> Vec3 {
    let a = verts[face[0]];
    let b = verts[face[1]];
    let c = verts[face[2]];
    b.sub(a).cross(c.sub(a)).normalize()
}

fn compute_edges(verts: &[Vec3], faces: &[[usize; 3]], crease_cos: f64) -> Vec<Edge> {
    // map sorted (u,v) -> face indices that share this edge
    let mut edge_map: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
    for (fi, f) in faces.iter().enumerate() {
        for (u, v) in [(f[0], f[1]), (f[1], f[2]), (f[2], f[0])] {
            let key = if u < v { (u, v) } else { (v, u) };
            edge_map.entry(key).or_default().push(fi);
        }
    }

    let mut out = Vec::new();
    for ((u, v), entries) in edge_map {
        match entries.as_slice() {
            [f0]      => out.push(Edge { a: u, b: v, kind: EdgeKind::Boundary, faces: [Some(*f0), None] }),
            [f0, f1]  => {
                let n0   = face_normal(verts, &faces[*f0]);
                let n1   = face_normal(verts, &faces[*f1]);
                let kind = if n0.dot(n1) < crease_cos { EdgeKind::Crease } else { EdgeKind::Smooth };
                out.push(Edge { a: u, b: v, kind, faces: [Some(*f0), Some(*f1)] });
            }
            _ => { /* non-manifold; skip */ }
        }
    }
    out
}

// ── Primitive builders ─────────────────────────────────────────────────────

pub fn cube(center: Vec3, size: f64) -> Mesh {
    let h = size * 0.5;
    let v = vec![
        Vec3::new(-h,-h,-h), Vec3::new( h,-h,-h), Vec3::new( h, h,-h), Vec3::new(-h, h,-h),
        Vec3::new(-h,-h, h), Vec3::new( h,-h, h), Vec3::new( h, h, h), Vec3::new(-h, h, h),
    ].into_iter().map(|p| p.add(center)).collect();
    // CCW when viewed from outside.
    let faces = vec![
        [0,2,1], [0,3,2], // -z bottom (looking from below, CCW: 0→3→2→1 traversed as 0→2→1, 0→3→2)
        [4,5,6], [4,6,7], // +z top
        [0,1,5], [0,5,4], // -y front
        [2,3,7], [2,7,6], // +y back
        [0,4,7], [0,7,3], // -x left
        [1,2,6], [1,6,5], // +x right
    ];
    Mesh::new(v, faces)
}

/// `n_lat` is the number of internal latitude rings between the poles,
/// `n_lon` the number of longitudinal slices. Both poles are single vertices
/// fanned to their adjacent rings (no degenerate triangles).
pub fn sphere(center: Vec3, radius: f64, n_lat: usize, n_lon: usize) -> Mesh {
    let mut v = Vec::new();

    let north = v.len();
    v.push(center.add(Vec3::new(0.0, 0.0,  radius)));

    let mut ring = vec![vec![0usize; n_lon]; n_lat];
    for i in 0..n_lat {
        let theta = std::f64::consts::PI * (i + 1) as f64 / (n_lat + 1) as f64;
        let (st, ct) = theta.sin_cos();
        for j in 0..n_lon {
            let phi = 2.0 * std::f64::consts::PI * j as f64 / n_lon as f64;
            let (sp, cp) = phi.sin_cos();
            ring[i][j] = v.len();
            v.push(center.add(Vec3::new(radius * st * cp, radius * st * sp, radius * ct)));
        }
    }

    let south = v.len();
    v.push(center.add(Vec3::new(0.0, 0.0, -radius)));

    let mut faces = Vec::new();
    // North polar cap (CCW viewed from +z = outside).
    for j in 0..n_lon {
        let j_n = (j + 1) % n_lon;
        faces.push([north, ring[0][j], ring[0][j_n]]);
    }
    // Middle stacks.
    for i in 0..n_lat - 1 {
        for j in 0..n_lon {
            let j_n = (j + 1) % n_lon;
            let a = ring[i][j];
            let b = ring[i][j_n];
            let c = ring[i + 1][j_n];
            let d = ring[i + 1][j];
            faces.push([a, b, c]);
            faces.push([a, c, d]);
        }
    }
    // South polar cap (CCW viewed from -z = outside; reverse winding).
    for j in 0..n_lon {
        let j_n = (j + 1) % n_lon;
        faces.push([south, ring[n_lat - 1][j_n], ring[n_lat - 1][j]]);
    }
    Mesh::with_crease_angle(v, faces, std::f64::consts::PI) // never crease — all smooth
}

pub fn cylinder(center: Vec3, radius: f64, height: f64, n: usize) -> Mesh {
    let h = height * 0.5;
    let mut v = Vec::new();
    // bottom ring (z=-h)
    for i in 0..n {
        let phi = 2.0 * std::f64::consts::PI * i as f64 / n as f64;
        v.push(center.add(Vec3::new(radius * phi.cos(), radius * phi.sin(), -h)));
    }
    // top ring (z=+h)
    for i in 0..n {
        let phi = 2.0 * std::f64::consts::PI * i as f64 / n as f64;
        v.push(center.add(Vec3::new(radius * phi.cos(), radius * phi.sin(),  h)));
    }
    let bot_center = v.len(); v.push(center.add(Vec3::new(0.0, 0.0, -h)));
    let top_center = v.len(); v.push(center.add(Vec3::new(0.0, 0.0,  h)));

    let mut faces = Vec::new();
    for i in 0..n {
        let i_next = (i + 1) % n;
        // side: quad (bot[i], bot[i_next], top[i_next], top[i])
        faces.push([i,        i_next,    n + i_next]);
        faces.push([i,        n + i_next, n + i]);
        // bottom cap (CCW viewed from below = -z): center, i_next, i
        faces.push([bot_center, i_next, i]);
        // top cap (CCW viewed from above = +z): center, n+i, n+i_next
        faces.push([top_center, n + i, n + i_next]);
    }
    Mesh::new(v, faces)
}

pub fn pyramid(center: Vec3, base: f64, height: f64) -> Mesh {
    let h = height * 0.5;
    let b = base * 0.5;
    let v = vec![
        Vec3::new(-b,-b,-h), Vec3::new( b,-b,-h), Vec3::new( b, b,-h), Vec3::new(-b, b,-h),
        Vec3::new( 0.0, 0.0,  h),
    ].into_iter().map(|p| p.add(center)).collect();
    let faces = vec![
        [0,2,1], [0,3,2], // base (-z, viewed from below, CCW)
        [0,1,4], [1,2,4], [2,3,4], [3,0,4], // sides
    ];
    Mesh::new(v, faces)
}

pub fn prism(center: Vec3, n: usize, radius: f64, height: f64) -> Mesh {
    let h     = height * 0.5;
    let mut v = Vec::new();
    for i in 0..n {
        let phi = 2.0 * std::f64::consts::PI * i as f64 / n as f64;
        v.push(center.add(Vec3::new(radius * phi.cos(), radius * phi.sin(), -h)));
    }
    for i in 0..n {
        let phi = 2.0 * std::f64::consts::PI * i as f64 / n as f64;
        v.push(center.add(Vec3::new(radius * phi.cos(), radius * phi.sin(),  h)));
    }
    let bot_center = v.len(); v.push(center.add(Vec3::new(0.0, 0.0, -h)));
    let top_center = v.len(); v.push(center.add(Vec3::new(0.0, 0.0,  h)));

    let mut faces = Vec::new();
    for i in 0..n {
        let i_next = (i + 1) % n;
        faces.push([i,          i_next,      n + i_next]);
        faces.push([i,          n + i_next,  n + i]);
        faces.push([bot_center, i_next,      i]);
        faces.push([top_center, n + i,       n + i_next]);
    }
    Mesh::new(v, faces)
}

// ── Render pipeline ────────────────────────────────────────────────────────

pub struct Scene {
    pub objects: Vec<Mesh>,
}

struct Projected<'a> {
    mesh:        &'a Mesh,
    verts2d:     Vec<(f64, f64)>,
    depths:      Vec<f64>,     // view-space depth per vertex
    front:       Vec<bool>,    // per face
    silhouette:  Vec<Vec<(f64, f64)>>, // one or more closed 2D loops (polygon-with-holes)
    sil_bbox:    (f64, f64, f64, f64), // (min_x, min_y, max_x, max_y) over all loops
    depth:       f64,          // representative depth (mean of vertex depths)
}

pub fn render(scene: &Scene, camera: &Camera) -> Vec<Path<f64>> {
    let projected: Vec<Projected> = scene.objects.iter().map(|mesh| project(mesh, camera)).collect();

    // For each object, collect its visible edges as 2D segments.
    // Then clip each segment against the silhouette polygons of all closer objects.
    let mut all_paths: Vec<Path<f64>> = Vec::new();

    for (i, p) in projected.iter().enumerate() {
        let mut segments: Vec<((f64, f64), (f64, f64))> = Vec::new();

        // Pass 1: collect visible mesh edges. For non-convex meshes, apply Appel's
        // Quantitative Invisibility (QI) for self-occlusion.
        let do_self_occlusion = !p.mesh.assume_convex;
        // Precompute silhouette edges (front/back boundary) for QI — only where the
        // facing switches, which is where QI changes as we trace along an edge.
        let sil_for_qi: Vec<(usize, usize, usize)> = if do_self_occlusion {
            p.mesh.edges.iter().filter_map(|e| {
                let f0 = e.faces[0].map(|f| p.front[f]).unwrap_or(false);
                let f1 = e.faces[1].map(|f| p.front[f]).unwrap_or(false);
                if f0 == f1 { return None; }
                let fi = if f0 { e.faces[0].unwrap() } else { e.faces[1].unwrap() };
                Some((e.a, e.b, fi))
            }).collect()
        } else { Vec::new() };

        for e in &p.mesh.edges {
            let visible = match e.kind {
                EdgeKind::Crease => {
                    let f0 = e.faces[0].map(|f| p.front[f]).unwrap_or(false);
                    let f1 = e.faces[1].map(|f| p.front[f]).unwrap_or(false);
                    f0 || f1
                }
                EdgeKind::Smooth => {
                    let f0 = e.faces[0].map(|f| p.front[f]).unwrap_or(false);
                    let f1 = e.faces[1].map(|f| p.front[f]).unwrap_or(false);
                    f0 != f1
                }
                EdgeKind::Boundary => {
                    e.faces[0].map(|f| p.front[f]).unwrap_or(false)
                }
            };
            if !visible { continue; }
            let seg = match camera.project_segment(p.mesh.vertices[e.a], p.mesh.vertices[e.b]) {
                Some(s) => s,
                None    => continue,
            };

            if !do_self_occlusion {
                segments.push(seg);
                continue;
            }

            // Self-occlusion via Appel's Quantitative Invisibility:
            // compute initial QI at edge start, track QI changes at silhouette crossings,
            // emit only QI=0 sub-segments.
            let ea = (seg.0.0, seg.0.1, p.depths[e.a]);
            let eb = (seg.1.0, seg.1.1, p.depths[e.b]);
            for s in appel_qi_segments(ea, eb, &sil_for_qi, &p.mesh.faces, &p.front, &p.verts2d, &p.depths) {
                segments.push(s);
            }
        }

        // Textured faces: emit paths if the face is front-facing (normal points toward camera).
        for tex in &p.mesh.textured_faces {
            // Front-facing: outward normal points toward eye for any face centroid.
            let centroid = tex.origin.add(tex.u_axis.scale(0.5)).add(tex.v_axis.scale(0.5));
            if tex.normal().dot(camera.eye.sub(centroid)) <= 0.0 { continue; }
            for path in &tex.paths {
                for w in path.windows(2) {
                    let p_a = tex.map(w[0]);
                    let p_b = tex.map(w[1]);
                    if let Some(seg) = camera.project_segment(p_a, p_b) {
                        segments.push(seg);
                    }
                }
            }
        }

        // Clip against all closer objects' silhouettes. Quick segment-bbox vs silhouette-bbox
        // overlap test skips the polygon clip when they can't possibly intersect.
        for (j, q) in projected.iter().enumerate() {
            if i == j || q.depth >= p.depth || q.silhouette.is_empty() { continue; }
            let (qx0, qy0, qx1, qy1) = q.sil_bbox;
            segments = segments.into_iter().flat_map(|s| {
                let s_x0 = s.0.0.min(s.1.0);
                let s_x1 = s.0.0.max(s.1.0);
                let s_y0 = s.0.1.min(s.1.1);
                let s_y1 = s.0.1.max(s.1.1);
                if s_x1 < qx0 || s_x0 > qx1 || s_y1 < qy0 || s_y0 > qy1 {
                    vec![s] // bboxes disjoint — segment passes through unchanged
                } else {
                    clip_segment_against_loops(s, &q.silhouette)
                }
            }).collect();
        }

        for (a, b) in segments {
            all_paths.push(Path::new(vec![
                Vec2d { x: a.0, y: a.1 },
                Vec2d { x: b.0, y: b.1 },
            ]));
        }
    }

    all_paths
}

fn project<'a>(mesh: &'a Mesh, cam: &Camera) -> Projected<'a> {
    let proj: Vec<(f64, f64, f64)> = mesh.vertices.iter().map(|v| cam.project(*v)).collect();
    let verts2d: Vec<(f64, f64)>   = proj.iter().map(|p| (p.0, p.1)).collect();
    let depths:  Vec<f64>          = proj.iter().map(|p| p.2).collect();
    // Front-facing test in 3D — robust when vertices are behind/near the camera.
    let front: Vec<bool> = mesh.faces.iter().map(|f| {
        let a = mesh.vertices[f[0]];
        let b = mesh.vertices[f[1]];
        let c = mesh.vertices[f[2]];
        let n = b.sub(a).cross(c.sub(a));
        let centroid = Vec3::new((a.x + b.x + c.x) / 3.0, (a.y + b.y + c.y) / 3.0, (a.z + b.z + c.z) / 3.0);
        let to_cam = cam.eye.sub(centroid);
        n.dot(to_cam) > 0.0 // outward normal points toward eye = visible
    }).collect();

    // Silhouette: convex hull of projected vertices, but only those at least at the near plane.
    // If any vertex is behind near, the projected hull would balloon to infinity, so we'd rather
    // skip this object's silhouette (no occlusion against farther objects) than emit a wrong polygon.
    let any_behind = depths.iter().any(|&d| d < cam.near);
    let silhouette = if any_behind { Vec::new() } else { build_silhouette_loops(mesh, &front, &verts2d) };
    let depth      = depths.iter().sum::<f64>() / depths.len().max(1) as f64;

    let sil_bbox = if silhouette.is_empty() {
        (0.0, 0.0, 0.0, 0.0)
    } else {
        let mut bx0 = f64::INFINITY; let mut bx1 = f64::NEG_INFINITY;
        let mut by0 = f64::INFINITY; let mut by1 = f64::NEG_INFINITY;
        for loop_v in &silhouette {
            for &(x, y) in loop_v {
                if x < bx0 { bx0 = x; } if x > bx1 { bx1 = x; }
                if y < by0 { by0 = y; } if y > by1 { by1 = y; }
            }
        }
        (bx0, by0, bx1, by1)
    };

    Projected { mesh, verts2d, depths, front, silhouette, sil_bbox, depth }
}

/// Walk silhouette edges (front face on one side, back on the other) into one or more
/// closed loops. For convex shapes this gives a single loop = the projected outline.
/// For shapes with holes (genus > 0), or non-convex outlines, multiple loops result.
/// The polygon-with-holes is later interpreted with the even-odd fill rule.
fn build_silhouette_loops(
    mesh: &Mesh,
    front: &[bool],
    verts2d: &[(f64, f64)],
) -> Vec<Vec<(f64, f64)>> {
    let mut sil_edges: Vec<(usize, usize)> = Vec::new();
    for e in &mesh.edges {
        let f0 = e.faces[0].map(|f| front[f]).unwrap_or(false);
        let f1 = e.faces[1].map(|f| front[f]).unwrap_or(false);
        if f0 != f1 { sil_edges.push((e.a, e.b)); }
    }
    if sil_edges.is_empty() { return Vec::new(); }

    let mut adj: HashMap<usize, Vec<usize>> = HashMap::new();
    for &(a, b) in &sil_edges {
        adj.entry(a).or_default().push(b);
        adj.entry(b).or_default().push(a);
    }

    use std::collections::HashSet;
    let edge_key = |a: usize, b: usize| if a < b { (a, b) } else { (b, a) };
    let mut visited: HashSet<(usize, usize)> = HashSet::new();
    let mut loops: Vec<Vec<(f64, f64)>> = Vec::new();

    for &(start_a, start_b) in &sil_edges {
        let key = edge_key(start_a, start_b);
        if visited.contains(&key) { continue; }
        visited.insert(key);

        let mut loop_v = vec![start_a];
        let mut prev = start_a;
        let mut cur  = start_b;
        loop {
            loop_v.push(cur);
            if cur == start_a { break; }
            let nbrs = match adj.get(&cur) { Some(n) => n, None => break };
            // Prefer an unvisited neighbor that isn't `prev` (continues the loop).
            let next = nbrs.iter().copied().find(|&nv| nv != prev && !visited.contains(&edge_key(cur, nv)))
                .or_else(|| nbrs.iter().copied().find(|&nv| !visited.contains(&edge_key(cur, nv))));
            match next {
                None => break,
                Some(nv) => {
                    visited.insert(edge_key(cur, nv));
                    prev = cur;
                    cur  = nv;
                }
            }
            if loop_v.len() > sil_edges.len() + 4 { break; } // safety
        }
        // Drop the duplicate closing vertex if the loop closed back to start.
        if loop_v.len() >= 2 && *loop_v.last().unwrap() == loop_v[0] { loop_v.pop(); }
        if loop_v.len() >= 3 {
            loops.push(loop_v.into_iter().map(|i| verts2d[i]).collect());
        }
    }
    loops
}

// ── 2D clipping helpers ────────────────────────────────────────────────────

/// Clip a 2D segment against a polygon-with-holes (multi-loop), using even-odd fill rule.
/// Returns the parts of the segment that are *outside* the filled region.
fn clip_segment_against_loops(
    seg:   ((f64, f64), (f64, f64)),
    loops: &[Vec<(f64, f64)>],
) -> Vec<((f64, f64), (f64, f64))> {
    if loops.is_empty() || loops.iter().all(|l| l.len() < 3) { return vec![seg]; }
    let (s, e) = seg;
    let mut ts: Vec<f64> = vec![0.0, 1.0];
    for loop_v in loops {
        if loop_v.len() < 2 { continue; }
        for i in 0..loop_v.len() {
            let p1 = loop_v[i];
            let p2 = loop_v[(i + 1) % loop_v.len()];
            if let Some(t) = segment_intersection_t(s, e, p1, p2) {
                if t > 1e-9 && t < 1.0 - 1e-9 { ts.push(t); }
            }
        }
    }
    ts.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let mut out = Vec::new();
    for w in ts.windows(2) {
        let (t0, t1) = (w[0], w[1]);
        if t1 - t0 < 1e-9 { continue; }
        let mid_t = 0.5 * (t0 + t1);
        let mid = (s.0 + (e.0 - s.0) * mid_t, s.1 + (e.1 - s.1) * mid_t);
        if !point_in_loops(mid, loops) {
            let p0 = (s.0 + (e.0 - s.0) * t0, s.1 + (e.1 - s.1) * t0);
            let p1 = (s.0 + (e.0 - s.0) * t1, s.1 + (e.1 - s.1) * t1);
            out.push((p0, p1));
        }
    }
    out
}

/// Returns `t` along segment a→b where it crosses segment c→d, if any (in `[0,1]`).
fn segment_intersection_t(a: (f64, f64), b: (f64, f64), c: (f64, f64), d: (f64, f64)) -> Option<f64> {
    let r = (b.0 - a.0, b.1 - a.1);
    let s = (d.0 - c.0, d.1 - c.1);
    let denom = r.0 * s.1 - r.1 * s.0;
    if denom.abs() < 1e-12 { return None; }
    let qp = (c.0 - a.0, c.1 - a.1);
    let t = (qp.0 * s.1 - qp.1 * s.0) / denom;
    let u = (qp.0 * r.1 - qp.1 * r.0) / denom;
    if t < -1e-9 || t > 1.0 + 1e-9 || u < -1e-9 || u > 1.0 + 1e-9 { return None; }
    Some(t.clamp(0.0, 1.0))
}

/// Appel's Quantitative Invisibility self-occlusion for one edge.
///
/// `ea`/`eb`: (x2d, y2d, depth) at the edge's start and end.
/// `sil`: silhouette edges of the same mesh — (vertex_a, vertex_b, front_face_idx).
///        QI changes by ±1 each time the edge crosses one of these in 2D (if the
///        silhouette edge is in front of the test edge at the crossing).
///
/// Returns the QI=0 (visible) sub-segments.
fn appel_qi_segments(
    ea: (f64, f64, f64),
    eb: (f64, f64, f64),
    sil: &[(usize, usize, usize)],
    faces: &[[usize; 3]],
    front: &[bool],
    verts2d: &[(f64, f64)],
    depths: &[f64],
) -> Vec<((f64, f64), (f64, f64))> {
    // ── Step 1: initial QI at edge start ─────────────────────────────────────
    // Count front-facing triangles whose 2D projection covers ea AND that are
    // clearly closer to the camera (depth < ea.2).  Co-surface triangles are
    // at the same depth as ea, so the 1e-4 guard excludes them.
    let mut qi: i32 = 0;
    {
        let pt = (ea.0, ea.1);
        for (ti, tri) in faces.iter().enumerate() {
            if !front[ti] { continue; }
            let tv = [verts2d[tri[0]], verts2d[tri[1]], verts2d[tri[2]]];
            // bbox pre-filter
            if pt.0 < tv[0].0.min(tv[1].0).min(tv[2].0) { continue; }
            if pt.0 > tv[0].0.max(tv[1].0).max(tv[2].0) { continue; }
            if pt.1 < tv[0].1.min(tv[1].1).min(tv[2].1) { continue; }
            if pt.1 > tv[0].1.max(tv[1].1).max(tv[2].1) { continue; }
            if !point_in_triangle(pt, &tv) { continue; }
            let td = [depths[tri[0]], depths[tri[1]], depths[tri[2]]];
            let denom = (tv[1].1 - tv[2].1) * (tv[0].0 - tv[2].0)
                      + (tv[2].0 - tv[1].0) * (tv[0].1 - tv[2].1);
            if denom.abs() < 1e-9 { continue; }
            let tri_d = barycentric_depth(pt, &tv, &td);
            if tri_d < ea.2 - 1e-4 { qi += 1; }
        }
    }

    // ── Step 2: silhouette crossings along E ─────────────────────────────────
    // For each silhouette edge S that crosses E in 2D and is in front of E at
    // the crossing, QI changes by ±1 depending on which side of S the front face is on.
    let e_dir = (eb.0 - ea.0, eb.1 - ea.1);
    let mut events: Vec<(f64, i32)> = Vec::new();
    for &(va, vb, fi) in sil {
        let sa = verts2d[va];
        let sb = verts2d[vb];
        let s_dir = (sb.0 - sa.0, sb.1 - sa.1);

        // 2D intersection: E at parameter t, S at parameter u.
        let denom = e_dir.0 * s_dir.1 - e_dir.1 * s_dir.0;
        if denom.abs() < 1e-12 { continue; }
        let diff = (sa.0 - ea.0, sa.1 - ea.1);
        let t = (diff.0 * s_dir.1 - diff.1 * s_dir.0) / denom;
        let u = (diff.0 * e_dir.1 - diff.1 * e_dir.0) / denom;
        if t <= 1e-9 || t >= 1.0 - 1e-9 { continue; }  // outside E
        if u < -1e-9 || u > 1.0 + 1e-9  { continue; }  // outside S

        // Depth check: S must be in front of E at this crossing point.
        let de = ea.2 + (eb.2 - ea.2) * t;
        let ds = depths[va] + (depths[vb] - depths[va]) * u;
        if ds >= de - 1e-4 { continue; }  // S is behind or co-depth with E

        // Sign: which side of S is the front-facing face on?
        // fc_side > 0 → front face is to the LEFT of S (when walking sa → sb).
        let f = faces[fi];
        let fc = (
            (verts2d[f[0]].0 + verts2d[f[1]].0 + verts2d[f[2]].0) / 3.0,
            (verts2d[f[0]].1 + verts2d[f[1]].1 + verts2d[f[2]].1) / 3.0,
        );
        let to_fc  = (fc.0 - sa.0, fc.1 - sa.1);
        let fc_side = s_dir.0 * to_fc.1 - s_dir.1 * to_fc.0;
        if fc_side.abs() < 1e-9 { continue; }  // degenerate: centroid on S line

        // denom = cross(e_dir, s_dir).
        // denom > 0 → E crosses S from left to right (relative to S direction).
        // Entering front-face side → QI++; leaving → QI--.
        let delta: i32 = if (denom > 0.0) == (fc_side > 0.0) { -1 } else { 1 };
        events.push((t, delta));
    }
    events.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

    // ── Step 3: emit QI=0 segments ───────────────────────────────────────────
    let lerp = |t: f64| -> (f64, f64) {
        (ea.0 + (eb.0 - ea.0) * t, ea.1 + (eb.1 - ea.1) * t)
    };
    let mut result = Vec::new();
    let mut prev_t = 0.0_f64;
    for (t, delta) in events {
        if t > prev_t + 1e-9 && qi == 0 {
            result.push((lerp(prev_t), lerp(t)));
        }
        qi += delta;
        qi = qi.max(0); // clamp: QI should never underflow, but guard against fp noise
        prev_t = t;
    }
    if 1.0 - prev_t > 1e-9 && qi == 0 {
        result.push((lerp(prev_t), lerp(1.0)));
    }
    result
}

fn point_in_triangle(p: (f64, f64), tri: &[(f64, f64); 3]) -> bool {
    let sign = |a: (f64, f64), b: (f64, f64), c: (f64, f64)| -> f64 {
        (a.0 - c.0) * (b.1 - c.1) - (b.0 - c.0) * (a.1 - c.1)
    };
    let d1 = sign(p, tri[0], tri[1]);
    let d2 = sign(p, tri[1], tri[2]);
    let d3 = sign(p, tri[2], tri[0]);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}

fn barycentric_depth(p: (f64, f64), tri: &[(f64, f64); 3], depths: &[f64; 3]) -> f64 {
    let (x, y)   = p;
    let (x1, y1) = tri[0];
    let (x2, y2) = tri[1];
    let (x3, y3) = tri[2];
    let denom = (y2 - y3) * (x1 - x3) + (x3 - x2) * (y1 - y3);
    if denom.abs() < 1e-12 {
        return (depths[0] + depths[1] + depths[2]) / 3.0;
    }
    let w1 = ((y2 - y3) * (x - x3) + (x3 - x2) * (y - y3)) / denom;
    let w2 = ((y3 - y1) * (x - x3) + (x1 - x3) * (y - y3)) / denom;
    let w3 = 1.0 - w1 - w2;
    w1 * depths[0] + w2 * depths[1] + w3 * depths[2]
}

/// Even-odd point-in-polygon for a polygon-with-holes (multiple loops).
fn point_in_loops(p: (f64, f64), loops: &[Vec<(f64, f64)>]) -> bool {
    let mut crossings = 0usize;
    for loop_v in loops {
        let n = loop_v.len();
        if n < 2 { continue; }
        let mut j = n - 1;
        for i in 0..n {
            let (xi, yi) = loop_v[i];
            let (xj, yj) = loop_v[j];
            if (yi > p.1) != (yj > p.1) && p.0 < (xj - xi) * (p.1 - yi) / (yj - yi) + xi {
                crossings += 1;
            }
            j = i;
        }
    }
    crossings & 1 == 1
}

#[allow(dead_code)]
fn point_in_polygon(p: (f64, f64), poly: &[(f64, f64)]) -> bool {
    let mut inside = false;
    let n = poly.len();
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = poly[i];
        let (xj, yj) = poly[j];
        let crosses = ((yi > p.1) != (yj > p.1))
            && (p.0 < (xj - xi) * (p.1 - yi) / (yj - yi) + xi);
        if crosses { inside = !inside; }
        j = i;
    }
    inside
}
