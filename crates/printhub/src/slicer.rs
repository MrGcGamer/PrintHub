//! Server-side slicing with the OrcaSlicer CLI and the Elegoo Centauri Carbon 2 profiles it
//! ships.
//!
//! The CLI loads profile files as they are and ignores `inherits` (only the GUI resolves it),
//! so a bundled leaf profile slices with defaults for everything its parents set. Each profile
//! is therefore flattened here. They are written back marked `from: system` because the CLI
//! only accepts a process as compatible when its `compatible_printers` names the machine, and
//! it takes a machine's own `name` for that comparison only from a system profile.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use serde_json::{Map, Value};
use thiserror::Error;
use tokio::{process::Command, sync::Semaphore};

use crate::config::Nozzle;

/// Deeper than any bundled chain (four levels in 2.4.2); anything past it is a loop.
const MAX_INHERITANCE: usize = 16;
const OUTPUT_LINES: usize = 20;

#[derive(Debug, Error)]
pub enum SliceError {
    #[error("unknown slicer profile {0:?}")]
    UnknownProfile(String),
    #[error("slicer profile {0:?} inherits in a loop")]
    InheritanceLoop(String),
    #[error("reading slicer profiles in {}: {source}", path.display())]
    Profiles {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("slicing took longer than {} minutes", .0.as_secs() / 60)]
    Timeout(Duration),
    #[error("the slicer failed ({status}): {output}")]
    Failed { status: String, output: String },
    #[error("the slicer finished without writing G-code")]
    NoOutput,
}

/// Every profile of one vendor, by `name`.
pub struct ProfileLibrary {
    profiles: HashMap<String, Map<String, Value>>,
}

impl ProfileLibrary {
    /// `vendor_dir` is a vendor's directory under OrcaSlicer's `resources/profiles`, which holds
    /// `machine/`, `process/` and `filament/` trees.
    pub fn load(vendor_dir: &Path) -> Result<Self, SliceError> {
        let mut profiles = HashMap::new();
        let mut pending = vec![vendor_dir.to_path_buf()];
        while let Some(dir) = pending.pop() {
            let entries = std::fs::read_dir(&dir).map_err(|source| SliceError::Profiles {
                path: dir.clone(),
                source,
            })?;
            for entry in entries {
                let path = entry?.path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                if path.extension().is_none_or(|ext| ext != "json") {
                    continue;
                }
                let parsed = std::fs::read(&path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Map<String, Value>>(&bytes).ok());
                if let Some(profile) = parsed
                    && let Some(Value::String(name)) = profile.get("name")
                {
                    profiles.insert(name.clone(), profile);
                }
            }
        }
        Ok(Self { profiles })
    }

    /// The profile with every inherited setting filled in, `inherits` removed.
    pub fn flatten(&self, name: &str) -> Result<Map<String, Value>, SliceError> {
        let mut flat = Map::new();
        let mut next = Some(name.to_owned());
        for _ in 0..MAX_INHERITANCE {
            let Some(current) = next.take() else {
                flat.remove("inherits");
                return Ok(flat);
            };
            let profile = self
                .profiles
                .get(&current)
                .ok_or_else(|| SliceError::UnknownProfile(current.clone()))?;
            for (key, value) in profile {
                flat.entry(key.clone()).or_insert_with(|| value.clone());
            }
            next = match profile.get("inherits") {
                Some(Value::String(parent)) if !parent.is_empty() => Some(parent.clone()),
                _ => None,
            };
        }
        Err(SliceError::InheritanceLoop(name.to_owned()))
    }

    /// Selectable profiles of `kind` (`process` or `filament`) that name `machine` as
    /// compatible, sorted by name.
    ///
    /// `compatible_printers` counts when inherited: Elegoo's Fine and Draft processes take it
    /// from Standard. `instantiation` is read from the profile's own file, because parents are
    /// marked `false`. Profiles that cannot be flattened are left out.
    pub fn compatible(&self, kind: &str, machine: &str) -> Vec<String> {
        let mut names: Vec<String> = self
            .profiles
            .iter()
            .filter(|(name, profile)| {
                profile.get("type").and_then(Value::as_str) == Some(kind)
                    && profile.get("instantiation").and_then(Value::as_str) == Some("true")
                    && self.flatten(name).is_ok_and(|flat| {
                        flat.get("compatible_printers")
                            .and_then(Value::as_array)
                            .is_some_and(|printers| printers.iter().any(|p| p == machine))
                    })
            })
            .map(|(name, _)| name.clone())
            .collect();
        names.sort();
        names
    }
}

