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

// ── Guide lines ───────────────────────────────────────────────────────────────

/// A display-only guide line (not plotted; shown as a dashed overlay in preview).
#[derive(Clone)]
enum Guide {
    Vertical(f64),   // x position in plot units
    Horizontal(f64), // y position in plot units
}

/// Parse a human-readable length string into plot units (1 unit = 0.3528 mm = 1 typographic point).
/// Supported suffixes: `mm`, `cm`, `in`, `inch`.
fn parse_length_to_units(s: &str) -> Result<f64, String> {
    const MM_PER_UNIT: f64 = 0.3528;
    let s = s.trim().to_lowercase();
    let (num_str, mm_per) =
        if      let Some(n) = s.strip_suffix("inch") { (n.trim(), 25.4) }
        else if let Some(n) = s.strip_suffix("in")   { (n.trim(), 25.4) }
        else if let Some(n) = s.strip_suffix("cm")   { (n.trim(), 10.0) }
        else if let Some(n) = s.strip_suffix("mm")   { (n.trim(),  1.0) }
        else { return Err(format!("unrecognized unit in \"{s}\" — use mm, cm, in, or inch")); };
    let val: f64 = num_str.parse()
        .map_err(|_| format!("invalid number in \"{s}\""))?;
    Ok(val * mm_per / MM_PER_UNIT)
}

// ── Block layout ─────────────────────────────────────────────────────────────

fn json_null() -> serde_json::Value { serde_json::Value::Null }

/// Shared text entry used at the top level and inside blocks.
#[derive(serde::Deserialize, Clone)]
struct TextEntry {
    font:   String,
    size:   f64,
    text:   String,
    pos:    [f64; 2],
    #[serde(default)] halign: Option<String>,
    #[serde(default)] valign: Option<String>,
}

/// Raw block as parsed from JSON (before layout).
#[derive(serde::Deserialize)]
struct BlockRaw {
    pos:    [serde_json::Value; 2],
    #[serde(default)] width:      Option<serde_json::Value>,
    #[serde(default)] height:     Option<serde_json::Value>,
    #[serde(default)] min_width:  Option<serde_json::Value>,
    #[serde(default)] max_width:  Option<serde_json::Value>,
    #[serde(default)] min_height: Option<serde_json::Value>,
    #[serde(default)] max_height: Option<serde_json::Value>,
    #[serde(default = "json_null")] padding: serde_json::Value,
    #[serde(default = "json_null")] border:  serde_json::Value,
    #[serde(default = "json_null")] fill:    serde_json::Value,
    #[serde(default)] text:   Vec<TextEntry>,
    #[serde(default)] blocks: Vec<BlockRaw>,
}

#[derive(Clone, Copy)]
enum BorderStyle { Solid, Dashed, Dotted }

struct BorderSides {
    top: Option<BorderStyle>, right:  Option<BorderStyle>,
    bottom: Option<BorderStyle>, left: Option<BorderStyle>,
}
impl BorderSides {
    fn none() -> Self { Self { top: None, right: None, bottom: None, left: None } }
    fn all(s: BorderStyle) -> Self { Self { top: Some(s), right: Some(s), bottom: Some(s), left: Some(s) } }
}

struct PaddingBox { top: f64, right: f64, bottom: f64, left: f64 }
impl PaddingBox {
    fn zero() -> Self { Self { top: 0.0, right: 0.0, bottom: 0.0, left: 0.0 } }
}

