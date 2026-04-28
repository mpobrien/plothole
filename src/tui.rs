use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal;

use plothole::iosevka::IosevkaFont;
use crate::device::{Commander, Device, Motor1Setting, Motor2Setting, StepMode};

const DEFAULT_SPEED_MM_S: f64 = 50.0;
const DEFAULT_ACCEL_MM_S2: f64 = 500.0;
const DEFAULT_STEP_MODE: StepMode = StepMode::Step1_16;
const TIMESLICE_S: f64 = 0.010; // 10 ms per XM command

struct SuspendedPlot {
    strokes: Vec<crate::font::Path<f64>>,
    pen:     (f64, f64), // font-unit position where the pen was raised
}

struct State {
    device:          Option<Device>,
    position:        (f64, f64), // mm, tracked in software
    step_mode:       StepMode,
    speed_mm_s:      f64,
    accel_mm_s2:     f64,
    cornering_factor: f64,
    font_name:       String,
    font_scale:      f64,
    iosevka_file:    Option<String>,
    pen_up_pos:      i32,
    pen_down_pos:    i32,
    calibrated:      bool,
    join_tol_mm:     f64,
    leading_mm:      f64,
    suspended_plot:  Option<SuspendedPlot>,
}

// EBB servo position range (raw units sent to SC,4 / SC,5).
// Typical AxiDraw: ~9855 (low) to ~27831 (high). Higher = servo arm further CW.
// The AxiDraw pen-lifter is geared so that higher servo values = pen more raised.
const PEN_POS_MIN: i32 = 9855;
const PEN_POS_MAX: i32 = 27831;
const PEN_POS_STEP: i32 = 250;
// Defaults match axidraw defaults (up ≈ 25%, down ≈ 45% of range)
const PEN_UP_DEFAULT:   i32 = 16638;
const PEN_DOWN_DEFAULT: i32 = 12243;

impl State {
    fn new(device: Option<Device>) -> Self {
        Self {
            device,
            position: (0.0, 0.0),
            step_mode: DEFAULT_STEP_MODE,
            speed_mm_s:      DEFAULT_SPEED_MM_S,
            accel_mm_s2:     DEFAULT_ACCEL_MM_S2,
            cornering_factor: 1.0,
            font_name: "FUTURAL".to_string(),
            font_scale:   1.0,
            iosevka_file: None,
            pen_up_pos:      PEN_UP_DEFAULT,
            pen_down_pos: PEN_DOWN_DEFAULT,
            calibrated:      false,
            join_tol_mm:     0.1,
            leading_mm:      0.0,
            suspended_plot:  None,
        }
    }

    /// Execute a planned multi-point move in mm. `pts_mm` must have at least 2 points.
    /// The first point is treated as the current pen position (relative origin).
    /// Does NOT update `self.position` — callers manage that.
    fn send_planned_mm(&mut self, pts_mm: &[(f64, f64)]) -> Result<(), Box<dyn std::error::Error>> {
        if pts_mm.len() < 2 { return Ok(()); }
        let vec2d: Vec<crate::motion::Vec2d> = pts_mm.iter()
            .map(|&(x, y)| crate::motion::Vec2d::new(x, y))
            .collect();
        let profile = crate::motion::AccelerationProfile {
            maximum_velocity: self.speed_mm_s,
            acceleration:     self.accel_mm_s2,
            cornering_factor: self.cornering_factor,
        };
        let plan  = crate::motion::plan_path(&vec2d, &profile);
        let total = plan.duration();
        let mut t  = 0.0;
        let (mut px, mut py) = pts_mm[0];
        while t < total {
            let next_t = (t + TIMESLICE_S).min(total);
            let inst   = plan.instant(next_t);
            let dx = inst.position.x - px;
            let dy = inst.position.y - py;
            if dx.abs() > 1e-9 || dy.abs() > 1e-9 {
                let mode = self.step_mode;
                require_device(&mut self.device)?.move_mm(dx, dy, Duration::from_secs_f64(next_t - t), mode)?;
                px = inst.position.x;
                py = inst.position.y;
            }
            t = next_t;
        }
        Ok(())
    }

    fn do_move(&mut self, dx_mm: f64, dy_mm: f64) -> Result<(), Box<dyn std::error::Error>> {
        if dx_mm.abs() + dy_mm.abs() < 1e-9 { return Ok(()); }
        self.send_planned_mm(&[(0.0, 0.0), (dx_mm, dy_mm)])?;
        self.position.0 += dx_mm;
        self.position.1 += dy_mm;
        Ok(())
    }
}

