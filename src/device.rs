#[repr(i32)]
enum Clear {
    None = 0,
    Motor1 = 1,
    Motor2 = 2,
    Both = 3,
}

#[repr(u8)]
enum ConfigParam {
    PenLift = 1,
    StepperSignal = 2,
    ServoMin = 4,
    ServoMax = 5,
    S2MaximumChannels = 8,
    S2ChannelDurationMS = 9,
    ServoRate = 10,
    ServoRateUp = 11,
    ServoRateDown = 12,
    UseAltPrg = 13,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnableMotor1Setting {
    DisableMotor1 = 0,

    /// 1: Enable motor 1, set global step mode to 1/16 step mode (default upon reset)
    EnableMotor1_16 = 1,

    /// 2: Enable motor 1, set global step mode to 1/8 step mode
    EnableMotor1_8 = 2,

    /// 3: Enable motor 1, set global step mode to 1/4 step mode
    EnableMotor1_4 = 3,

    /// 4: Enable motor 1, set global step mode to 1/2 step mode
    EnableMotor1_2 = 4,

    /// 5: Enable motor 1, set global step mode to full step mode
    EnableMotor1Full = 5,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnableMotor2Setting {
    DisableMotor2 = 0,
    EnableMotor2 = 1,
}

struct EBBDevice {}
impl EBBDevice {
    fn xm(duration_ms: u32, axis_steps_a: u32, axis_steps_b: u32, clear: Clear) {
        todo!()
    }

    // Stepper and Servo Mode Configure
    fn sc(config_param: ConfigParam, value: u16) {}

    // Clear Step position
    fn cs() {}

    // Version query
    fn v() {}

    // Enable Motors
    fn em(motor1: EnableMotor1Setting, motor2: EnableMotor2Setting) {}

    // Emergency Stop
    fn es(disable_motors: bool) {}

    // Query motors
    fn qm() {
        // Response (future mode): QM,CommandStatus,Motor1Status,Motor2Status,FIFOStatus<NL>
    }

    // Query step position
    fn qs() {
        //Response (future mode):QS,GlobalMotor1StepPosition,GlobalMotor2StepPosition<NL>
    }

    // Set pen state
    fn sp(setting: PenSetting, duration_ms: u16) {
        // Command: SP,Value[,Duration[,PortB_Pin]]<CR>
        // Response (future mode): SP<NL>
        // Response (legacy mode; default): OK<CR><NL>
    }
}

#[repr(u8)]
enum PenSetting {
    QueueMoveToMax = 0, // Pen Down position
    QueueMoveToMin = 1, // Pen Up position
    ImmediateMoveToMin = 2,
    ImmediateMoveToMinReset = 3, // idk
}
