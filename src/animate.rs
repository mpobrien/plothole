use std::fs::File;
use rayon::prelude::*;
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke as SkiaStroke, Transform};
use crate::motion::{self, AccelerationProfile, Vec2d};

/// A sequence of points to be drawn with the pen either up or down.
#[derive(Clone)]
pub struct DrawnPath {
    pub points: Vec<(f64, f64)>,
    pub pen_down: bool,
}

/// Generate an animated GIF of a drawing being plotted.
///
/// The pen travels at constant speed through all paths (both pen-up and
/// pen-down), so the total animation time equals `duration_s` regardless
/// of drawing complexity. A red dot tracks the current pen position.
pub fn animate(
    paths: &[DrawnPath],
    output: &str,
    width: u32,
    height: u32,
    fps: u32,
    duration_s: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    let (min_x, min_y, max_x, max_y) = bounding_box(paths).ok_or("empty drawing")?;

    let padding = 20.0f64;
    let draw_w = (max_x - min_x).max(1e-9);
    let draw_h = (max_y - min_y).max(1e-9);
    let scale = ((width as f64 - 2.0 * padding) / draw_w)
        .min((height as f64 - 2.0 * padding) / draw_h);
    let off_x = padding + (width as f64 - 2.0 * padding - draw_w * scale) / 2.0 - min_x * scale;
    let off_y = padding + (height as f64 - 2.0 * padding - draw_h * scale) / 2.0 - min_y * scale;

    struct Seg {
        pen_down: bool,
        x1: f64, y1: f64,
        x2: f64, y2: f64,
        dist_start: f64,
        len: f64,
    }
    let mut segments: Vec<Seg> = vec![];
    let mut cum_dist = 0.0f64;
    for path in paths {
        for w in path.points.windows(2) {
            let (x1, y1) = w[0];
            let (x2, y2) = w[1];
            let len = ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt();
            segments.push(Seg { pen_down: path.pen_down, x1, y1, x2, y2, dist_start: cum_dist, len });
            cum_dist += len;
        }
    }
    if cum_dist < 1e-9 {
        return Err("drawing has no length".into());
    }

    let speed = cum_dist / duration_s;
    let total_frames = (duration_s * fps as f64).ceil() as u32 + 1;
    let frame_delay = (100.0 / fps as f64).round() as u16;

    let frames: Vec<gif::Frame<'static>> = (0..total_frames)
        .into_par_iter()
        .map(|frame_i| {
            let drawn_dist = (frame_i as f64 / fps as f64) * speed;
            let to_px = |x: f64, y: f64| -> (f32, f32) {
                ((x * scale + off_x) as f32, (y * scale + off_y) as f32)
            };

            let mut pixmap = Pixmap::new(width, height).expect("valid dimensions");
            pixmap.fill(Color::WHITE);

            let mut ink_paint = Paint::default();
            ink_paint.set_color_rgba8(20, 20, 20, 255);
            ink_paint.anti_alias = true;
            let mut ink_stroke = SkiaStroke::default();
            ink_stroke.width = 1.5;
            let mut dot_paint = Paint::default();
            dot_paint.set_color_rgba8(220, 50, 50, 255);
            dot_paint.anti_alias = true;

            let mut pen: Option<(f32, f32)> = None;

            for seg in &segments {
                if seg.dist_start >= drawn_dist {
                    break;
                }
                let (px1, py1) = to_px(seg.x1, seg.y1);
                let dist_end = seg.dist_start + seg.len;

                if dist_end <= drawn_dist {
                    if seg.pen_down && seg.len > 1e-9 {
                        let (px2, py2) = to_px(seg.x2, seg.y2);
                        let mut pb = PathBuilder::new();
                        pb.move_to(px1, py1);
                        pb.line_to(px2, py2);
                        if let Some(path) = pb.finish() {
                            pixmap.stroke_path(&path, &ink_paint, &ink_stroke, Transform::identity(), None);
                        }
                    }
                    pen = Some(to_px(seg.x2, seg.y2));
                } else {
                    let t = if seg.len > 1e-9 { (drawn_dist - seg.dist_start) / seg.len } else { 0.0 };
                    let mx = seg.x1 + t * (seg.x2 - seg.x1);
                    let my = seg.y1 + t * (seg.y2 - seg.y1);
                    let (pmx, pmy) = to_px(mx, my);
                    if seg.pen_down && seg.len > 1e-9 {
                        let mut pb = PathBuilder::new();
                        pb.move_to(px1, py1);
                        pb.line_to(pmx, pmy);
                        if let Some(path) = pb.finish() {
                            pixmap.stroke_path(&path, &ink_paint, &ink_stroke, Transform::identity(), None);
                        }
                    }
                    pen = Some((pmx, pmy));
                }
            }

            if let Some((px, py)) = pen {
                if let Some(dot) = PathBuilder::from_circle(px, py, 3.5) {
                    pixmap.fill_path(&dot, &dot_paint, FillRule::Winding, Transform::identity(), None);
                }
            }

            let rgba = pixmap.data();
            let rgb: Vec<u8> = rgba.chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
            let mut frame = gif::Frame::from_rgb_speed(width as u16, height as u16, &rgb, 10);
            frame.delay = frame_delay;
            frame
        })
        .collect();

    let file = File::create(output)?;
    let mut encoder = gif::Encoder::new(file, width as u16, height as u16, &[])?;
    encoder.set_repeat(gif::Repeat::Infinite)?;
    for frame in frames {
        encoder.write_frame(&frame)?;
    }

    Ok(())
}

