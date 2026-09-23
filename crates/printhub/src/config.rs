//! Runtime configuration, read once from the environment at startup.

use std::{fmt, net::SocketAddr, path::PathBuf, time::Duration};

use jiff::tz::TimeZone;
use thiserror::Error;

#[derive(Debug, Error)]
#[error("{var}: {message}")]
pub struct ConfigError {
    pub var: &'static str,
    pub message: String,
}

/// Nozzle diameters OrcaSlicer ships Centauri Carbon 2 machine profiles for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nozzle {
    Mm02,
    Mm04,
    Mm06,
    Mm08,
}

impl Nozzle {
    /// The diameter as written in OrcaSlicer's profile names, e.g. `0.4`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mm02 => "0.2",
            Self::Mm04 => "0.4",
            Self::Mm06 => "0.6",
            Self::Mm08 => "0.8",
        }
    }

    pub const ALL: [Self; 4] = [Self::Mm02, Self::Mm04, Self::Mm06, Self::Mm08];

    pub fn millimetres(self) -> f64 {
        match self {
            Self::Mm02 => 0.2,
            Self::Mm04 => 0.4,
            Self::Mm06 => 0.6,
            Self::Mm08 => 0.8,
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "0.2" => Some(Self::Mm02),
            "0.4" => Some(Self::Mm04),
            "0.6" => Some(Self::Mm06),
            "0.8" => Some(Self::Mm08),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct AdminBootstrap {
    pub username: String,
    pub password: String,
}

#[derive(Clone)]
pub struct Config {
    pub printer_host: String,
    pub printer_access_code: String,
    pub printer_sn: Option<String>,
    pub printer_mqtt_port: u16,
    pub printer_upload_port: u16,
    pub printer_camera_port: u16,
    /// Assumed mounted until someone records the nozzle on the dashboard.
    pub nozzle: Nozzle,
    pub listen_addr: SocketAddr,
    pub data_dir: PathBuf,
    pub admin_bootstrap: Option<AdminBootstrap>,
    pub trust_proxy: bool,
    pub camera_enabled: bool,
    pub max_upload_bytes: u64,
    pub slice_timeout: Duration,
    pub estimate_margin: f64,
    pub orca_slicer: PathBuf,
    /// OrcaSlicer's `resources/profiles/Elegoo` directory.
    pub orca_profiles: PathBuf,
    /// Schedule rules are wall-clock times in this zone. Without `TZ` it is the system's zone,
    /// the same one jiff's `TimeZone::system()` gives the pages that format times.
    pub timezone: TimeZone,
}

/// Printer firmware default when no access code has been set on the touchscreen.
pub const DEFAULT_ACCESS_CODE: &str = "123456";

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|var| std::env::var(var).ok())
    }

    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let env = Env(lookup);

        let printer_host = env
            .get("PRINTER_HOST")
            .map(|host| host.trim().to_owned())
            .ok_or_else(|| ConfigError {
                var: "PRINTER_HOST",
                message: "required: the printer's hostname or IP address".into(),
            })?;

        let admin_bootstrap = match (env.get("ADMIN_USERNAME"), env.get("ADMIN_PASSWORD")) {
            (Some(username), Some(password)) => Some(AdminBootstrap {
                username: username.trim().to_owned(),
                password,
            }),
            (None, None) => None,
            (Some(_), None) => {
                return Err(ConfigError {
                    var: "ADMIN_PASSWORD",
                    message: "required when ADMIN_USERNAME is set".into(),
                });
            }
            (None, Some(_)) => {
                return Err(ConfigError {
                    var: "ADMIN_USERNAME",
                    message: "required when ADMIN_PASSWORD is set".into(),
                });
            }
        };

        Ok(Self {
            printer_host,
            printer_access_code: env
                .get("PRINTER_ACCESS_CODE")
                .unwrap_or_else(|| DEFAULT_ACCESS_CODE.into()),
            printer_sn: env.get("PRINTER_SN").map(|sn| sn.trim().to_owned()),
            printer_mqtt_port: env.parse("PRINTER_MQTT_PORT", 1883, "a port number", parse_port)?,
            printer_upload_port: env.parse(
                "PRINTER_UPLOAD_PORT",
                80,
                "a port number",
                parse_port,
            )?,
            printer_camera_port: env.parse(
                "PRINTER_CAMERA_PORT",
                8080,
                "a port number",
                parse_port,
            )?,
            nozzle: env.parse(
                "PRINTER_NOZZLE",
                Nozzle::Mm04,
                "0.2, 0.4, 0.6 or 0.8",
                Nozzle::parse,
            )?,
            listen_addr: env.parse(
                "LISTEN_ADDR",
                SocketAddr::from(([0, 0, 0, 0], 8080)),
                "an address like 0.0.0.0:8080",
                |raw| raw.parse().ok(),
            )?,
            data_dir: env
                .get("DATA_DIR")
                .map_or_else(|| "/data".into(), PathBuf::from),
            admin_bootstrap,
            trust_proxy: env.parse("TRUST_PROXY", false, "true or false", parse_bool)?,
            camera_enabled: env.parse("CAMERA_ENABLED", true, "true or false", parse_bool)?,
            max_upload_bytes: env.parse(
                "MAX_UPLOAD_MB",
                200 * 1024 * 1024,
                "a whole number of megabytes above 0",
                |raw| {
                    raw.parse::<u64>()
                        .ok()
                        .filter(|&mb| mb > 0)
                        .and_then(|mb| mb.checked_mul(1024 * 1024))
                },
            )?,
            slice_timeout: env.parse(
                "SLICE_TIMEOUT",
                Duration::from_secs(15 * 60),
                "a duration like 90s, 15m or 1h",
                parse_duration,
            )?,
            estimate_margin: env.parse(
                "ESTIMATE_MARGIN",
                1.15,
                "a factor of at least 1.0, like 1.15",
                |raw| {
                    raw.parse::<f64>()
                        .ok()
                        .filter(|m| m.is_finite() && *m >= 1.0)
                },
            )?,
            orca_slicer: env
                .get("ORCA_SLICER")
                .map_or_else(|| "/opt/orcaslicer/bin/orca-slicer".into(), PathBuf::from),
            orca_profiles: env.get("ORCA_PROFILES").map_or_else(
                || "/opt/orcaslicer/resources/profiles/Elegoo".into(),
                PathBuf::from,
            ),
            timezone: env.parse(
                "TZ",
                TimeZone::system(),
                "an IANA time zone like Europe/Berlin",
                |raw| TimeZone::get(raw).ok(),
            )?,
        })
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("printer_host", &self.printer_host)
            .field("printer_access_code", &"<redacted>")
            .field("printer_sn", &self.printer_sn)
            .field("printer_mqtt_port", &self.printer_mqtt_port)
            .field("printer_upload_port", &self.printer_upload_port)
            .field("printer_camera_port", &self.printer_camera_port)
            .field("nozzle", &self.nozzle)
            .field("listen_addr", &self.listen_addr)
            .field("data_dir", &self.data_dir)
            .field(
                "admin_bootstrap",
                &self.admin_bootstrap.as_ref().map(|a| &a.username),
            )
            .field("trust_proxy", &self.trust_proxy)
            .field("camera_enabled", &self.camera_enabled)
            .field("max_upload_bytes", &self.max_upload_bytes)
            .field("slice_timeout", &self.slice_timeout)
            .field("estimate_margin", &self.estimate_margin)
            .field("orca_slicer", &self.orca_slicer)
            .field("orca_profiles", &self.orca_profiles)
            .field("timezone", &self.timezone.iana_name())
            .finish()
    }
}