fn require_device(device: &mut Option<Device>) -> Result<&mut Device, Box<dyn std::error::Error>> {
    device.as_mut().ok_or_else(|| "no AxiDraw connected".into())
}

fn step_mode_label(mode: StepMode) -> &'static str {
    match mode {
        StepMode::Step1_16 => "1/16",
        StepMode::Step1_8  => "1/8",
        StepMode::Step1_4  => "1/4",
        StepMode::Step1_2  => "1/2",
        StepMode::Full     => "full",
    }
}

fn parse_step_mode(s: &str) -> Option<StepMode> {
    match s {
        "1" | "1/16" => Some(StepMode::Step1_16),
        "2" | "1/8"  => Some(StepMode::Step1_8),
        "3" | "1/4"  => Some(StepMode::Step1_4),
        "4" | "1/2"  => Some(StepMode::Step1_2),
        "5" | "full" => Some(StepMode::Full),
        _ => None,
    }
}

/// Returns `true` when the loop should exit.
fn handle(state: &mut State, line: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let parts: Vec<&str> = line.trim().split_whitespace().collect();
    if parts.is_empty() { return Ok(false); }

    match parts[0] {
        "help" | "h" | "?" => {
            println!("  on [mode]          Enable motors  (mode: 1/16 1/8 1/4 1/2 full; default 1/16)");
            println!("  off                Disable motors");
            println!("  up                 Pen up");
            println!("  down               Pen down");
            println!("  move <x> <y>       Relative move in mm");
            println!("  goto <x> <y>       Move to absolute position in mm");
            println!("  speed [mm/s]       Show or set movement speed");
            println!("  accel [mm/s²]      Show or set movement acceleration");
            println!("  cornering [factor] Show or set cornering aggressiveness (higher = faster corners, default 1.0)");
            println!("  pos                Show current position");
            println!("  top                Pen up and move to the top of travel (y = 0)");
            println!("  home               Disable motors, wait for manual positioning at origin, then reset position");
            println!("  raw <cmd...>       Send a raw EBB command and print the response");
            println!("  font [name]        Show or set the Hershey font for preview/plot");
            println!("  iosevka [path]     Use an Iosevka skeleton.json for preview/plot (omit path to show current; 'none' to clear)");
            println!("  size [scale]       Show or set font scale / Iosevka em size (e.g. 0.5, 2.0; default 1.0)");
            println!("  preview <text...>          Open a preview window for the given text");
            println!("  preview --file <path>      Open a preview window for text from a file");
            println!("  plot <text...>             Plot the given text on the AxiDraw");
            println!("  plot --file <path>         Plot text from a file");
            println!("  continue                   Resume a suspended plot (Esc during plot to suspend)");
            println!("  join [mm]          Get/set stroke join tolerance (default 0.1 mm)");
            println!("  leading [mm]       Get/set extra line spacing added between lines (default 0)");
            println!("  bbox <text...>             Trace the bounding box of text with pen up (for paper alignment)");
            println!("  bbox --file <path>         Trace bounding box of text from a file");
            println!("  tpreview <text...>         Render a preview image in the terminal (Kitty image protocol)");
            println!("  tpreview --file <path>     Render a preview image from text in a file");
            println!("  calibrate          Interactively set pen up/down positions");
            println!("  raise              Drive pen to servo maximum (clear paper/obstacles)");
            println!("  quit / q           Exit");
        }

        "on" => {
            let mode = parts.get(1)
                .and_then(|s| parse_step_mode(s))
                .unwrap_or(DEFAULT_STEP_MODE);
            state.step_mode = mode;
            require_device(&mut state.device)?.enable_motors(Motor1Setting::Enable(mode), Motor2Setting::Enable)?;
            println!("Motors enabled ({} step)", step_mode_label(mode));
        }

        "off" => {
            require_device(&mut state.device)?.steppers_off()?;
            println!("Motors disabled");
        }

        "up" => {
            require_device(&mut state.device)?.pen_up()?;
            println!("Pen up");
        }

        "raise" => {
            // Drive servo to hardware maximum, then restore calibrated up position
            // so subsequent pen-up commands still land at the right height.
            require_device(&mut state.device)?.command(&["SC", "4", &PEN_POS_MAX.to_string()])?;
            require_device(&mut state.device)?.command(&["SP", "1", "500"])?;
            std::thread::sleep(Duration::from_millis(600));
            require_device(&mut state.device)?.command(&["SC", "4", &state.pen_up_pos.to_string()])?;
            println!("Pen raised to maximum");
        }

        "down" => {
            require_device(&mut state.device)?.pen_down()?;
            println!("Pen down");
        }

        "move" => {
            if parts.len() < 3 {
                println!("Usage: move <x_mm> <y_mm>");
                return Ok(false);
            }
            let dx: f64 = parts[1].parse()?;
            let dy: f64 = parts[2].parse()?;
            let target = (state.position.0 + dx, state.position.1 + dy);
            println!("({:.2}, {:.2}) → ({:.2}, {:.2}) mm",
                state.position.0, state.position.1, target.0, target.1);
            state.do_move(dx, dy)?;
        }

        "goto" => {
            if parts.len() < 3 {
                println!("Usage: goto <x_mm> <y_mm>");
                return Ok(false);
            }
            let x: f64 = parts[1].parse()?;
            let y: f64 = parts[2].parse()?;
            let dx = x - state.position.0;
            let dy = y - state.position.1;
            println!("({:.2}, {:.2}) → ({:.2}, {:.2}) mm",
                state.position.0, state.position.1, x, y);
            state.do_move(dx, dy)?;
        }

        "speed" => {
            if parts.len() < 2 {
                println!("Speed: {:.0} mm/s", state.speed_mm_s);
            } else {
                let s: f64 = parts[1].parse()?;
                if s <= 0.0 { return Err("speed must be positive".into()); }
                state.speed_mm_s = s;
                println!("Speed: {:.0} mm/s", s);
            }
        }

        "accel" => {
            if parts.len() < 2 {
                println!("Accel: {:.0} mm/s²", state.accel_mm_s2);
            } else {
                let a: f64 = parts[1].parse()?;
                if a <= 0.0 { return Err("accel must be positive".into()); }
                state.accel_mm_s2 = a;
                println!("Accel: {:.0} mm/s²", a);
            }
        }

        "cornering" => {
            if parts.len() < 2 {
                println!("Cornering: {:.1}", state.cornering_factor);
            } else {
                let c: f64 = parts[1].parse()?;
                if c <= 0.0 { return Err("cornering factor must be positive".into()); }
                state.cornering_factor = c;
                println!("Cornering: {:.1}", c);
            }
        }

        "pos" | "position" => {
            println!("Position: ({:.2}, {:.2}) mm", state.position.0, state.position.1);
        }

        "top" => {
            require_device(&mut state.device)?.pen_up()?;
            let dy = -state.position.1;
            if dy.abs() > 0.01 {
                println!("Moving to top (y = 0.00 mm)...");
                state.do_move(0.0, dy)?;
            } else {
                println!("Already at top.");
            }
        }

        "home" => {
            require_device(&mut state.device)?.steppers_off()?;
            println!("Motors disabled. Manually move the pen to the origin (0, 0), then press Enter.");
            let mut buf = String::new();
            io::stdin().read_line(&mut buf)?;
            require_device(&mut state.device)?.enable_motors(Motor1Setting::Enable(state.step_mode), Motor2Setting::Enable)?;
            state.position = (0.0, 0.0);
            println!("Position reset to (0.00, 0.00) mm. Motors re-enabled ({} step).",
                step_mode_label(state.step_mode));
        }

        "raw" => {
            if parts.len() < 2 {
                println!("Usage: raw <command> [args...]");
                return Ok(false);
            }
            let response = require_device(&mut state.device)?.raw(&parts[1..])?;
            print!("{}", response);
        }

        "font" => {
            if parts.len() < 2 {
                println!("Font: {}", state.font_name);
            } else {
                state.font_name = parts[1].to_uppercase();
                state.iosevka_file = None;
                println!("Font: {}", state.font_name);
            }
        }

        "iosevka" => {
            if parts.len() < 2 {
                match &state.iosevka_file {
                    Some(p) => println!("Iosevka: {p}"),
                    None    => println!("Iosevka: not set (using Hershey font '{}')", state.font_name),
                }
            } else if parts[1] == "none" {
                state.iosevka_file = None;
                println!("Iosevka cleared — using Hershey font '{}'", state.font_name);
            } else {
                state.iosevka_file = Some(parts[1].to_string());
                // Reset to a default size that keeps movements above the plotter's
                // minimum threshold. em_size=21 ≈ 7 mm cap height (same as Hershey
                // at size=1). Use 'size' to adjust.
                state.font_scale = 21.0;
                println!("Iosevka: {}  (size reset to 21 — use 'size' to adjust)", parts[1]);
            }
        }

        "size" => {
            if parts.len() < 2 {
                println!("Size: {}", state.font_scale);
            } else {
                let s: f64 = parts[1].parse()?;
                if s <= 0.0 { return Err("size must be positive".into()); }
                state.font_scale = s;
                println!("Size: {}", s);
            }
        }

        "join" => {
            if parts.len() < 2 {
                println!("Join tolerance: {:.3} mm", state.join_tol_mm);
            } else {
                let t: f64 = parts[1].parse()?;
                if t < 0.0 { return Err("join tolerance must be >= 0".into()); }
                state.join_tol_mm = t;
                println!("Join tolerance: {:.3} mm", t);
            }
        }

        "leading" => {
            if parts.len() < 2 {
                println!("Leading: {:.1} mm", state.leading_mm);
            } else {
                let l: f64 = parts[1].parse()?;
                if l < 0.0 { return Err("leading must be >= 0".into()); }
                state.leading_mm = l;
                println!("Leading: {:.1} mm", l);
            }
        }

        "preview" => {
            let text = if parts.get(1) == Some(&"--file") {
                match parts.get(2) {
                    Some(path) => match std::fs::read_to_string(path) {
                        Ok(contents) => contents,
                        Err(e) => { eprintln!("Error reading file: {e}"); return Ok(false); }
                    },
                    None => { println!("Usage: preview --file <path>"); return Ok(false); }
                }
            } else if parts.len() >= 2 {
                parts[1..].join(" ")
            } else {
                println!("Usage: preview <text...>  |  preview --file <path>");
                return Ok(false);
            };
            let renderer = if let Some(path) = &state.iosevka_file {
                IosevkaFont::from_file(path)
                    .map(|f| plothole::PlotRenderer::from_grouped(
                        f.text_to_paths(&text, state.font_scale, state.leading_mm / crate::MM_PER_UNIT),
                        plothole::DEFAULT_MAX_VELOCITY,
                        plothole::DEFAULT_ACCELERATION,
                        plothole::DEFAULT_CORNERING,
                        1.0,
                    ))
                    .map_err(|e| e.to_string())
            } else {
                plothole::PlotRenderer::new_hershey(&text, &state.font_name, state.font_scale)
            };
            match renderer {
                Ok(r) => crate::preview::run(r),
                Err(e) => eprintln!("Error: {e}"),
            }
        }

        "tpreview" => {
            let text = if parts.get(1) == Some(&"--file") {
                match parts.get(2) {
                    Some(path) => match std::fs::read_to_string(path) {
                        Ok(contents) => contents,
                        Err(e) => { eprintln!("Error reading file: {e}"); return Ok(false); }
                    },
                    None => { println!("Usage: tpreview --file <path>"); return Ok(false); }
                }
            } else if parts.len() >= 2 {
                parts[1..].join(" ")
            } else {
                println!("Usage: tpreview <text...>  |  tpreview --file <path>");
                return Ok(false);
            };
            terminal_preview(state, &text)?;
        }

        "bbox" => {
            let text = if parts.get(1) == Some(&"--file") {
                match parts.get(2) {
                    Some(path) => match std::fs::read_to_string(path) {
                        Ok(contents) => contents,
                        Err(e) => { eprintln!("Error reading file: {e}"); return Ok(false); }
                    },
                    None => { println!("Usage: bbox --file <path>"); return Ok(false); }
                }
            } else if parts.len() >= 2 {
                parts[1..].join(" ")
            } else {
                println!("Usage: bbox <text...>  |  bbox --file <path>");
                return Ok(false);
            };
            bbox_text(state, &text)?;
        }

        "plot" => {
            let text = if parts.get(1) == Some(&"--file") {
                match parts.get(2) {
                    Some(path) => match std::fs::read_to_string(path) {
                        Ok(contents) => contents,
                        Err(e) => { eprintln!("Error reading file: {e}"); return Ok(false); }
                    },
                    None => { println!("Usage: plot --file <path>"); return Ok(false); }
                }
            } else if parts.len() >= 2 {
                parts[1..].join(" ")
            } else {
                println!("Usage: plot <text...>  |  plot --file <path>");
                return Ok(false);
            };
            plot_text(state, &text)?;
        }

        "continue" => {
            match state.suspended_plot.take() {
                None => println!("No suspended plot."),
                Some(suspended) => {
                    println!("Resuming ({} stroke(s) remaining)...", suspended.strokes.len());
                    execute_plot(state, suspended.strokes, suspended.pen)?;
                }
            }
        }

        "calibrate" => calibrate_pen(state)?,

        "quit" | "exit" | "q" => return Ok(true),

        other => println!("Unknown command '{other}'. Type 'help' for commands."),
    }

    Ok(false)
}