/// A build plate the Centauri Carbon 2 takes. Filament profiles set a bed temperature per plate
/// type, and the CLI slices for OrcaSlicer's Cool Plate unless told otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plate {
    TexturedPei,
    HighTemp,
}

impl Plate {
    pub const ALL: [Self; 2] = [Self::TexturedPei, Self::HighTemp];

    /// OrcaSlicer's name for the plate type, as `curr_bed_type` takes it and G-code records it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TexturedPei => "Textured PEI Plate",
            Self::HighTemp => "High Temp Plate",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|plate| plate.as_str() == raw)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceSettings {
    pub nozzle: Nozzle,
    pub plate: Plate,
    pub process: String,
    pub filament: String,
    /// `#RRGGBB`. Recorded in the G-code, where the printer's own screen shows it.
    pub color_hex: String,
    pub supports: bool,
    pub infill_percent: u8,
}

pub struct Slicer {
    binary: PathBuf,
    library: ProfileLibrary,
    timeout: Duration,
    /// One slice at a time: the CLI uses every core.
    running: Semaphore,
}

impl Slicer {
    /// Holds every nozzle's profiles: the mounted nozzle can change while the server runs.
    pub fn new(binary: PathBuf, vendor_dir: &Path, timeout: Duration) -> Result<Self, SliceError> {
        Ok(Self {
            binary,
            library: ProfileLibrary::load(vendor_dir)?,
            timeout,
            running: Semaphore::new(1),
        })
    }

    pub fn processes(&self, nozzle: Nozzle) -> Vec<String> {
        self.library.compatible("process", &machine_name(nozzle))
    }

    pub fn filaments(&self, nozzle: Nozzle) -> Vec<String> {
        self.library.compatible("filament", &machine_name(nozzle))
    }

    /// Slices `model` inside `workdir`, which must exist, and returns the G-code's path.
    pub async fn slice(
        &self,
        model: &Path,
        workdir: &Path,
        settings: &SliceSettings,
    ) -> Result<PathBuf, SliceError> {
        let _running = self
            .running
            .acquire()
            .await
            .expect("the semaphore is never closed");

        let machine = self.library.flatten(&machine_name(settings.nozzle))?;
        let mut process = self.library.flatten(&settings.process)?;
        let mut filament = self.library.flatten(&settings.filament)?;
        process.insert("curr_bed_type".into(), Value::from(settings.plate.as_str()));
        process.insert(
            "enable_support".into(),
            Value::from(if settings.supports { "1" } else { "0" }),
        );
        process.insert(
            "sparse_infill_density".into(),
            Value::from(format!("{}%", settings.infill_percent.min(100))),
        );
        filament.insert(
            "filament_colour".into(),
            Value::from(vec![settings.color_hex.clone()]),
        );

        let profiles = workdir.join("profiles");
        let output = workdir.join("out");
        let datadir = workdir.join("orca-data");
        for dir in [&profiles, &output, &datadir] {
            tokio::fs::create_dir_all(dir).await?;
        }
        let mut paths = Vec::new();
        for (file, mut profile) in [
            ("machine.json", machine),
            ("process.json", process),
            ("filament.json", filament),
        ] {
            profile.insert("from".into(), Value::from("system"));
            let path = profiles.join(file);
            tokio::fs::write(
                &path,
                serde_json::to_vec_pretty(&profile).expect("JSON values"),
            )
            .await?;
            paths.push(path);
        }

        let child = Command::new(&self.binary)
            .arg("--datadir")
            .arg(&datadir)
            .args(["--slice", "1"])
            .arg("--load-settings")
            .arg(join_paths(&paths[..2]))
            .arg("--load-filaments")
            .arg(&paths[2])
            .arg("--outputdir")
            .arg(&output)
            .arg(model)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let finished = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| SliceError::Timeout(self.timeout))??;
        if !finished.status.success() {
            return Err(SliceError::Failed {
                status: finished.status.to_string(),
                output: last_lines(&finished.stdout, &finished.stderr),
            });
        }
        let gcode = output.join("plate_1.gcode");
        if !tokio::fs::try_exists(&gcode).await? {
            return Err(SliceError::NoOutput);
        }
        Ok(gcode)
    }
}

pub fn machine_name(nozzle: Nozzle) -> String {
    format!("Elegoo Centauri Carbon 2 {} nozzle", nozzle.as_str())
}

