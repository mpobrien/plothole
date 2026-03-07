use std::io::{self, Write};
use std::time::Duration;

use crate::device::{Commander, Device, Motor1Setting, Motor2Setting, StepMode};

const DEFAULT_SPEED_MM_S: f64 = 50.0;
const DEFAULT_STEP_MODE: StepMode = StepMode::Step1_16;

struct State {
    device:     Device,
    position:   (f64, f64), // mm, tracked in software
    step_mode:  StepMode,
    speed_mm_s: f64,
}

impl State {
    fn new(device: Device) -> Self {
        Self {
            device,
            position: (0.0, 0.0),
            step_mode: DEFAULT_STEP_MODE,
            speed_mm_s: DEFAULT_SPEED_MM_S,
        }
    }

    fn do_move(&mut self, dx_mm: f64, dy_mm: f64) -> Result<(), Box<dyn std::error::Error>> {
        let dist = (dx_mm * dx_mm + dy_mm * dy_mm).sqrt();
        if dist < 1e-9 { return Ok(()); }
        let duration = Duration::from_secs_f64(dist / self.speed_mm_s);
        self.device.move_mm(dx_mm, dy_mm, duration, self.step_mode)?;
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
            println!("  on [mode]       Enable motors  (mode: 1/16 1/8 1/4 1/2 full; default 1/16)");
            println!("  off             Disable motors");
            println!("  up              Pen up");
            println!("  down            Pen down");
            println!("  move <x> <y>   Relative move in mm");
            println!("  goto <x> <y>   Move to absolute position in mm");
            println!("  speed [mm/s]   Show or set movement speed");
            println!("  pos             Show current position");
            println!("  home            Disable motors, wait for manual positioning at origin, then reset position");
            println!("  raw <cmd...>    Send a raw EBB command and print the response");
            println!("  quit / q        Exit");
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

        "pos" | "position" => {
            println!("Position: ({:.2}, {:.2}) mm", state.position.0, state.position.1);
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

        "quit" | "exit" | "q" => return Ok(true),

        other => println!("Unknown command '{other}'. Type 'help' for commands."),
    }

    Ok(false)
}

pub fn run(device: Device) {
    let mut state = State::new(device);
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
            Ok(true)  => break,
            Ok(false) => {}
            Err(e)    => eprintln!("Error: {e}"),
        }
    }

    println!("Bye.");
}
