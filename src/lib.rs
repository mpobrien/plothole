pub mod font;
pub mod hershey;
pub mod iosevka;
pub mod motion;
pub mod optimize;
pub mod scene3d;
pub mod ttf;

use font::{Font, Path, Vec2d};
use motion::{AccelerationProfile, plan_path};
use optimize::{HeldKarp, NearestNeighbor, PathEndpoints, PathOptimizer, HELD_KARP_LIMIT};
use ttf::TtfFont;

#[cfg(feature = "wasm")]
use wasm_bindgen::prelude::*;
#[cfg(feature = "wasm")]
use web_sys::HtmlCanvasElement;

// ── Default motion profile (matches CLI defaults) ─────────────────────────────

pub const DEFAULT_MAX_VELOCITY: f64 = 500.0;
pub const DEFAULT_ACCELERATION: f64 = 2000.0;
pub const DEFAULT_CORNERING:    f64 = 1.0;

// ── Internal timeline types ───────────────────────────────────────────────────

struct Segment {
    pen_down:     bool,
    points:       Vec<(f64, f64)>,
    plan:         motion::Plan,
    /// Cumulative arc-length at each point index.
    seg_starts:   Vec<f64>,
    total_length: f64,
    start_time:   f64,
}

// Plan is not Send/Sync; single-threaded WASM is fine.
unsafe impl Send for Segment {}
unsafe impl Sync for Segment {}

// ── PlotRenderer ─────────────────────────────────────────────────────────────

#[cfg_attr(feature = "wasm", wasm_bindgen)]
pub struct PlotRenderer {
    timeline:       Vec<Segment>,
    total_duration: f64,
    min_x: f64, min_y: f64,
    max_x: f64, max_y: f64,
}

// ── Core constructors (no wasm-bindgen, callable from native Rust too) ────────

impl PlotRenderer {
    /// `pen_up_speed` multiplies max-velocity and acceleration for pen-up segments only.
    /// Use 1.0 to disable; higher values let the pen rocket between strokes.
    pub fn from_grouped(
        grouped: Vec<Vec<Path<f64>>>,
        max_velocity: f64,
        acceleration: f64,
        cornering: f64,
        pen_up_speed: f64,
    ) -> Self {
        let flat = optimize_path_order(grouped, 0.1 / 0.3528);
        let down = AccelerationProfile { maximum_velocity: max_velocity,                acceleration,                              cornering_factor: cornering };
        let up   = AccelerationProfile { maximum_velocity: max_velocity * pen_up_speed, acceleration: acceleration * pen_up_speed, cornering_factor: cornering };
        build_timeline(flat, &down, &up)
    }

    pub fn new_hershey(text: &str, font_name: &str, scale: f64) -> Result<Self, String> {
        let fonts = hershey::fonts();
        let font = fonts.get(&font_name.to_uppercase() as &str)
            .ok_or_else(|| format!("unknown font \"{font_name}\""))?;
        let grouped = scale_grouped(hershey_text_to_paths(text, font), scale);
        Ok(Self::from_grouped(grouped, DEFAULT_MAX_VELOCITY, DEFAULT_ACCELERATION, DEFAULT_CORNERING, 1.0))
    }

    pub fn new_ttf(
        font_data:  &[u8],
        text:       &str,
        face_index: u32,
        axes:       &[(String, f32)],
        em_size:    f32,
        dp_epsilon: f64,
        scale:      f64,
    ) -> Result<Self, String> {
        let font = TtfFont::from_bytes(font_data, face_index).map_err(|e| e.to_string())?;
        let grouped = scale_grouped(font.text_to_paths(text, em_size, dp_epsilon, axes), scale);
        Ok(Self::from_grouped(grouped, DEFAULT_MAX_VELOCITY, DEFAULT_ACCELERATION, DEFAULT_CORNERING, 1.0))
    }

