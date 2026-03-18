//! Control-point visualiser.
//!
//! Renders text using the raw TTF Bézier outlines and overlays:
//!   • red filled circles   — on-curve points (where the curve passes through)
//!   • blue filled diamonds — off-curve control points (Bézier handles)
//!   • pale-blue lines      — handle connections between on- and off-curve points

use std::num::NonZeroU32;
use std::sync::Arc;

use softbuffer::{Context, Surface};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};
use ttf_parser::{Face, OutlineBuilder};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

// ── Point type ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct Pt {
    x: f32,
    y: f32,
}

// ── Outline collector ─────────────────────────────────────────────────────────

struct CpBuilder {
    pb:        PathBuilder,
    handles:   Vec<(Pt, Pt)>,
    on_curve:  Vec<Pt>,
    off_curve: Vec<Pt>,
    cur:       Pt,
    scale:     f32,
    tx:        f32, // x-translation (cursor position)
    ty:        f32, // y-translation (baseline)
}

impl CpBuilder {
    #[inline] fn sx(&self, x: f32) -> f32 { x * self.scale + self.tx }
    #[inline] fn sy(&self, y: f32) -> f32 { -y * self.scale + self.ty }
    #[inline] fn spt(&self, x: f32, y: f32) -> Pt { Pt { x: self.sx(x), y: self.sy(y) } }
}

impl OutlineBuilder for CpBuilder {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.spt(x, y);
        self.pb.move_to(p.x, p.y);
        self.on_curve.push(p);
        self.cur = p;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.spt(x, y);
        self.pb.line_to(p.x, p.y);
        self.on_curve.push(p);
        self.cur = p;
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let c = self.spt(x1, y1);
        let p = self.spt(x, y);
        self.pb.quad_to(c.x, c.y, p.x, p.y);
        self.off_curve.push(c);
        self.on_curve.push(p);
        self.handles.push((self.cur, c));
        self.handles.push((c, p));
        self.cur = p;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let c1 = self.spt(x1, y1);
        let c2 = self.spt(x2, y2);
        let p  = self.spt(x,  y);
        self.pb.cubic_to(c1.x, c1.y, c2.x, c2.y, p.x, p.y);
        self.off_curve.push(c1);
        self.off_curve.push(c2);
        self.on_curve.push(p);
        self.handles.push((self.cur, c1));
        self.handles.push((c1, c2));
        self.handles.push((c2, p));
        self.cur = p;
    }

    fn close(&mut self) { self.pb.close(); }
}

// ── Primitive drawing helpers ─────────────────────────────────────────────────

fn line_seg(pixmap: &mut Pixmap, a: Pt, b: Pt, r: u8, g: u8, b8: u8, alpha: u8, width: f32) {
    let mut pb = PathBuilder::new();
    pb.move_to(a.x, a.y);
    pb.line_to(b.x, b.y);
    let Some(path) = pb.finish() else { return };
    let mut paint = Paint::default();
    paint.set_color_rgba8(r, g, b8, alpha);
    paint.anti_alias = true;
    pixmap.stroke_path(&path, &paint, &Stroke { width, ..Default::default() }, Transform::identity(), None);
}

fn filled_circle(pixmap: &mut Pixmap, cx: f32, cy: f32, radius: f32, r: u8, g: u8, b: u8) {
    let k = 0.5523 * radius;
    let mut pb = PathBuilder::new();
    pb.move_to(cx - radius, cy);
    pb.cubic_to(cx - radius, cy - k,    cx - k,    cy - radius, cx, cy - radius);
    pb.cubic_to(cx + k,    cy - radius, cx + radius, cy - k,    cx + radius, cy);
    pb.cubic_to(cx + radius, cy + k,    cx + k,    cy + radius, cx, cy + radius);
    pb.cubic_to(cx - k,    cy + radius, cx - radius, cy + k,    cx - radius, cy);
    pb.close();
    let Some(path) = pb.finish() else { return };
    let mut paint = Paint::default();
    paint.set_color_rgba8(r, g, b, 255);
    paint.anti_alias = true;
    pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
}

fn filled_diamond(pixmap: &mut Pixmap, cx: f32, cy: f32, r: f32, red: u8, g: u8, b: u8) {
    let mut pb = PathBuilder::new();
    pb.move_to(cx,     cy - r);
    pb.line_to(cx + r, cy    );
    pb.line_to(cx,     cy + r);
    pb.line_to(cx - r, cy    );
    pb.close();
    let Some(path) = pb.finish() else { return };
    let mut paint = Paint::default();
    paint.set_color_rgba8(red, g, b, 255);
    paint.anti_alias = true;
    pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
}

// ── Render ────────────────────────────────────────────────────────────────────

