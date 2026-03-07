pub mod device;
pub mod font;
mod animate;
mod hershey;
use crate::font::ToF64;
use num_traits::{Num, Signed, Zero};
use std::ops::Add;
use std::ops::Mul;
use std::ops::Neg;
use std::ops::Sub;

mod motion;
mod optimize;
use optimize::PathOptimizer;

use std::{fs::File, ops::Deref};

use font::{Path, Vec2d};
use piet::{
    Color, RenderContext,
    kurbo::{Line, Size},
};
use piet_svg::RenderContext as SvgRenderContext;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Render text using a given font
    RenderText {
        #[arg(short, long)]
        text: String,

        #[arg(short, long)]
        font_name: String,
    },
    /// Render text as an animated GIF showing the plot being drawn,
    /// with pen speed driven by the motion planner (acceleration/deceleration visible)
    Animate {
        #[arg(short, long)]
        text: String,

        #[arg(short, long)]
        font_name: String,

        #[arg(short, long, default_value = "out.gif")]
        output: String,

        #[arg(long, default_value = "30")]
        fps: u32,

        #[arg(long, default_value = "800")]
        width: u32,

        #[arg(long, default_value = "600")]
        height: u32,

        /// Maximum pen velocity (font units/second)
        #[arg(long, default_value = "500.0")]
        max_velocity: f64,

        /// Pen acceleration (font units/second²)
        #[arg(long, default_value = "2000.0")]
        acceleration: f64,

        /// Cornering factor — higher = faster through corners (font units)
        #[arg(long, default_value = "1.0")]
        cornering: f64,

        /// Target animation duration (e.g. "5s", "1m30s", "1h20m5s"). When set,
        /// the plotter timeline is scaled to fit; omit to use real plotter time.
        #[arg(long, value_parser = parse_duration)]
        duration: Option<f64>,
    },
    /// List available font names
    ListFonts,
    /// Run the motion planner and print a summary of the drawing and plan
    Inspect {
        #[arg(short, long)]
        text: String,

        #[arg(short, long)]
        font_name: String,

        /// Maximum pen velocity (font units/second)
        #[arg(long, default_value = "500.0")]
        max_velocity: f64,

        /// Pen acceleration (font units/second²)
        #[arg(long, default_value = "2000.0")]
        acceleration: f64,

        /// Cornering factor — higher = faster through corners (font units)
        #[arg(long, default_value = "1.0")]
        cornering: f64,
    },
}

/// Parse a human-readable duration string into seconds.
/// Accepts hours (h), minutes (m), and seconds (s) in any combination,
/// e.g. "5s", "1m", "1m30s", "1h20m5s".
fn parse_duration(s: &str) -> Result<f64, String> {
    let mut total = 0.0f64;
    let mut num = String::new();
    for ch in s.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            num.push(ch);
        } else {
            if num.is_empty() {
                return Err(format!("expected number before '{ch}' in \"{s}\""));
            }
            let n: f64 = num.parse().map_err(|_| format!("invalid number in \"{s}\""))?;
            num.clear();
            match ch {
                'h' => total += n * 3600.0,
                'm' => total += n * 60.0,
                's' => total += n,
                _ => return Err(format!("unknown unit '{ch}' in \"{s}\" (expected h, m, or s)")),
            }
        }
    }
    if !num.is_empty() {
        return Err(format!("trailing number without unit in \"{s}\""));
    }
    if total <= 0.0 {
        return Err(format!("duration must be positive, got \"{s}\""));
    }
    Ok(total)
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::ListFonts => {
            let mut names: Vec<String> = hershey::fonts().keys().cloned().collect();
            names.sort();
            for name in names {
                println!("{}", name);
            }
        }
        Commands::RenderText { text, font_name } => {
            render_text(&text, &font_name);
        }
        Commands::Inspect { text, font_name, max_velocity, acceleration, cornering } => {
            let font = hershey::fonts()
                .get(&font_name.to_uppercase() as &str)
                .expect("unknown font name");
            let raw = text_to_paths(&text, font);
            let original = drawing_to_drawn_paths(&Drawing::new(
                raw.clone().into_iter().flatten().collect()
            ));
            let optimized = drawing_to_drawn_paths(&Drawing::new(optimize_path_order(raw)));
            let profile = motion::AccelerationProfile {
                acceleration,
                maximum_velocity: max_velocity,
                cornering_factor: cornering,
            };
            inspect(&original, &optimized, &profile);
        }
        Commands::Animate { text, font_name, output, fps, width, height, max_velocity, acceleration, cornering, duration } => {
            let font = hershey::fonts()
                .get(&font_name.to_uppercase() as &str)
                .expect("unknown font name");
            let drawing = Drawing::new(optimize_path_order(text_to_paths(&text, font)));

            let paths = drawing_to_drawn_paths(&drawing);
            let profile = motion::AccelerationProfile {
                acceleration,
                maximum_velocity: max_velocity,
                cornering_factor: cornering,
            };
            animate::animate_planned(&paths, &profile, &output, width, height, fps, duration)
                .expect("animation failed");
        }
    }
}