// ── Terminal preview (Kitty image protocol) ────────────────────────────────────

fn terminal_preview(state: &State, text: &str) -> Result<(), Box<dyn std::error::Error>> {
    let renderer = if let Some(path) = &state.iosevka_file {
        IosevkaFont::from_file(path)
            .map(|f| plothole::PlotRenderer::from_grouped(
                f.text_to_paths(text, state.font_scale, state.leading_mm / crate::MM_PER_UNIT),
                plothole::DEFAULT_MAX_VELOCITY,
                plothole::DEFAULT_ACCELERATION,
                plothole::DEFAULT_CORNERING,
                1.0,
            ))
            .map_err(|e| e.to_string())
    } else {
        plothole::PlotRenderer::new_hershey(text, &state.font_name, state.font_scale)
    }?;

    // Detect terminal width; fall back to 120 columns.
    let (cols, _rows) = crossterm::terminal::size().unwrap_or((120, 40));
    // Render at ~10 px per column, 3:1 aspect ratio.
    let w = (cols as u32) * 10;
    let h = w / 3;

    let mut pixmap = tiny_skia::Pixmap::new(w, h).unwrap();
    renderer.render_preview_native(&mut pixmap.as_mut());

    kitty_display(pixmap.data(), w, h, cols as u32)
}