    /// Render text using the embedded Iosevka skeleton font.
    /// `em_size` controls glyph cap-height in path units; line breaking on `\n`.
    pub fn new_iosevka(text: &str, em_size: f64) -> Result<Self, String> {
        const SKELETON_JSON: &str = include_str!("../assets/iosevka_skeleton.json");
        let font = iosevka::IosevkaFont::from_json(SKELETON_JSON).map_err(|e| e.to_string())?;
        let grouped = font.text_to_paths(text, em_size, 0.0);
        Ok(Self::from_grouped(grouped, DEFAULT_MAX_VELOCITY, DEFAULT_ACCELERATION, DEFAULT_CORNERING, 1.0))
    }

    pub fn duration(&self) -> f64 { self.total_duration }
}

fn scale_grouped(paths: Vec<Vec<Path<f64>>>, scale: f64) -> Vec<Vec<Path<f64>>> {
    if (scale - 1.0).abs() < f64::EPSILON { return paths; }
    paths.into_iter().map(|group| {
        group.into_iter().map(|path| {
            Path::new(path.points().iter().map(|p| Vec2d::new(p.x * scale, p.y * scale)).collect())
        }).collect()
    }).collect()
}

// ── wasm-bindgen JS-facing API ────────────────────────────────────────────────

/// Returns a sorted list of all available Hershey font names.
#[cfg(feature = "wasm")]
#[wasm_bindgen(js_name = listFonts)]
pub fn js_list_fonts() -> Vec<String> {
    let mut names: Vec<String> = hershey::fonts().keys().cloned().collect();
    names.sort();
    names
}

/// Returns the available 3D scene preset names.
#[cfg(feature = "wasm")]
#[wasm_bindgen(js_name = listScene3dPresets)]
pub fn js_list_scene3d_presets() -> Vec<String> {
    scene3d::presets::names().iter().map(|s| s.to_string()).collect()
}

/// Cached 3D scene. Construct once per (preset, count) and call `render` for each
/// camera change to avoid rebuilding the scene on every slider tick.
#[cfg(feature = "wasm")]
#[wasm_bindgen]
pub struct Scene3d {
    scene:    scene3d::Scene,
    centroid: scene3d::Vec3,
}

#[cfg(feature = "wasm")]
#[wasm_bindgen]
impl Scene3d {
    #[wasm_bindgen(constructor)]
    pub fn new(preset: &str, swarm_count: u32) -> Result<Scene3d, JsValue> {
        let scene = scene3d::presets::build(preset, swarm_count as usize)
            .map_err(|e| JsValue::from_str(&e))?;
        let mut c = scene3d::Vec3::zero();
        let mut n = 0;
        for m in &scene.objects { for v in &m.vertices { c = c.add(*v); n += 1; } }
        let centroid = if n == 0 { c } else { c.scale(1.0 / n as f64) };
        Ok(Scene3d { scene, centroid })
    }

    /// Build the camera and run the hidden-line render pipeline. Internal helper.
    fn build_paths(&self, az_deg: f64, el_deg: f64, fov_deg: f64, distance: f64) -> Vec<Path<f64>> {
        use scene3d::{Vec3, Camera, render};
        let az_r = az_deg.to_radians();
        let el_r = el_deg.to_radians();
        let eye = Vec3::new(
            self.centroid.x + distance * el_r.cos() * az_r.cos(),
            self.centroid.y + distance * el_r.cos() * az_r.sin(),
            self.centroid.z + distance * el_r.sin(),
        );
        let cam = Camera {
            eye, target: self.centroid, up: Vec3::new(0.0, 0.0, 1.0),
            scale: 1.0, fov_deg, near: 0.1,
        };
        render(&self.scene, &cam)
    }