fn path_length(points: &[(f64, f64)]) -> f64 {
    points.windows(2).map(|w| {
        let (x1, y1) = w[0];
        let (x2, y2) = w[1];
        ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt()
    }).sum()
}

fn plan_duration(paths: &[animate::DrawnPath], profile: &motion::AccelerationProfile) -> f64 {
    paths.iter().filter(|p| p.points.len() >= 2).map(|p| {
        let vec2d: Vec<motion::Vec2d> = p.points.iter()
            .map(|&(x, y)| motion::Vec2d::new(x, y))
            .collect();
        motion::plan_path(&vec2d, profile).duration()
    }).sum()
}

fn inspect(
    original: &[animate::DrawnPath],
    optimized: &[animate::DrawnPath],
    profile: &motion::AccelerationProfile,
) {
    // Fixed metrics (same for both orderings).
    let n_pen_down = original.iter().filter(|p| p.pen_down).count();
    let n_points: usize = original.iter().map(|p| p.points.len()).sum();
    let length_drawn: f64 = original.iter().filter(|p| p.pen_down)
        .map(|p| path_length(&p.points)).sum();

    // Metrics that differ between orderings.
    let penup_orig: f64 = original.iter().filter(|p| !p.pen_down)
        .map(|p| path_length(&p.points)).sum();
    let penup_opt: f64  = optimized.iter().filter(|p| !p.pen_down)
        .map(|p| path_length(&p.points)).sum();

    let total_orig = length_drawn + penup_orig;
    let total_opt  = length_drawn + penup_opt;

    let t0 = std::time::Instant::now();
    let time_orig = plan_duration(original, profile);
    let time_opt  = plan_duration(optimized, profile);
    let plan_time = t0.elapsed();

    let optimizer_name = if n_pen_down <= optimize::HELD_KARP_LIMIT {
        "Held-Karp (exact)"
    } else {
        "NearestNeighbor (greedy)"
    };

    // Format each cell as a string first so we can measure widths.
    let rows: Vec<(&str, String, String, f64)> = vec![
        ("Pen-up length:", format!("{:.1} units", penup_orig), format!("{:.1} units", penup_opt), (penup_opt - penup_orig) / penup_orig * 100.0),
        ("Total length:",  format!("{:.1} units", total_orig), format!("{:.1} units", total_opt),  (total_opt  - total_orig)  / total_orig  * 100.0),
        ("Plotter time:",  format!("{:.2} s",     time_orig),  format!("{:.2} s",     time_opt),   (time_opt   - time_orig)   / time_orig   * 100.0),
    ];

    let lw = rows.iter().map(|r| r.0.len()).max().unwrap_or(0);
    let c1 = "Original".len().max(rows.iter().map(|r| r.1.len()).max().unwrap_or(0));
    let c2 = optimizer_name.len().max(rows.iter().map(|r| r.2.len()).max().unwrap_or(0));
    let cw = "Change".len().max(7); // "+100.0%"

    println!("Paths:          {} ({} pen-down, {} pen-up)", original.len(), n_pen_down, original.len() - n_pen_down);
    println!("Points:         {}", n_points);
    println!("Length (drawn): {:.1} units", length_drawn);
    println!("Plan time:      {:?}  (vel={}, accel={})", plan_time, profile.maximum_velocity, profile.acceleration);
    println!();
    println!("{:<lw$}  {:>c1$}  {:>c2$}  {:>cw$}", "", "Original", optimizer_name, "Change");
    println!("{}", "-".repeat(lw + 2 + c1 + 2 + c2 + 2 + cw));
    for (label, v1, v2, pct) in &rows {
        println!("{:<lw$}  {:>c1$}  {:>c2$}  {:>+cw$.1}%", label, v1, v2, pct);
    }
}