/// Emit a Kitty image-protocol sequence for raw RGBA data.
/// `display_cols` tells Kitty how many terminal columns wide to render it.
fn kitty_display(rgba: &[u8], w: u32, h: u32, display_cols: u32) -> Result<(), Box<dyn std::error::Error>> {
    use io::Write as _;

    let b64 = kitty_base64(rgba);
    let chunks: Vec<&str> = b64
        .as_bytes()
        .chunks(4096)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect();

    let mut out = io::stdout().lock();
    for (i, chunk) in chunks.iter().enumerate() {
        let m = if i == chunks.len() - 1 { 0 } else { 1 };
        if i == 0 {
            write!(out, "\x1b_Ga=T,f=32,s={w},v={h},c={display_cols},m={m};{chunk}\x1b\\")?;
        } else {
            write!(out, "\x1b_Gm={m};{chunk}\x1b\\")?;
        }
    }
    writeln!(out)?;
    out.flush()?;
    Ok(())
}

fn kitty_base64(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize]);
        out.push(T[((n >> 12) & 63) as usize]);
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] } else { b'=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] } else { b'=' });
    }
    String::from_utf8(out).unwrap()
}

/// Compute the bounding box of the given text and trace it with pen up.
fn bbox_text(state: &mut State, text: &str) -> Result<(), Box<dyn std::error::Error>> {
    let grouped: Vec<Vec<crate::font::Path<f64>>> = if let Some(path) = &state.iosevka_file {
        IosevkaFont::from_file(path)?.text_to_paths(text, state.font_scale, state.leading_mm / crate::MM_PER_UNIT)
            .into_iter().map(|group| group.into_iter().map(|p| {
                crate::font::Path::new(p.points().iter()
                    .map(|pt| crate::font::Vec2d { x: pt.x, y: pt.y })
                    .collect())
            }).collect()).collect()
    } else {
        let fonts = crate::hershey::fonts();
        let font = fonts.get(&state.font_name as &str)
            .ok_or_else(|| format!("unknown font '{}'", state.font_name))?;
        crate::scale_grouped(crate::text_to_paths(text, font), state.font_scale)
    };

    let flat = crate::optimize_path_order(grouped, state.join_tol_mm / crate::MM_PER_UNIT);
    let strokes: Vec<_> = flat.into_iter().filter(|p| p.points().len() >= 2).collect();

    if strokes.is_empty() {
        println!("Nothing to plot.");
        return Ok(());
    }

    let mut min_x = f64::MAX;
    let mut min_y = f64::MAX;
    let mut max_x = f64::MIN;
    let mut max_y = f64::MIN;
    for stroke in &strokes {
        for pt in stroke.points() {
            min_x = min_x.min(pt.x);
            min_y = min_y.min(pt.y);
            max_x = max_x.max(pt.x);
            max_y = max_y.max(pt.y);
        }
    }

    let x0 = min_x * crate::MM_PER_UNIT;
    let y0 = min_y * crate::MM_PER_UNIT;
    let x1 = max_x * crate::MM_PER_UNIT;
    let y1 = max_y * crate::MM_PER_UNIT;

    println!("Bounding box: ({:.1}, {:.1}) → ({:.1}, {:.1}) mm  ({:.1} × {:.1} mm)",
        x0, y0, x1, y1, x1 - x0, y1 - y0);
    println!("Tracing with pen up...");

    require_device(&mut state.device)?.pen_up()?;

    // x0/y0 are offsets from the current position (same coordinate system as execute_plot).
    // Trace the four corners and return to start.
    state.do_move(x0, y0)?;
    state.do_move(x1 - x0, 0.0)?;
    state.do_move(0.0, y1 - y0)?;
    state.do_move(x0 - x1, 0.0)?;
    state.do_move(0.0, y0 - y1)?;
    state.do_move(-x0, -y0)?;

    println!("Done.");
    Ok(())
}