pub fn render(text: &str, data: &[u8], face_index: u32, em_px: f32) -> Pixmap {
    let face = Face::parse(data, face_index).unwrap();
    let scale    = em_px / face.units_per_em() as f32;
    let ascender = face.ascender()  as f32 * scale;
    let line_h   = (face.ascender() - face.descender()) as f32 * scale;
    let pad      = 20.0f32;
    let line_gap = 12.0f32;

    let lines: Vec<&str> = text.split('\n').collect();

    // Compute canvas size from the glyph advance widths.
    let line_widths: Vec<f32> = lines.iter().map(|line| {
        line.chars().filter_map(|c| face.glyph_index(c))
            .filter_map(|id| face.glyph_hor_advance(id))
            .map(|adv| adv as f32 * scale)
            .sum()
    }).collect();

    let max_w = line_widths.iter().cloned().fold(0.0f32, f32::max);
    let pix_w = ((max_w + 2.0 * pad) as u32).max(300);
    let pix_h = ((lines.len() as f32 * line_h
                 + (lines.len() as f32 - 1.0).max(0.0) * line_gap
                 + 2.0 * pad) as u32).max(100);

    let mut pixmap = Pixmap::new(pix_w, pix_h).unwrap();
    pixmap.fill(tiny_skia::Color::WHITE);

    for (li, line) in lines.iter().enumerate() {
        let baseline_y = pad + ascender + li as f32 * (line_h + line_gap);
        let mut cursor_x = pad;

        for ch in line.chars() {
            let Some(glyph_id) = face.glyph_index(ch) else {
                // Advance by the space glyph width, or a fallback.
                cursor_x += face.glyph_index(' ')
                    .and_then(|id| face.glyph_hor_advance(id))
                    .unwrap_or(face.units_per_em() / 4) as f32 * scale;
                continue;
            };

            let mut builder = CpBuilder {
                pb: PathBuilder::new(),
                handles: vec![],
                on_curve: vec![],
                off_curve: vec![],
                cur: Pt { x: cursor_x, y: baseline_y },
                scale,
                tx: cursor_x,
                ty: baseline_y,
            };
            face.outline_glyph(glyph_id, &mut builder);

            let CpBuilder { pb, handles, on_curve, off_curve, .. } = builder;

            // 1. Filled glyph in very light gray.
            if let Some(path) = pb.finish() {
                let mut paint = Paint::default();
                paint.set_color_rgba8(225, 225, 225, 255);
                pixmap.fill_path(&path, &paint, FillRule::EvenOdd, Transform::identity(), None);

                // 2. Thin outline.
                let mut paint = Paint::default();
                paint.set_color_rgba8(120, 120, 120, 255);
                paint.anti_alias = true;
                pixmap.stroke_path(&path, &paint, &Stroke { width: 0.8, ..Default::default() }, Transform::identity(), None);
            }

            // 3. Handle lines (pale blue).
            for (a, b) in &handles {
                line_seg(&mut pixmap, *a, *b, 140, 160, 230, 180, 0.9);
            }

            // 4. Off-curve control points — blue diamonds.
            for pt in &off_curve {
                filled_diamond(&mut pixmap, pt.x, pt.y, 4.5, 55, 100, 215);
            }

            // 5. On-curve points — red circles.
            for pt in &on_curve {
                filled_circle(&mut pixmap, pt.x, pt.y, 3.5, 210, 50, 50);
            }

            cursor_x += face.glyph_hor_advance(glyph_id).unwrap_or(0) as f32 * scale;
        }
    }

    pixmap
}

// ── Window ────────────────────────────────────────────────────────────────────

struct WinState {
    window:   Arc<Window>,
    surface:  Surface<Arc<Window>, Arc<Window>>,
    _context: Context<Arc<Window>>,
}

struct CpApp {
    pixmap: Pixmap,
    width:  u32,
    height: u32,
    state:  Option<WinState>,
}

impl ApplicationHandler for CpApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("Font Control Points  |  ● on-curve  ◆ off-curve / handle")
                    .with_inner_size(winit::dpi::LogicalSize::new(self.width, self.height)),
            ).unwrap(),
        );
        let context = Context::new(window.clone()).unwrap();
        let surface = Surface::new(&context, window.clone()).unwrap();
        self.state = Some(WinState { window, surface, _context: context });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                let Some(st) = self.state.as_mut() else { return };
                st.surface
                    .resize(NonZeroU32::new(self.width).unwrap(), NonZeroU32::new(self.height).unwrap())
                    .unwrap();
                let mut buf = st.surface.buffer_mut().unwrap();
                let data = self.pixmap.data();
                for (i, pixel) in buf.iter_mut().enumerate() {
                    let base = i * 4;
                    if base + 2 >= data.len() { break; }
                    *pixel = ((data[base] as u32) << 16)
                           | ((data[base + 1] as u32) << 8)
                           |  (data[base + 2] as u32);
                }
                buf.present().unwrap();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _: &ActiveEventLoop) {
        if let Some(st) = self.state.as_ref() {
            st.window.request_redraw();
        }
    }
}

pub fn run(text: &str, data: &[u8], face_index: u32, em_px: f32) {
    println!("Control points:  red circle = on-curve   blue diamond = off-curve / handle");
    let pixmap = render(text, data, face_index, em_px);
    let (w, h) = (pixmap.width(), pixmap.height());

    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = CpApp { pixmap, width: w, height: h, state: None };
    event_loop.run_app(&mut app).unwrap();
}