/// Generate an animated GIF driven by the motion planner.
///
/// Each frame corresponds to exactly `1/fps` seconds of real plotter time.
/// The pen position and ink accumulation follow the acceleration/deceleration
/// profile produced by [`motion::plan_path`], so the pen visibly slows through
/// corners and at path ends.
pub fn animate_planned(
    paths: &[DrawnPath],
    profile: &AccelerationProfile,
    output: &str,
    width: u32,
    height: u32,
    fps: u32,
    target_duration: Option<f64>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (min_x, min_y, max_x, max_y) = bounding_box(paths).ok_or("empty drawing")?;

    let padding = 20.0f64;
    let draw_w = (max_x - min_x).max(1e-9);
    let draw_h = (max_y - min_y).max(1e-9);
    let scale = ((width as f64 - 2.0 * padding) / draw_w)
        .min((height as f64 - 2.0 * padding) / draw_h);
    let off_x = padding + (width as f64 - 2.0 * padding - draw_w * scale) / 2.0 - min_x * scale;
    let off_y = padding + (height as f64 - 2.0 * padding - draw_h * scale) / 2.0 - min_y * scale;

    struct PlannedPath {
        start_time: f64,
        plan: motion::Plan,
        pen_down: bool,
        points_f64: Vec<(f64, f64)>,
        seg_starts: Vec<f64>,
        total_length: f64,
    }

    // Safety: PlannedPath contains only Send+Sync types (f64, bool, Vec of
    // primitives, motion::Plan which wraps Vec<Block> of plain-data structs).
    unsafe impl Send for PlannedPath {}
    unsafe impl Sync for PlannedPath {}

    let mut timeline: Vec<PlannedPath> = vec![];
    let mut current_time = 0.0f64;

    for path in paths {
        if path.points.len() < 2 {
            continue;
        }
        let vec2d: Vec<Vec2d> = path.points.iter().map(|&(x, y)| Vec2d::new(x, y)).collect();
        let plan = motion::plan_path(&vec2d, profile);
        let duration = plan.duration();

        let mut seg_starts = vec![0.0f64];
        let mut cum = 0.0f64;
        for w in path.points.windows(2) {
            let (x1, y1) = w[0];
            let (x2, y2) = w[1];
            cum += ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt();
            seg_starts.push(cum);
        }

        timeline.push(PlannedPath {
            start_time: current_time,
            plan,
            pen_down: path.pen_down,
            points_f64: path.points.clone(),
            seg_starts,
            total_length: cum,
        });
        current_time += duration;
    }

    let plotter_duration = current_time;
    // If a target duration was given, scale plotter time to fit; otherwise run at real speed.
    let anim_duration = target_duration.unwrap_or(plotter_duration);
    let time_scale = plotter_duration / anim_duration;
    let total_frames = (anim_duration * fps as f64).ceil() as u32 + 1;
    let frame_delay = (100.0 / fps as f64).round() as u16;

    let frames: Vec<gif::Frame<'static>> = (0..total_frames)
        .into_par_iter()
        .map(|frame_i| {
            // Convert animation time → plotter time.
            let t = frame_i as f64 / fps as f64 * time_scale;
            let to_px = |x: f64, y: f64| -> (f32, f32) {
                ((x * scale + off_x) as f32, (y * scale + off_y) as f32)
            };

            let mut pixmap = Pixmap::new(width, height).expect("valid dimensions");
            pixmap.fill(Color::WHITE);

            let mut ink_paint = Paint::default();
            ink_paint.set_color_rgba8(20, 20, 20, 255);
            ink_paint.anti_alias = true;
            let mut ink_stroke = SkiaStroke::default();
            ink_stroke.width = 1.5;
            let mut dot_paint = Paint::default();
            dot_paint.set_color_rgba8(220, 50, 50, 255);
            dot_paint.anti_alias = true;

            let mut pen: Option<(f32, f32)> = None;

            for pp in &timeline {
                if t < pp.start_time {
                    break;
                }
                let local_t = t - pp.start_time;
                let duration = pp.plan.duration();

                let drawn_distance = if local_t >= duration {
                    pp.total_length
                } else {
                    pp.plan.instant(local_t).distance_m.min(pp.total_length)
                };

                pen = if local_t >= duration {
                    pp.points_f64.last().map(|&(x, y)| to_px(x, y))
                } else {
                    let inst = pp.plan.instant(local_t);
                    Some(to_px(inst.position.x, inst.position.y))
                };

                if !pp.pen_down {
                    continue;
                }

                for (i, w) in pp.points_f64.windows(2).enumerate() {
                    let seg_start_d = pp.seg_starts[i];
                    let seg_end_d = pp.seg_starts[i + 1];

                    if seg_start_d >= drawn_distance {
                        break;
                    }

                    let (x1, y1) = w[0];
                    let (x2, y2) = w[1];
                    let (px1, py1) = to_px(x1, y1);
                    let (px2, py2) = if seg_end_d <= drawn_distance {
                        to_px(x2, y2)
                    } else {
                        let seg_len = seg_end_d - seg_start_d;
                        let frac = if seg_len > 1e-9 {
                            (drawn_distance - seg_start_d) / seg_len
                        } else {
                            0.0
                        };
                        to_px(x1 + frac * (x2 - x1), y1 + frac * (y2 - y1))
                    };

                    let mut pb = PathBuilder::new();
                    pb.move_to(px1, py1);
                    pb.line_to(px2, py2);
                    if let Some(path) = pb.finish() {
                        pixmap.stroke_path(&path, &ink_paint, &ink_stroke, Transform::identity(), None);
                    }
                }
            }

            if let Some((px, py)) = pen {
                if let Some(dot) = PathBuilder::from_circle(px, py, 3.5) {
                    pixmap.fill_path(&dot, &dot_paint, FillRule::Winding, Transform::identity(), None);
                }
            }

            let rgba = pixmap.data();
            let rgb: Vec<u8> = rgba.chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
            let mut frame = gif::Frame::from_rgb_speed(width as u16, height as u16, &rgb, 10);
            frame.delay = frame_delay;
            frame
        })
        .collect();

    let file = File::create(output)?;
    let mut encoder = gif::Encoder::new(file, width as u16, height as u16, &[])?;
    encoder.set_repeat(gif::Repeat::Infinite)?;
    for frame in frames {
        encoder.write_frame(&frame)?;
    }

    Ok(())
}