/// Prepare paths for the given text and hand off to `execute_plot`.
fn plot_text(state: &mut State, text: &str) -> Result<(), Box<dyn std::error::Error>> {
    let grouped: Vec<Vec<crate::font::Path<f64>>> = if let Some(path) = &state.iosevka_file {
        // IosevkaFont lives in the lib crate so its Path<f64> is a different type instance.
        // Convert field-by-field (same data, different crate paths).
        IosevkaFont::from_file(path)?.text_to_paths(text, state.font_scale, state.leading_mm / crate::MM_PER_UNIT)
            .into_iter().map(|group| group.into_iter().map(|p| {
                crate::font::Path::new(p.points().iter()
                    .map(|pt| crate::font::Vec2d { x: pt.x, y: pt.y })
                    .collect())
            }).collect()).collect()
    } else {
        let fonts = crate::hershey::fonts();
        let font = fonts.get(&state.font_name as &str)
            .ok_or_else(|| format!("unknown font '{}'", state.font_name))?;
        crate::scale_grouped(crate::text_to_paths(text, font), state.font_scale)
    };

    let flat = crate::optimize_path_order(grouped, state.join_tol_mm / crate::MM_PER_UNIT);
    let strokes: Vec<_> = flat.into_iter().filter(|p| p.points().len() >= 2).collect();

    if strokes.is_empty() {
        println!("Nothing to plot.");
        return Ok(());
    }

    execute_plot(state, strokes, (0.0, 0.0))
}