fn drawing_to_drawn_paths(drawing: &Drawing<f64>) -> Vec<animate::DrawnPath> {
    drawing.paths.iter().map(|pp| {
        let (pen_down, path) = match pp {
            PenPath::PenUp(p) => (false, p),
            PenPath::PenDown(p) => (true, p),
        };
        animate::DrawnPath {
            pen_down,
            points: path.points().iter().map(|v| (v.x, v.y)).collect(),
        }
    }).collect()
}

fn render_text(text: &str, font_name: &str) {
    let font = hershey::fonts()
        .get(&font_name.to_uppercase() as &str)
        .expect("unknown font name");
    let drawing = Drawing::new(text_to_paths(text, &font).into_iter().flatten().collect());
    let bounds = drawing.bounding_box();
    let size = bounds.size();

    // Create an SVG render context with the given size
    let mut rc = SvgRenderContext::new(Size::new(size.x, size.y));
    render(&mut rc, &drawing);
    rc.finish().unwrap();
    println!("{}", rc.display());
    let out = File::create("out.svg").unwrap();
    rc.write(out).unwrap();
}

fn render(rc: &mut impl RenderContext, drawing: &Drawing<f64>) {
    let bb = drawing.bounding_box();
    let offset = bb.offset();

    rc.clear(None, Color::WHITE);
    for path in &drawing.paths {
        let path = match path {
            PenPath::PenUp(_) => {
                continue;
            }
            PenPath::PenDown(path) => path,
        };
        for segment in path.points().iter().zip(path.points().iter().skip(1)) {
            let (start, end) = segment;
            rc.stroke(
                Line::new((start + &offset).tuple(), (end + &offset).tuple()),
                &Color::BLACK,
                1.0,
            );
        }
    }

    rc.finish().unwrap();
    // rctx.stroke(shape, brush, width);
}

fn do_stuff(rc: &mut impl RenderContext) {
    rc.clear(None, Color::WHITE);
    rc.stroke(Line::new((10.0, 10.0), (100.0, 50.0)), &Color::BLUE, 1.0);
    rc.finish().unwrap();
    // rctx.stroke(shape, brush, width);
}

/// Optimize stroke ordering within each character group independently, then
/// concatenate groups in character order. The pen position carried into each
/// group is the exit point of the previous group.
fn optimize_path_order(grouped: Vec<Vec<Path<f64>>>) -> Vec<Path<f64>> {
    let mut result = vec![];
    let mut pen = (0.0f64, 0.0f64);

    for group in grouped {
        // Drop empty paths within the group.
        let paths: Vec<Path<f64>> = group.into_iter()
            .filter(|p| !p.points().is_empty())
            .collect();
        if paths.is_empty() { continue; }

        let endpoints: Vec<optimize::PathEndpoints> = paths.iter().map(|p| {
            optimize::PathEndpoints {
                start: (p.start().x, p.start().y),
                end:   (p.end().x,   p.end().y),
            }
        }).collect();

        let order = if endpoints.len() <= optimize::HELD_KARP_LIMIT {
            optimize::HeldKarp.optimize(&endpoints, pen)
        } else {
            optimize::NearestNeighbor.optimize(&endpoints, pen)
        };

        for o in order {
            let mut pts = paths[o.index].points().clone();
            if o.reversed { pts.reverse(); }
            let last = pts.last().unwrap();
            pen = (last.x, last.y);
            result.push(Path::new(pts));
        }
    }

    result
}

// Returns a set of paths that will render a string of text
// using the given font.
// Returns one Vec<Path> per character, preserving glyph grouping.
fn text_to_paths<'a>(input: &str, ft: &'a font::Font) -> Vec<Vec<Path<f64>>> {
    let spacing = 0;
    let mut x = 0;
    let mut out = vec![];
    for ch in input.chars() {
        let index = (ch as usize) - 32;
        if index > ft.len() {
            x = x + spacing;
            continue;
        }
        let glyph = &ft[index];

        let mut glyph_paths = vec![];
        for glyph_path in &glyph.paths {
            let mut new_path: Path<f64> = Path::empty();
            for point in glyph_path.points() {
                new_path.push(Vec2d {
                    x: (x as f64) + (point.x as f64) - (glyph.left as f64),
                    y: point.y as f64,
                });
            }
            glyph_paths.push(new_path);
        }
        out.push(glyph_paths);
        x = x + glyph.right - glyph.left + spacing
    }
    out
}

