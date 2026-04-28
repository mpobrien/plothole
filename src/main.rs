pub mod device;
pub mod font;
mod animate;
mod hershey;
mod preview;
mod scene3d;
use crate::font::ToF64;
use num_traits::{Num, Signed, Zero};
use std::ops::Add;
use std::ops::Mul;
use std::ops::Neg;
use std::ops::Sub;

mod cp_view;
mod motion;
mod optimize;
mod ttf;
mod tui;
use plothole::iosevka::IosevkaFont;
use optimize::PathOptimizer;
use ttf::TtfFont;

/// Millimetres per drawing unit.
///
/// This is the only value that needs calibration. Plot a shape whose size you
/// know in drawing units, measure it physically, and set:
///     MM_PER_UNIT = measured_mm / drawing_units
///
/// Device resolution background (for reference, not needed here):
///   The AxiDraw EBB native axes run at 113 steps/mm (SM command, 1/16 mode).
///   XM is an alias for SM where A→(A+B) and B→(A-B), so Cartesian X and Y
///   each get √2 fewer native steps, giving 113/√2 ≈ 80 steps/mm — matching
///   `StepMode::steps_per_mm()` in device.rs. No device calibration needed.
///
/// The font-unit → mm scale (this constant) is independent of that and must
/// be determined from a real plot. Placeholder is 1 typographic point ≈ 0.353 mm.
const MM_PER_UNIT: f64 = 0.3528;
/// Default join tolerance for CLI commands (0.1 mm in path units).
const DEFAULT_MERGE_TOL: f64 = 0.1 / MM_PER_UNIT;

use std::{fs::File, ops::Deref};

