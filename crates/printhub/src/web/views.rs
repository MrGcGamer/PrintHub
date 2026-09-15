//! Display-ready values for templates, so templates hold no formatting logic.

use crate::{
    accounts::User,
    cc2::{
        LinkState, PrinterSnapshot,
        model::{Heater, MachineState, StatusView, TrayState, printing_sub_status as sub},
    },
};

pub struct PrinterCard {
    pub connected: bool,
    pub link: String,
    pub state: String,
    pub busy: bool,
    pub paused: bool,
    pub progress: Option<i64>,
    pub filename: String,
    pub layers: Option<String>,
    pub remaining: Option<String>,
    pub nozzle: Option<String>,
    pub bed: Option<String>,
    pub trays: Vec<TrayView>,
    pub can_control: bool,
}

pub struct TrayView {
    pub label: String,
    pub material: String,
    /// Always a valid `#RRGGBB`: it is written into an SVG attribute.
    pub color: String,
    pub state: &'static str,
    pub loaded: bool,
}

impl PrinterCard {
    pub fn new(snapshot: &PrinterSnapshot, user: &User) -> Self {
        let connected = snapshot.link == LinkState::Registered;
        let link = match &snapshot.link {
            LinkState::Registered => "Connected".to_owned(),
            LinkState::Connecting => "Connecting".to_owned(),
            LinkState::Discovering(_) => "Looking for the printer".to_owned(),
            LinkState::Rejected(reason) => format!("Refused: {reason}"),
            LinkState::Disconnected(_) => "Disconnected".to_owned(),
        };
        let trays = trays(snapshot);

        let Some(status) = &snapshot.status else {
            return Self {
                connected,
                link,
                state: "Waiting for the printer".to_owned(),
                busy: false,
                paused: false,
                progress: None,
                filename: String::new(),
                layers: None,
                remaining: None,
                nozzle: None,
                bed: None,
                trays,
                can_control: false,
            };
        };

        let machine = &status.machine_status;
        let print = &status.print_status;
        let busy = machine.state() == MachineState::Printing;
        let finished =
            machine.state() == MachineState::Idle && machine.sub_status == sub::COMPLETED;
        Self {
            connected,
            link,
            state: state_label(status),
            busy,
            paused: busy && matches!(machine.sub_status, sub::PAUSED | sub::PAUSED_ALT),
            progress: (busy || finished).then_some(machine.progress.clamp(0, 100)),
            filename: print.filename.clone(),
            layers: (print.total_layer > 0)
                .then(|| format!("{} of {}", print.current_layer, print.total_layer)),
            remaining: (busy && print.remaining_time_sec > 0)
                .then(|| human_duration(print.remaining_time_sec)),
            nozzle: Some(temperature(&status.extruder)),
            bed: Some(temperature(&status.heater_bed)),
            trays,
            can_control: connected && user.is_admin(),
        }
    }
}

pub fn state_label(status: &StatusView) -> String {
    let machine = &status.machine_status;
    let label = match machine.state() {
        MachineState::Printing => match machine.sub_status {
            sub::PAUSED | sub::PAUSED_ALT => "Paused",
            sub::PAUSING => "Pausing",
            sub::RESUMING => "Resuming",
            sub::STOPPING => "Stopping",
            sub::STOPPED => "Stopped",
            sub::EXTRUDER_PREHEATING | sub::EXTRUDER_PREHEATING_ALT => "Heating the nozzle",
            sub::BED_PREHEATING | sub::BED_PREHEATING_ALT => "Heating the bed",
            sub::HOMING | sub::HOMING_COMPLETED => "Homing",
            sub::AUTO_LEVELING | sub::AUTO_LEVELING_COMPLETED => "Levelling the bed",
            _ => "Printing",
        },
        MachineState::Idle if machine.sub_status == sub::COMPLETED => "Finished",
        MachineState::Idle => "Idle",
        MachineState::Initializing => "Starting up",
        MachineState::FilamentOperating => "Loading or unloading filament",
        MachineState::AutoLeveling => "Levelling the bed",
        MachineState::PidCalibrating => "Calibrating heaters",
        MachineState::ResonanceTesting => "Measuring resonance",
        MachineState::SelfChecking => "Running a self-check",
        MachineState::Updating => "Updating firmware",
        MachineState::Homing => "Homing",
        MachineState::FileTransferring => "Receiving a file",
        MachineState::VideoComposing => "Rendering a timelapse",
        MachineState::ExtruderOperating => "Extruder maintenance",
        MachineState::EmergencyStop => "Emergency stop",
        MachineState::PowerLossRecovery => "Recovering from power loss",
        MachineState::Unknown(code) => return format!("Unknown state {code}"),
    };
    label.to_owned()
}