/// The process the upload form preselects: the bundled "Standard" one, whose layer height is
/// half the nozzle diameter.
pub fn default_process(nozzle: Nozzle) -> String {
    let layer = match nozzle {
        Nozzle::Mm02 => "0.10",
        Nozzle::Mm04 => "0.20",
        Nozzle::Mm06 => "0.30",
        Nozzle::Mm08 => "0.40",
    };
    format!("{layer}mm Standard @Elegoo CC2 {} nozzle", nozzle.as_str())
}

/// An ASCII STL of a cube with `size` mm sides, for `slice-selftest` and the real-slicer test.
pub fn cube_stl(size: f32) -> String {
    let v = |i: usize| {
        let bit = |b: usize| if i >> b & 1 == 1 { size } else { 0.0 };
        (bit(2), bit(1), bit(0))
    };
    let faces = [
        (0, 1, 3),
        (0, 3, 2),
        (4, 6, 7),
        (4, 7, 5),
        (0, 4, 5),
        (0, 5, 1),
        (2, 3, 7),
        (2, 7, 6),
        (0, 2, 6),
        (0, 6, 4),
        (1, 5, 7),
        (1, 7, 3),
    ];
    let mut stl = String::from("solid cube\n");
    for (a, b, c) in faces {
        stl.push_str(" facet normal 0 0 0\n  outer loop\n");
        for i in [a, b, c] {
            let (x, y, z) = v(i);
            stl.push_str(&format!("   vertex {x} {y} {z}\n"));
        }
        stl.push_str("  endloop\n endfacet\n");
    }
    stl + "endsolid cube\n"
}

/// `--load-settings` takes one argument with the files separated by `;`.
fn join_paths(paths: &[PathBuf]) -> std::ffi::OsString {
    let mut joined = std::ffi::OsString::new();
    for (i, path) in paths.iter().enumerate() {
        if i > 0 {
            joined.push(";");
        }
        joined.push(path);
    }
    joined
}