enum PenPath<T>
where
    T: Num
        + Signed
        + Zero
        + Neg<Output = T>
        + Sub<Output = T>
        + Mul<Output = T>
        + Add<Output = T>
        + PartialOrd
        + ToF64
        + Copy,
{
    PenUp(Path<T>),
    PenDown(Path<T>),
}

impl<T> Deref for PenPath<T>
where
    T: Num
        + Signed
        + Zero
        + Neg<Output = T>
        + Sub<Output = T>
        + Mul<Output = T>
        + Add<Output = T>
        + PartialOrd
        + ToF64
        + Copy,
{
    type Target = Path<T>;

    fn deref(&self) -> &Self::Target {
        match self {
            PenPath::PenUp(p) | PenPath::PenDown(p) => p,
        }
    }
}

struct Drawing<T>
where
    T: Num
        + Signed
        + Zero
        + Neg<Output = T>
        + Sub<Output = T>
        + Mul<Output = T>
        + Add<Output = T>
        + PartialOrd
        + ToF64
        + Copy,
{
    paths: Vec<PenPath<T>>,
}

impl<T> Drawing<T>
where
    T: Num
        + Signed
        + Zero
        + Neg<Output = T>
        + Sub<Output = T>
        + Mul<Output = T>
        + Add<Output = T>
        + ToF64
        + PartialOrd
        + Copy,
{
    fn new(paths: Vec<Path<T>>) -> Self {
        let mut prev_position = Vec2d {
            x: T::zero(),
            y: T::zero(),
        };
        let mut out_paths = vec![];
        for path in paths {
            if path.points().is_empty() {
                continue;
            }
            // Move to the start of the next path (pen up).
            out_paths.push(PenPath::PenUp(Path::new(vec![
                prev_position.clone(),
                path.points()[0].clone(),
            ])));
            // Update final position so the next path knows where to move from.
            prev_position = path.points().last().unwrap().clone();
            // Draw the path.
            out_paths.push(PenPath::PenDown(path));
        }
        assert!(!out_paths.is_empty());
        Self { paths: out_paths }
    }

    fn bounding_box(&self) -> BoundingBox<T> {
        let first_point = self
            .paths
            .first()
            .expect("paths are empty")
            .points()
            .first()
            .expect("path has no points");
        let mut bounding_box = BoundingBox::new(&first_point);
        for path in self.paths.iter() {
            for point in path.points() {
                bounding_box.update(point)
            }
        }
        bounding_box
    }
}

#[derive(Clone, Debug)]
struct BoundingBox<T>
where
    T: Num
        + Signed
        + Zero
        + Neg<Output = T>
        + Sub<Output = T>
        + Mul<Output = T>
        + Add<Output = T>
        + PartialOrd
        + Copy,
{
    /// smallest value on X axis
    left: T,
    /// largest value on X axis
    right: T,

    /// smallest value on Y axis
    top: T,
    /// largest value on Y axis
    bottom: T,
}

impl<T> BoundingBox<T>
where
    T: Num
        + Signed
        + Zero
        + Neg<Output = T>
        + Sub<Output = T>
        + Mul<Output = T>
        + Add<Output = T>
        + ToF64
        + PartialOrd
        + Copy,
{
    pub fn new(defaults: &Vec2d<T>) -> Self {
        Self {
            left: defaults.x,
            right: defaults.x,
            top: defaults.y,
            bottom: defaults.y,
        }
    }

    // Returns the width (x) and height (y) of the box as a Vec2d
    pub fn size(&self) -> Vec2d<T> {
        Vec2d {
            x: self.right - self.left,
            y: self.bottom - self.top,
        }
    }

    // Returns a vector Vec2d which, when added to the top-left corner,
    // translates it to the origin (0,0).
    pub fn offset(&self) -> Vec2d<T> {
        Vec2d {
            x: -self.left,
            y: -self.top,
        }
    }

    pub fn update(&mut self, point: &Vec2d<T>) {
        if point.x < self.left {
            self.left = point.x;
        }
        if point.x > self.right {
            self.right = point.x;
        }
        if point.y < self.top {
            self.top = point.y;
        }
        if point.y > self.bottom {
            self.bottom = point.y;
        }
    }
}