    /// Convert the rendered 3D scene to a `PlotRenderer` so the plotter motion can be animated.
    #[wasm_bindgen(js_name = toPlotRenderer)]
    pub fn js_to_plot_renderer(
        &self,
        azimuth_deg:     f64,
        elevation_deg:   f64,
        fov_deg:         f64,
        camera_distance: f64,
        max_velocity:    f64,
        acceleration:    f64,
        cornering:       f64,
        pen_up_speed:    f64,
    ) -> PlotRenderer {
        let paths = self.build_paths(azimuth_deg, elevation_deg, fov_deg, camera_distance);
        PlotRenderer::from_grouped(vec![paths], max_velocity, acceleration, cornering, pen_up_speed)
    }

    /// Render with the given camera params and draw to the canvas.
    pub fn render(
        &self,
        canvas:          &HtmlCanvasElement,
        azimuth_deg:     f64,
        elevation_deg:   f64,
        fov_deg:         f64,
        camera_distance: f64,
    ) -> Result<u32, JsValue> {
        use wasm_bindgen::JsCast;

        let paths = self.build_paths(azimuth_deg, elevation_deg, fov_deg, camera_distance);

    // Robust IQR-based bbox over all path coords (drops streak outliers).
    let mut xs: Vec<f64> = Vec::new();
    let mut ys: Vec<f64> = Vec::new();
    for p in &paths { for pt in p.points() { xs.push(pt.x); ys.push(pt.y); } }
    if xs.is_empty() { return Ok(0); }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let robust_bbox = |sorted: &[f64]| -> (f64, f64) {
        let n = sorted.len();
        let q1 = sorted[n / 4];
        let q3 = sorted[3 * n / 4];
        let iqr = q3 - q1;
        let lo = sorted.iter().find(|&&v| v >= q1 - 5.0 * iqr).copied().unwrap_or(sorted[0]);
        let hi = sorted.iter().rev().find(|&&v| v <= q3 + 5.0 * iqr).copied().unwrap_or(sorted[n - 1]);
        (lo, hi)
    };
    let (min_x, max_x) = robust_bbox(&xs);
    let (min_y, max_y) = robust_bbox(&ys);

    // Auto-fit to canvas.
    let ctx: web_sys::CanvasRenderingContext2d = canvas
        .get_context("2d").unwrap().unwrap()
        .dyn_into().unwrap();
    let cw = canvas.width()  as f64;
    let ch = canvas.height() as f64;
    let pad = 16.0;
    let world_w = (max_x - min_x).max(1e-9);
    let world_h = (max_y - min_y).max(1e-9);
    let s = ((cw - 2.0 * pad) / world_w).min((ch - 2.0 * pad) / world_h);
    let off_x = pad + (cw - 2.0 * pad - world_w * s) / 2.0 - min_x * s;
    let off_y = pad + (ch - 2.0 * pad - world_h * s) / 2.0 - min_y * s;

    // Liang-Barsky clip to the canvas rect.
    let clip_to_rect = |a: (f64, f64), b: (f64, f64)| -> Option<((f64, f64), (f64, f64))> {
        let (xmin, ymin) = (0.0, 0.0);
        let (xmax, ymax) = (cw, ch);
        let (mut t0, mut t1) = (0.0_f64, 1.0_f64);
        let dx = b.0 - a.0; let dy = b.1 - a.1;
        for &(p, q) in &[(-dx, a.0 - xmin), (dx, xmax - a.0), (-dy, a.1 - ymin), (dy, ymax - a.1)] {
            if p.abs() < 1e-12 { if q < 0.0 { return None; } }
            else {
                let r = q / p;
                if p < 0.0 { if r > t1 { return None; } if r > t0 { t0 = r; } }
                else        { if r < t0 { return None; } if r < t1 { t1 = r; } }
            }
        }
        Some(((a.0 + t0 * dx, a.1 + t0 * dy), (a.0 + t1 * dx, a.1 + t1 * dy)))
    };

    ctx.set_fill_style_str("white");
    ctx.fill_rect(0.0, 0.0, cw, ch);
    ctx.set_stroke_style_str("#141414");
    ctx.set_line_width(1.0);
    ctx.set_line_cap("round");

    let mut drawn = 0u32;
    ctx.begin_path();
    for path in &paths {
        let pts = path.points();
        if pts.len() < 2 { continue; }
        for w in pts.windows(2) {
            let p0 = (w[0].x * s + off_x, w[0].y * s + off_y);
            let p1 = (w[1].x * s + off_x, w[1].y * s + off_y);
            if let Some((c0, c1)) = clip_to_rect(p0, p1) {
                ctx.move_to(c0.0, c0.1);
                ctx.line_to(c1.0, c1.1);
                drawn += 1;
            }
        }
    }
    ctx.stroke();
    Ok(drawn)
    }
}

