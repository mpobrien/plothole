use serialport::{SerialPort, SerialPortType};
use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

const AXIDRAW_VID: u16 = 0x04D8;
const AXIDRAW_PID: u16 = 0xFD92;

type Error = Box<dyn std::error::Error>;

// ── Motor configuration types ─────────────────────────────────────────────────

/// Global step resolution, set via the Enable1 parameter of the EM command.
/// Both motors always share the same step mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepMode {
    /// 1/16 step — 3200 steps/rev at 200-step motor (default after reset)
    Step1_16 = 1,
    /// 1/8 step — 1600 steps/rev
    Step1_8  = 2,
    /// 1/4 step — 800 steps/rev
    Step1_4  = 3,
    /// 1/2 step — 400 steps/rev
    Step1_2  = 4,
    /// Full step — 200 steps/rev
    Full     = 5,
}

/// Motor 1 setting for the EM command. Controls both its enable state and the
/// global step mode (which applies to both motors).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motor1Setting {
    Disable,
    Enable(StepMode),
}

/// Motor 2 setting for the EM command. Only controls whether the motor is
/// enabled; it cannot change the global step mode.
/// `Unchanged` omits the Enable2 argument entirely, leaving motor 2 as-is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motor2Setting {
    Disable,
    Enable,
    /// Omit Enable2 from the command — motor 2 state is not changed.
    Unchanged,
}

// ── Device ───────────────────────────────────────────────────────────────────

pub struct Device {
    port:   Box<dyn SerialPort>,
    reader: BufReader<Box<dyn SerialPort>>,
}

impl Device {
    /// Send a command (params joined with commas, terminated by CR) and return
    /// the response line from the device.
    pub fn command(&mut self, params: &[&str]) -> Result<String, Error> {
        let out = params.join(",");
        self.port.write_all(out.as_bytes())?;
        self.port.write_all(b"\r")?;
        let mut response = String::new();
        self.reader.read_line(&mut response)?;
        Ok(response)
    }
}

// ── Commander trait ───────────────────────────────────────────────────────────

pub trait Commander {
    /// Enable or disable motors and optionally set the global step mode.
    /// `motor2` may be `Unchanged` to leave motor 2's state unmodified.
    fn enable_motors(&mut self, motor1: Motor1Setting, motor2: Motor2Setting) -> Result<(), Error>;

    /// Convenience: enable both motors at the given step mode.
    fn steppers_on(&mut self, mode: StepMode) -> Result<(), Error> {
        self.enable_motors(Motor1Setting::Enable(mode), Motor2Setting::Enable)
    }

    /// Convenience: disable both motors (both freewheel).
    fn steppers_off(&mut self) -> Result<(), Error> {
        self.enable_motors(Motor1Setting::Disable, Motor2Setting::Disable)
    }

    fn pen_up(&mut self)   -> Result<(), Error>;
    fn pen_down(&mut self) -> Result<(), Error>;
    fn move_motors(&mut self, steps_x: i32, steps_y: i32, duration: Duration) -> Result<(), Error>;
    fn raw(&mut self, command: &[&str]) -> Result<String, Error>;
}

impl Commander for Device {
    fn enable_motors(&mut self, motor1: Motor1Setting, motor2: Motor2Setting) -> Result<(), Error> {
        let e1 = match motor1 {
            Motor1Setting::Disable       => "0".to_string(),
            Motor1Setting::Enable(mode)  => (mode as u8).to_string(),
        };
        match motor2 {
            Motor2Setting::Unchanged => self.command(&["EM", &e1])?,
            Motor2Setting::Disable   => self.command(&["EM", &e1, "0"])?,
            Motor2Setting::Enable    => self.command(&["EM", &e1, "1"])?,
        };
        Ok(())
    }

    fn pen_up(&mut self) -> Result<(), Error> {
        self.command(&["SP", "1", "0"])?;
        Ok(())
    }

    fn pen_down(&mut self) -> Result<(), Error> {
        self.command(&["SP", "0", "0"])?;
        Ok(())
    }

    fn move_motors(&mut self, steps_x: i32, steps_y: i32, duration: Duration) -> Result<(), Error> {
        self.command(&[
            "XM",
            &duration.as_millis().to_string(),
            &steps_x.to_string(),
            &steps_y.to_string(),
        ])?;
        Ok(())
    }

    fn raw(&mut self, command: &[&str]) -> Result<String, Error> {
        self.command(command)
    }
}

// ── Port discovery & opening ──────────────────────────────────────────────────

fn find_axidraw_port() -> Result<String, Error> {
    let ports = serialport::available_ports()?;
    for port in ports {
        if let SerialPortType::UsbPort(info) = port.port_type {
            if info.vid == AXIDRAW_VID && info.pid == AXIDRAW_PID {
                return Ok(port.port_name);
            }
        }
    }
    Err("no AxiDraw connection detected".into())
}

pub fn open_device() -> Result<Device, Error> {
    let port_name = find_axidraw_port()?;
    let port = serialport::new(&port_name, 9600)
        .data_bits(serialport::DataBits::Eight)
        .parity(serialport::Parity::None)
        .stop_bits(serialport::StopBits::One)
        .timeout(Duration::from_secs(5))
        .open()?;
    let reader = BufReader::new(port.try_clone()?);
    Ok(Device { port, reader })
}