fn trays(snapshot: &PrinterSnapshot) -> Vec<TrayView> {
    let Some(canvas) = &snapshot.canvas else {
        return Vec::new();
    };
    canvas
        .canvas_list
        .iter()
        .flat_map(|unit| {
            let letter = char::from(b'A' + u8::try_from(unit.canvas_id % 26).unwrap_or(0));
            unit.tray_list.iter().map(move |tray| {
                let loaded = tray.has_filament();
                let name = if tray.filament_name.is_empty() {
                    &tray.filament_type
                } else {
                    &tray.filament_name
                };
                TrayView {
                    label: format!("{letter}{}", tray.tray_id + 1),
                    material: if loaded { name.clone() } else { String::new() },
                    color: safe_color(&tray.filament_color),
                    state: match tray.state() {
                        TrayState::Empty => "empty",
                        TrayState::Loaded => "loaded",
                        TrayState::Active => "in use",
                        TrayState::Unknown(_) => "unknown",
                    },
                    loaded,
                }
            })
        })
        .collect()
}

fn safe_color(raw: &str) -> String {
    let hex = raw.strip_prefix('#').unwrap_or(raw);
    if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        format!("#{hex}")
    } else {
        "#888888".to_owned()
    }
}

fn temperature(heater: &Heater) -> String {
    if heater.target > 0.0 {
        format!(
            "{:.0} °C, target {:.0} °C",
            heater.temperature, heater.target
        )
    } else {
        format!("{:.0} °C", heater.temperature)
    }
}

pub fn human_duration(seconds: i64) -> String {
    let minutes = (seconds + 59) / 60;
    match (minutes / 60, minutes % 60) {
        (0, 0) => "under a minute".to_owned(),
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

pub fn format_time(unix: i64) -> String {
    jiff::Timestamp::from_second(unix)
        .map(|ts| {
            ts.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cc2::model::MachineStatus;

    fn status(code: i64, sub_status: i64) -> StatusView {
        StatusView {
            machine_status: MachineStatus {
                status: code,
                sub_status,
                ..MachineStatus::default()
            },
            ..StatusView::default()
        }
    }

    #[test]
    fn labels_depend_on_state_before_sub_status() {
        assert_eq!(state_label(&status(2, sub::PAUSED)), "Paused");
        assert_eq!(state_label(&status(1, sub::COMPLETED)), "Finished");
        assert_eq!(state_label(&status(1, 0)), "Idle");
        // 2075 means "printing" only while printing; under updating it means "update failed".
        assert_eq!(state_label(&status(9, 2075)), "Updating firmware");
        assert_eq!(state_label(&status(42, 0)), "Unknown state 42");
    }

    #[test]
    fn durations_round_up_to_the_minute() {
        assert_eq!(human_duration(0), "under a minute");
        assert_eq!(human_duration(1), "1 min");
        assert_eq!(human_duration(3600), "1 h");
        assert_eq!(human_duration(5 * 3600 + 61), "5 h 2 min");
    }

    #[test]
    fn colors_are_sanitised() {
        assert_eq!(safe_color("#2850DF"), "#2850DF");
        assert_eq!(safe_color("2850df"), "#2850df");
        assert_eq!(safe_color("\" onload=\"x"), "#888888");
        assert_eq!(safe_color(""), "#888888");
    }
}