#[cfg(feature = "wasm")]
#[wasm_bindgen]
impl PlotRenderer {
    /// Construct from a Hershey font by name.
    #[wasm_bindgen(js_name = fromHershey)]
    pub fn js_from_hershey(text: &str, font_name: &str, scale: f64) -> Result<PlotRenderer, JsValue> {
        Self::new_hershey(text, font_name, scale).map_err(|e| JsValue::from_str(&e))
    }

    /// Construct from TTF font bytes.
    ///
    /// `axes_json`: JSON array of `[tag, value]` pairs, e.g. `[["wght",700]]`.
    #[wasm_bindgen(js_name = fromTtf)]
    pub fn js_from_ttf(
        font_data:  &[u8],
        text:       &str,
        face_index: u32,
        axes_json:  &str,
        em_size:    f32,
        dp_epsilon: f64,
        scale:      f64,
    ) -> Result<PlotRenderer, JsValue> {
        let axes: Vec<(String, f32)> = serde_json::from_str(axes_json)
            .map_err(|e| JsValue::from_str(&format!("axes_json parse error: {e}")))?;
        Self::new_ttf(font_data, text, face_index, &axes, em_size, dp_epsilon, scale)
            .map_err(|e| JsValue::from_str(&e))
    }

    /// Construct using the embedded Iosevka skeleton font.
    #[wasm_bindgen(js_name = fromIosevka)]
    pub fn js_from_iosevka(text: &str, em_size: f64) -> Result<PlotRenderer, JsValue> {
        Self::new_iosevka(text, em_size).map_err(|e| JsValue::from_str(&e))
    }

    /// Total plotter-time duration in seconds.
    #[wasm_bindgen(js_name = duration)]
    pub fn js_duration(&self) -> f64 { self.total_duration }

