//! MQTT method codes and request payloads, as used by Elegoo's `elegoo-link` SDK.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const GET_ATTRIBUTES: u32 = 1001;
pub const GET_STATUS: u32 = 1002;
pub const START_PRINT: u32 = 1020;
pub const PAUSE_PRINT: u32 = 1021;
pub const STOP_PRINT: u32 = 1022;
pub const RESUME_PRINT: u32 = 1023;
pub const SET_LIGHT: u32 = 1029;
pub const PRINT_TASK_LIST: u32 = 1036;
pub const VIDEO_STREAM: u32 = 1042;
pub const GET_FILE_LIST: u32 = 1044;
pub const GET_FILE_THUMBNAIL: u32 = 1045;
pub const GET_FILE_DETAIL: u32 = 1046;
pub const DELETE_FILE: u32 = 1047;
pub const GET_CANVAS_STATUS: u32 = 2005;

/// Page of the print history; one page is enough to find a print that just ended.
#[derive(Serialize)]
pub struct TaskPage {
    pub page: i64,
    pub page_size: i64,
}

pub const EVENT_STATUS: u32 = 6000;
pub const EVENT_ATTRIBUTES: u32 = 6008;

pub mod error_code {
    pub const SUCCESS: i64 = 0;
    pub const TOKEN_FAILED: i64 = 1000;
    pub const INVALID_PARAMETER: i64 = 1003;
    pub const PRINTER_BUSY: i64 = 1009;
    pub const NOT_PRINTING: i64 = 1010;
    pub const PRINT_FILE_NOT_FOUND: i64 = 1021;
}

pub const STORAGE_LOCAL: &str = "local";

#[derive(Debug, Serialize)]
pub struct Request<P> {
    pub id: u64,
    pub method: u32,
    pub params: P,
}

/// Anything the printer publishes: command responses, status events, and heartbeat replies,
/// which carry only `type`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Envelope {
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(default)]
    pub method: Option<u32>,
    #[serde(default)]
    pub result: Value,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
}

impl Envelope {
    pub fn error_code(&self) -> Option<i64> {
        self.result.get("error_code").and_then(Value::as_i64)
    }

    pub fn is_pong(&self) -> bool {
        self.kind.as_deref() == Some("PONG")
    }
}

#[derive(Debug, Serialize)]
pub struct Empty {}

#[derive(Debug, Serialize)]
pub struct FileRef<'a> {
    pub storage_media: &'a str,
    pub filename: &'a str,
}

impl<'a> FileRef<'a> {
    pub fn local(filename: &'a str) -> Self {
        Self {
            storage_media: STORAGE_LOCAL,
            filename,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct FileList<'a> {
    pub storage_media: &'a str,
    pub path: &'a str,
    pub page: u32,
    pub page_size: u32,
}

#[derive(Debug, Serialize)]
pub struct VideoStream {
    pub enable: bool,
}

#[derive(Debug, Serialize)]
pub struct StartPrint<'a> {
    pub storage_media: &'a str,
    pub filename: &'a str,
    pub config: StartConfig,
}

#[derive(Debug, Serialize)]
pub struct StartConfig {
    pub delay_video: bool,
    /// Auto bed leveling (about three minutes) before the print. ElegooSlicer sends `true` on
    /// every job; with `false` the printer only levels when it decides to itself.
    pub printer_check: bool,
    pub print_layout: &'static str,
    pub bedlevel_force: bool,
    /// Empty leaves the tray choice to the printer.
    pub slot_map: Vec<SlotMapEntry>,
}

impl<'a> StartPrint<'a> {
    pub fn local(filename: &'a str, slot_map: Vec<SlotMapEntry>) -> Self {
        Self {
            storage_media: STORAGE_LOCAL,
            filename,
            config: StartConfig {
                delay_video: false,
                printer_check: true,
                print_layout: "A",
                bedlevel_force: false,
                slot_map,
            },
        }
    }
}

/// Routes one G-code tool to a physical CANVAS tray. The printer does not validate `tray_id`:
/// an out-of-range tray is acknowledged and silently printed from tray 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotMapEntry {
    pub t: u32,
    pub canvas_id: u32,
    pub tray_id: u32,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn start_print_matches_documented_shape() {
        let request = Request {
            id: 7,
            method: START_PRINT,
            params: StartPrint::local(
                "multicolor.gcode",
                vec![SlotMapEntry {
                    t: 0,
                    canvas_id: 0,
                    tray_id: 2,
                }],
            ),
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({
                "id": 7,
                "method": 1020,
                "params": {
                    "storage_media": "local",
                    "filename": "multicolor.gcode",
                    "config": {
                        "delay_video": false,
                        "printer_check": true,
                        "print_layout": "A",
                        "bedlevel_force": false,
                        "slot_map": [{"t": 0, "canvas_id": 0, "tray_id": 2}]
                    }
                }
            })
        );
    }

    #[test]
    fn empty_params_serialize_as_object() {
        let request = Request {
            id: 1,
            method: PAUSE_PRINT,
            params: Empty {},
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({"id": 1, "method": 1021, "params": {}})
        );
    }

    #[test]
    fn envelope_recognises_pong_and_error_codes() {
        let pong: Envelope = serde_json::from_str(r#"{"type":"PONG"}"#).unwrap();
        assert!(pong.is_pong());

        let busy: Envelope =
            serde_json::from_str(r#"{"id":3,"method":1020,"result":{"error_code":1009}}"#).unwrap();
        assert!(!busy.is_pong());
        assert_eq!(busy.error_code(), Some(error_code::PRINTER_BUSY));
    }
}