/// The CLI reports most refusals on stdout, crashes on stderr.
fn last_lines(stdout: &[u8], stderr: &[u8]) -> String {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    );
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(OUTPUT_LINES)..].join("\n")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "printhub-slicer-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, relative: &str, value: serde_json::Value) {
        let path = dir.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, value.to_string()).unwrap();
    }

    const MACHINE: &str = "Elegoo Centauri Carbon 2 0.4 nozzle";

    /// A miniature vendor tree shaped like the bundled one.
    fn vendor() -> PathBuf {
        let dir = scratch("vendor");
        write(
            &dir,
            "machine/fdm_machine_common.json",
            json!({"name": "fdm_machine_common", "type": "machine", "gcode_flavor": "marlin", "printable_height": "100"}),
        );
        write(
            &dir,
            "machine/ECC2/cc2.json",
            json!({"name": MACHINE, "type": "machine", "inherits": "fdm_machine_common", "from": "system", "instantiation": "true", "gcode_flavor": "klipper"}),
        );
        write(
            &dir,
            "process/fdm_process_common.json",
            json!({"name": "fdm_process_common", "type": "process", "wall_loops": "2", "sparse_infill_density": "15%"}),
        );
        write(
            &dir,
            "process/ECC2/standard.json",
            json!({"name": "0.20mm Standard @Elegoo CC2 0.4 nozzle", "type": "process", "inherits": "fdm_process_common", "instantiation": "true", "compatible_printers": [MACHINE]}),
        );
        write(
            &dir,
            "process/ECC2/fine.json",
            json!({"name": "0.12mm Fine @Elegoo CC2 0.4 nozzle", "type": "process", "inherits": "0.20mm Standard @Elegoo CC2 0.4 nozzle", "instantiation": "true"}),
        );
        write(
            &dir,
            "process/ECC2/common.json",
            json!({"name": "fdm_process_cc2_common", "type": "process", "instantiation": "false", "compatible_printers": [MACHINE]}),
        );
        write(
            &dir,
            "process/ECC2/orphan.json",
            json!({"name": "0.20mm Orphan @Elegoo CC2 0.4 nozzle", "type": "process", "inherits": "missing", "instantiation": "true", "compatible_printers": [MACHINE]}),
        );
        write(
            &dir,
            "process/ECC2/other.json",
            json!({"name": "0.20mm Standard @Other", "type": "process", "instantiation": "true", "compatible_printers": ["Other printer"]}),
        );
        write(
            &dir,
            "filament/fdm_filament_pla.json",
            json!({"name": "fdm_filament_pla", "type": "filament", "filament_density": ["1.24"], "filament_type": ["PLA"]}),
        );
        write(
            &dir,
            "filament/BASE/base.json",
            json!({"name": "Elegoo PLA @base", "type": "filament", "inherits": "fdm_filament_pla", "filament_density": ["1.25"]}),
        );
        write(
            &dir,
            "filament/ECC2/pla.json",
            json!({"name": "Elegoo PLA @ECC2", "type": "filament", "inherits": "Elegoo PLA @base", "instantiation": "true", "compatible_printers": [MACHINE]}),
        );
        write(
            &dir,
            "filament/loop.json",
            json!({"name": "loop", "type": "filament", "inherits": "loop"}),
        );
        std::fs::write(dir.join("filament/readme.txt"), "not a profile").unwrap();
        dir
    }

    #[test]
    fn nearer_profiles_win_and_inherits_is_dropped() {
        let library = ProfileLibrary::load(&vendor()).unwrap();
        let pla = library.flatten("Elegoo PLA @ECC2").unwrap();
        assert_eq!(pla["filament_density"], json!(["1.25"]));
        assert_eq!(pla["filament_type"], json!(["PLA"]));
        assert_eq!(pla["name"], "Elegoo PLA @ECC2");
        assert!(!pla.contains_key("inherits"));

        assert!(matches!(
            library.flatten("loop"),
            Err(SliceError::InheritanceLoop(_))
        ));
        assert!(matches!(
            library.flatten("nope"),
            Err(SliceError::UnknownProfile(_))
        ));
    }

    #[test]
    fn only_compatible_selectable_profiles_are_listed() {
        let library = ProfileLibrary::load(&vendor()).unwrap();
        assert_eq!(
            library.compatible("process", MACHINE),
            [
                "0.12mm Fine @Elegoo CC2 0.4 nozzle",
                "0.20mm Standard @Elegoo CC2 0.4 nozzle"
            ],
            "inherited compatibility counts; parents and unresolvable profiles are left out"
        );
        assert_eq!(
            library.compatible("filament", MACHINE),
            ["Elegoo PLA @ECC2"]
        );
    }

    fn settings() -> SliceSettings {
        SliceSettings {
            nozzle: Nozzle::Mm04,
            plate: Plate::HighTemp,
            process: "0.20mm Standard @Elegoo CC2 0.4 nozzle".into(),
            filament: "Elegoo PLA @ECC2".into(),
            color_hex: "#2850DF".into(),
            supports: true,
            infill_percent: 25,
        }
    }

    /// A stand-in CLI: records its arguments and writes `plate_1.gcode` into `--outputdir`.
    #[cfg(unix)]
    fn stub(body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("stub");
        let path = dir.join("orca-slicer");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/args\"\n\
                 while [ $# -gt 0 ]; do [ \"$1\" = --outputdir ] && out=$2; shift; done\n{body}\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn slice_writes_flattened_system_profiles() {
        let binary = stub("echo '; generated by stub' > \"$out/plate_1.gcode\"");
        let slicer = Slicer::new(binary.clone(), &vendor(), Duration::from_secs(10)).unwrap();
        let work = scratch("work");
        let model = work.join("model.stl");
        std::fs::write(&model, "solid x\nendsolid x\n").unwrap();

        let gcode = slicer.slice(&model, &work, &settings()).await.unwrap();
        assert_eq!(gcode, work.join("out/plate_1.gcode"));

        let args = std::fs::read_to_string(binary.with_file_name("args")).unwrap();
        let args: Vec<&str> = args.lines().collect();
        let settings_arg = args[args.iter().position(|a| *a == "--load-settings").unwrap() + 1];
        assert_eq!(settings_arg.split(';').count(), 2);
        assert_eq!(*args.last().unwrap(), model.to_str().unwrap());

        let read = |name: &str| -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(work.join("profiles").join(name)).unwrap())
                .unwrap()
        };
        let (machine, process, filament) = (
            read("machine.json"),
            read("process.json"),
            read("filament.json"),
        );
        assert_eq!(machine["from"], "system");
        assert_eq!(machine["gcode_flavor"], "klipper");
        assert_eq!(machine["printable_height"], "100");
        assert_eq!(process["curr_bed_type"], "High Temp Plate");
        assert_eq!(process["enable_support"], "1");
        assert_eq!(process["sparse_infill_density"], "25%");
        assert_eq!(process["wall_loops"], "2");
        assert_eq!(filament["filament_colour"], json!(["#2850DF"]));
        assert_eq!(filament["filament_density"], json!(["1.25"]));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failures_and_timeouts_are_reported() {
        let vendor = vendor();
        let work = scratch("fail");
        let model = work.join("model.stl");
        std::fs::write(&model, "solid x\n").unwrap();

        let failing = Slicer::new(
            stub("echo 'process not compatible with printer.'; exit 3"),
            &vendor,
            Duration::from_secs(10),
        )
        .unwrap();
        match failing.slice(&model, &work, &settings()).await {
            Err(SliceError::Failed { output, .. }) => {
                assert!(output.contains("not compatible"), "{output}")
            }
            other => panic!("expected a failure, got {other:?}"),
        }

        let silent = Slicer::new(stub("exit 0"), &vendor, Duration::from_secs(10)).unwrap();
        assert!(matches!(
            silent
                .slice(&model, &work.join("silent"), &settings())
                .await,
            Err(SliceError::NoOutput)
        ));

        let slow = Slicer::new(stub("sleep 5"), &vendor, Duration::from_millis(200)).unwrap();
        assert!(matches!(
            slow.slice(&model, &work.join("slow"), &settings()).await,
            Err(SliceError::Timeout(_))
        ));
    }

    #[tokio::test]
    async fn a_nozzle_without_a_machine_profile_is_an_error() {
        let slicer = Slicer::new("orca".into(), &vendor(), Duration::from_secs(1)).unwrap();
        assert!(slicer.processes(Nozzle::Mm06).is_empty());
        let work = scratch("missing");
        let result = slicer
            .slice(
                &work.join("model.stl"),
                &work,
                &SliceSettings {
                    nozzle: Nozzle::Mm06,
                    ..settings()
                },
            )
            .await;
        assert!(matches!(result, Err(SliceError::UnknownProfile(name)) if name.contains("0.6")));
    }

    /// Against a real OrcaSlicer, when `ORCA_SLICER` (the binary) and `ORCA_PROFILES` (the
    /// `resources/profiles/Elegoo` directory) are set; skipped otherwise.
    #[tokio::test]
    async fn real_slicer_when_available() {
        let (Ok(binary), Ok(profiles)) =
            (std::env::var("ORCA_SLICER"), std::env::var("ORCA_PROFILES"))
        else {
            eprintln!("ORCA_SLICER and ORCA_PROFILES not set; skipping");
            return;
        };
        let slicer = Slicer::new(
            binary.into(),
            Path::new(&profiles),
            Duration::from_secs(300),
        )
        .unwrap();
        let processes = slicer.processes(Nozzle::Mm04);
        for process in [
            "0.20mm Standard @Elegoo CC2 0.4 nozzle",
            "0.12mm Fine @Elegoo CC2 0.4 nozzle",
        ] {
            assert!(processes.iter().any(|p| p == process), "{processes:?}");
        }
        let work = scratch("real");
        let model = work.join("cube.stl");
        std::fs::write(&model, cube_stl(20.0)).unwrap();
        let slice = |dir: &str, settings: SliceSettings| {
            let (slicer, model, workdir) = (&slicer, &model, work.join(dir));
            async move {
                let gcode = slicer.slice(model, &workdir, &settings).await.unwrap();
                let text = std::fs::read_to_string(&gcode).unwrap();
                let bed = text
                    .lines()
                    .find_map(|line| line.strip_prefix("M190 S"))
                    .map(|rest| rest.split_whitespace().next().unwrap().to_owned());
                (crate::gcode::read(&gcode).await.unwrap(), bed)
            }
        };

        let (info, bed) = slice(
            "pla",
            SliceSettings {
                plate: Plate::TexturedPei,
                filament: "Elegoo PLA @ECC2".into(),
                color_hex: "#2850DF".into(),
                supports: false,
                infill_percent: 15,
                ..settings()
            },
        )
        .await;
        assert_eq!(info.printer_model, "Elegoo Centauri Carbon 2");
        assert_eq!(info.tools.len(), 1);
        assert_eq!(info.tools[0].color, "#2850DF");
        assert!(
            info.tools[0].grams > 1.0,
            "density came through the flattened chain"
        );
        assert_eq!(info.plate, "Textured PEI Plate");
        assert_eq!(bed.as_deref(), Some("60"));

        // Elegoo's PET-CF profile heats the two plates differently.
        for (plate, temperature) in [(Plate::TexturedPei, "100"), (Plate::HighTemp, "70")] {
            let (info, bed) = slice(
                plate.as_str(),
                SliceSettings {
                    plate,
                    process: "0.12mm Fine @Elegoo CC2 0.4 nozzle".into(),
                    filament: "Elegoo PET-CF @ECC2".into(),
                    supports: false,
                    ..settings()
                },
            )
            .await;
            assert_eq!(info.plate, plate.as_str());
            assert_eq!(bed.as_deref(), Some(temperature), "{plate:?}");
            assert_eq!(info.layers, Some(166), "0.12 mm layers over 20 mm");
        }
    }
}