use font::{Path, Vec2d};
use piet::{
    Color, RenderContext,
    kurbo::{Line, Rect, Size},
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
        /// Input text (mutually exclusive with --text-file)
        #[arg(short, long)]
        text: Option<String>,

        /// Path to a text file to read input from (mutually exclusive with --text)
        #[arg(long)]
        text_file: Option<String>,

        #[arg(short, long)]
        font_name: String,

        /// Scale factor applied to all output coordinates (default 1.0)
        #[arg(long, default_value = "1.0")]
        scale: f64,
    },
    /// Render text as an animated GIF showing the plot being drawn,
    /// with pen speed driven by the motion planner (acceleration/deceleration visible)
    Animate {
        /// Input text (mutually exclusive with --text-file)
        #[arg(short, long)]
        text: Option<String>,

        /// Path to a text file to read input from (mutually exclusive with --text)
        #[arg(long)]
        text_file: Option<String>,

        /// Hershey font name (see list-fonts). Mutually exclusive with --ttf-file.
        #[arg(short, long)]
        font_name: Option<String>,

        /// Path to a TTF font file. Mutually exclusive with --font-name / --iosevka-file.
        #[arg(long)]
        ttf_file: Option<String>,

        /// Path to an Iosevka skeleton.json file. Mutually exclusive with --font-name / --ttf-file.
        #[arg(long)]
        iosevka_file: Option<String>,

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

        /// Render only the final frame as a PNG instead of an animated GIF.
        #[arg(long)]
        snapshot: bool,

        /// TTF face index (for .ttc collections with multiple variants)
        #[arg(long, default_value = "0")]
        ttf_face: u32,

        /// Variable font axis override, e.g. --ttf-axis wght=700. Repeatable.
        #[arg(long = "ttf-axis", value_parser = parse_ttf_axis, action = clap::ArgAction::Append)]
        ttf_axes: Vec<(String, f32)>,

        /// TTF raster resolution in pixels
        #[arg(long, default_value = "128.0")]
        raster_px: f32,

        /// Douglas-Peucker simplification tolerance in output units (lower = more detail)
        #[arg(long, default_value = "0.5")]
        dp_epsilon: f64,

        /// Scale factor applied to all output coordinates (default 1.0)
        #[arg(long, default_value = "1.0")]
        scale: f64,
    },
    /// Open an interactive preview window showing the animation
    Preview {
        /// Input text (mutually exclusive with --text-file)
        #[arg(short, long)]
        text: Option<String>,

        /// Path to a text file to read input from (mutually exclusive with --text)
        #[arg(long)]
        text_file: Option<String>,

        /// Hershey font name. Mutually exclusive with --ttf-file.
        #[arg(short, long)]
        font_name: Option<String>,

        /// Path to a TTF font file. Mutually exclusive with --font-name / --iosevka-file.
        #[arg(long)]
        ttf_file: Option<String>,

        /// Path to an Iosevka skeleton.json file. Mutually exclusive with --font-name / --ttf-file.
        #[arg(long)]
        iosevka_file: Option<String>,

        /// TTF face index (for .ttc collections)
        #[arg(long, default_value = "0")]
        ttf_face: u32,

        /// Variable font axis override, e.g. --ttf-axis wght=700. Repeatable.
        #[arg(long = "ttf-axis", value_parser = parse_ttf_axis, action = clap::ArgAction::Append)]
        ttf_axes: Vec<(String, f32)>,

        /// TTF raster resolution in pixels
        #[arg(long, default_value = "128.0")]
        raster_px: f32,

        /// Douglas-Peucker simplification tolerance in output units
        #[arg(long, default_value = "0.5")]
        dp_epsilon: f64,

        /// Scale factor applied to all output coordinates (default 1.0)
        #[arg(long, default_value = "1.0")]
        scale: f64,

        /// Write a snapshot PNG instead of opening the live window
        #[arg(long)]
        snapshot: bool,

        /// Output path for --snapshot (default: out.png)
        #[arg(long, default_value = "out.png")]
        output: String,
    },
    /// Export centerline paths as an SVG file
    Svg {
        /// Input text (mutually exclusive with --text-file)
        #[arg(short, long)]
        text: Option<String>,

        /// Path to a text file to read input from (mutually exclusive with --text)
        #[arg(long)]
        text_file: Option<String>,

        /// Hershey font name. Mutually exclusive with --ttf-file.
        #[arg(short, long)]
        font_name: Option<String>,

        /// Path to a TTF font file. Mutually exclusive with --font-name / --iosevka-file.
        #[arg(long)]
        ttf_file: Option<String>,

        /// Path to an Iosevka skeleton.json file. Mutually exclusive with --font-name / --ttf-file.
        #[arg(long)]
        iosevka_file: Option<String>,

        /// TTF face index (for .ttc collections)
        #[arg(long, default_value = "0")]
        ttf_face: u32,

        /// Variable font axis override, e.g. --ttf-axis wght=700. Repeatable.
        #[arg(long = "ttf-axis", value_parser = parse_ttf_axis, action = clap::ArgAction::Append)]
        ttf_axes: Vec<(String, f32)>,

        /// TTF raster resolution in pixels
        #[arg(long, default_value = "128.0")]
        raster_px: f32,

        /// Douglas-Peucker simplification tolerance in output units
        #[arg(long, default_value = "0.5")]
        dp_epsilon: f64,

        /// Scale factor applied to all output coordinates
        #[arg(long, default_value = "1.0")]
        scale: f64,

        /// Output SVG file path
        #[arg(short, long, default_value = "out.svg")]
        output: String,
    },

    /// Lay out sample text on a virtual A4 page and export it as an SVG
    PrintPage {
        /// Built-in Hershey font name (see list-fonts); mutually exclusive with --iosevka-file
        #[arg(long)]
        font_name: Option<String>,

        /// Path to an Iosevka skeleton.json file; mutually exclusive with --font-name
        #[arg(long)]
        iosevka_file: Option<String>,

        /// Target line height in mm (controls font size)
        #[arg(long, default_value = "7.5")]
        line_height_mm: f64,

        /// Extra space between lines in mm (added on top of the natural 1.2× line height)
        #[arg(long, default_value = "0.0")]
        line_gap_mm: f64,

        /// Page size: a3, a4, a5, or letter  (default: a4)
        #[arg(long, default_value = "a4")]
        page_size: String,

        /// Margin on each side of the page in mm
        #[arg(long, default_value = "20.0")]
        margin_mm: f64,

        /// Input text (default: lorem ipsum)
        #[arg(short, long)]
        text: Option<String>,

        /// Path to a text file (mutually exclusive with --text)
        #[arg(long)]
        text_file: Option<String>,

        /// Maximum pen-down speed in mm/s (default ≈ 176 mm/s)
        #[arg(long, default_value = "176.4")]
        max_velocity_mm: f64,

        /// Pen acceleration in mm/s²  (default ≈ 706 mm/s²)
        #[arg(long, default_value = "705.6")]
        acceleration_mm: f64,

        /// Cornering factor — higher = faster through corners
        #[arg(long, default_value = "1.0")]
        cornering: f64,

        /// Pen-up speed multiplier — pen-up moves run this many times faster than pen-down
        #[arg(long, default_value = "1.0")]
        pen_up_speed: f64,

        /// Alternate line draw direction (boustrophedon) to eliminate end-of-line returns
        #[arg(long)]
        boustrophedon: bool,

        /// Use greedy (first-fit) line breaking instead of DP (Knuth-Plass style)
        #[arg(long)]
        greedy_wrap: bool,

        /// Output SVG file path
        #[arg(short, long, default_value = "page.svg")]
        output: String,
    },

    /// Render a 3D scene of primitives (cube/sphere/cylinder/pyramid/prism) with hidden-line removal
    Scene3d {
        /// Built-in scene preset: showcase, cubes, tower, mixed
        #[arg(long, default_value = "showcase")]
        preset: String,

        /// Page size: a3, a4, a5, or letter
        #[arg(long, default_value = "a4")]
        page_size: String,

        /// Page margin in mm
        #[arg(long, default_value = "20.0")]
        margin_mm: f64,

        /// Camera azimuth in degrees (rotation around the vertical axis; 0 = +x)
        #[arg(long, default_value = "35.0")]
        azimuth: f64,

        /// Camera elevation in degrees (tilt; 0 = level, 90 = top-down)
        #[arg(long, default_value = "25.0")]
        elevation: f64,

        /// Vertical field of view in degrees (0 = orthographic; ~60 = wide perspective, ~25 = mild)
        #[arg(long, default_value = "0.0")]
        fov: f64,

        /// Camera distance from scene centroid in world units (controls perspective intensity)
        #[arg(long, default_value = "20.0")]
        camera_distance: f64,

        /// Output SVG file path
        #[arg(short, long, default_value = "scene.svg")]
        output: String,
    },

    /// List available font names
    ListFonts,
    /// List all faces in a TTF/TTC file
    ListFaces {
        #[arg(long)]
        ttf_file: String,
    },
    /// Visualise the raw Bézier control points of a TTF font overlaid on rendered text
    ControlPoints {
        /// Text to render
        text: String,

        /// Path to the TTF font file
        #[arg(long)]
        ttf_file: String,

        /// TTF face index (for .ttc collections)
        #[arg(long, default_value = "0")]
        ttf_face: u32,

        /// Em height in pixels — controls how large the glyphs are drawn
        #[arg(long, default_value = "200.0")]
        em_px: f32,
    },

    /// Open an interactive terminal controller for the connected AxiDraw
    Control,
    /// Run the motion planner and print a summary of the drawing and plan
    Inspect {
        /// Input text (mutually exclusive with --text-file)
        #[arg(short, long)]
        text: Option<String>,

        /// Path to a text file to read input from (mutually exclusive with --text)
        #[arg(long)]
        text_file: Option<String>,

        /// Hershey font name (see list-fonts). Mutually exclusive with --ttf-file.
        #[arg(short, long)]
        font_name: Option<String>,

        /// Path to a TTF font file. Mutually exclusive with --font-name / --iosevka-file.
        #[arg(long)]
        ttf_file: Option<String>,

        /// Path to an Iosevka skeleton.json file. Mutually exclusive with --font-name / --ttf-file.
        #[arg(long)]
        iosevka_file: Option<String>,

        /// Maximum pen velocity (font units/second)
        #[arg(long, default_value = "500.0")]
        max_velocity: f64,

        /// Pen acceleration (font units/second²)
        #[arg(long, default_value = "2000.0")]
        acceleration: f64,

        /// Cornering factor — higher = faster through corners (font units)
        #[arg(long, default_value = "1.0")]
        cornering: f64,

        /// TTF face index (for .ttc collections with multiple variants)
        #[arg(long, default_value = "0")]
        ttf_face: u32,

        /// Variable font axis override, e.g. --ttf-axis wght=700. Repeatable.
        #[arg(long = "ttf-axis", value_parser = parse_ttf_axis, action = clap::ArgAction::Append)]
        ttf_axes: Vec<(String, f32)>,

        /// TTF raster resolution in pixels
        #[arg(long, default_value = "128.0")]
        raster_px: f32,

        /// Douglas-Peucker simplification tolerance in output units
        #[arg(long, default_value = "0.5")]
        dp_epsilon: f64,

        /// Scale factor applied to all output coordinates (default 1.0)
        #[arg(long, default_value = "1.0")]
        scale: f64,
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

/// Resolve `--text` / `--text-file` to a string; exactly one must be `Some`.
fn resolve_text(text: Option<String>, text_file: Option<String>) -> String {
    match (text, text_file) {
        (Some(t), None) => t,
        (None, Some(f)) => std::fs::read_to_string(&f)
            .unwrap_or_else(|e| panic!("failed to read text file \"{f}\": {e}")),
        _ => panic!("provide exactly one of --text or --text-file"),
    }
}

/// Scale all points in a grouped-path collection by `scale`.
pub(crate) fn scale_grouped(paths: Vec<Vec<Path<f64>>>, scale: f64) -> Vec<Vec<Path<f64>>> {
    if (scale - 1.0).abs() < f64::EPSILON { return paths; }
    paths.into_iter().map(|group| {
        group.into_iter().map(|path| {
            Path::new(path.points().iter().map(|p| Vec2d { x: p.x * scale, y: p.y * scale }).collect())
        }).collect()
    }).collect()
}

/// Resolve text → grouped paths from either a Hershey font name or a TTF file.
/// Exactly one of `font_name` / `ttf_file` / `iosevka_file` must be `Some`.
fn resolve_paths(
    text:         &str,
    font_name:    Option<&str>,
    ttf_file:     Option<&str>,
    iosevka_file: Option<&str>,
    ttf_face:     u32,
    raster_px:    f32,
    dp_epsilon:   f64,
    ttf_axes:     &[(String, f32)],
    scale:        f64,
) -> Vec<Vec<Path<f64>>> {
    let paths = match (font_name, ttf_file, iosevka_file) {
        (Some(name), None, None) => {
            let font = hershey::fonts()
                .get(&name.to_uppercase() as &str)
                .expect("unknown font name");
            text_to_paths(text, font)
        }
        (None, Some(path), None) => {
            TtfFont::from_file(path, ttf_face)
                .expect("failed to load TTF font")
                .text_to_paths(text, raster_px, dp_epsilon, ttf_axes)
        }
        (None, None, Some(path)) => {
            IosevkaFont::from_file(path)
                .expect("failed to load Iosevka skeleton file")
                .text_to_paths(text, raster_px as f64, 0.0)
                .into_iter().map(|group| group.into_iter().map(|p| {
                    font::Path::new(p.points().iter()
                        .map(|pt| font::Vec2d { x: pt.x, y: pt.y })
                        .collect())
                }).collect()).collect()
        }
        _ => panic!("provide exactly one of --font-name, --ttf-file, or --iosevka-file"),
    };
    scale_grouped(paths, scale)
}

fn parse_ttf_axis(s: &str) -> Result<(String, f32), String> {
    let (tag, val) = s.split_once('=')
        .ok_or_else(|| format!("expected TAG=VALUE, got \"{s}\""))?;
    if tag.len() != 4 {
        return Err(format!("axis tag must be exactly 4 characters, got \"{tag}\""));
    }
    let value = val.parse::<f32>()
        .map_err(|_| format!("invalid axis value \"{val}\""))?;
    Ok((tag.to_string(), value))
}

const LOREM_IPSUM: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.\n\nSed ut perspiciatis unde omnis iste natus error sit voluptatem accusantium doloremque laudantium, totam rem aperiam, eaque ipsa quae ab illo inventore veritatis et quasi architecto beatae vitae dicta sunt explicabo. Nemo enim ipsam voluptatem quia voluptas sit aspernatur aut odit aut fugit, sed quia consequuntur magni dolores eos qui ratione voluptatem sequi nesciunt. Neque porro quisquam est, qui dolorem ipsum quia dolor sit amet, consectetur, adipisci velit, sed quia non numquam eius modi tempora incidunt ut labore et dolore magnam aliquam quaerat voluptatem. Ut enim ad minima veniam, quis nostrum exercitationem ullam corporis suscipit laboriosam, nisi ut aliquid ex ea commodi consequatur?\n\nAt vero eos et accusamus et iusto odio dignissimos ducimus qui blanditiis praesentium voluptatum deleniti atque corrupti quos dolores et quas molestias excepturi sint occaecati cupiditate non provident, similique sunt in culpa qui officia deserunt mollitia animi, id est laborum et dolorum fuga. Et harum quidem rerum facilis est et expedita distinctio. Nam libero tempore, cum soluta nobis est eligendi optio cumque nihil impedit quo minus id quod maxime placeat facere possimus, omnis voluptas assumenda est, omnis dolor repellendus. Temporibus autem quibusdam et aut officiis debitis aut rerum necessitatibus saepe eveniet ut et voluptates repudiandae sint et molestiae non recusandae.";

/// Break one paragraph's words into lines using DP (Knuth-Plass style, ragged-right).
/// Minimises sum of squared slack; the last line of each paragraph is free.
fn break_paragraph(words: &[&str], max_width: usize) -> Vec<String> {
    let n = words.len();
    if n == 0 {
        return vec![String::new()];
    }
    let mut cost = vec![u64::MAX; n + 1];
    let mut from = vec![0usize; n + 1];
    cost[n] = 0;

    for i in (0..n).rev() {
        let mut len = 0usize;
        for j in i..n {
            if j > i { len += 1; } // inter-word space
            len += words[j].len();
            if len > max_width { break; }
            let slack = (max_width - len) as u64;
            let line_cost = if j == n - 1 { 0 } else { slack * slack };
            let total = line_cost.saturating_add(cost[j + 1]);
            if total < cost[i] {
                cost[i] = total;
                from[i] = j + 1;
            }
        }
    }

    let mut lines = Vec::new();
    let mut i = 0;
    while i < n {
        let j = from[i];
        lines.push(words[i..j].join(" "));
        i = j;
    }
    lines
}

/// Word-wrap `text` to at most `width` characters per line.
/// Paragraph breaks (blank lines) in the input are preserved.
/// Uses DP line breaking for even rag.
fn word_wrap(text: &str, width: usize) -> String {
    let mut out: Vec<String> = Vec::new();
    for para in text.split('\n') {
        let words: Vec<&str> = para.split_whitespace().collect();
        if words.is_empty() {
            out.push(String::new()); // preserve blank lines
        } else {
            out.extend(break_paragraph(&words, width));
        }
    }
    out.join("\n")
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::Preview { text, text_file, font_name, ttf_file, iosevka_file, ttf_face, ttf_axes, raster_px, dp_epsilon, scale, snapshot, output } => {
            let text = resolve_text(text, text_file);
            let renderer = match (font_name.as_deref(), ttf_file.as_deref(), iosevka_file.as_deref()) {
                (Some(name), None, None) => plothole::PlotRenderer::new_hershey(&text, name, scale)
                    .expect("unknown font name"),
                (None, Some(path), None) => {
                    let data = std::fs::read(path).expect("failed to read TTF file");
                    plothole::PlotRenderer::new_ttf(&data, &text, ttf_face, &ttf_axes, raster_px, dp_epsilon, scale)
                        .expect("failed to load TTF font")
                }
                _ => panic!("provide exactly one of --font-name, --ttf-file, or --iosevka-file"),
            };
            if snapshot {
                let raw = resolve_paths(&text, font_name.as_deref(), ttf_file.as_deref(), iosevka_file.as_deref(), ttf_face, raster_px, dp_epsilon, &ttf_axes, scale);
                let drawing = Drawing::new(optimize_path_order(raw, DEFAULT_MERGE_TOL));
                animate::render_snapshot(&drawing_to_drawn_paths(&drawing), &output, 800, 500)
                    .expect("snapshot failed");
            } else {
                preview::run(renderer);
            }
        }
        Commands::ControlPoints { text, ttf_file, ttf_face, em_px } => {
            let data = std::fs::read(&ttf_file).expect("failed to read TTF file");
            cp_view::run(&text, &data, ttf_face, em_px);
        }
        Commands::Control => {
            let device = match device::open_device() {
                Ok(d) => Some(d),
                Err(e) => {
                    eprintln!("Warning: could not open AxiDraw: {e}");
                    None
                }
            };
            tui::run(device);
        }
        Commands::Svg { text, text_file, font_name, ttf_file, iosevka_file, ttf_face, ttf_axes, raster_px, dp_epsilon, scale, output } => {
            let text = resolve_text(text, text_file);
            let raw = resolve_paths(&text, font_name.as_deref(), ttf_file.as_deref(), iosevka_file.as_deref(), ttf_face, raster_px, dp_epsilon, &ttf_axes, scale);
            let drawing = Drawing::new(optimize_path_order(raw, DEFAULT_MERGE_TOL));
            let bounds = drawing.bounding_box();
            let size = bounds.size();
            let margin = 10.0;
            let mut rc = SvgRenderContext::new(piet::kurbo::Size::new(size.x + 2.0 * margin, size.y + 2.0 * margin));
            render(&mut rc, &drawing, margin, 1.0);
            let out = File::create(&output).expect("failed to create output file");
            rc.write(out).expect("failed to write SVG");
            println!("Wrote {output}");
        }
        Commands::PrintPage { font_name, iosevka_file, line_height_mm, line_gap_mm, margin_mm,
                              page_size, text, text_file, max_velocity_mm, acceleration_mm,
                              cornering, pen_up_speed, boustrophedon, greedy_wrap, output } => {
            // Resolve font: exactly one of --font-name or --iosevka-file must be given.
            // Returns (ascender, avg_advance, text_to_paths_fn).
            type PathsResult = Vec<Vec<font::Path<f64>>>;
            let (font_ascender, font_avg_advance, render_text): (f64, f64, Box<dyn Fn(&str, f64, f64) -> PathsResult>) =
                match (font_name.as_deref(), iosevka_file.as_deref()) {
                    (Some(name), None) => {
                        let hf = hershey::fonts()
                            .get(&name.to_uppercase() as &str)
                            .unwrap_or_else(|| panic!("unknown font \"{name}\" — run list-fonts to see options"));
                        let asc = hershey_ascender(hf);
                        let adv = hershey_avg_advance(hf);
                        (asc, adv, Box::new(move |t, em, gap| hershey_text_to_paths_scaled(t, hf, em, gap)))
                    }
                    (None, Some(path)) => {
                        let iosevka = IosevkaFont::from_file(path)
                            .expect("failed to load Iosevka skeleton file");
                        let asc = iosevka.ascender();
                        let adv = iosevka.cell_advance();
                        (asc, adv, Box::new(move |t, em, gap| {
                            iosevka.text_to_paths(t, em, gap)
                                .into_iter().map(|group| group.into_iter().map(|p| {
                                    font::Path::new(p.points().iter()
                                        .map(|pt| font::Vec2d { x: pt.x, y: pt.y })
                                        .collect())
                                }).collect()).collect()
                        }))
                    }
                    _ => panic!("provide exactly one of --font-name or --iosevka-file"),
                };

            let (page_w_mm, page_h_mm) = match page_size.to_lowercase().as_str() {
                "a3"     => (297.0_f64, 420.0_f64),
                "a4"     => (210.0_f64, 297.0_f64),
                "a5"     => (148.0_f64, 210.0_f64),
                "letter" => (215.9_f64, 279.4_f64),
                other    => panic!("unknown page size \"{other}\" — try a3, a4, a5, or letter"),
            };
            let pw_mm = page_w_mm - 2.0 * margin_mm;
            let ph_mm = page_h_mm - 2.0 * margin_mm;

            let em_size    = line_height_mm / (1.2 * MM_PER_UNIT);
            let line_gap   = line_gap_mm / MM_PER_UNIT; // in path units
            let scale      = em_size / font_ascender;
            let advance_mm = font_avg_advance * scale * MM_PER_UNIT;

            let chars_per_line = (pw_mm / advance_mm).floor() as usize;
            let lines_per_page = (ph_mm / (line_height_mm + line_gap_mm)).floor() as usize;

            let input = match (text, text_file) {
                (Some(t), None) => t,
                (None, Some(f)) => std::fs::read_to_string(&f)
                    .unwrap_or_else(|e| panic!("failed to read text file: {e}")),
                (None, None)    => LOREM_IPSUM.to_string(),
                _               => panic!("provide at most one of --text or --text-file"),
            };

            let wrap = |text: &str| -> String {
                if greedy_wrap {
                    // Greedy (first-fit) wrapping
                    let mut out: Vec<String> = Vec::new();
                    for para in text.split('\n') {
                        let mut line = String::new();
                        for word in para.split_whitespace() {
                            if line.is_empty() {
                                line.push_str(word);
                            } else if line.len() + 1 + word.len() <= chars_per_line {
                                line.push(' ');
                                line.push_str(word);
                            } else {
                                out.push(line.clone());
                                line.clear();
                                line.push_str(word);
                            }
                        }
                        out.push(line);
                    }
                    out.join("\n")
                } else {
                    word_wrap(text, chars_per_line)
                }
            };

            // Tile the input until we have enough wrapped lines to fill the page.
            let mut tiled = input.clone();
            while wrap(&tiled).lines().count() < lines_per_page {
                tiled.push_str(" \n\n");
                tiled.push_str(&input);
            }
            let wrapped   = wrap(&tiled);
            let page_text = wrapped.lines().take(lines_per_page).collect::<Vec<_>>().join("\n");
            let placed_lines = page_text.lines().count();
            let placed_chars: usize = page_text.chars().filter(|&c| c != '\n').count();

            println!("Page:   {page_w_mm}×{page_h_mm} mm ({page_size}), {margin_mm} mm margins → {pw_mm}×{ph_mm} mm printable");
            println!("Font:   em={em_size:.1} units  cap≈{:.1} mm  advance≈{advance_mm:.2} mm/char  line={line_height_mm:.1} mm",
                em_size * MM_PER_UNIT);
            println!("Grid:   {chars_per_line} chars/line × {lines_per_page} lines/page");
            println!("Text:   {placed_lines} lines, {placed_chars} chars placed");

            let mut raw: Vec<Vec<font::Path<f64>>> = render_text(&page_text, em_size, line_gap);
            if boustrophedon {
                let line_lengths: Vec<usize> = page_text.lines()
                    .map(|l| l.chars().count()).collect();
                raw = self::boustrophedon(raw, &line_lengths);
            }
            let flat    = optimize_path_order(raw, DEFAULT_MERGE_TOL);
            let drawing = Drawing::new(flat);
            let bb      = drawing.bounding_box();

            let margin_units = margin_mm / MM_PER_UNIT;
            let page_w_units = page_w_mm / MM_PER_UNIT;
            let page_h_units = page_h_mm / MM_PER_UNIT;
            // Shift so the top of the first line's ascenders sits at the margin.
            let offset_x = -bb.left + margin_units;
            let offset_y = -bb.top  + margin_units;

            let mut rc = SvgRenderContext::new(Size::new(page_w_units, page_h_units));
            rc.clear(None, Color::WHITE);
            rc.stroke(
                Rect::new(0.5, 0.5, page_w_units - 0.5, page_h_units - 0.5),
                &Color::rgb8(180, 180, 180),
                0.5,
            );
            for path in &drawing.paths {
                let path = match path {
                    PenPath::PenDown(p) => p,
                    PenPath::PenUp(_)   => continue,
                };
                for seg in path.points().windows(2) {
                    rc.stroke(
                        Line::new(
                            (seg[0].x + offset_x, seg[0].y + offset_y),
                            (seg[1].x + offset_x, seg[1].y + offset_y),
                        ),
                        &Color::BLACK,
                        1.0,
                    );
                }
            }
            rc.finish().unwrap();
            let out = File::create(&output).expect("failed to create SVG");
            rc.write(out).expect("failed to write SVG");
            println!("Wrote  {output}");

            let max_vel   = max_velocity_mm / MM_PER_UNIT;
            let accel     = acceleration_mm / MM_PER_UNIT;
            let down_profile = motion::AccelerationProfile {
                maximum_velocity: max_vel,
                acceleration:     accel,
                cornering_factor: cornering,
            };
            let up_profile = motion::AccelerationProfile {
                maximum_velocity: max_vel * pen_up_speed,
                acceleration:     accel  * pen_up_speed,
                cornering_factor: cornering,
            };
            let drawn      = drawing_to_drawn_paths(&drawing);
            let n_down     = drawn.iter().filter(|p|  p.pen_down).count();
            let n_up       = drawn.iter().filter(|p| !p.pen_down).count();
            let t          = plan_duration(&drawn, &down_profile, &up_profile);
            let opts: Vec<&str> = [
                (cornering    != 1.0).then_some("cornering"),
                (pen_up_speed != 1.0).then_some("fast pen-up"),
                boustrophedon        .then_some("boustrophedon"),
            ].into_iter().flatten().collect();
            let opts_str = if opts.is_empty() { "defaults".to_string() } else { opts.join(", ") };
            println!("Strokes: {n_down} pen-down, {n_up} pen-up");
            println!("Time:   {t:.0} s  ({:.1} min)  [{opts_str}]", t / 60.0);
        }
        Commands::Scene3d { preset, page_size, margin_mm, azimuth, elevation, fov, camera_distance, output } => {
            use scene3d::{Vec3, Camera, render};

            let (page_w_mm, page_h_mm) = match page_size.to_lowercase().as_str() {
                "a3"     => (297.0_f64, 420.0_f64),
                "a4"     => (210.0_f64, 297.0_f64),
                "a5"     => (148.0_f64, 210.0_f64),
                "letter" => (215.9_f64, 279.4_f64),
                other    => panic!("unknown page size \"{other}\""),
            };
            let pw_mm = page_w_mm - 2.0 * margin_mm;
            let ph_mm = page_h_mm - 2.0 * margin_mm;

            // Use the shared preset library so the binary and the wasm webapp stay in sync.
            let scene = scene3d::presets::build(preset.as_str(), 1000)
                .unwrap_or_else(|e| panic!("{e}"));

            // Camera: orbit around the origin at given azimuth/elevation, look at the centroid of all vertices.
            let centroid = {
                let mut c = Vec3::zero();
                let mut n = 0;
                for m in &scene.objects {
                    for v in &m.vertices { c = c.add(*v); n += 1; }
                }
                if n == 0 { c } else { c.scale(1.0 / n as f64) }
            };
            let az_r = azimuth.to_radians();
            let el_r = elevation.to_radians();
            let eye = Vec3::new(
                centroid.x + camera_distance * el_r.cos() * az_r.cos(),
                centroid.y + camera_distance * el_r.cos() * az_r.sin(),
                centroid.z + camera_distance * el_r.sin(),
            );
            // Provisional scale; recomputed below to fit page.
            let cam = Camera { eye, target: centroid, up: Vec3::new(0.0, 0.0, 1.0), scale: 1.0, fov_deg: fov, near: 0.1 };

            // Auto-fit: project at scale 1, find world-space extent on page, pick a scale that fills the printable area.
            // Use 5–95 percentiles instead of full min/max so a few near-plane vertices that project huge
            // don't dominate the bounding box.
            let forward_unit = cam.target.sub(cam.eye).normalize();
            let mut xs: Vec<f64> = Vec::new();
            let mut ys: Vec<f64> = Vec::new();
            for m in &scene.objects {
                for v in &m.vertices {
                    let depth = v.sub(cam.eye).dot(forward_unit);
                    if depth < cam.near { continue; }
                    let (x, y, _) = cam.project(*v);
                    xs.push(x); ys.push(y);
                }
            }
            if xs.is_empty() {
                println!("All vertices behind the near plane — try a larger --camera-distance.");
                return;
            }
            xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
            ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
            // Robust bbox via IQR: keep min/max of all values that are within 5×IQR of the quartiles.
            // Filters streak-endpoint outliers without trimming legitimate edge content.
            let robust_bbox = |sorted: &[f64]| -> (f64, f64) {
                let n = sorted.len();
                let q1 = sorted[n / 4];
                let q3 = sorted[3 * n / 4];
                let iqr = q3 - q1;
                let lo = q1 - 5.0 * iqr;
                let hi = q3 + 5.0 * iqr;
                let lo_v = sorted.iter().find(|&&v| v >= lo).copied().unwrap_or(sorted[0]);
                let hi_v = sorted.iter().rev().find(|&&v| v <= hi).copied().unwrap_or(sorted[n - 1]);
                (lo_v, hi_v)
            };
            let (min_x, max_x) = robust_bbox(&xs);
            let (min_y, max_y) = robust_bbox(&ys);
            let world_w = max_x - min_x;
            let world_h = max_y - min_y;
            let pw_units = pw_mm / MM_PER_UNIT;
            let ph_units = ph_mm / MM_PER_UNIT;
            let scale_fit = (pw_units / world_w).min(ph_units / world_h) * 0.95;
            let cam = Camera { eye, target: centroid, up: Vec3::new(0.0, 0.0, 1.0), scale: scale_fit, fov_deg: fov, near: 0.1 };

            let t0 = std::time::Instant::now();
            let paths = render(&scene, &cam);
            let render_ms = t0.elapsed().as_secs_f64() * 1000.0;

            // Bounding-box recentered to the page margin.
            // Path-coord bbox via the same IQR robust trim, so streak endpoints don't bias centering.
            let mut path_xs: Vec<f64> = Vec::new();
            let mut path_ys: Vec<f64> = Vec::new();
            for p in &paths { for pt in p.points() { path_xs.push(pt.x); path_ys.push(pt.y); } }
            path_xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
            path_ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let (bb_min, bb_max) = if path_xs.is_empty() {
                ((0.0, 0.0), (1.0, 1.0))
            } else {
                let (lo_x, hi_x) = robust_bbox(&path_xs);
                let (lo_y, hi_y) = robust_bbox(&path_ys);
                ((lo_x, lo_y), (hi_x, hi_y))
            };
            let _ = margin_mm; // margin already baked into the auto-fit scale
            let page_w_units = page_w_mm / MM_PER_UNIT;
            let page_h_units = page_h_mm / MM_PER_UNIT;
            let draw_w = bb_max.0 - bb_min.0;
            let draw_h = bb_max.1 - bb_min.1;
            let offset_x = -bb_min.0 + (page_w_units - draw_w) * 0.5;
            let offset_y = -bb_min.1 + (page_h_units - draw_h) * 0.5;

            // Liang-Barsky 2D segment clip against the page rect.
            let clip_to_rect = |a: (f64, f64), b: (f64, f64)| -> Option<((f64, f64), (f64, f64))> {
                let (xmin, ymin) = (0.0_f64, 0.0_f64);
                let (xmax, ymax) = (page_w_units, page_h_units);
                let (mut t0, mut t1) = (0.0_f64, 1.0_f64);
                let dx = b.0 - a.0;
                let dy = b.1 - a.1;
                for &(p, q) in &[(-dx, a.0 - xmin), (dx, xmax - a.0), (-dy, a.1 - ymin), (dy, ymax - a.1)] {
                    if p.abs() < 1e-12 {
                        if q < 0.0 { return None; }
                    } else {
                        let r = q / p;
                        if p < 0.0 { if r > t1 { return None; } if r > t0 { t0 = r; } }
                        else        { if r < t0 { return None; } if r < t1 { t1 = r; } }
                    }
                }
                Some(((a.0 + t0 * dx, a.1 + t0 * dy), (a.0 + t1 * dx, a.1 + t1 * dy)))
            };

            let mut rc = SvgRenderContext::new(Size::new(page_w_units, page_h_units));
            rc.clear(None, Color::WHITE);
            rc.stroke(
                Rect::new(0.5, 0.5, page_w_units - 0.5, page_h_units - 0.5),
                &Color::rgb8(180, 180, 180),
                0.5,
            );
            for path in &paths {
                let pts = path.points();
                if pts.len() < 2 { continue; }
                for w in pts.windows(2) {
                    let p0 = (w[0].x + offset_x, w[0].y + offset_y);
                    let p1 = (w[1].x + offset_x, w[1].y + offset_y);
                    if let Some((c0, c1)) = clip_to_rect(p0, p1) {
                        rc.stroke(Line::new(c0, c1), &Color::BLACK, 1.0);
                    }
                }
            }
            rc.finish().unwrap();
            let out = File::create(&output).expect("failed to create SVG");
            rc.write(out).expect("failed to write SVG");
            println!("Scene:  {} objects, {} segments  ({render_ms:.1} ms)", scene.objects.len(), paths.len());
            println!("Wrote   {output}");
        }
        Commands::ListFonts => {
            let fonts = hershey::fonts();
            let mut rows: Vec<(String, f64, f64)> = fonts.iter().map(|(name, ft)| {
                let asc = hershey_ascender(ft);
                let adv = hershey_avg_advance(ft);
                // scale to default 7.5mm line height
                let em_size = 7.5 / (1.2 * MM_PER_UNIT);
                let scale   = em_size / asc;
                let adv_mm  = adv * scale * MM_PER_UNIT;
                (name.clone(), adv_mm, asc)
            }).collect();
            rows.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
            println!("{:<14} {:>12}  {:>10}", "FONT", "adv mm/char", "ascender");
            for (name, adv_mm, asc) in &rows {
                println!("{:<14} {:>11.2}   {:>9.1}", name, adv_mm, asc);
            }
        }
        Commands::ListFaces { ttf_file } => {
            let data = std::fs::read(&ttf_file).expect("failed to read TTF file");

            // Helper: extract a name-table string by name ID.
            let get_name = |face: &ttf_parser::Face, id: u16| -> String {
                face.names().into_iter()
                    .find(|n| n.name_id == id)
                    .and_then(|n| n.to_string())
                    .unwrap_or_default()
            };

            // Count TTC faces (stops when Face::parse fails).
            let mut ttc_count = 0u32;
            for index in 0.. {
                match ttf_parser::Face::parse(&data, index) {
                    Ok(_) => ttc_count += 1,
                    Err(_) => break,
                }
            }

            for index in 0..ttc_count {
                let face = ttf_parser::Face::parse(&data, index).unwrap();
                let family = get_name(&face, 1);
                let style  = get_name(&face, 2);
                println!("face {index}: {family} {style}");

                // Variable fonts have axes rather than separate TTC faces.
                // The named instances your font browser shows are predefined
                // axis combinations; ttf-parser 0.21 exposes the axes only.
                if face.is_variable() {
                    for axis in face.variation_axes() {
                        let axis_name = get_name(&face, axis.name_id);
                        println!("  axis [{tag}] {axis_name}: {min} – {max} (default {def})",
                            tag = axis.tag,
                            axis_name = axis_name,
                            min = axis.min_value,
                            max = axis.max_value,
                            def = axis.def_value,
                        );
                    }
                    println!("  (use --ttf-axis TAG=VALUE to set axes, e.g. --ttf-axis wght=700)");
                }
            }
        }
        Commands::RenderText { text, text_file, font_name, scale } => {
            let text = resolve_text(text, text_file);
            render_text(&text, &font_name, scale);
        }
        Commands::Inspect { text, text_file, font_name, ttf_file, iosevka_file, max_velocity, acceleration, cornering, ttf_face, ttf_axes, raster_px, dp_epsilon, scale } => {
            let text   = resolve_text(text, text_file);
            let raw    = resolve_paths(&text, font_name.as_deref(), ttf_file.as_deref(), iosevka_file.as_deref(), ttf_face, raster_px, dp_epsilon, &ttf_axes, scale);
            let original = drawing_to_drawn_paths(&Drawing::new(
                raw.clone().into_iter().flatten().collect()
            ));
            let optimized = drawing_to_drawn_paths(&Drawing::new(optimize_path_order(raw, DEFAULT_MERGE_TOL)));
            let profile = motion::AccelerationProfile {
                acceleration,
                maximum_velocity: max_velocity,
                cornering_factor: cornering,
            };
            inspect(&original, &optimized, &profile);
        }
        Commands::Animate { text, text_file, font_name, ttf_file, iosevka_file, output, fps, width, height, max_velocity, acceleration, cornering, duration, snapshot, ttf_face, ttf_axes, raster_px, dp_epsilon, scale } => {
            let text = resolve_text(text, text_file);
            let raw  = resolve_paths(&text, font_name.as_deref(), ttf_file.as_deref(), iosevka_file.as_deref(), ttf_face, raster_px, dp_epsilon, &ttf_axes, scale);
            let drawing = Drawing::new(optimize_path_order(raw, DEFAULT_MERGE_TOL));
            let paths = drawing_to_drawn_paths(&drawing);
            if snapshot {
                animate::render_snapshot(&paths, &output, width, height)
                    .expect("snapshot failed");
            } else {
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
}

fn path_length(points: &[(f64, f64)]) -> f64 {
    points.windows(2).map(|w| {
        let (x1, y1) = w[0];
        let (x2, y2) = w[1];
        ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt()
    }).sum()
}

fn plan_duration(
    paths:    &[animate::DrawnPath],
    down:     &motion::AccelerationProfile,
    up:       &motion::AccelerationProfile,
) -> f64 {
    paths.iter().filter(|p| p.points.len() >= 2).map(|p| {
        let profile = if p.pen_down { down } else { up };
        let vec2d: Vec<motion::Vec2d> = p.points.iter()
            .map(|&(x, y)| motion::Vec2d::new(x, y))
            .collect();
        motion::plan_path(&vec2d, profile).duration()
    }).sum()
}

/// Reverse the character-group order for every other line (boustrophedon / snake order).
/// Odd lines are reversed so the pen travels right-to-left, eliminating end-of-line returns.
fn boustrophedon(groups: Vec<Vec<font::Path<f64>>>, line_lengths: &[usize]) -> Vec<Vec<font::Path<f64>>> {
    let mut result = Vec::with_capacity(groups.len());
    let mut i = 0;
    for (line_idx, &len) in line_lengths.iter().enumerate() {
        let end = (i + len).min(groups.len());
        let chunk = &groups[i..end];
        if line_idx % 2 == 1 {
            result.extend(chunk.iter().cloned().rev());
        } else {
            result.extend(chunk.iter().cloned());
        }
        i = end;
    }
    result
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

    // Bounding box of all pen-down points.
    let (mut min_x, mut min_y) = (f64::INFINITY, f64::INFINITY);
    let (mut max_x, mut max_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for p in original.iter().filter(|p| p.pen_down) {
        for &(x, y) in &p.points {
            min_x = min_x.min(x); max_x = max_x.max(x);
            min_y = min_y.min(y); max_y = max_y.max(y);
        }
    }
    let (bb_w, bb_h) = (max_x - min_x, max_y - min_y);

    // Metrics that differ between orderings.
    let penup_orig: f64 = original.iter().filter(|p| !p.pen_down)
        .map(|p| path_length(&p.points)).sum();
    let penup_opt: f64  = optimized.iter().filter(|p| !p.pen_down)
        .map(|p| path_length(&p.points)).sum();

    let total_orig = length_drawn + penup_orig;
    let total_opt  = length_drawn + penup_opt;

    let t0 = std::time::Instant::now();
    let time_orig = plan_duration(original, profile, profile);
    let time_opt  = plan_duration(optimized, profile, profile);
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
    let bb_w_mm = bb_w * MM_PER_UNIT;
    let bb_h_mm = bb_h * MM_PER_UNIT;
    println!("Bounding box:   {:.1} × {:.1} units  ({:.1} × {:.1} mm  /  {:.2} × {:.2} in)",
        bb_w, bb_h,
        bb_w_mm, bb_h_mm,
        bb_w_mm / 25.4, bb_h_mm / 25.4);
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

fn render_text(text: &str, font_name: &str, scale: f64) {
    let font = hershey::fonts()
        .get(&font_name.to_uppercase() as &str)
        .expect("unknown font name");
    let drawing = Drawing::new(scale_grouped(text_to_paths(text, &font), scale).into_iter().flatten().collect());
    let bounds = drawing.bounding_box();
    let size = bounds.size();

    // Create an SVG render context with the given size
    let mut rc = SvgRenderContext::new(Size::new(size.x, size.y));
    render(&mut rc, &drawing, 0.0, 1.0);
    rc.finish().unwrap();
    println!("{}", rc.display());
    let out = File::create("out.svg").unwrap();
    rc.write(out).unwrap();
}

fn render(rc: &mut impl RenderContext, drawing: &Drawing<f64>, margin: f64, stroke_width: f64) {
    let bb = drawing.bounding_box();
    let offset = Vec2d { x: -bb.left + margin, y: -bb.top + margin };

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
                stroke_width,
            );
        }
    }

    rc.finish().unwrap();
}

fn do_stuff(rc: &mut impl RenderContext) {
    rc.clear(None, Color::WHITE);
    rc.stroke(Line::new((10.0, 10.0), (100.0, 50.0)), &Color::BLUE, 1.0);
    rc.finish().unwrap();
    // rctx.stroke(shape, brush, width);
}

/// Optimize stroke ordering within each character group independently, then
/// Merge consecutive strokes whose endpoint/startpoint are within `tol` of
/// each other into a single continuous path, eliminating the pen-up/down
/// that would otherwise occur at that join.
fn chain_merge_strokes(strokes: Vec<Path<f64>>, tol: f64) -> Vec<Path<f64>> {
    if strokes.is_empty() { return strokes; }
    let mut result: Vec<Path<f64>> = Vec::new();
    let mut current: Vec<Vec2d<f64>> = strokes[0].points().clone();

    for stroke in strokes.into_iter().skip(1) {
        let pts = stroke.points();
        if pts.is_empty() { continue; }
        let last = current.last().unwrap().clone();
        let first = pts[0].clone();
        let dist = ((last.x - first.x).powi(2) + (last.y - first.y).powi(2)).sqrt();
        if dist <= tol {
            // Endpoints touch — extend current chain, skip the duplicate first point.
            current.extend_from_slice(&pts[1..]);
        } else {
            result.push(Path::new(current));
            current = pts.clone();
        }
    }
    result.push(Path::new(current));
    result
}

/// concatenate groups in character order. The pen position carried into each
/// group is the exit point of the previous group.
fn optimize_path_order(grouped: Vec<Vec<Path<f64>>>, merge_tol: f64) -> Vec<Path<f64>> {
    // Pre-filter so we can index ahead to the next non-empty group.
    let groups: Vec<Vec<Path<f64>>> = grouped.into_iter()
        .map(|g| g.into_iter().filter(|p| !p.points().is_empty()).collect::<Vec<_>>())
        .filter(|g| !g.is_empty())
        .collect();

    let mut result = vec![];
    let mut pen = (0.0f64, 0.0f64);

    for (gi, group) in groups.iter().enumerate() {
        let endpoints: Vec<optimize::PathEndpoints> = group.iter().map(|p| {
            optimize::PathEndpoints {
                start: (p.start().x, p.start().y),
                end:   (p.end().x,   p.end().y),
            }
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

        let order = if endpoints.len() <= optimize::HELD_KARP_LIMIT {
            optimize::HeldKarp.optimize(&endpoints, pen, exit_target)
        } else {
            optimize::NearestNeighbor.optimize(&endpoints, pen, exit_target)
        };

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

// Returns a set of paths that will render a string of text
// using the given font.
// Returns one Vec<Path> per character, preserving glyph grouping.
fn text_to_paths<'a>(input: &str, ft: &'a font::Font) -> Vec<Vec<Path<f64>>> {
    let spacing = 0;
    let line_height = hershey_line_height(ft);
    let mut out = vec![];
    for (line_idx, line) in input.split('\n').enumerate() {
        let y_offset = line_idx as f64 * line_height;
        let mut x = 0;
        for ch in line.chars() {
            let index = (ch as usize).wrapping_sub(32);
            if index >= ft.len() {
                x += spacing;
                continue;
            }
            let glyph = &ft[index];
            let mut glyph_paths = vec![];
            for glyph_path in &glyph.paths {
                let mut new_path: Path<f64> = Path::empty();
                for point in glyph_path.points() {
                    new_path.push(Vec2d {
                        x: (x as f64) + (point.x as f64) - (glyph.left as f64),
                        y: point.y as f64 + y_offset,
                    });
                }
                glyph_paths.push(new_path);
            }
            out.push(glyph_paths);
            x += glyph.right - glyph.left + spacing;
        }
    }
    out
}

/// Compute line height for a Hershey font: full Y range of all glyphs × 1.2.
fn hershey_line_height(ft: &font::Font) -> f64 {
    let mut min_y = i32::MAX;
    let mut max_y = i32::MIN;
    for glyph in ft.iter() {
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

/// Height of ascenders above the baseline (|min_y| across all glyphs).
fn hershey_ascender(ft: &font::Font) -> f64 {
    let min_y = ft.iter()
        .flat_map(|g| g.paths.iter())
        .flat_map(|p| p.points().iter())
        .map(|pt| pt.y)
        .min()
        .unwrap_or(-9);
    (-min_y).max(1) as f64
}

/// Average advance width across printable ASCII glyphs (native Hershey units).
fn hershey_avg_advance(ft: &font::Font) -> f64 {
    let n = ft.len().min(95); // space through ~
    if n == 0 { return 16.0; }
    ft[..n].iter().map(|g| (g.right - g.left) as f64).sum::<f64>() / n as f64
}

/// Render `text` using a Hershey font, scaled so the ascender height equals `em_size`
/// output units. Lines are spaced `em_size * 1.2 + line_gap` apart (same formula as
/// `IosevkaFont::text_to_paths`) so the layout math in `print-page` stays consistent.
fn hershey_text_to_paths_scaled(
    text:     &str,
    ft:       &font::Font,
    em_size:  f64,
    line_gap: f64,
) -> Vec<Vec<font::Path<f64>>> {
    let ascender    = hershey_ascender(ft);
    let scale       = em_size / ascender;
    let line_height = em_size * 1.2 + line_gap;

    let mut result  = Vec::new();
    let mut line_y  = 0.0f64;

    for (line_idx, line) in text.split('\n').enumerate() {
        if line_idx > 0 { line_y += line_height; }
        let mut cursor_x = 0.0f64;
        for ch in line.chars() {
            let index = (ch as usize).wrapping_sub(32);
            if index >= ft.len() { continue; }
            let glyph   = &ft[index];
            let advance = (glyph.right - glyph.left) as f64 * scale;
            let paths: Vec<font::Path<f64>> = glyph.paths.iter()
                .filter(|p| p.points().len() >= 2)
                .map(|path| font::Path::new(path.points().iter().map(|pt| font::Vec2d {
                    x: cursor_x + (pt.x - glyph.left) as f64 * scale,
                    y: line_y   + pt.y as f64 * scale,
                }).collect()))
                .collect();
            result.push(paths);
            cursor_x += advance;
        }
    }
    result
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
            // Move to the start of the next path (pen up), but only if we
            // aren't already there.
            let next_start = &path.points()[0];
            if prev_position.x != next_start.x || prev_position.y != next_start.y {
                out_paths.push(PenPath::PenUp(Path::new(vec![
                    prev_position.clone(),
                    next_start.clone(),
                ])));
            }
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