/// Execute a plot from `start_pen` (font-unit coordinates), checking for Esc
/// between strokes. On Esc the pen is raised and the remaining strokes are
/// stored in `state.suspended_plot` for a later `continue`.
fn execute_plot(
    state: &mut State,
    strokes: Vec<crate::font::Path<f64>>,
    start_pen: (f64, f64),
) -> Result<(), Box<dyn std::error::Error>> {
    let total = strokes.len();
    println!("Plotting {total} stroke(s) at {:.0} mm/s... (Esc to suspend)", state.speed_mm_s);

    // Ensure pen is up before starting.
    require_device(&mut state.device)?.command(&["SP", "1", "300"])?;
    std::thread::sleep(Duration::from_millis(350));

    terminal::enable_raw_mode()?;

    let mut pen = start_pen;
    let mut pen_is_down = false;
    let mut suspend_from: Option<usize> = None;

    'outer: for (i, stroke) in strokes.iter().enumerate() {
        // Non-blocking check for Esc before each stroke.
        while event::poll(std::time::Duration::ZERO)? {
            if let Event::Key(KeyEvent { code: KeyCode::Esc, .. }) = event::read()? {
                suspend_from = Some(i);
                break 'outer;
            }
        }

        let pts = stroke.points();
        let dx_mm = (pts[0].x - pen.0) * crate::MM_PER_UNIT;
        let dy_mm = (pts[0].y - pen.1) * crate::MM_PER_UNIT;
        let dist_mm = (dx_mm * dx_mm + dy_mm * dy_mm).sqrt();

        if dist_mm > 0.01 {
            if pen_is_down {
                require_device(&mut state.device)?.command(&["SP", "1", "200"])?;
                std::thread::sleep(Duration::from_millis(250));
                pen_is_down = false;
            }
            state.send_planned_mm(&[(0.0, 0.0), (dx_mm, dy_mm)])?;
        }

        if !pen_is_down {
            require_device(&mut state.device)?.command(&["SP", "0", "200"])?;
            std::thread::sleep(Duration::from_millis(250));
            pen_is_down = true;
        }

            // Plan and execute the full stroke as one motion profile.
        let stroke_mm: Vec<(f64, f64)> = pts.iter()
            .map(|p| ((p.x - pts[0].x) * crate::MM_PER_UNIT,
                      (p.y - pts[0].y) * crate::MM_PER_UNIT))
            .collect();
        state.send_planned_mm(&stroke_mm)?;

        pen = { let last = pts.last().unwrap(); (last.x, last.y) };

        if (i + 1) % 10 == 0 || i + 1 == total {
            print!("  {}/{}\r\n", i + 1, total);
            io::stdout().flush()?;
        }
    }

    terminal::disable_raw_mode()?;

    // Raise pen to hardware maximum (whether done or suspended).
    require_device(&mut state.device)?.command(&["SC", "4", &PEN_POS_MAX.to_string()])?;
    require_device(&mut state.device)?.command(&["SP", "1", "500"])?;
    std::thread::sleep(Duration::from_millis(600));
    require_device(&mut state.device)?.command(&["SC", "4", &state.pen_up_pos.to_string()])?;

    // Update tracked position by the delta traveled in this session.
    state.position.0 += (pen.0 - start_pen.0) * crate::MM_PER_UNIT;
    state.position.1 += (pen.1 - start_pen.1) * crate::MM_PER_UNIT;

    if let Some(i) = suspend_from {
        state.suspended_plot = Some(SuspendedPlot {
            strokes: strokes[i..].to_vec(),
            pen,
        });
        println!("Suspended after stroke {i}/{total}. Type 'continue' to resume.");
    } else {
        state.suspended_plot = None;
        println!("Done.");
    }

    Ok(())
}

