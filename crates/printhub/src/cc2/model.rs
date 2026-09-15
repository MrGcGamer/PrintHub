//! Typed read-only views over printer JSON. Every field has a default, because deltas and
//! older firmware omit fields freely; a view is never the source of truth, the raw cache is.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct StatusView {
    pub machine_status: MachineStatus,
    pub print_status: PrintStatus,
    pub extruder: Heater,
    pub heater_bed: Heater,
    pub external_device: ExternalDevice,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct MachineStatus {
    #[serde(deserialize_with = "lenient::int")]
    pub status: i64,
    #[serde(deserialize_with = "lenient::int")]
    pub sub_status: i64,
    pub exception_status: Vec<i64>,
    #[serde(deserialize_with = "lenient::int")]
    pub progress: i64,
}

impl MachineStatus {
    pub fn state(&self) -> MachineState {
        MachineState::from_code(self.status)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct PrintStatus {
    pub filename: String,
    pub uuid: String,
    #[serde(deserialize_with = "lenient::int")]
    pub current_layer: i64,
    #[serde(deserialize_with = "lenient::int")]
    pub total_layer: i64,
    #[serde(deserialize_with = "lenient::int")]
    pub print_duration: i64,
    #[serde(deserialize_with = "lenient::int")]
    pub total_duration: i64,
    #[serde(deserialize_with = "lenient::int")]
    pub remaining_time_sec: i64,
    #[serde(deserialize_with = "lenient::int")]
    pub progress: i64,
    /// Klipper's own state name: `standby`, `printing`, `paused`, `complete`, `cancelled`, `error`.
    pub state: String,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Heater {
    pub temperature: f64,
    pub target: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct ExternalDevice {
    pub camera: bool,
    pub u_disk: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum MachineState {
    Initializing,
    Idle,
    Printing,
    FilamentOperating,
    AutoLeveling,
    PidCalibrating,
    ResonanceTesting,
    SelfChecking,
    Updating,
    Homing,
    FileTransferring,
    VideoComposing,
    ExtruderOperating,
    EmergencyStop,
    PowerLossRecovery,
    Unknown(i64),
}

impl MachineState {
    pub fn from_code(code: i64) -> Self {
        match code {
            0 => Self::Initializing,
            1 => Self::Idle,
            2 => Self::Printing,
            3 | 4 => Self::FilamentOperating,
            5 => Self::AutoLeveling,
            6 => Self::PidCalibrating,
            7 => Self::ResonanceTesting,
            8 => Self::SelfChecking,
            9 => Self::Updating,
            10 => Self::Homing,
            11 => Self::FileTransferring,
            12 => Self::VideoComposing,
            13 => Self::ExtruderOperating,
            14 => Self::EmergencyStop,
            15 => Self::PowerLossRecovery,
            other => Self::Unknown(other),
        }
    }
}

/// Sub-status codes within [`MachineState::Printing`]. Several are reused with other meanings
/// under other machine states (2075 is also "update failed"), so always check the state first.
pub mod printing_sub_status {
    pub const EXTRUDER_PREHEATING: i64 = 1045;
    pub const EXTRUDER_PREHEATING_ALT: i64 = 1096;
    pub const BED_PREHEATING: i64 = 1405;
    pub const BED_PREHEATING_ALT: i64 = 1906;
    pub const HOMING: i64 = 2801;
    pub const HOMING_COMPLETED: i64 = 2802;
    pub const AUTO_LEVELING: i64 = 2901;
    pub const AUTO_LEVELING_COMPLETED: i64 = 2902;
    pub const PRINTING: i64 = 2075;
    pub const COMPLETED: i64 = 2077;
    pub const RESUMING: i64 = 2401;
    pub const RESUMING_COMPLETED: i64 = 2402;
    pub const PAUSING: i64 = 2501;
    pub const PAUSED: i64 = 2502;
    pub const PAUSED_ALT: i64 = 2505;
    pub const STOPPING: i64 = 2503;
    pub const STOPPED: i64 = 2504;
}

/// Result of `GET_ATTRIBUTES` (1001).
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Attributes {
    pub hostname: String,
    pub machine_model: String,
    pub sn: String,
    pub ip: String,
    pub software_version: SoftwareVersion,
    pub camera_connected: bool,
    #[serde(deserialize_with = "lenient::int")]
    pub max_video_connections: i64,
    #[serde(deserialize_with = "lenient::int")]
    pub video_connections: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct SoftwareVersion {
    pub ota_version: String,
    pub mcu_version: String,
    pub soc_version: String,
}

/// Result of `GET_CANVAS_STATUS` (2005).
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct CanvasStatus {
    pub canvas_info: CanvasInfo,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct CanvasInfo {
    #[serde(deserialize_with = "lenient::int")]
    pub active_canvas_id: i64,
    #[serde(deserialize_with = "lenient::int")]
    pub active_tray_id: i64,
    pub auto_refill: bool,
    pub canvas_list: Vec<Canvas>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Canvas {
    pub canvas_id: u32,
    #[serde(deserialize_with = "lenient::int")]
    pub connected: i64,
    pub tray_list: Vec<Tray>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Tray {
    pub tray_id: u32,
    pub brand: String,
    pub filament_type: String,
    pub filament_name: String,
    /// `#RRGGBB`, with the `#`.
    pub filament_color: String,
    #[serde(deserialize_with = "lenient::int")]
    pub min_nozzle_temp: i64,
    #[serde(deserialize_with = "lenient::int")]
    pub max_nozzle_temp: i64,
    #[serde(deserialize_with = "lenient::int")]
    pub status: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TrayState {
    Empty,
    Loaded,
    /// Feeding the nozzle right now.
    Active,
    Unknown(i64),
}

impl Tray {
    pub fn state(&self) -> TrayState {
        match self.status {
            0 => TrayState::Empty,
            1 => TrayState::Loaded,
            2 => TrayState::Active,
            other => TrayState::Unknown(other),
        }
    }

    pub fn has_filament(&self) -> bool {
        matches!(self.state(), TrayState::Loaded | TrayState::Active)
    }
}

/// Result of `GET_FILE_DETAIL` (1046).
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct FileDetail {
    pub filename: String,
    #[serde(deserialize_with = "lenient::int")]
    pub size: i64,
    // Layer count arrives under any of three names, sometimes two at once, so these cannot be
    // serde aliases of one field: a second alias present is a duplicate-field error.
    #[serde(deserialize_with = "lenient::opt_int")]
    pub layer: Option<i64>,
    #[serde(rename = "TotalLayers", deserialize_with = "lenient::opt_int")]
    pub total_layers: Option<i64>,
    #[serde(deserialize_with = "lenient::opt_int")]
    pub total_layer: Option<i64>,
    #[serde(deserialize_with = "lenient::int")]
    pub print_time: i64,
    pub total_filament_used: f64,
    pub color_map: Vec<ColorMapEntry>,
}

impl FileDetail {
    pub fn layers(&self) -> Option<i64> {
        self.total_layers.or(self.layer).or(self.total_layer)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct ColorMapEntry {
    pub color: String,
    /// Filament type as configured in the slicer, e.g. `PLA`.
    pub name: String,
    /// G-code tool index, not a tray: trays are chosen at start time via `slot_map`.
    pub t: u32,
}

/// Numbers from the printer are not reliably typed across firmware versions: an integer field
/// may arrive as `45.0`. Rejecting that would discard the whole view.
mod lenient {
    use serde::{Deserialize, Deserializer};
    use serde_json::Value;

    fn to_int(value: &Value) -> Option<i64> {
        match value {
            Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
            Value::String(s) => s.trim().parse().ok(),
            Value::Bool(b) => Some(i64::from(*b)),
            _ => None,
        }
    }

    pub fn int<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
        Ok(to_int(&Value::deserialize(deserializer)?).unwrap_or_default())
    }

    pub fn opt_int<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<i64>, D::Error> {
        Ok(to_int(&Value::deserialize(deserializer)?))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn status_view_tolerates_floats_and_missing_fields() {
        let view = StatusView::deserialize(&json!({
            "machine_status": {"status": 2, "sub_status": 2075.0, "exception_status": [], "progress": "45"},
            "print_status": {"filename": "benchy.gcode", "current_layer": 225},
            "extruder": {"temperature": 215, "target": 220}
        }))
        .unwrap();
        assert_eq!(view.machine_status.state(), MachineState::Printing);
        assert_eq!(
            view.machine_status.sub_status,
            printing_sub_status::PRINTING
        );
        assert_eq!(view.machine_status.progress, 45);
        assert_eq!(view.print_status.current_layer, 225);
        assert_eq!(view.print_status.total_layer, 0);
        assert_eq!(view.extruder.temperature, 215.0);
        assert_eq!(view.heater_bed, Heater::default());
    }

    #[test]
    fn canvas_status_from_protocol_doc_sample() {
        let canvas = CanvasStatus::deserialize(&json!({
            "error_code": 0,
            "canvas_info": {
                "active_canvas_id": 0,
                "active_tray_id": 3,
                "auto_refill": false,
                "canvas_list": [{
                    "canvas_id": 0,
                    "connected": 1,
                    "tray_list": [
                        {"tray_id": 0, "brand": "ELEGOO", "filament_type": "PLA", "filament_name": "PLA",
                         "filament_color": "#2850DF", "min_nozzle_temp": 190, "max_nozzle_temp": 230, "status": 1},
                        {"tray_id": 3, "brand": "ELEGOO", "filament_type": "PLA", "filament_name": "PLA",
                         "filament_color": "#000000", "min_nozzle_temp": 190, "max_nozzle_temp": 230, "status": 2},
                        {"tray_id": 2, "status": 0}
                    ]
                }]
            }
        }))
        .unwrap();
        let trays = &canvas.canvas_info.canvas_list[0].tray_list;
        assert_eq!(trays[0].state(), TrayState::Loaded);
        assert_eq!(trays[1].state(), TrayState::Active);
        assert!(trays[1].has_filament());
        assert!(!trays[2].has_filament());
        assert_eq!(trays[0].filament_color, "#2850DF");
    }

    #[test]
    fn file_detail_accepts_both_layer_names_at_once() {
        let detail = FileDetail::deserialize(&json!({
            "filename": "cube.gcode",
            "TotalLayers": 500,
            "layer": 500,
            "print_time": 4690,
            "total_filament_used": 24.8,
            "color_map": [{"color": "#0B6283", "name": "PLA", "t": 3}]
        }))
        .unwrap();
        assert_eq!(detail.layers(), Some(500));
        assert_eq!(detail.color_map[0].t, 3);

        let only_legacy = FileDetail::deserialize(&json!({"total_layer": 7})).unwrap();
        assert_eq!(only_legacy.layers(), Some(7));
    }
}
