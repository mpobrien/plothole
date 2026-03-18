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
    device:          Device,
    position:        (f64, f64), // mm, tracked in software
    step_mode:       StepMode,
    speed_mm_s:      f64,
    font_name:       String,
    font_scale:      f64,
    iosevka_file:    Option<String>,
    accel_mm_s2:     f64,
    pen_up_pos:      i32,
    pen_down_pos:    i32,
    calibrated:      bool,
    join_tol_mm:     f64,
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
    fn new(device: Device) -> Self {
        Self {
            device,
            position: (0.0, 0.0),
            step_mode: DEFAULT_STEP_MODE,
            speed_mm_s: DEFAULT_SPEED_MM_S,
            font_name: "FUTURAL".to_string(),
            font_scale:   1.0,
            iosevka_file: None,
            accel_mm_s2:     DEFAULT_ACCEL_MM_S2,
            pen_up_pos:      PEN_UP_DEFAULT,
            pen_down_pos: PEN_DOWN_DEFAULT,
            calibrated:      false,
            join_tol_mm:     0.1,
            suspended_plot:  None,
        }
    }

    fn do_move(&mut self, dx_mm: f64, dy_mm: f64) -> Result<(), Box<dyn std::error::Error>> {
        let dist = (dx_mm * dx_mm + dy_mm * dy_mm).sqrt();
        if dist < 1e-9 { return Ok(()); }

        let profile = crate::motion::AccelerationProfile {
            maximum_velocity: self.speed_mm_s,
            acceleration:     self.accel_mm_s2,
            cornering_factor: 1.0,
        };
        let points = vec![
            crate::motion::Vec2d::new(0.0, 0.0),
            crate::motion::Vec2d::new(dx_mm, dy_mm),
        ];
        let plan = crate::motion::plan_path(&points, &profile);

        let total = plan.duration();
        let mut t = 0.0;
        let mut prev_x = 0.0f64;
        let mut prev_y = 0.0f64;

        while t < total {
            let next_t = (t + TIMESLICE_S).min(total);
            let inst = plan.instant(next_t);
            let slice_dx = inst.position.x - prev_x;
            let slice_dy = inst.position.y - prev_y;
            let slice_dur = Duration::from_secs_f64(next_t - t);
            if slice_dx.abs() > 1e-6 || slice_dy.abs() > 1e-6 {
                self.device.move_mm(slice_dx, slice_dy, slice_dur, self.step_mode)?;
            }
            prev_x = inst.position.x;
            prev_y = inst.position.y;
            t = next_t;
        }

        self.position.0 += dx_mm;
        self.position.1 += dy_mm;
        Ok(())
    }
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
            println!("  calibrate          Interactively set pen up/down positions");
            println!("  raise              Drive pen to servo maximum (clear paper/obstacles)");
            println!("  quit / q           Exit");
        }

        "on" => {
            let mode = parts.get(1)
                .and_then(|s| parse_step_mode(s))
                .unwrap_or(DEFAULT_STEP_MODE);
            state.step_mode = mode;
            state.device.enable_motors(Motor1Setting::Enable(mode), Motor2Setting::Enable)?;
            println!("Motors enabled ({} step)", step_mode_label(mode));
        }

        "off" => {
            state.device.steppers_off()?;
            println!("Motors disabled");
        }

        "up" => {
            state.device.pen_up()?;
            println!("Pen up");
        }

        "raise" => {
            // Drive servo to hardware maximum, then restore calibrated up position
            // so subsequent pen-up commands still land at the right height.
            state.device.command(&["SC", "4", &PEN_POS_MAX.to_string()])?;
            state.device.command(&["SP", "1", "500"])?;
            std::thread::sleep(Duration::from_millis(600));
            state.device.command(&["SC", "4", &state.pen_up_pos.to_string()])?;
            println!("Pen raised to maximum");
        }

        "down" => {
            state.device.pen_down()?;
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

        "pos" | "position" => {
            println!("Position: ({:.2}, {:.2}) mm", state.position.0, state.position.1);
        }

        "top" => {
            state.device.pen_up()?;
            let dy = -state.position.1;
            if dy.abs() > 0.01 {
                println!("Moving to top (y = 0.00 mm)...");
                state.do_move(0.0, dy)?;
            } else {
                println!("Already at top.");
            }
        }

        "home" => {
            state.device.steppers_off()?;
            println!("Motors disabled. Manually move the pen to the origin (0, 0), then press Enter.");
            let mut buf = String::new();
            io::stdin().read_line(&mut buf)?;
            state.device.enable_motors(Motor1Setting::Enable(state.step_mode), Motor2Setting::Enable)?;
            state.position = (0.0, 0.0);
            println!("Position reset to (0.00, 0.00) mm. Motors re-enabled ({} step).",
                step_mode_label(state.step_mode));
        }

        "raw" => {
            if parts.len() < 2 {
                println!("Usage: raw <command> [args...]");
                return Ok(false);
            }
            let response = state.device.raw(&parts[1..])?;
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
                        f.text_to_paths(&text, state.font_scale),
                        plothole::DEFAULT_MAX_VELOCITY,
                        plothole::DEFAULT_ACCELERATION,
                        plothole::DEFAULT_CORNERING,
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

/// Prepare paths for the given text and hand off to `execute_plot`.
fn plot_text(state: &mut State, text: &str) -> Result<(), Box<dyn std::error::Error>> {
    let grouped: Vec<Vec<crate::font::Path<f64>>> = if let Some(path) = &state.iosevka_file {
        // IosevkaFont lives in the lib crate so its Path<f64> is a different type instance.
        // Convert field-by-field (same data, different crate paths).
        IosevkaFont::from_file(path)?.text_to_paths(text, state.font_scale)
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
    state.device.command(&["SP", "1", "300"])?;
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
                state.device.command(&["SP", "1", "200"])?;
                std::thread::sleep(Duration::from_millis(250));
                pen_is_down = false;
            }
            let dur = Duration::from_secs_f64(dist_mm / state.speed_mm_s);
            state.device.move_mm(dx_mm, dy_mm, dur, state.step_mode)?;
        }

        if !pen_is_down {
            state.device.command(&["SP", "0", "200"])?;
            std::thread::sleep(Duration::from_millis(250));
            pen_is_down = true;
        }

        let mut cx = pts[0].x;
        let mut cy = pts[0].y;
        for pt in pts.iter().skip(1) {
            let dx_mm = (pt.x - cx) * crate::MM_PER_UNIT;
            let dy_mm = (pt.y - cy) * crate::MM_PER_UNIT;
            let dist_mm = (dx_mm * dx_mm + dy_mm * dy_mm).sqrt();
            if dist_mm > 0.1 {
                let dur = Duration::from_secs_f64(dist_mm / state.speed_mm_s);
                state.device.move_mm(dx_mm, dy_mm, dur, state.step_mode)?;
                cx = pt.x;
                cy = pt.y;
            }
        }

        pen = { let last = pts.last().unwrap(); (last.x, last.y) };

        if (i + 1) % 10 == 0 || i + 1 == total {
            print!("  {}/{}\r\n", i + 1, total);
            io::stdout().flush()?;
        }
    }

    terminal::disable_raw_mode()?;

    // Raise pen to hardware maximum (whether done or suspended).
    state.device.command(&["SC", "4", &PEN_POS_MAX.to_string()])?;
    state.device.command(&["SP", "1", "500"])?;
    std::thread::sleep(Duration::from_millis(600));
    state.device.command(&["SC", "4", &state.pen_up_pos.to_string()])?;

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
    set_and_move(&mut state.device, "4", pos, true)?;

    println!("=== Pen UP position ===");
    println!("↑ / ↓  adjust height    Enter  confirm    Esc  cancel");

    terminal::enable_raw_mode()?;

    let up_pos = loop {
        print!("\r  servo {:5}   ", pos);
        io::stdout().flush()?;

        match event::read()? {
            Event::Key(KeyEvent { code: KeyCode::Up,    modifiers: KeyModifiers::NONE, .. }) => {
                pos = (pos + PEN_POS_STEP).min(PEN_POS_MAX);
                set_and_move(&mut state.device, "4", pos, true)?;
            }
            Event::Key(KeyEvent { code: KeyCode::Down,  modifiers: KeyModifiers::NONE, .. }) => {
                pos = (pos - PEN_POS_STEP).max(PEN_POS_MIN);
                set_and_move(&mut state.device, "4", pos, true)?;
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
    set_and_move(&mut state.device, "5", pos, false)?;

    println!("=== Pen DOWN position ===");
    println!("↑ / ↓  adjust height    Enter  confirm    Esc  cancel");

    terminal::enable_raw_mode()?;

    let down_pos = loop {
        print!("\r  servo {:5}   ", pos);
        io::stdout().flush()?;

        match event::read()? {
            Event::Key(KeyEvent { code: KeyCode::Up,    modifiers: KeyModifiers::NONE, .. }) => {
                pos = (pos + PEN_POS_STEP).min(up_pos);   // can't go above up_pos
                set_and_move(&mut state.device, "5", pos, false)?;
            }
            Event::Key(KeyEvent { code: KeyCode::Down,  modifiers: KeyModifiers::NONE, .. }) => {
                pos = (pos - PEN_POS_STEP).max(PEN_POS_MIN);
                set_and_move(&mut state.device, "5", pos, false)?;
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
    state.device.pen_up()?;
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
    step_mode:    String,
    font_name:    String,
    font_scale:   f64,
    iosevka_file: Option<String>,
    #[serde(default = "default_join_tol_mm")]
    join_tol_mm:  f64,
}

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
        position_x:   state.position.0,
        position_y:   state.position.1,
        pen_up_pos:   state.pen_up_pos,
        pen_down_pos:  state.pen_down_pos,
        calibrated:   state.calibrated,
        speed_mm_s:   state.speed_mm_s,
        accel_mm_s2:  state.accel_mm_s2,
        step_mode:    step_mode_label(state.step_mode).to_string(),
        font_name:    state.font_name.clone(),
        font_scale:   state.font_scale,
        iosevka_file: state.iosevka_file.clone(),
        join_tol_mm:  state.join_tol_mm,
    };
    if let Ok(json) = serde_json::to_string_pretty(&ps) {
        let _ = std::fs::write(state_path(), json);
    }
}

pub fn run(device: Device) {
    let mut state = State::new(device);

    if let Some(ps) = load_persisted() {
        state.position    = (ps.position_x, ps.position_y);
        state.pen_up_pos  = ps.pen_up_pos;
        state.pen_down_pos = ps.pen_down_pos;
        state.calibrated  = ps.calibrated;
        state.speed_mm_s  = ps.speed_mm_s;
        state.accel_mm_s2 = ps.accel_mm_s2;
        state.font_name   = ps.font_name;
        state.font_scale  = ps.font_scale;
        state.iosevka_file = ps.iosevka_file;
        state.join_tol_mm = ps.join_tol_mm;
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

    println!("AxiDraw controller — type 'help' for commands");

    loop {
        print!("> ");
        io::stdout().flush().unwrap();

        let mut line = String::new();
        match io::stdin().read_line(&mut line) {
            Ok(0) => break, // EOF (Ctrl-D)
            Ok(_) => {}
            Err(e) => { eprintln!("Input error: {e}"); break; }
        }

        match handle(&mut state, &line) {
            Ok(true)  => { save_persisted(&state); break; }
            Ok(false) => save_persisted(&state),
            Err(e)    => eprintln!("Error: {e}"),
        }
    }

    println!("Bye.");
}