/// Interactive pen-position calibrator.
///
/// Puts the terminal in raw mode and lets the user nudge the servo with
/// ↑/↓, confirming each position with Enter. Writes SC,4 (up) and SC,5 (down)
/// to the EBB and saves both values in State for the session.
fn calibrate_pen(state: &mut State) -> Result<(), Box<dyn std::error::Error>> {
    fn set_and_move(device: &mut crate::device::Device, sc_param: &str, pos: i32, phase_up: bool)
        -> Result<(), Box<dyn std::error::Error>>
    {
        device.command(&["SC", sc_param, &pos.to_string()])?;
        let sp_state = if phase_up { "1" } else { "0" };
        device.command(&["SP", sp_state, "150"])?;
        Ok(())
    }

    // ── Phase 1: pen-UP position ───────────────────────────────────────────
    // If we have a previous calibration, resume from it; otherwise start at
    // the top of the servo range and let the user work down.
    let mut pos = if state.calibrated { state.pen_up_pos } else { PEN_POS_MAX };
    set_and_move(require_device(&mut state.device)?, "4", pos, true)?;

    println!("=== Pen UP position ===");
    println!("↑ / ↓  adjust height    Enter  confirm    Esc  cancel");

    terminal::enable_raw_mode()?;

    let up_pos = loop {
        print!("\r  servo {:5}   ", pos);
        io::stdout().flush()?;

        match event::read()? {
            Event::Key(KeyEvent { code: KeyCode::Up,    modifiers: KeyModifiers::NONE, .. }) => {
                pos = (pos + PEN_POS_STEP).min(PEN_POS_MAX);
                set_and_move(require_device(&mut state.device)?, "4", pos, true)?;
            }
            Event::Key(KeyEvent { code: KeyCode::Down,  modifiers: KeyModifiers::NONE, .. }) => {
                pos = (pos - PEN_POS_STEP).max(PEN_POS_MIN);
                set_and_move(require_device(&mut state.device)?, "4", pos, true)?;
            }
            Event::Key(KeyEvent { code: KeyCode::Enter, .. }) => break pos,
            Event::Key(KeyEvent { code: KeyCode::Esc,   .. }) => {
                terminal::disable_raw_mode()?;
                println!("\r  Cancelled.                      ");
                return Ok(());
            }
            _ => {}
        }
    };

    terminal::disable_raw_mode()?;
    println!("\r  Locked: servo {}             ", up_pos);

    // ── Phase 2: pen-DOWN position ─────────────────────────────────────────
    // If we have a previous calibration, resume from it; otherwise start
    // slightly below up_pos so the pen just touches the paper.
    pos = if state.calibrated { state.pen_down_pos } else { (up_pos - 4 * PEN_POS_STEP).max(PEN_POS_MIN) };
    set_and_move(require_device(&mut state.device)?, "5", pos, false)?;

    println!("=== Pen DOWN position ===");
    println!("↑ / ↓  adjust height    Enter  confirm    Esc  cancel");

    terminal::enable_raw_mode()?;

    let down_pos = loop {
        print!("\r  servo {:5}   ", pos);
        io::stdout().flush()?;

        match event::read()? {
            Event::Key(KeyEvent { code: KeyCode::Up,    modifiers: KeyModifiers::NONE, .. }) => {
                pos = (pos + PEN_POS_STEP).min(up_pos);   // can't go above up_pos
                set_and_move(require_device(&mut state.device)?, "5", pos, false)?;
            }
            Event::Key(KeyEvent { code: KeyCode::Down,  modifiers: KeyModifiers::NONE, .. }) => {
                pos = (pos - PEN_POS_STEP).max(PEN_POS_MIN);
                set_and_move(require_device(&mut state.device)?, "5", pos, false)?;
            }
            Event::Key(KeyEvent { code: KeyCode::Enter, .. }) => break pos,
            Event::Key(KeyEvent { code: KeyCode::Esc,   .. }) => {
                terminal::disable_raw_mode()?;
                println!("\r  Cancelled.                      ");
                return Ok(());
            }
            _ => {}
        }
    };

    terminal::disable_raw_mode()?;
    println!("\r  Locked: servo {}             ", down_pos);

    state.pen_up_pos   = up_pos;
    state.pen_down_pos = down_pos;
    state.calibrated   = true;

    // Return pen to up position.
    require_device(&mut state.device)?.pen_up()?;
    println!("Calibration done. Up: {}  Down: {}", up_pos, down_pos);
    Ok(())
}