/// Render the completed drawing as a single PNG frame (no animation, no pen dot).
pub fn render_snapshot(
    paths: &[DrawnPath],
    output: &str,
    width: u32,
    height: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let (min_x, min_y, max_x, max_y) = bounding_box(paths).ok_or("empty drawing")?;

    let padding = 20.0f64;
    let draw_w = (max_x - min_x).max(1e-9);
    let draw_h = (max_y - min_y).max(1e-9);
    let scale = ((width as f64 - 2.0 * padding) / draw_w)
        .min((height as f64 - 2.0 * padding) / draw_h);
    let off_x = padding + (width as f64 - 2.0 * padding - draw_w * scale) / 2.0 - min_x * scale;
    let off_y = padding + (height as f64 - 2.0 * padding - draw_h * scale) / 2.0 - min_y * scale;

    let to_px = |x: f64, y: f64| -> (f32, f32) {
        ((x * scale + off_x) as f32, (y * scale + off_y) as f32)
    };

    let mut pixmap = Pixmap::new(width, height).ok_or("invalid dimensions")?;
    pixmap.fill(Color::WHITE);

    let mut ink_paint = Paint::default();
    ink_paint.set_color_rgba8(20, 20, 20, 255);
    ink_paint.anti_alias = true;
    let mut ink_stroke = SkiaStroke::default();
    ink_stroke.width = 1.5;

    for path in paths {
        if !path.pen_down { continue; }
        for w in path.points.windows(2) {
            let (px1, py1) = to_px(w[0].0, w[0].1);
            let (px2, py2) = to_px(w[1].0, w[1].1);
            let mut pb = PathBuilder::new();
            pb.move_to(px1, py1);
            pb.line_to(px2, py2);
            if let Some(p) = pb.finish() {
                pixmap.stroke_path(&p, &ink_paint, &ink_stroke, Transform::identity(), None);
            }
        }
    }

    pixmap.save_png(output)?;
    Ok(())
}

fn bounding_box(paths: &[DrawnPath]) -> Option<(f64, f64, f64, f64)> {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut any = false;
    for path in paths {
        for &(x, y) in &path.points {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
            any = true;
        }
    }
    if any { Some((min_x, min_y, max_x, max_y)) } else { None }
}