struct Env<F>(F);

impl<F: Fn(&str) -> Option<String>> Env<F> {
    /// Unset and empty are the same thing, so `FOO=` in a compose file falls back to the default.
    /// Not trimmed: secrets may legitimately carry surrounding spaces.
    fn get(&self, var: &str) -> Option<String> {
        (self.0)(var).filter(|value| !value.trim().is_empty())
    }

    fn parse<T>(
        &self,
        var: &'static str,
        default: T,
        expected: &str,
        parse: impl FnOnce(&str) -> Option<T>,
    ) -> Result<T, ConfigError> {
        let Some(raw) = self.get(var) else {
            return Ok(default);
        };
        parse(raw.trim()).ok_or_else(|| ConfigError {
            var,
            message: format!("expected {expected}, got {raw:?}"),
        })
    }
}

fn parse_bool(raw: &str) -> Option<bool> {
    match raw.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn parse_port(raw: &str) -> Option<u16> {
    raw.parse().ok().filter(|&port| port > 0)
}

fn parse_duration(raw: &str) -> Option<Duration> {
    let split = raw.find(|c: char| !c.is_ascii_digit())?;
    let (number, unit) = raw.split_at(split);
    let number: u64 = number.parse().ok().filter(|&n| n > 0)?;
    let seconds = match unit {
        "s" => number,
        "m" => number.checked_mul(60)?,
        "h" => number.checked_mul(3600)?,
        _ => return None,
    };
    Some(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn config(pairs: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Config::from_lookup(|var| map.get(var).cloned())
    }

    #[test]
    fn defaults_with_only_printer_host() {
        let config = config(&[("PRINTER_HOST", " cc2 ")]).unwrap();
        assert_eq!(config.printer_host, "cc2");
        assert_eq!(config.printer_access_code, DEFAULT_ACCESS_CODE);
        assert_eq!(config.printer_sn, None);
        assert_eq!(
            (
                config.printer_mqtt_port,
                config.printer_upload_port,
                config.printer_camera_port
            ),
            (1883, 80, 8080)
        );
        assert_eq!(config.nozzle, Nozzle::Mm04);
        assert_eq!(config.listen_addr, "0.0.0.0:8080".parse().unwrap());
        assert_eq!(config.data_dir, PathBuf::from("/data"));
        assert!(config.admin_bootstrap.is_none());
        assert!(!config.trust_proxy);
        assert!(config.camera_enabled);
        assert_eq!(config.max_upload_bytes, 200 * 1024 * 1024);
        assert_eq!(config.slice_timeout, Duration::from_secs(900));
        assert_eq!(config.estimate_margin, 1.15);
        assert_eq!(config.timezone, TimeZone::system());
    }

    #[test]
    fn missing_printer_host_names_the_variable() {
        let err = config(&[]).unwrap_err();
        assert_eq!(err.var, "PRINTER_HOST");
    }

    #[test]
    fn empty_value_falls_back_to_default() {
        let config = config(&[("PRINTER_HOST", "cc2"), ("CAMERA_ENABLED", "")]).unwrap();
        assert!(config.camera_enabled);
    }

    #[test]
    fn invalid_values_name_the_variable() {
        for (var, value) in [
            ("TRUST_PROXY", "maybe"),
            ("PRINTER_NOZZLE", "0.5"),
            ("PRINTER_CAMERA_PORT", "0"),
            ("LISTEN_ADDR", "8080"),
            ("MAX_UPLOAD_MB", "0"),
            ("SLICE_TIMEOUT", "15"),
            ("ESTIMATE_MARGIN", "0.9"),
            ("TZ", "Mars/Olympus_Mons"),
        ] {
            let err = config(&[("PRINTER_HOST", "cc2"), (var, value)]).unwrap_err();
            assert_eq!(err.var, var, "{value:?} should be rejected");
        }
    }

    #[test]
    fn time_zone_by_name() {
        let config = config(&[("PRINTER_HOST", "cc2"), ("TZ", "Europe/Berlin")]).unwrap();
        assert_eq!(config.timezone.iana_name(), Some("Europe/Berlin"));
    }

    #[test]
    fn admin_bootstrap_needs_both_halves() {
        let err = config(&[("PRINTER_HOST", "cc2"), ("ADMIN_USERNAME", "alex")]).unwrap_err();
        assert_eq!(err.var, "ADMIN_PASSWORD");

        let config = config(&[
            ("PRINTER_HOST", "cc2"),
            ("ADMIN_USERNAME", "alex"),
            ("ADMIN_PASSWORD", " secret "),
        ])
        .unwrap();
        let admin = config.admin_bootstrap.unwrap();
        assert_eq!(admin.username, "alex");
        assert_eq!(admin.password, " secret ");
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90s"), Some(Duration::from_secs(90)));
        assert_eq!(parse_duration("15m"), Some(Duration::from_secs(900)));
        assert_eq!(parse_duration("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_duration("0m"), None);
        assert_eq!(parse_duration("m"), None);
        assert_eq!(parse_duration("5d"), None);
    }

    #[test]
    fn debug_redacts_secrets() {
        let config = config(&[
            ("PRINTER_HOST", "cc2"),
            ("PRINTER_ACCESS_CODE", "hunter2"),
            ("ADMIN_USERNAME", "alex"),
            ("ADMIN_PASSWORD", "swordfish"),
        ])
        .unwrap();
        let debug = format!("{config:?}");
        assert!(!debug.contains("hunter2"));
        assert!(!debug.contains("swordfish"));
        assert!(debug.contains("alex"));
    }
}