    /// Render the drawing state at plotter-time `t` onto `canvas`.
    #[wasm_bindgen(js_name = renderFrame)]
    pub fn js_render_frame(&self, canvas: &HtmlCanvasElement, t: f64) {
        use wasm_bindgen::JsCast;

        let ctx: web_sys::CanvasRenderingContext2d = canvas
            .get_context("2d").unwrap().unwrap()
            .dyn_into().unwrap();

        let width  = canvas.width()  as f64;
        let height = canvas.height() as f64;

        let padding = 20.0f64;
        let draw_w = (self.max_x - self.min_x).max(1e-9);
        let draw_h = (self.max_y - self.min_y).max(1e-9);
        let scale = ((width  - 2.0 * padding) / draw_w)
            .min( (height - 2.0 * padding) / draw_h);
        let off_x = padding + (width  - 2.0 * padding - draw_w * scale) / 2.0 - self.min_x * scale;
        let off_y = padding + (height - 2.0 * padding - draw_h * scale) / 2.0 - self.min_y * scale;

        let to_px = |x: f64, y: f64| -> (f64, f64) {
            (x * scale + off_x, y * scale + off_y)
        };

        ctx.set_fill_style_str("white");
        ctx.fill_rect(0.0, 0.0, width, height);

        ctx.set_stroke_style_str("#141414");
        ctx.set_line_width(1.5);
        ctx.set_line_cap("round");
        ctx.set_line_join("round");

        let mut pen_pos: Option<(f64, f64)> = None;

        for seg in &self.timeline {
            if t < seg.start_time { break; }

            let local_t  = t - seg.start_time;
            let duration = seg.plan.duration();

            let drawn_dist = if local_t >= duration {
                seg.total_length
            } else {
                seg.plan.instant(local_t).distance_m.min(seg.total_length)
            };

            pen_pos = if local_t >= duration {
                seg.points.last().map(|&(x, y)| to_px(x, y))
            } else {
                let inst = seg.plan.instant(local_t);
                Some(to_px(inst.position.x, inst.position.y))
            };

            if !seg.pen_down { continue; }

            ctx.begin_path();
            let mut started = false;
            for (i, w) in seg.points.windows(2).enumerate() {
                let s0 = seg.seg_starts[i];
                let s1 = seg.seg_starts[i + 1];
                if s0 >= drawn_dist { break; }

                let (px1, py1) = to_px(w[0].0, w[0].1);
                if !started {
                    ctx.move_to(px1, py1);
                    started = true;
                }
                let (px2, py2) = if s1 <= drawn_dist {
                    to_px(w[1].0, w[1].1)
                } else {
                    let frac = (drawn_dist - s0) / (s1 - s0).max(1e-9);
                    to_px(
                        w[0].0 + frac * (w[1].0 - w[0].0),
                        w[0].1 + frac * (w[1].1 - w[0].1),
                    )
                };
                ctx.line_to(px2, py2);
            }
            ctx.stroke();
        }

        if let Some((px, py)) = pen_pos {
            ctx.set_fill_style_str("rgb(220,50,50)");
            ctx.begin_path();
            ctx.arc(px, py, 4.0, 0.0, std::f64::consts::TAU).unwrap();
            ctx.fill();
        }
    }
}

// ── Native rendering (tiny-skia) ─────────────────────────────────────────────

impl PlotRenderer {
    /// Render the drawing state at plotter-time `t` into a tiny-skia pixmap.
    pub fn render_frame_native(&self, pixmap: &mut tiny_skia::PixmapMut, t: f64) {
        use tiny_skia::*;

        let width  = pixmap.width()  as f64;
        let height = pixmap.height() as f64;

        pixmap.fill(Color::WHITE);

        let padding = 20.0_f64;
        let draw_w  = (self.max_x - self.min_x).max(1e-9);
        let draw_h  = (self.max_y - self.min_y).max(1e-9);
        let scale   = ((width  - 2.0 * padding) / draw_w)
            .min( (height - 2.0 * padding) / draw_h);
        let off_x   = padding + (width  - 2.0 * padding - draw_w * scale) / 2.0 - self.min_x * scale;
        let off_y   = padding + (height - 2.0 * padding - draw_h * scale) / 2.0 - self.min_y * scale;

        let to_px = |x: f64, y: f64| -> (f32, f32) {
            ((x * scale + off_x) as f32, (y * scale + off_y) as f32)
        };

        let mut ink_paint = Paint::default();
        ink_paint.set_color_rgba8(20, 20, 20, 255);
        ink_paint.anti_alias = true;
        let mut ink_stroke = Stroke::default();
        ink_stroke.width = 1.5;

        let mut dot_paint = Paint::default();
        dot_paint.set_color_rgba8(220, 50, 50, 255);
        dot_paint.anti_alias = true;

        let mut pen_pos: Option<(f32, f32)> = None;

        for seg in &self.timeline {
            if t < seg.start_time { break; }

            let local_t  = t - seg.start_time;
            let duration = seg.plan.duration();

            let drawn_dist = if local_t >= duration {
                seg.total_length
            } else {
                seg.plan.instant(local_t).distance_m.min(seg.total_length)
            };

            pen_pos = if local_t >= duration {
                seg.points.last().map(|&(x, y)| to_px(x, y))
            } else {
                let inst = seg.plan.instant(local_t);
                Some(to_px(inst.position.x, inst.position.y))
            };

            if !seg.pen_down { continue; }

            for (i, w) in seg.points.windows(2).enumerate() {
                let s0 = seg.seg_starts[i];
                let s1 = seg.seg_starts[i + 1];
                if s0 >= drawn_dist { break; }

                let (px1, py1) = to_px(w[0].0, w[0].1);
                let (px2, py2) = if s1 <= drawn_dist {
                    to_px(w[1].0, w[1].1)
                } else {
                    let frac = (drawn_dist - s0) / (s1 - s0).max(1e-9);
                    to_px(
                        w[0].0 + frac * (w[1].0 - w[0].0),
                        w[0].1 + frac * (w[1].1 - w[0].1),
                    )
                };

                let mut pb = PathBuilder::new();
                pb.move_to(px1, py1);
                pb.line_to(px2, py2);
                if let Some(path) = pb.finish() {
                    pixmap.stroke_path(&path, &ink_paint, &ink_stroke, Transform::identity(), None);
                }
            }
        }

        if let Some((cx, cy)) = pen_pos {
            if let Some(dot) = PathBuilder::from_circle(cx, cy, 3.5) {
                pixmap.fill_path(&dot, &dot_paint, FillRule::Winding, Transform::identity(), None);
            }
        }
    }