// ── State persistence ──────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedState {
    position_x:   f64,
    position_y:   f64,
    pen_up_pos:   i32,
    pen_down_pos:  i32,
    calibrated:   bool,
    speed_mm_s:   f64,
    accel_mm_s2:  f64,
    #[serde(default = "default_cornering")]
    cornering_factor: f64,
    step_mode:    String,
    font_name:    String,
    font_scale:   f64,
    iosevka_file: Option<String>,
    #[serde(default = "default_join_tol_mm")]
    join_tol_mm:  f64,
    #[serde(default)]
    leading_mm:   f64,
}

fn default_cornering()    -> f64 { 1.0 }
fn default_join_tol_mm() -> f64 { 0.1 }

fn state_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".plothole.json")
}

fn load_persisted() -> Option<PersistedState> {
    let data = std::fs::read_to_string(state_path()).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_persisted(state: &State) {
    let ps = PersistedState {
        position_x:      state.position.0,
        position_y:      state.position.1,
        pen_up_pos:      state.pen_up_pos,
        pen_down_pos:    state.pen_down_pos,
        calibrated:      state.calibrated,
        speed_mm_s:      state.speed_mm_s,
        accel_mm_s2:     state.accel_mm_s2,
        cornering_factor: state.cornering_factor,
        step_mode:       step_mode_label(state.step_mode).to_string(),
        font_name:       state.font_name.clone(),
        font_scale:      state.font_scale,
        iosevka_file:    state.iosevka_file.clone(),
        join_tol_mm:     state.join_tol_mm,
        leading_mm:      state.leading_mm,
    };
    if let Ok(json) = serde_json::to_string_pretty(&ps) {
        let _ = std::fs::write(state_path(), json);
    }
}

pub fn run(device: Option<Device>) {
    let mut state = State::new(device);

    if let Some(ps) = load_persisted() {
        state.position    = (ps.position_x, ps.position_y);
        state.pen_up_pos  = ps.pen_up_pos;
        state.pen_down_pos = ps.pen_down_pos;
        state.calibrated  = ps.calibrated;
        state.speed_mm_s      = ps.speed_mm_s;
        state.accel_mm_s2     = ps.accel_mm_s2;
        state.cornering_factor = ps.cornering_factor;
        state.font_name   = ps.font_name;
        state.font_scale  = ps.font_scale;
        state.iosevka_file = ps.iosevka_file;
        state.join_tol_mm = ps.join_tol_mm;
        state.leading_mm  = ps.leading_mm;
        if let Some(mode) = parse_step_mode(&ps.step_mode) {
            state.step_mode = mode;
        }
        println!(
            "Restored: pos=({:.1},{:.1})mm  pen up={} down={}{}",
            state.position.0, state.position.1,
            state.pen_up_pos, state.pen_down_pos,
            if state.calibrated { "" } else { "  (uncalibrated)" },
        );
    }

    if state.device.is_some() {
        println!("AxiDraw controller — type 'help' for commands");
    } else {
        println!("AxiDraw controller (offline — no device connected) — type 'help' for commands");
    }

    let mut rl = rustyline::DefaultEditor::new().expect("failed to init line editor");

    loop {
        match rl.readline("> ") {
            Ok(line) => {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    let _ = rl.add_history_entry(trimmed);
                }
                match handle(&mut state, &line) {
                    Ok(true)  => { save_persisted(&state); break; }
                    Ok(false) => save_persisted(&state),
                    Err(e)    => eprintln!("Error: {e}"),
                }
            }
            Err(rustyline::error::ReadlineError::Interrupted) => {
                // Ctrl-C: clear the line and continue, like a real shell.
            }
            Err(rustyline::error::ReadlineError::Eof) => break, // Ctrl-D
            Err(e) => { eprintln!("Input error: {e}"); break; }
        }
    }

    println!("Bye.");
}