fn parse_pos_value(v: &serde_json::Value) -> Result<f64, String> {
    match v {
        serde_json::Value::Number(n) => Ok(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => parse_length_to_units(s),
        _ => Err(format!("pos must be a number or length string, got {v}")),
    }
}

fn parse_border_side(v: &serde_json::Value) -> Result<Option<BorderStyle>, String> {
    match v {
        serde_json::Value::Bool(true)  => Ok(Some(BorderStyle::Solid)),
        serde_json::Value::Bool(false) | serde_json::Value::Null => Ok(None),
        serde_json::Value::String(s) => match s.as_str() {
            "solid"  => Ok(Some(BorderStyle::Solid)),
            "dashed" => Ok(Some(BorderStyle::Dashed)),
            "dotted" => Ok(Some(BorderStyle::Dotted)),
            other    => Err(format!("unknown border style \"{other}\"")),
        },
        _ => Err("border side must be bool or style string".to_string()),
    }
}

fn parse_border(v: &serde_json::Value) -> Result<BorderSides, String> {
    match v {
        serde_json::Value::Null | serde_json::Value::Bool(false) => Ok(BorderSides::none()),
        serde_json::Value::Bool(true)  => Ok(BorderSides::all(BorderStyle::Solid)),
        serde_json::Value::String(s) => match s.as_str() {
            "solid"  => Ok(BorderSides::all(BorderStyle::Solid)),
            "dashed" => Ok(BorderSides::all(BorderStyle::Dashed)),
            "dotted" => Ok(BorderSides::all(BorderStyle::Dotted)),
            other    => Err(format!("unknown border style \"{other}\"")),
        },
        serde_json::Value::Object(map) => {
            let g = |k: &str| parse_border_side(map.get(k).unwrap_or(&serde_json::Value::Null));
            Ok(BorderSides { top: g("top")?, right: g("right")?, bottom: g("bottom")?, left: g("left")? })
        }
        _ => Err("border must be bool, style string, or per-side object".to_string()),
    }
}

fn parse_padding(v: &serde_json::Value) -> Result<PaddingBox, String> {
    match v {
        serde_json::Value::Null => Ok(PaddingBox::zero()),
        serde_json::Value::Number(n) => {
            let f = n.as_f64().unwrap_or(0.0);
            Ok(PaddingBox { top: f, right: f, bottom: f, left: f })
        }
        serde_json::Value::String(s) => {
            let f = parse_length_to_units(s)?;
            Ok(PaddingBox { top: f, right: f, bottom: f, left: f })
        }
        serde_json::Value::Object(map) => {
            let side = |k: &str| -> Result<f64, String> { match map.get(k) {
                None => Ok(0.0),
                Some(serde_json::Value::Number(n)) => Ok(n.as_f64().unwrap_or(0.0)),
                Some(serde_json::Value::String(s)) => parse_length_to_units(s),
                _ => Err(format!("padding.{k} must be a number or length string")),
            }};
            Ok(PaddingBox { top: side("top")?, right: side("right")?, bottom: side("bottom")?, left: side("left")? })
        }
        _ => Err("padding must be a number, length string, or per-side object".to_string()),
    }
}

/// Parse an outer dimension: length string/number → absolute units; `"100%"` → `parent`.
fn parse_dim(v: &serde_json::Value, parent: Option<f64>) -> Result<f64, String> {
    if let serde_json::Value::String(s) = v {
        if s.trim() == "100%" {
            return parent.ok_or_else(|| "\"100%\" requires a parent with known width".to_string());
        }
        return parse_length_to_units(s);
    }
    if let serde_json::Value::Number(n) = v { return Ok(n.as_f64().unwrap_or(0.0)); }
    Err(format!("dimension must be a length string or \"100%\", got {v}"))
}

// ── Border path generation ────────────────────────────────────────────────────

fn dash_line(x1: f64, y1: f64, x2: f64, y2: f64, dash: f64, gap: f64) -> Vec<Path<f64>> {
    let (dx, dy) = (x2 - x1, y2 - y1);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-9 { return vec![]; }
    let (ux, uy) = (dx / len, dy / len);
    let mut paths = vec![];
    let mut t = 0.0_f64;
    while t < len {
        let end = (t + dash).min(len);
        if end > t + 1e-9 {
            paths.push(Path::new(vec![
                Vec2d::new(x1 + ux * t,   y1 + uy * t),
                Vec2d::new(x1 + ux * end, y1 + uy * end),
            ]));
        }
        t += dash + gap;
    }
    paths
}

fn styled_line(x1: f64, y1: f64, x2: f64, y2: f64, s: BorderStyle) -> Vec<Path<f64>> {
    match s {
        BorderStyle::Solid  => vec![Path::new(vec![Vec2d::new(x1, y1), Vec2d::new(x2, y2)])],
        BorderStyle::Dashed => dash_line(x1, y1, x2, y2, 11.34, 5.67), // 4 mm / 2 mm
        BorderStyle::Dotted => dash_line(x1, y1, x2, y2,  1.42, 5.67), // 0.5 mm / 2 mm
    }
}

fn box_border_paths(bx: f64, by: f64, bw: f64, bh: f64, b: &BorderSides) -> Vec<Path<f64>> {
    let (x0, x1, y0, y1) = (bx, bx + bw, by, by + bh);
    let mut p = vec![];
    if let Some(s) = b.top    { p.extend(styled_line(x0, y0, x1, y0, s)); }
    if let Some(s) = b.right  { p.extend(styled_line(x1, y0, x1, y1, s)); }
    if let Some(s) = b.bottom { p.extend(styled_line(x0, y1, x1, y1, s)); }
    if let Some(s) = b.left   { p.extend(styled_line(x0, y0, x0, y1, s)); }
    p
}

// ── Fill path generation ──────────────────────────────────────────────────────

enum FillKind {
    Hatch(Vec<f64>), // angles in degrees
    Dots,
    Zigzag,
    Waves,
    Concentric,
    Hex,
    Brick,
}

struct FillSpec {
    kind:    FillKind,
    spacing: f64,
}

fn default_fill_spacing() -> f64 { 5.67 } // ~2 mm

fn parse_fill(v: &serde_json::Value) -> Result<Option<FillSpec>, String> {
    const VALID: &str = "\"hatch\", \"crosshatch\", \"dots\", \"zigzag\", \"waves\", \"concentric\", \"hex\", or \"brick\"";
    match v {
        serde_json::Value::Null | serde_json::Value::Bool(false) => return Ok(None),
        serde_json::Value::String(s) => {
            let kind = match s.as_str() {
                "hatch"      => FillKind::Hatch(vec![45.0]),
                "crosshatch" => FillKind::Hatch(vec![45.0, 135.0]),
                "dots"       => FillKind::Dots,
                "zigzag"     => FillKind::Zigzag,
                "waves"      => FillKind::Waves,
                "concentric" => FillKind::Concentric,
                "hex"        => FillKind::Hex,
                "brick"      => FillKind::Brick,
                other        => return Err(format!("unknown fill \"{other}\" — use {VALID}")),
            };
            return Ok(Some(FillSpec { kind, spacing: default_fill_spacing() }));
        }
        serde_json::Value::Object(map) => {
            let spacing = match map.get("spacing") {
                None                                     => default_fill_spacing(),
                Some(serde_json::Value::Number(n))       => n.as_f64().unwrap_or(default_fill_spacing()),
                Some(serde_json::Value::String(s))       => parse_length_to_units(s)?,
                Some(other) => return Err(format!("fill spacing must be number or length string, got {other}")),
            };
            let fill_type = map.get("type").and_then(|v| v.as_str()).unwrap_or("hatch");
            let kind = match fill_type {
                "dots"       => FillKind::Dots,
                "zigzag"     => FillKind::Zigzag,
                "waves"      => FillKind::Waves,
                "concentric" => FillKind::Concentric,
                "hex"        => FillKind::Hex,
                "brick"      => FillKind::Brick,
                "hatch" => {
                    let angle = match map.get("angle") {
                        None                               => 45.0,
                        Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(45.0),
                        Some(other) => return Err(format!("fill angle must be a number, got {other}")),
                    };
                    FillKind::Hatch(vec![angle])
                }
                "crosshatch" => {
                    let angles = match map.get("angles") {
                        None => vec![45.0, 135.0],
                        Some(serde_json::Value::Array(arr)) => arr.iter()
                            .map(|v| v.as_f64().ok_or_else(|| "fill angles must be numbers".to_string()))
                            .collect::<Result<Vec<_>, _>>()?,
                        Some(other) => return Err(format!("fill angles must be an array, got {other}")),
                    };
                    FillKind::Hatch(angles)
                }
                other => return Err(format!("unknown fill type \"{other}\" — use {VALID}")),
            };
            Ok(Some(FillSpec { kind, spacing }))
        }
        _ => Err("fill must be a string or object".to_string()),
    }
}

/// Clip line through (px, py) with direction (dx, dy) to axis-aligned rect [x0,x1]×[y0,y1].
fn clip_line_to_rect(
    px: f64, py: f64, dx: f64, dy: f64,
    x0: f64, y0: f64, x1: f64, y1: f64,
) -> Option<((f64, f64), (f64, f64))> {
    let mut s_min = f64::NEG_INFINITY;
    let mut s_max = f64::INFINITY;
    if dx.abs() < 1e-12 {
        if px < x0 || px > x1 { return None; }
    } else {
        let t1 = (x0 - px) / dx;
        let t2 = (x1 - px) / dx;
        s_min = s_min.max(t1.min(t2));
        s_max = s_max.min(t1.max(t2));
    }
    if dy.abs() < 1e-12 {
        if py < y0 || py > y1 { return None; }
    } else {
        let t1 = (y0 - py) / dy;
        let t2 = (y1 - py) / dy;
        s_min = s_min.max(t1.min(t2));
        s_max = s_max.min(t1.max(t2));
    }
    if s_min >= s_max - 1e-9 { return None; }
    Some(((px + dx * s_min, py + dy * s_min), (px + dx * s_max, py + dy * s_max)))
}

fn hatch_rect_paths(x0: f64, y0: f64, x1: f64, y1: f64, angle_deg: f64, spacing: f64) -> Vec<Path<f64>> {
    let r = angle_deg.to_radians();
    let (da, db) = (r.cos(), r.sin()); // line direction
    let (na, nb) = (-db,     da);       // perpendicular
    let corners  = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
    let ts: Vec<f64> = corners.iter().map(|(x, y)| x * na + y * nb).collect();
    let t_min = ts.iter().cloned().fold(f64::INFINITY,     f64::min);
    let t_max = ts.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let mut paths = vec![];
    let mut t = (t_min / spacing).floor() * spacing;
    while t <= t_max + 1e-9 {
        if let Some((p1, p2)) = clip_line_to_rect(t * na, t * nb, da, db, x0, y0, x1, y1) {
            paths.push(Path::new(vec![Vec2d::new(p1.0, p1.1), Vec2d::new(p2.0, p2.1)]));
        }
        t += spacing;
    }
    paths
}

fn dots_rect_paths(x0: f64, y0: f64, x1: f64, y1: f64, spacing: f64) -> Vec<Path<f64>> {
    let mut paths = vec![];
    let mut y = y0 + spacing * 0.5;
    while y <= y1 + 1e-9 {
        let mut x = x0 + spacing * 0.5;
        while x <= x1 + 1e-9 {
            paths.push(Path::new(vec![Vec2d::new(x, y), Vec2d::new(x, y)]));
            x += spacing;
        }
        y += spacing;
    }
    paths
}

/// Liang-Barsky segment clipping to an axis-aligned rect.
fn clip_segment_to_rect(
    ax: f64, ay: f64, bx: f64, by: f64,
    x0: f64, y0: f64, x1: f64, y1: f64,
) -> Option<((f64, f64), (f64, f64))> {
    let dx = bx - ax;
    let dy = by - ay;
    let mut t0 = 0.0f64;
    let mut t1 = 1.0f64;
    for (p, q) in [(-dx, ax - x0), (dx, x1 - ax), (-dy, ay - y0), (dy, y1 - ay)] {
        if p.abs() < 1e-12 {
            if q < 0.0 { return None; }
        } else {
            let r = q / p;
            if p < 0.0 { t0 = t0.max(r); } else { t1 = t1.min(r); }
        }
    }
    if t0 >= t1 - 1e-9 { return None; }
    Some(((ax + t0 * dx, ay + t0 * dy), (ax + t1 * dx, ay + t1 * dy)))
}

/// Rows of connected V-shapes; adjacent rows interlock to form a diamond lattice.
fn zigzag_rect_paths(x0: f64, y0: f64, x1: f64, y1: f64, spacing: f64) -> Vec<Path<f64>> {
    let amplitude = spacing * 0.5;
    let tooth_w   = spacing;
    let mut paths = vec![];
    let mut row   = 0i64;
    let mut y_c   = y0 + spacing * 0.5;
    while y_c <= y1 + 1e-9 {
        // Alternate rows offset by half a tooth so peaks of one row meet troughs of the next.
        let phase    = row.rem_euclid(2) as f64 * tooth_w;
        let i_start  = ((x0 - phase) / tooth_w).floor() as i64;
        let i_end    = ((x1 - phase) / tooth_w).ceil()  as i64;
        let xs: Vec<f64> = (i_start..=i_end).map(|i| phase + i as f64 * tooth_w).collect();
        let ys: Vec<f64> = (i_start..=i_end).map(|i| {
            let y = if i.rem_euclid(2) == 0 { y_c - amplitude } else { y_c + amplitude };
            y.clamp(y0, y1)
        }).collect();
        let mut pts: Vec<Vec2d<f64>> = vec![];
        for seg in 0..xs.len().saturating_sub(1) {
            let (ax, ay) = (xs[seg],     ys[seg]);
            let (bx, by) = (xs[seg + 1], ys[seg + 1]);
            if bx <= x0 || ax >= x1 { continue; }
            let ta = if ax < x0 { (x0 - ax) / (bx - ax) } else { 0.0 };
            let tb = if bx > x1 { (x1 - ax) / (bx - ax) } else { 1.0 };
            let pa = (ax + ta * (bx - ax), ay + ta * (by - ay));
            let pb = (ax + tb * (bx - ax), ay + tb * (by - ay));
            if pts.is_empty() { pts.push(Vec2d::new(pa.0, pa.1)); }
            pts.push(Vec2d::new(pb.0, pb.1));
        }
        if pts.len() >= 2 { paths.push(Path::new(pts)); }
        y_c += spacing;
        row += 1;
    }
    paths
}

/// Rows of sinusoidal waves; adjacent rows are phase-inverted so they interlock.
fn waves_rect_paths(x0: f64, y0: f64, x1: f64, y1: f64, spacing: f64) -> Vec<Path<f64>> {
    let amplitude  = spacing * 0.38;
    let wavelength = spacing * 2.0;
    let steps = (((x1 - x0) / spacing * 12.0).ceil() as usize).max(4);
    let mut paths = vec![];
    let mut row   = 0i64;
    let mut y_c   = y0 + spacing * 0.5;
    while y_c <= y1 + 1e-9 {
        let phase = row.rem_euclid(2) as f64 * std::f64::consts::PI;
        let pts: Vec<Vec2d<f64>> = (0..=steps).map(|i| {
            let x = x0 + (x1 - x0) * i as f64 / steps as f64;
            let y = y_c + amplitude * (phase + 2.0 * std::f64::consts::PI * x / wavelength).sin();
            Vec2d::new(x, y.clamp(y0, y1))
        }).collect();
        if pts.len() >= 2 { paths.push(Path::new(pts)); }
        y_c += spacing;
        row += 1;
    }
    paths
}

/// Rectangles inset from the boundary, stepping inward by `spacing`.
fn concentric_rect_paths(x0: f64, y0: f64, x1: f64, y1: f64, spacing: f64) -> Vec<Path<f64>> {
    let mut paths = vec![];
    let mut inset = spacing * 0.5;
    while x0 + inset < x1 - inset && y0 + inset < y1 - inset {
        let (ix0, iy0, ix1, iy1) = (x0 + inset, y0 + inset, x1 - inset, y1 - inset);
        paths.push(Path::new(vec![
            Vec2d::new(ix0, iy0), Vec2d::new(ix1, iy0),
            Vec2d::new(ix1, iy1), Vec2d::new(ix0, iy1),
            Vec2d::new(ix0, iy0),
        ]));
        inset += spacing;
    }
    paths
}

/// Pointy-top hexagonal grid; each hex edge clipped to the bounding rect.
fn hex_rect_paths(x0: f64, y0: f64, x1: f64, y1: f64, spacing: f64) -> Vec<Path<f64>> {
    let r     = spacing;
    let sqrt3 = 3f64.sqrt();
    let sx    = r * sqrt3;   // horizontal center-to-center distance
    let sy    = r * 1.5;     // vertical center-to-center distance
    // Pointy-top vertices relative to center, starting from top, clockwise.
    let rel_v: [(f64, f64); 6] = [
        ( 0.0,            r     ),
        ( r * sqrt3 / 2.0,  r * 0.5),
        ( r * sqrt3 / 2.0, -r * 0.5),
        ( 0.0,           -r     ),
        (-r * sqrt3 / 2.0, -r * 0.5),
        (-r * sqrt3 / 2.0,  r * 0.5),
    ];
    let mut paths = vec![];
    let row_end = ((y1 - y0) / sy).ceil() as i64 + 2;
    for row in -2..=row_end {
        let cy       = y0 + row as f64 * sy;
        let x_offset = if row.rem_euclid(2) == 0 { 0.0 } else { sx * 0.5 };
        let col_start = ((x0 - x_offset) / sx).floor() as i64 - 1;
        let col_end   = ((x1 - x_offset) / sx).ceil()  as i64 + 1;
        for col in col_start..=col_end {
            let cx = x_offset + col as f64 * sx;
            let verts: Vec<(f64, f64)> = rel_v.iter().map(|(dx, dy)| (cx + dx, cy + dy)).collect();
            for i in 0..6 {
                let (ax, ay) = verts[i];
                let (bx, by) = verts[(i + 1) % 6];
                if let Some(((px, py), (qx, qy))) = clip_segment_to_rect(ax, ay, bx, by, x0, y0, x1, y1) {
                    paths.push(Path::new(vec![Vec2d::new(px, py), Vec2d::new(qx, qy)]));
                }
            }
        }
    }
    paths
}

/// Brick-wall pattern: horizontal mortar lines with alternating vertical joints.
fn brick_rect_paths(x0: f64, y0: f64, x1: f64, y1: f64, spacing: f64) -> Vec<Path<f64>> {
    let brick_w = spacing * 2.0;
    let brick_h = spacing;
    let mut paths = vec![];
    // Horizontal mortar lines.
    let mut y = y0;
    while y <= y1 + 1e-9 {
        paths.push(Path::new(vec![Vec2d::new(x0, y), Vec2d::new(x1, y)]));
        y += brick_h;
    }
    // Vertical joints, alternating phase per row.
    let mut row   = 0i64;
    let mut y_top = y0;
    while y_top < y1 - 1e-9 {
        let phase = row.rem_euclid(2) as f64 * brick_w * 0.5;
        let col_start = ((x0 - phase) / brick_w).floor() as i64;
        let col_end   = ((x1 - phase) / brick_w).ceil()  as i64;
        for col in col_start..=col_end {
            let x = phase + col as f64 * brick_w;
            if x > x0 + 1e-9 && x < x1 - 1e-9 {
                let y_bot = (y_top + brick_h).min(y1);
                paths.push(Path::new(vec![Vec2d::new(x, y_top), Vec2d::new(x, y_bot)]));
            }
        }
        y_top += brick_h;
        row   += 1;
    }
    paths
}

fn fill_rect_paths(x0: f64, y0: f64, x1: f64, y1: f64, fill: &FillSpec) -> Vec<Path<f64>> {
    match &fill.kind {
        FillKind::Hatch(angles) => angles.iter()
            .flat_map(|&a| hatch_rect_paths(x0, y0, x1, y1, a, fill.spacing))
            .collect(),
        FillKind::Dots       => dots_rect_paths(x0, y0, x1, y1, fill.spacing),
        FillKind::Zigzag     => zigzag_rect_paths(x0, y0, x1, y1, fill.spacing),
        FillKind::Waves      => waves_rect_paths(x0, y0, x1, y1, fill.spacing),
        FillKind::Concentric => concentric_rect_paths(x0, y0, x1, y1, fill.spacing),
        FillKind::Hex        => hex_rect_paths(x0, y0, x1, y1, fill.spacing),
        FillKind::Brick      => brick_rect_paths(x0, y0, x1, y1, fill.spacing),
    }
}

// ── Text entry helpers ────────────────────────────────────────────────────────

/// Generate raw (unpositioned) font paths for a text entry.
fn raw_font_paths(entry: &TextEntry) -> Result<Vec<Vec<Path<f64>>>, String> {
    if entry.font.to_uppercase() == "IOSEVKA" {
        const SK: &str = include_str!("../assets/iosevka_skeleton.json");
        let font = iosevka::IosevkaFont::from_json(SK).map_err(|e| format!("Iosevka: {e}"))?;
        Ok(font.text_to_paths(&entry.text, entry.size, 0.0))
    } else {
        let fonts = hershey::fonts();
        let font  = fonts.get(&entry.font.to_uppercase() as &str)
            .ok_or_else(|| format!("unknown font \"{}\"", entry.font))?;
        Ok(scale_grouped(hershey_text_to_paths(&entry.text, font), entry.size))
    }
}

/// Shift `raw` paths to their final absolute position.
/// `origin_{x,y}`: absolute content-area origin.
/// `center_ref_x`: the x value (within the content area) that `halign:"center"` pivots on.
fn place_text(
    raw: Vec<Vec<Path<f64>>>,
    entry: &TextEntry,
    origin_x: f64, origin_y: f64,
    center_ref_x: f64,
) -> Result<Vec<Vec<Path<f64>>>, String> {
    let (bb_min_x, bb_max_x, bb_min_y, bb_max_y) = grouped_bbox(&raw);
    let [px, py] = entry.pos;
    let x_ref = origin_x + px + match entry.halign.as_deref() {
        Some("center") => center_ref_x,
        _ => 0.0,
    };
    let dx = x_ref - match entry.halign.as_deref() {
        None | Some("left")  => bb_min_x,
        Some("center")       => (bb_min_x + bb_max_x) * 0.5,
        Some("right")        => bb_max_x,
        Some(other)          => return Err(format!("unknown halign \"{other}\"")),
    };
    let dy = origin_y + py - match entry.valign.as_deref() {
        None | Some("top")   => bb_min_y,
        Some("middle")       => (bb_min_y + bb_max_y) * 0.5,
        Some("bottom")       => bb_max_y,
        Some(other)          => return Err(format!("unknown valign \"{other}\"")),
    };
    Ok(raw.into_iter().map(|group| {
        group.into_iter().map(|path| {
            Path::new(path.points().iter().map(|p| Vec2d::new(p.x + dx, p.y + dy)).collect())
        }).collect()
    }).collect())
}

// ── Recursive block layout ────────────────────────────────────────────────────

struct BlockResult {
    grouped: Vec<Vec<Path<f64>>>,
    outer_w: f64,
    outer_h: f64,
}

fn layout_block(
    block:    &BlockRaw,
    origin_x: f64,         // absolute top-left of parent content area
    origin_y: f64,
    parent_w: Option<f64>, // parent content width (needed for "100%")
) -> Result<BlockResult, String> {
    let pos_x = parse_pos_value(&block.pos[0])?;
    let pos_y = parse_pos_value(&block.pos[1])?;
    let bx = origin_x + pos_x;
    let by = origin_y + pos_y;

    let pad    = parse_padding(&block.padding)?;
    let border = parse_border(&block.border)?;
    let fill   = parse_fill(&block.fill)?;

    // Content-area origin (inside padding).
    let cx = bx + pad.left;
    let cy = by + pad.top;

    // Outer dimension → content dimension.
    // Subtract pos_x so "100%" fills from this block's left edge to the parent's right edge.
    let effective_parent_w = parent_w.map(|pw| pw - pos_x);
    let explicit_cw: Option<f64> = block.width.as_ref()
        .map(|v| parse_dim(v, effective_parent_w).map(|ow| ow - pad.left - pad.right))
        .transpose()?;
    let explicit_ch: Option<f64> = block.height.as_ref()
        .map(|v| parse_dim(v, None).map(|oh| oh - pad.top - pad.bottom))
        .transpose()?;

    // Generate raw font paths once (used for both sizing and rendering).
    let raw_texts: Vec<Vec<Vec<Path<f64>>>> = block.text.iter()
        .map(raw_font_paths).collect::<Result<_, _>>()?;

    // Recursively layout children (pass explicit_cw so they can use "100%").
    let child_results: Vec<BlockResult> = block.blocks.iter()
        .map(|b| layout_block(b, cx, cy, explicit_cw))
        .collect::<Result<_, _>>()?;

    // Helper: parse an outer dimension and convert to content dimension.
    let to_cw = |v: &serde_json::Value| -> Result<f64, String> {
        parse_dim(v, effective_parent_w).map(|ow| (ow - pad.left - pad.right).max(0.0))
    };
    let to_ch = |v: &serde_json::Value| -> Result<f64, String> {
        parse_dim(v, None).map(|oh| (oh - pad.top - pad.bottom).max(0.0))
    };

    // Auto content-width: max right extent across unpositioned text + child boxes.
    let raw_cw = explicit_cw.unwrap_or_else(|| {
        let from_text = raw_texts.iter().zip(block.text.iter()).map(|(raw, e)| {
            let (min_x, max_x, _, _) = grouped_bbox(raw);
            let w = (max_x - min_x).max(0.0);
            match e.halign.as_deref() {
                Some("right")  => e.pos[0],
                Some("center") => e.pos[0] + w * 0.5,
                _              => e.pos[0] + w,
            }
        }).fold(0.0_f64, f64::max);
        let from_children = block.blocks.iter().zip(child_results.iter())
            .map(|(b, r)| parse_pos_value(&b.pos[0]).unwrap_or(0.0) + r.outer_w)
            .fold(0.0_f64, f64::max);
        from_text.max(from_children).max(0.0)
    });
    let min_cw = block.min_width.as_ref().map(|v| to_cw(v)).transpose()?;
    let max_cw = block.max_width.as_ref().map(|v| to_cw(v)).transpose()?;
    let content_w = raw_cw
        .max(min_cw.unwrap_or(0.0))
        .min(max_cw.unwrap_or(f64::INFINITY));

    // Render pass: position all text and collect child paths.
    let mut inner: Vec<Vec<Path<f64>>> = vec![];
    for (raw, entry) in raw_texts.into_iter().zip(block.text.iter()) {
        inner.extend(place_text(raw, entry, cx, cy, content_w * 0.5)?);
    }
    for child in &child_results {
        inner.extend(child.grouped.iter().cloned());
    }

    // Auto content-height: max bottom extent of all rendered content.
    let raw_ch = explicit_ch.unwrap_or_else(|| {
        if inner.is_empty() { return 0.0; }
        let (_, _, _, max_y) = grouped_bbox(&inner);
        if max_y.is_finite() { (max_y - cy).max(0.0) } else { 0.0 }
    });
    let min_ch = block.min_height.as_ref().map(|v| to_ch(v)).transpose()?;
    let max_ch = block.max_height.as_ref().map(|v| to_ch(v)).transpose()?;
    let content_h = raw_ch
        .max(min_ch.unwrap_or(0.0))
        .min(max_ch.unwrap_or(f64::INFINITY));

    let outer_w = content_w + pad.left + pad.right;
    let outer_h = content_h + pad.top  + pad.bottom;

    // Build final grouped: fill → content → border (so border draws last / on top).
    let mut grouped: Vec<Vec<Path<f64>>> = vec![];
    if let Some(ref f) = fill {
        for path in fill_rect_paths(bx, by, bx + outer_w, by + outer_h, f) {
            grouped.push(vec![path]);
        }
    }
    grouped.extend(inner);
    for path in box_border_paths(bx, by, outer_w, outer_h, &border) {
        grouped.push(vec![path]);
    }

    Ok(BlockResult { grouped, outer_w, outer_h })
}

// ── PlotRenderer ─────────────────────────────────────────────────────────────

#[cfg_attr(feature = "wasm", wasm_bindgen)]
pub struct PlotRenderer {
    timeline:       Vec<Segment>,
    total_duration: f64,
    min_x: f64, min_y: f64,
    max_x: f64, max_y: f64,
    guides:         Vec<Guide>,
    /// Paper dimensions in plot units `[width, height]`, if defined.
    paper:          Option<[f64; 2]>,
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

    /// Build a renderer from a JSON layout descriptor.
    ///
    /// Expected format: `{"text":[{"font":"FUTURAL","size":1.0,"text":"hi","pos":[x,y]},...]}`
    ///
    /// `font` may be any Hershey font name or `"IOSEVKA"`.
    /// For Hershey fonts `size` is a scale multiplier; for Iosevka it is the em-size.
    /// `pos` offsets the rendered text in output-coordinate units.
    pub fn from_text_layout(json: &str) -> Result<Self, String> {
        #[derive(serde::Deserialize)]
        struct GuideEntry {
            left: Option<String>, right:  Option<String>,
            top:  Option<String>, bottom: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct PaperEntry { width: String, height: String }
        #[derive(serde::Deserialize)]
        struct TopLayout {
            #[serde(default)] paper:  Option<PaperEntry>,
            #[serde(default)] text:   Vec<TextEntry>,
            #[serde(default)] blocks: Vec<BlockRaw>,
            #[serde(default)] guides: Vec<GuideEntry>,
        }

        let layout: TopLayout = serde_json::from_str(json)
            .map_err(|e| format!("JSON parse error: {e}"))?;

        let paper: Option<[f64; 2]> = layout.paper.map(|p| -> Result<_, String> {
            Ok([parse_length_to_units(&p.width)?, parse_length_to_units(&p.height)?])
        }).transpose()?;

        let paper_center_x = paper.map(|[w, _]| w * 0.5).unwrap_or(0.0);
        let mut all_grouped: Vec<Vec<Path<f64>>> = Vec::new();

        // Top-level text entries.
        for entry in &layout.text {
            let raw = raw_font_paths(entry)?;
            all_grouped.extend(place_text(raw, entry, 0.0, 0.0, paper_center_x)?);
        }

        // Top-level blocks.
        let paper_content_w = paper.map(|[w, _]| w);
        for block in &layout.blocks {
            let result = layout_block(block, 0.0, 0.0, paper_content_w)?;
            all_grouped.extend(result.grouped);
        }

        // Guide lines.
        let mut guides: Vec<Guide> = Vec::new();
        for g in layout.guides {
            if let Some(s) = g.left   { guides.push(Guide::Vertical(parse_length_to_units(&s)?)); }
            if let Some(s) = g.right  {
                let v = parse_length_to_units(&s)?;
                guides.push(Guide::Vertical(paper.map(|[w, _]| w - v).unwrap_or(v)));
            }
            if let Some(s) = g.top    { guides.push(Guide::Horizontal(parse_length_to_units(&s)?)); }
            if let Some(s) = g.bottom {
                let v = parse_length_to_units(&s)?;
                guides.push(Guide::Horizontal(paper.map(|[_, h]| h - v).unwrap_or(v)));
            }
        }

        let mut renderer = Self::from_grouped(all_grouped, DEFAULT_MAX_VELOCITY, DEFAULT_ACCELERATION, DEFAULT_CORNERING, 1.0);
        renderer.guides = guides;
        renderer.paper  = paper;

        for guide in &renderer.guides {
            match guide {
                Guide::Vertical(x)   => { renderer.min_x = renderer.min_x.min(*x); renderer.max_x = renderer.max_x.max(*x); }
                Guide::Horizontal(y) => { renderer.min_y = renderer.min_y.min(*y); renderer.max_y = renderer.max_y.max(*y); }
            }
        }
        if let Some([pw, ph]) = renderer.paper {
            renderer.min_x = renderer.min_x.min(0.0); renderer.max_x = renderer.max_x.max(pw);
            renderer.min_y = renderer.min_y.min(0.0); renderer.max_y = renderer.max_y.max(ph);
        }
        Ok(renderer)
    }
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

    /// Build a Scene3d with a single cube whose 6 faces all carry the same custom texture
    /// (typically extracted from an SVG by the JS layer).
    /// `paths_flat` is a flat `[x0, y0, x1, y1, ...]` coordinate array; `path_lengths[i]`
    /// is the number of *points* in the i-th path. Coordinates should be in `[0, 1]²`.
    #[wasm_bindgen(js_name = customTexturedCube)]
    pub fn js_custom_textured_cube(paths_flat: &[f64], path_lengths: &[u32]) -> Scene3d {
        use scene3d::{Vec3, TexturedFace, cube};
        let mut paths: Vec<Vec<(f64, f64)>> = Vec::with_capacity(path_lengths.len());
        let mut idx = 0;
        for &len in path_lengths {
            let mut pts = Vec::with_capacity(len as usize);
            for _ in 0..len {
                pts.push((paths_flat[idx], paths_flat[idx + 1]));
                idx += 2;
            }
            if pts.len() >= 2 { paths.push(pts); }
        }
        let h = 1.0_f64;
        let v = [
            Vec3::new(-h,-h,-h), Vec3::new( h,-h,-h), Vec3::new( h, h,-h), Vec3::new(-h, h,-h),
            Vec3::new(-h,-h, h), Vec3::new( h,-h, h), Vec3::new( h, h, h), Vec3::new(-h, h, h),
        ];
        let cube_t = cube(Vec3::zero(), 2.0)
            .with_texture(TexturedFace::from_quad(v[4], v[5], v[6], v[7], paths.clone())) // +z
            .with_texture(TexturedFace::from_quad(v[0], v[3], v[2], v[1], paths.clone())) // -z
            .with_texture(TexturedFace::from_quad(v[1], v[2], v[6], v[5], paths.clone())) // +x
            .with_texture(TexturedFace::from_quad(v[0], v[4], v[7], v[3], paths.clone())) // -x
            .with_texture(TexturedFace::from_quad(v[3], v[7], v[6], v[2], paths.clone())) // +y
            .with_texture(TexturedFace::from_quad(v[0], v[1], v[5], v[4], paths));        // -y
        Scene3d { scene: scene3d::Scene { objects: vec![cube_t] }, centroid: scene3d::Vec3::zero() }
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

    /// Construct from a JSON text-layout descriptor (see `from_text_layout`).
    #[wasm_bindgen(js_name = fromTextLayout)]
    pub fn js_from_text_layout(json: &str) -> Result<PlotRenderer, JsValue> {
        Self::from_text_layout(json).map_err(|e| JsValue::from_str(&e))
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

        let (scale, off_x, off_y) = self.viewport_transform(width, height);

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

            // Zero-length segment = plotter dot: render as a small filled circle.
            if seg.total_length < 1e-9 {
                let (px, py) = to_px(seg.points[0].0, seg.points[0].1);
                ctx.set_fill_style_str("#141414");
                ctx.begin_path();
                ctx.arc(px, py, 1.5, 0.0, std::f64::consts::TAU).unwrap();
                ctx.fill();
                continue;
            }

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

        // Draw paper boundary as an orange rectangle.
        if let Some([pw, ph]) = self.paper {
            ctx.save();
            ctx.set_stroke_style_str("rgba(255, 140, 0, 0.9)");
            ctx.set_line_width(1.5);
            let _ = ctx.set_line_dash(&js_sys::Array::new()); // solid
            ctx.stroke_rect(
                0.0_f64 * scale + off_x,
                0.0_f64 * scale + off_y,
                pw * scale,
                ph * scale,
            );
            ctx.restore();
        }

        // Draw guide lines as blue dashed overlays.
        if !self.guides.is_empty() {
            let dash = js_sys::Array::of2(
                &wasm_bindgen::JsValue::from_f64(6.0),
                &wasm_bindgen::JsValue::from_f64(4.0),
            );
            ctx.save();
            ctx.set_stroke_style_str("rgba(60,140,255,0.75)");
            ctx.set_line_width(1.0);
            ctx.set_line_dash(&dash).unwrap();
            ctx.begin_path();
            for guide in &self.guides {
                match guide {
                    Guide::Vertical(x) => {
                        let px = x * scale + off_x;
                        ctx.move_to(px, 0.0);
                        ctx.line_to(px, height);
                    }
                    Guide::Horizontal(y) => {
                        let py = y * scale + off_y;
                        ctx.move_to(0.0, py);
                        ctx.line_to(width, py);
                    }
                }
            }
            ctx.stroke();
            ctx.restore();
        }
    }
}

// ── Shared helpers ────────────────────────────────────────────────────────────

impl PlotRenderer {
    /// Returns `(scale, off_x, off_y)` mapping plot coords → canvas pixels.
    fn viewport_transform(&self, width: f64, height: f64) -> (f64, f64, f64) {
        let padding = 20.0_f64;
        let draw_w  = (self.max_x - self.min_x).max(1e-9);
        let draw_h  = (self.max_y - self.min_y).max(1e-9);
        let scale   = ((width  - 2.0 * padding) / draw_w)
            .min( (height - 2.0 * padding) / draw_h);
        let off_x   = padding + (width  - 2.0 * padding - draw_w * scale) / 2.0 - self.min_x * scale;
        let off_y   = padding + (height - 2.0 * padding - draw_h * scale) / 2.0 - self.min_y * scale;
        (scale, off_x, off_y)
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

    PlotRenderer { timeline, total_duration: current_time, min_x, min_y, max_x, max_y, guides: vec![], paper: None }
}

/// Bounding box of a grouped path set: `(min_x, max_x, min_y, max_y)`.
fn grouped_bbox(grouped: &[Vec<Path<f64>>]) -> (f64, f64, f64, f64) {
    let (mut min_x, mut max_x, mut min_y, mut max_y) =
        (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
    for group in grouped {
        for path in group {
            for pt in path.points() {
                if pt.x < min_x { min_x = pt.x; }
                if pt.x > max_x { max_x = pt.x; }
                if pt.y < min_y { min_y = pt.y; }
                if pt.y > max_y { max_y = pt.y; }
            }
        }
    }
    // Return a zero-size box centred at origin if there were no points.
    if min_x > max_x { (0.0, 0.0, 0.0, 0.0) } else { (min_x, max_x, min_y, max_y) }
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