    /// Like `render_frame_native` but draws the full motion path including pen-up
    /// travels, with colored dots at each lift/touch point.
    ///
    /// - Pen-down strokes: dark ink
    /// - Pen-up travels: light blue
    /// - Pen-touch dots (pen down): green
    /// - Pen-lift dots (pen up): orange
    #[cfg(feature = "native")]
    pub fn render_preview_native(&self, pixmap: &mut tiny_skia::PixmapMut) {
        use tiny_skia::*;

        let width  = pixmap.width()  as f64;
        let height = pixmap.height() as f64;

        pixmap.fill(Color::WHITE);

        let padding = 20.0_f64;
        let draw_w  = (self.max_x - self.min_x).max(1e-9);
        let draw_h  = (self.max_y - self.min_y).max(1e-9);
        let scale   = ((width  - 2.0 * padding) / draw_w)
            .min( (height - 2.0 * padding) / draw_h);
        let off_x   = padding + (width  - 2.0 * padding - draw_w * scale) / 2.0 - self.min_x * scale;
        let off_y   = padding + (height - 2.0 * padding - draw_h * scale) / 2.0 - self.min_y * scale;

        let to_px = |x: f64, y: f64| -> (f32, f32) {
            ((x * scale + off_x) as f32, (y * scale + off_y) as f32)
        };

        let mut ink_paint = Paint::default();
        ink_paint.set_color_rgba8(20, 20, 20, 255);
        ink_paint.anti_alias = true;
        let mut ink_stroke = Stroke::default();
        ink_stroke.width = 1.5;

        let mut up_paint = Paint::default();
        up_paint.set_color_rgba8(100, 160, 220, 180);
        up_paint.anti_alias = true;
        let mut up_stroke = Stroke::default();
        up_stroke.width = 0.8;

        let mut touch_paint = Paint::default();  // pen-down start: green
        touch_paint.set_color_rgba8(30, 160, 60, 220);
        touch_paint.anti_alias = true;

        let mut lift_paint = Paint::default();   // pen-up start: orange
        lift_paint.set_color_rgba8(220, 110, 20, 220);
        lift_paint.anti_alias = true;

        let dot_r = (scale * draw_w.min(draw_h) * 0.012).clamp(2.0, 6.0) as f32;

        for (i, seg) in self.timeline.iter().enumerate() {
            if seg.points.len() < 2 { continue; }

            let (paint, stroke) = if seg.pen_down {
                (&ink_paint, &ink_stroke)
            } else {
                (&up_paint, &up_stroke)
            };

            let mut pb = PathBuilder::new();
            let (fx, fy) = to_px(seg.points[0].0, seg.points[0].1);
            pb.move_to(fx, fy);
            for &(x, y) in &seg.points[1..] {
                let (px, py) = to_px(x, y);
                pb.line_to(px, py);
            }
            if let Some(path) = pb.finish() {
                pixmap.stroke_path(&path, paint, stroke, Transform::identity(), None);
            }

            // At a pen-down transition (previous segment was pen-up, this is pen-down):
            // draw a green dot at the touch point.
            // At a pen-up transition (previous segment was pen-down, this is pen-up):
            // draw an orange dot at the lift point.
            let prev_pen_down = i > 0 && self.timeline[i - 1].pen_down;
            let dot_paint = if seg.pen_down && !prev_pen_down {
                Some(&touch_paint)
            } else if !seg.pen_down && prev_pen_down {
                Some(&lift_paint)
            } else {
                None
            };
            if let Some(dp) = dot_paint {
                let (cx, cy) = to_px(seg.points[0].0, seg.points[0].1);
                if let Some(dot) = PathBuilder::from_circle(cx, cy, dot_r) {
                    pixmap.fill_path(&dot, dp, FillRule::Winding, Transform::identity(), None);
                }
            }
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Merge consecutive strokes whose endpoints are within `tol` of each other
/// into a single continuous path, avoiding unnecessary pen-up/down between them.
fn chain_merge_strokes(strokes: Vec<Path<f64>>, tol: f64) -> Vec<Path<f64>> {
    if strokes.is_empty() { return strokes; }
    let mut result: Vec<Path<f64>> = Vec::new();
    let mut current: Vec<Vec2d<f64>> = strokes[0].points().clone();

    for stroke in strokes.into_iter().skip(1) {
        let pts = stroke.points();
        if pts.is_empty() { continue; }
        let last  = current.last().unwrap().clone();
        let first = pts[0].clone();
        let dist  = ((last.x - first.x).powi(2) + (last.y - first.y).powi(2)).sqrt();
        if dist <= tol {
            current.extend_from_slice(&pts[1..]);
        } else {
            result.push(Path::new(current));
            current = pts.clone();
        }
    }
    result.push(Path::new(current));
    result
}

fn optimize_path_order(grouped: Vec<Vec<Path<f64>>>, merge_tol: f64) -> Vec<Path<f64>> {
    // Pre-filter so we can index ahead to the next non-empty group.
    let groups: Vec<Vec<Path<f64>>> = grouped.into_iter()
        .map(|g| g.into_iter().filter(|p| !p.points().is_empty()).collect::<Vec<_>>())
        .filter(|g| !g.is_empty())
        .collect();

    let mut result = vec![];
    let mut pen = (0.0f64, 0.0f64);

    for (gi, group) in groups.iter().enumerate() {
        let endpoints: Vec<PathEndpoints> = group.iter().map(|p| PathEndpoints {
            start: (p.start().x, p.start().y),
            end:   (p.end().x,   p.end().y),
        }).collect();

        // Hint the optimizer towards whichever endpoint in the next group is
        // nearest to the current pen, so the current group exits close to it.
        let exit_target = groups.get(gi + 1).map(|next| {
            next.iter()
                .flat_map(|p| [(p.start().x, p.start().y), (p.end().x, p.end().y)])
                .min_by(|a, b| {
                    let da = (a.0 - pen.0).powi(2) + (a.1 - pen.1).powi(2);
                    let db = (b.0 - pen.0).powi(2) + (b.1 - pen.1).powi(2);
                    da.partial_cmp(&db).unwrap()
                })
                .unwrap()
        });

        let order = if endpoints.len() <= HELD_KARP_LIMIT {
            HeldKarp.optimize(&endpoints, pen, exit_target)
        } else {
            NearestNeighbor.optimize(&endpoints, pen, exit_target)
        };

        // Collect this group's paths in optimized order, then chain-merge within
        // the group only — never across group boundaries, so characters can't bleed together.
        let mut group_out: Vec<Path<f64>> = order.iter().map(|o| {
            let mut pts = group[o.index].points().clone();
            if o.reversed { pts.reverse(); }
            Path::new(pts)
        }).collect();
        group_out = chain_merge_strokes(group_out, merge_tol);

        for path in group_out {
            pen = { let last = path.points().last().unwrap(); (last.x, last.y) };
            result.push(path);
        }
    }
    result
}

fn make_segment(points: Vec<(f64, f64)>, pen_down: bool, start_time: f64, profile: &AccelerationProfile) -> Segment {
    let vec2d: Vec<Vec2d<f64>> = points.iter().map(|&(x, y)| Vec2d::new(x, y)).collect();
    let plan = plan_path(&vec2d, profile);

    let mut cum = 0.0f64;
    let mut seg_starts = vec![0.0f64];
    for w in points.windows(2) {
        cum += ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt();
        seg_starts.push(cum);
    }

    Segment { pen_down, points, plan, seg_starts, total_length: cum, start_time }
}

/// Convert flat optimized paths into a timeline with interleaved pen-up moves,
/// applying separate motion profiles for pen-down strokes vs. pen-up travels.
fn build_timeline(flat: Vec<Path<f64>>, down: &AccelerationProfile, up: &AccelerationProfile) -> PlotRenderer {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    for path in &flat {
        for v in path.points() {
            min_x = min_x.min(v.x); min_y = min_y.min(v.y);
            max_x = max_x.max(v.x); max_y = max_y.max(v.y);
        }
    }

    let mut timeline: Vec<Segment> = vec![];
    let mut current_time = 0.0f64;
    let mut pen = (0.0f64, 0.0f64);

    for path in flat {
        if path.points().len() < 2 { continue; }
        let start = (path.points()[0].x, path.points()[0].y);

        // Pen-up travel to path start.
        if pen != start {
            let up_pts = vec![pen, start];
            let seg = make_segment(up_pts, false, current_time, up);
            current_time += seg.plan.duration();
            timeline.push(seg);
        }

        // Pen-down stroke.
        let pts: Vec<(f64, f64)> = path.points().iter().map(|v| (v.x, v.y)).collect();
        pen = *pts.last().unwrap();
        let seg = make_segment(pts, true, current_time, down);
        current_time += seg.plan.duration();
        timeline.push(seg);
    }

    PlotRenderer { timeline, total_duration: current_time, min_x, min_y, max_x, max_y }
}

/// Mirror of `text_to_paths` in main.rs — maps chars to Hershey glyphs.
fn hershey_text_to_paths(text: &str, font: &Font) -> Vec<Vec<Path<f64>>> {
    let spacing = 0i32;
    let line_height = hershey_line_height(font);
    let mut out = vec![];
    for (line_idx, line) in text.split('\n').enumerate() {
        let y_offset = line_idx as f64 * line_height;
        let mut x = 0i32;
        for ch in line.chars() {
            let index = (ch as usize).wrapping_sub(32);
            if index >= font.len() {
                x += spacing;
                out.push(vec![]);
                continue;
            }
            let glyph = &font[index];
            let mut glyph_paths = vec![];
            for glyph_path in &glyph.paths {
                let mut new_path: Path<f64> = Path::empty();
                for point in glyph_path.points() {
                    new_path.push(Vec2d::new(
                        (x as f64) + (point.x as f64) - (glyph.left as f64),
                        point.y as f64 + y_offset,
                    ));
                }
                glyph_paths.push(new_path);
            }
            out.push(glyph_paths);
            x += glyph.right - glyph.left + spacing;
        }
    }
    out
}

fn hershey_line_height(font: &Font) -> f64 {
    let mut min_y = i32::MAX;
    let mut max_y = i32::MIN;
    for glyph in font.iter() {
        for path in &glyph.paths {
            for pt in path.points() {
                if pt.y < min_y { min_y = pt.y; }
                if pt.y > max_y { max_y = pt.y; }
            }
        }
    }
    if min_y > max_y { return 32.0; }
    (max_y - min_y) as f64 * 1.2
}
