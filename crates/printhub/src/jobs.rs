//! Print jobs: their states, the queue, and whether the next one may start. Like `accounts`,
//! functions take timestamps rather than reading the clock.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use jiff::{SignedDuration, Timestamp, tz::TimeZone};
use thiserror::Error;

use crate::{
    cc2::{
        LinkState, PrinterSnapshot,
        methods::SlotMapEntry,
        model::{MachineState, Tray},
    },
    config::Nozzle,
    gcode::GcodeInfo,
    inventory::{self, Binding, ConsumptionKind, InventoryError},
    schedule::{self, Rule, ScheduleBlock},
    store::Db,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Slicing,
    AwaitingConfirm,
    Queued,
    Uploading,
    Printing,
    Done,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Slicing => "slicing",
            Self::AwaitingConfirm => "awaiting_confirm",
            Self::Queued => "queued",
            Self::Uploading => "uploading",
            Self::Printing => "printing",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "slicing" => Self::Slicing,
            "awaiting_confirm" => Self::AwaitingConfirm,
            "queued" => Self::Queued,
            "uploading" => Self::Uploading,
            "printing" => Self::Printing,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Slicing => "Slicing",
            Self::AwaitingConfirm => "Waiting for confirmation",
            Self::Queued => "Queued",
            Self::Uploading => "Sending to the printer",
            Self::Printing => "Printing",
            Self::Done => "Done",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }

    pub fn can_become(self, next: Self) -> bool {
        use JobState::*;
        matches!(
            (self, next),
            (Slicing, AwaitingConfirm | Failed | Cancelled)
                | (AwaitingConfirm, Queued | Cancelled)
                | (Queued, Uploading | Cancelled)
                | (Uploading, Printing | Queued | Failed)
                | (Printing, Done | Failed | Cancelled)
                | (Failed, Queued)
        )
    }

    pub fn is_finished(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Stl,
    Gcode,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stl => "stl",
            Self::Gcode => "gcode",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: i64,
    pub owner: Option<(i64, String)>,
    pub name: String,
    pub source: Source,
    pub state: JobState,
    pub position: i64,
    pub process_profile: Option<String>,
    pub filament_profile: Option<String>,
    pub supports: Option<bool>,
    pub infill_percent: Option<i64>,
    pub estimated_seconds: Option<i64>,
    pub layers: Option<i64>,
    pub printer_task_uuid: Option<String>,
    pub progress: i64,
    pub error: String,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    /// The nozzle diameter the G-code was sliced for, when it names one.
    pub nozzle_mm: Option<f64>,
    /// The build plate type the G-code was sliced for, when it names one.
    pub plate: Option<String>,
}

impl Job {
    pub fn owner_id(&self) -> Option<i64> {
        self.owner.as_ref().map(|(id, _)| *id)
    }

    /// Plain ASCII: the upload sends it in an HTTP header.
    pub fn printer_filename(&self) -> String {
        format!("printhub-{}.gcode", self.id)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobTool {
    pub tool_index: u32,
    pub material: String,
    pub color_hex: String,
    pub grams: f64,
    pub spool_id: Option<i64>,
    pub canvas_id: Option<i64>,
    pub tray_id: Option<i64>,
}

#[derive(Debug, Error)]
pub enum JobError {
    #[error("no such job")]
    NotFound,
    #[error("a job cannot go from {} to {}", from.label(), to.label())]
    IllegalTransition { from: JobState, to: JobState },
    #[error("the job is no longer {}", expected.label())]
    StateChanged { expected: JobState },
    #[error("{0}")]
    Inventory(#[from] InventoryError),
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
}

/// Where a job's files live: `<DATA_DIR>/jobs/<id>/`.
pub fn dir(data_dir: &Path, id: i64) -> PathBuf {
    data_dir.join("jobs").join(id.to_string())
}

pub fn model_path(data_dir: &Path, id: i64) -> PathBuf {
    dir(data_dir, id).join("model.stl")
}

pub fn gcode_path(data_dir: &Path, id: i64) -> PathBuf {
    dir(data_dir, id).join("job.gcode")
}

pub struct NewJob<'a> {
    pub owner_id: i64,
    pub name: &'a str,
    pub source: Source,
    pub process_profile: Option<&'a str>,
    pub filament_profile: Option<&'a str>,
    pub supports: Option<bool>,
    pub infill_percent: Option<u8>,
}

/// A new job starts slicing (STL) or waiting for confirmation (G-code).
pub async fn create(db: &Db, job: &NewJob<'_>, now: i64) -> Result<i64, JobError> {
    let state = match job.source {
        Source::Stl => JobState::Slicing,
        Source::Gcode => JobState::AwaitingConfirm,
    }
    .as_str();
    let source = job.source.as_str();
    let supports = job.supports.map(i64::from);
    let infill = job.infill_percent.map(i64::from);
    let inserted = sqlx::query!(
        "INSERT INTO jobs (owner_id, name, source, state, position, process_profile,
                           filament_profile, supports, infill_percent, created_at)
         VALUES (?, ?, ?, ?, (SELECT COALESCE(MAX(position), 0) + 1 FROM jobs), ?, ?, ?, ?, ?)",
        job.owner_id,
        job.name,
        source,
        state,
        job.process_profile,
        job.filament_profile,
        supports,
        infill,
        now,
    )
    .execute(db)
    .await?;
    Ok(inserted.last_insert_rowid())
}

/// The only way a job changes state. It applies only if the job is still in `from`, so two
/// tasks racing on one job cannot both win.
///
/// Going to `Queued` from anywhere but `Uploading` moves the job to the back of the queue; a
/// failed upload keeps its place. `error` replaces the job's error text when given.
pub async fn transition(
    db: impl sqlx::SqliteExecutor<'_>,
    id: i64,
    from: JobState,
    to: JobState,
    error: Option<&str>,
    now: i64,
) -> Result<(), JobError> {
    if !from.can_become(to) {
        return Err(JobError::IllegalTransition { from, to });
    }
    let (to_state, from_state) = (to.as_str(), from.as_str());
    let to_back = to == JobState::Queued && from != JobState::Uploading;
    let starts = to == JobState::Printing;
    let finishes = to.is_finished();
    let updated = sqlx::query!(
        "UPDATE jobs SET
             state = ?,
             error = COALESCE(?, error),
             position = CASE WHEN ? THEN (SELECT COALESCE(MAX(position), 0) + 1 FROM jobs)
                             ELSE position END,
             started_at = CASE WHEN ? THEN ? ELSE started_at END,
             finished_at = CASE WHEN ? THEN ? ELSE finished_at END
         WHERE id = ? AND state = ?",
        to_state,
        error,
        to_back,
        starts,
        now,
        finishes,
        now,
        id,
        from_state,
    )
    .execute(db)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(JobError::StateChanged { expected: from });
    }
    Ok(())
}

struct JobRow {
    id: i64,
    owner_id: Option<i64>,
    owner_name: Option<String>,
    name: String,
    source: String,
    state: String,
    position: i64,
    process_profile: Option<String>,
    filament_profile: Option<String>,
    supports: Option<i64>,
    infill_percent: Option<i64>,
    estimated_seconds: Option<i64>,
    layers: Option<i64>,
    printer_task_uuid: Option<String>,
    progress: i64,
    error: String,
    created_at: i64,
    started_at: Option<i64>,
    finished_at: Option<i64>,
    nozzle_mm: Option<f64>,
    plate: Option<String>,
}

impl From<JobRow> for Job {
    fn from(row: JobRow) -> Self {
        Self {
            id: row.id,
            owner: row.owner_id.zip(row.owner_name),
            name: row.name,
            source: if row.source == "stl" {
                Source::Stl
            } else {
                Source::Gcode
            },
            // The column's CHECK constraint admits only the states `parse` knows.
            state: JobState::parse(&row.state).unwrap_or(JobState::Failed),
            position: row.position,
            process_profile: row.process_profile,
            filament_profile: row.filament_profile,
            supports: row.supports.map(|s| s != 0),
            infill_percent: row.infill_percent,
            estimated_seconds: row.estimated_seconds,
            layers: row.layers,
            printer_task_uuid: row.printer_task_uuid,
            progress: row.progress,
            error: row.error,
            created_at: row.created_at,
            started_at: row.started_at,
            finished_at: row.finished_at,
            nozzle_mm: row.nozzle_mm,
            plate: row.plate,
        }
    }
}

pub async fn get(db: &Db, id: i64) -> Result<Option<Job>, JobError> {
    let row = sqlx::query_as!(
        JobRow,
        r#"SELECT j.id AS "id!", j.owner_id, u.username AS "owner_name?", j.name, j.source,
                  j.state, j.position, j.process_profile, j.filament_profile, j.supports,
                  j.infill_percent, j.estimated_seconds, j.layers, j.printer_task_uuid,
                  j.progress, j.error, j.created_at, j.started_at, j.finished_at, j.nozzle_mm,
                  j.plate
           FROM jobs j LEFT JOIN users u ON u.id = j.owner_id
           WHERE j.id = ?"#,
        id,
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map(Job::from))
}

/// Every unfinished job in queue order, then the `finished` most recently finished ones.
pub async fn list(db: &Db, finished: i64) -> Result<Vec<Job>, JobError> {
    let rows = sqlx::query_as!(
        JobRow,
        r#"SELECT j.id AS "id!", j.owner_id, u.username AS "owner_name?", j.name, j.source,
                  j.state, j.position, j.process_profile, j.filament_profile, j.supports,
                  j.infill_percent, j.estimated_seconds, j.layers, j.printer_task_uuid,
                  j.progress, j.error, j.created_at, j.started_at, j.finished_at, j.nozzle_mm,
                  j.plate
           FROM jobs j LEFT JOIN users u ON u.id = j.owner_id
           WHERE j.state NOT IN ('done', 'failed', 'cancelled')
              OR j.id IN (SELECT id FROM jobs WHERE state IN ('done', 'failed', 'cancelled')
                          ORDER BY finished_at DESC, id DESC LIMIT ?)
           ORDER BY CASE j.state WHEN 'printing' THEN 0 WHEN 'uploading' THEN 0
                                 WHEN 'queued' THEN 1 WHEN 'awaiting_confirm' THEN 2
                                 WHEN 'slicing' THEN 2 ELSE 3 END,
                    CASE WHEN j.state IN ('done', 'failed', 'cancelled')
                         THEN -j.finished_at ELSE j.position END,
                    j.id"#,
        finished,
    )
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(Job::from).collect())
}

/// Layers of the job printing now, for the printer card: the printer's own total is 0.
pub async fn printing_layers(db: &Db) -> Result<Option<i64>, JobError> {
    let row = sqlx::query!("SELECT layers FROM jobs WHERE state = 'printing' ORDER BY id LIMIT 1")
        .fetch_optional(db)
        .await?;
    Ok(row.and_then(|row| row.layers))
}

pub async fn in_state(db: &Db, state: JobState) -> Result<Vec<Job>, JobError> {
    let state = state.as_str();
    let rows = sqlx::query_as!(
        JobRow,
        r#"SELECT j.id AS "id!", j.owner_id, u.username AS "owner_name?", j.name, j.source,
                  j.state, j.position, j.process_profile, j.filament_profile, j.supports,
                  j.infill_percent, j.estimated_seconds, j.layers, j.printer_task_uuid,
                  j.progress, j.error, j.created_at, j.started_at, j.finished_at, j.nozzle_mm,
                  j.plate
           FROM jobs j LEFT JOIN users u ON u.id = j.owner_id
           WHERE j.state = ?
           ORDER BY j.position, j.id"#,
        state,
    )
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(Job::from).collect())
}

pub async fn tools(db: &Db, job_id: i64) -> Result<Vec<JobTool>, JobError> {
    let rows = sqlx::query!(
        "SELECT tool_index, material, color_hex, grams, spool_id, canvas_id, tray_id
         FROM job_tools WHERE job_id = ? ORDER BY tool_index",
        job_id,
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| JobTool {
            tool_index: u32::try_from(r.tool_index).unwrap_or(u32::MAX),
            material: r.material,
            color_hex: r.color_hex,
            grams: r.grams,
            spool_id: r.spool_id,
            canvas_id: r.canvas_id,
            tray_id: r.tray_id,
        })
        .collect())
}

/// Records what the G-code needs: the estimate, its nozzle and plate, and one row per tool that
/// uses filament. With `spool_id`, every tool is assigned that spool, as when the job was sliced
/// for it.
pub async fn store_gcode_info(
    db: &Db,
    job_id: i64,
    info: &GcodeInfo,
    spool_id: Option<i64>,
) -> Result<(), JobError> {
    let mut tx = db.begin().await?;
    let nozzle_mm = info.nozzle_mm();
    let plate = (!info.plate.is_empty()).then_some(info.plate.as_str());
    sqlx::query!(
        "UPDATE jobs SET estimated_seconds = ?, layers = ?, nozzle_mm = ?, plate = ?
         WHERE id = ?",
        info.estimated_seconds,
        info.layers,
        nozzle_mm,
        plate,
        job_id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!("DELETE FROM job_tools WHERE job_id = ?", job_id)
        .execute(&mut *tx)
        .await?;
    for tool in info.used_tools() {
        sqlx::query!(
            "INSERT INTO job_tools (job_id, tool_index, material, color_hex, grams, spool_id)
             VALUES (?, ?, ?, ?, ?, ?)",
            job_id,
            tool.index,
            tool.material,
            tool.color,
            tool.grams,
            spool_id,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Assigns spools to tools, as `(tool_index, spool_id)`.
pub async fn choose_spools(db: &Db, job_id: i64, choices: &[(u32, i64)]) -> Result<(), JobError> {
    let mut tx = db.begin().await?;
    for (tool, spool) in choices {
        sqlx::query!(
            "UPDATE job_tools SET spool_id = ? WHERE job_id = ? AND tool_index = ?",
            spool,
            job_id,
            tool,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Records which tray each tool printed from, and the printer's task id once known.
pub async fn record_start(
    db: &Db,
    job_id: i64,
    slot_map: &[SlotMapEntry],
    task_uuid: Option<&str>,
) -> Result<(), JobError> {
    let mut tx = db.begin().await?;
    for slot in slot_map {
        sqlx::query!(
            "UPDATE job_tools SET canvas_id = ?, tray_id = ? WHERE job_id = ? AND tool_index = ?",
            slot.canvas_id,
            slot.tray_id,
            job_id,
            slot.t,
        )
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query!(
        "UPDATE jobs SET printer_task_uuid = COALESCE(?, printer_task_uuid), progress = 0
         WHERE id = ?",
        task_uuid,
        job_id,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn set_progress(db: &Db, job_id: i64, progress: i64) -> Result<(), JobError> {
    let progress = progress.clamp(0, 100);
    sqlx::query!(
        "UPDATE jobs SET progress = ? WHERE id = ?",
        progress,
        job_id
    )
    .execute(db)
    .await?;
    Ok(())
}

/// Swaps a queued job with its neighbour. Returns whether it moved.
pub async fn move_queued(db: &Db, job_id: i64, earlier: bool) -> Result<bool, JobError> {
    let mut tx = db.begin().await?;
    let Some(job) = sqlx::query!(
        "SELECT position FROM jobs WHERE id = ? AND state = 'queued'",
        job_id
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(false);
    };
    let neighbour = if earlier {
        sqlx::query!(
            r#"SELECT id AS "id!", position FROM jobs
               WHERE state = 'queued' AND position < ? ORDER BY position DESC LIMIT 1"#,
            job.position,
        )
        .fetch_optional(&mut *tx)
        .await?
        .map(|r| (r.id, r.position))
    } else {
        sqlx::query!(
            r#"SELECT id AS "id!", position FROM jobs
               WHERE state = 'queued' AND position > ? ORDER BY position LIMIT 1"#,
            job.position,
        )
        .fetch_optional(&mut *tx)
        .await?
        .map(|r| (r.id, r.position))
    };
    let Some((other, other_position)) = neighbour else {
        return Ok(false);
    };
    sqlx::query!(
        "UPDATE jobs SET position = ? WHERE id = ?",
        other_position,
        job_id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE jobs SET position = ? WHERE id = ?",
        job.position,
        other
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Ends a printing job and deducts its filament: all of it when done, the printed share
/// otherwise (as an estimate).
pub async fn finish(
    db: &Db,
    job: &Job,
    outcome: JobState,
    progress: i64,
    error: Option<&str>,
    now: i64,
) -> Result<(), JobError> {
    let tools = tools(db, job.id).await?;
    let mut tx = db.begin().await?;
    transition(&mut *tx, job.id, JobState::Printing, outcome, error, now).await?;
    let (kind, share) = match outcome {
        JobState::Done => (ConsumptionKind::Print, 1.0),
        _ => (
            ConsumptionKind::Estimate,
            progress.clamp(0, 100) as f64 / 100.0,
        ),
    };
    for tool in &tools {
        let Some(spool_id) = tool.spool_id else {
            continue;
        };
        let grams = tool.grams * share;
        if grams <= 0.0 {
            continue;
        }
        inventory::record_use(
            &mut tx,
            spool_id,
            job.owner_id(),
            job.id,
            kind,
            grams,
            &job.name,
            now,
        )
        .await?;
    }
    sqlx::query!(
        "UPDATE jobs SET progress = ? WHERE id = ?",
        progress,
        job.id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn bed_clear(db: &Db) -> Result<bool, JobError> {
    let row = sqlx::query!("SELECT bed_clear FROM printer_state WHERE id = 1")
        .fetch_one(db)
        .await?;
    Ok(row.bed_clear != 0)
}

pub async fn set_bed_clear(
    db: impl sqlx::SqliteExecutor<'_>,
    clear: bool,
    user_id: Option<i64>,
    now: i64,
) -> Result<(), JobError> {
    let clear = i64::from(clear);
    sqlx::query!(
        "UPDATE printer_state SET bed_clear = ?, bed_changed_by = ?, bed_changed_at = ? WHERE id = 1",
        clear,
        user_id,
        now,
    )
    .execute(db)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct MountedNozzle {
    pub nozzle: Nozzle,
    /// `None` while nobody has recorded one and `PRINTER_NOZZLE` is assumed.
    pub recorded: Option<NozzleRecord>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NozzleRecord {
    /// `None` once that account is deleted.
    pub by: Option<String>,
    pub at: i64,
}

pub async fn mounted_nozzle(db: &Db, assumed: Nozzle) -> Result<MountedNozzle, JobError> {
    let row = sqlx::query!(
        r#"SELECT p.nozzle, p.nozzle_changed_at, u.username AS "username?"
           FROM printer_state p LEFT JOIN users u ON u.id = p.nozzle_changed_by
           WHERE p.id = 1"#
    )
    .fetch_one(db)
    .await?;
    // The column's CHECK constraint admits only the diameters `Nozzle::parse` knows.
    let recorded = row.nozzle.as_deref().and_then(Nozzle::parse);
    Ok(match (recorded, row.nozzle_changed_at) {
        (Some(nozzle), Some(at)) => MountedNozzle {
            nozzle,
            recorded: Some(NozzleRecord {
                by: row.username,
                at,
            }),
        },
        _ => MountedNozzle {
            nozzle: assumed,
            recorded: None,
        },
    })
}

pub async fn set_mounted_nozzle(
    db: &Db,
    nozzle: Nozzle,
    user_id: i64,
    now: i64,
) -> Result<(), JobError> {
    let nozzle = nozzle.as_str();
    sqlx::query!(
        "UPDATE printer_state SET nozzle = ?, nozzle_changed_by = ?, nozzle_changed_at = ?
         WHERE id = 1",
        nozzle,
        user_id,
        now,
    )
    .execute(db)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub enum BlockReason {
    PrinterOffline,
    AnotherJobActive,
    PrinterBusy,
    BedNotClear,
    SpoolNotChosen {
        tool: u32,
    },
    SpoolNotLoaded {
        tool: u32,
    },
    TrayEmpty {
        tool: u32,
        canvas_id: i64,
        tray_id: i64,
    },
    NotEnoughFilament {
        tool: u32,
        needed: f64,
        left: f64,
    },
    NozzleMismatch {
        sliced_mm: f64,
        mounted: Nozzle,
    },
    Schedule(ScheduleBlock),
}

impl BlockReason {
    pub fn describe(&self, tz: &TimeZone) -> String {
        let time = |ts: &Timestamp| ts.to_zoned(tz.clone()).strftime("%a %H:%M").to_string();
        match self {
            Self::PrinterOffline => "The printer is not connected.".into(),
            Self::AnotherJobActive => "Another job is printing.".into(),
            Self::PrinterBusy => "The printer is busy.".into(),
            Self::BedNotClear => "Nobody has confirmed that the bed is clear.".into(),
            Self::NozzleMismatch { sliced_mm, mounted } => format!(
                "It was sliced for a {sliced_mm} mm nozzle, but a {} mm nozzle is mounted.",
                mounted.as_str()
            ),
            Self::SpoolNotChosen { tool } => {
                format!("No spool is chosen for filament {}.", tool + 1)
            }
            Self::SpoolNotLoaded { tool } => {
                format!("The spool for filament {} is not in a tray.", tool + 1)
            }
            Self::TrayEmpty {
                tool,
                canvas_id,
                tray_id,
            } => format!(
                "Tray {} for filament {} is empty.",
                inventory::tray_label(*canvas_id, *tray_id),
                tool + 1
            ),
            Self::NotEnoughFilament { tool, needed, left } => format!(
                "Filament {} needs {needed:.0} g, but its spool has {left:.0} g left.",
                tool + 1
            ),
            Self::Schedule(ScheduleBlock::DenyActive { label, until }) => {
                format!("No printing during “{label}”, until {}.", time(until))
            }
            Self::Schedule(ScheduleBlock::OutsideAllowed { next: Some(next) }) => {
                format!(
                    "Outside the print windows; the next one opens {}.",
                    time(next)
                )
            }
            Self::Schedule(ScheduleBlock::OutsideAllowed { next: None }) => {
                "Outside the print windows.".into()
            }
            Self::Schedule(ScheduleBlock::WouldOverrun { label, deadline }) => {
                format!("It would not finish before {} (“{label}”).", time(deadline))
            }
            Self::Schedule(ScheduleBlock::NoEstimate { label }) => {
                format!("“{label}” needs a time estimate, and this job has none.")
            }
        }
    }
}

pub struct StartContext<'a> {
    pub snapshot: &'a PrinterSnapshot,
    pub bindings: &'a [Binding],
    pub bed_clear: bool,
    pub nozzle: Nozzle,
    /// A job is already uploading or printing.
    pub other_job_active: bool,
    pub rules: &'a [Rule],
    pub now: Timestamp,
    pub tz: &'a TimeZone,
    pub estimate_margin: f64,
}

/// The first reason `job` cannot start now, or the tray for each tool if it can.
pub fn check_start(
    job: &Job,
    tools: &[JobTool],
    ctx: &StartContext<'_>,
) -> Result<Vec<SlotMapEntry>, BlockReason> {
    let status = match (&ctx.snapshot.link, &ctx.snapshot.status) {
        (LinkState::Registered, Some(status)) => status,
        _ => return Err(BlockReason::PrinterOffline),
    };
    if ctx.other_job_active {
        return Err(BlockReason::AnotherJobActive);
    }
    if status.machine_status.state() != MachineState::Idle {
        return Err(BlockReason::PrinterBusy);
    }
    // Before the bed: clearing the bed does not help a job that needs another nozzle.
    if let Some(sliced_mm) = job.nozzle_mm
        && sliced_mm != ctx.nozzle.millimetres()
    {
        return Err(BlockReason::NozzleMismatch {
            sliced_mm,
            mounted: ctx.nozzle,
        });
    }
    if !ctx.bed_clear {
        return Err(BlockReason::BedNotClear);
    }

    let mut slot_map = Vec::new();
    let mut needed: HashMap<i64, f64> = HashMap::new();
    for tool in tools.iter().filter(|tool| tool.grams > 0.0) {
        let spool_id = tool.spool_id.ok_or(BlockReason::SpoolNotChosen {
            tool: tool.tool_index,
        })?;
        let not_loaded = BlockReason::SpoolNotLoaded {
            tool: tool.tool_index,
        };
        let binding = ctx
            .bindings
            .iter()
            .find(|binding| binding.spool.id == spool_id)
            .ok_or_else(|| not_loaded.clone())?;
        // The printer silently prints from tray 0 when given a tray it does not have.
        let (Ok(canvas_id), Ok(tray_id)) = (
            u32::try_from(binding.canvas_id),
            u32::try_from(binding.tray_id),
        ) else {
            return Err(not_loaded);
        };
        let loaded = ctx
            .snapshot
            .canvas
            .as_ref()
            .and_then(|canvas| inventory::find_tray(canvas, binding.canvas_id, binding.tray_id))
            .is_some_and(Tray::has_filament);
        if !loaded {
            return Err(BlockReason::TrayEmpty {
                tool: tool.tool_index,
                canvas_id: binding.canvas_id,
                tray_id: binding.tray_id,
            });
        }
        let total = needed.entry(spool_id).or_default();
        *total += tool.grams;
        if *total > binding.spool.remaining_grams {
            return Err(BlockReason::NotEnoughFilament {
                tool: tool.tool_index,
                needed: *total,
                left: binding.spool.remaining_grams,
            });
        }
        slot_map.push(SlotMapEntry {
            t: tool.tool_index,
            canvas_id,
            tray_id,
        });
    }

    let estimated_end = job.estimated_seconds.and_then(|seconds| {
        let seconds = (seconds as f64 * ctx.estimate_margin).round() as i64;
        ctx.now.checked_add(SignedDuration::from_secs(seconds)).ok()
    });
    schedule::check(ctx.rules, ctx.now, estimated_end, ctx.tz).map_err(BlockReason::Schedule)?;
    Ok(slot_map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        accounts::{self, Role},
        cc2::model::{Canvas, CanvasInfo, MachineStatus, StatusView},
        gcode::ToolUse,
        inventory::{Spool, SpoolFields},
        schedule::RuleKind,
        store,
    };

    async fn db_with_user() -> (Db, i64) {
        let db = store::open_in_memory().await.unwrap();
        let user = accounts::create_user(&db, "sam", "h", Role::Member, 1)
            .await
            .unwrap();
        (db, user)
    }

    fn gcode_job(owner_id: i64) -> NewJob<'static> {
        NewJob {
            owner_id,
            name: "cube.gcode",
            source: Source::Gcode,
            process_profile: None,
            filament_profile: None,
            supports: None,
            infill_percent: None,
        }
    }

    async fn queued_job(db: &Db, owner: i64) -> i64 {
        let id = create(db, &gcode_job(owner), 10).await.unwrap();
        transition(
            db,
            id,
            JobState::AwaitingConfirm,
            JobState::Queued,
            None,
            11,
        )
        .await
        .unwrap();
        id
    }

    #[test]
    fn state_names_round_trip() {
        for state in [
            JobState::Slicing,
            JobState::AwaitingConfirm,
            JobState::Queued,
            JobState::Uploading,
            JobState::Printing,
            JobState::Done,
            JobState::Failed,
            JobState::Cancelled,
        ] {
            assert_eq!(JobState::parse(state.as_str()), Some(state));
            assert!(
                !state.can_become(JobState::Slicing),
                "nothing goes back to slicing"
            );
        }
        assert!(!JobState::Done.can_become(JobState::Queued));
        assert!(JobState::Failed.can_become(JobState::Queued));
    }

    #[tokio::test]
    async fn transitions_apply_only_from_the_expected_state() {
        let (db, sam) = db_with_user().await;
        let id = queued_job(&db, sam).await;

        assert!(matches!(
            transition(&db, id, JobState::Queued, JobState::Done, None, 12).await,
            Err(JobError::IllegalTransition { .. })
        ));
        transition(&db, id, JobState::Queued, JobState::Uploading, None, 12)
            .await
            .unwrap();
        assert!(matches!(
            transition(&db, id, JobState::Queued, JobState::Cancelled, None, 13).await,
            Err(JobError::StateChanged { .. })
        ));
        transition(&db, id, JobState::Uploading, JobState::Printing, None, 14)
            .await
            .unwrap();
        let job = get(&db, id).await.unwrap().unwrap();
        assert_eq!((job.state, job.started_at), (JobState::Printing, Some(14)));
    }

    #[tokio::test]
    async fn queue_order_moves_and_requeues_go_to_the_back() {
        let (db, sam) = db_with_user().await;
        let a = queued_job(&db, sam).await;
        let b = queued_job(&db, sam).await;
        let c = queued_job(&db, sam).await;
        let order = |jobs: Vec<Job>| jobs.iter().map(|j| j.id).collect::<Vec<_>>();
        assert_eq!(
            order(in_state(&db, JobState::Queued).await.unwrap()),
            [a, b, c]
        );

        assert!(move_queued(&db, c, true).await.unwrap());
        assert!(!move_queued(&db, a, true).await.unwrap(), "already first");
        assert_eq!(
            order(in_state(&db, JobState::Queued).await.unwrap()),
            [a, c, b]
        );

        // A failed upload returns to its place; a retried failure goes to the back.
        transition(&db, a, JobState::Queued, JobState::Uploading, None, 20)
            .await
            .unwrap();
        transition(
            &db,
            a,
            JobState::Uploading,
            JobState::Queued,
            Some("busy"),
            21,
        )
        .await
        .unwrap();
        assert_eq!(
            order(in_state(&db, JobState::Queued).await.unwrap()),
            [a, c, b]
        );
        transition(&db, a, JobState::Queued, JobState::Uploading, None, 22)
            .await
            .unwrap();
        transition(
            &db,
            a,
            JobState::Uploading,
            JobState::Failed,
            Some("refused"),
            23,
        )
        .await
        .unwrap();
        transition(&db, a, JobState::Failed, JobState::Queued, Some(""), 24)
            .await
            .unwrap();
        assert_eq!(
            order(in_state(&db, JobState::Queued).await.unwrap()),
            [c, b, a]
        );
    }

    async fn spool(db: &Db, grams: f64) -> i64 {
        inventory::create_spool(
            db,
            &SpoolFields {
                material: "PLA".into(),
                brand: String::new(),
                color_name: String::new(),
                color_hex: "#000000".into(),
                price_cents: None,
                initial_grams: grams,
                notes: String::new(),
            },
            None,
            1,
        )
        .await
        .unwrap()
    }

    fn info(grams: &[f64]) -> GcodeInfo {
        GcodeInfo {
            generator: String::new(),
            printer_model: String::new(),
            nozzle: String::new(),
            plate: String::new(),
            estimated_seconds: Some(600),
            layers: Some(10),
            tools: grams
                .iter()
                .enumerate()
                .map(|(i, grams)| ToolUse {
                    index: i as u32,
                    material: "PLA".into(),
                    color: "#000000".into(),
                    grams: *grams,
                    profile: String::new(),
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn finishing_deducts_filament() {
        let (db, sam) = db_with_user().await;
        let red = spool(&db, 1000.0).await;
        let blue = spool(&db, 1000.0).await;

        let done = queued_job(&db, sam).await;
        store_gcode_info(&db, done, &info(&[0.0, 30.0, 10.0]), None)
            .await
            .unwrap();
        assert_eq!(
            tools(&db, done).await.unwrap().len(),
            2,
            "unused tools are dropped"
        );
        choose_spools(&db, done, &[(1, red), (2, blue)])
            .await
            .unwrap();
        let mut priced = SpoolFields {
            material: "PLA".into(),
            brand: String::new(),
            color_name: String::new(),
            color_hex: "#000000".into(),
            price_cents: Some(2000),
            initial_grams: 1000.0,
            notes: String::new(),
        };
        inventory::update_spool(&db, blue, &priced, Some(sam))
            .await
            .unwrap();
        transition(&db, done, JobState::Queued, JobState::Uploading, None, 2)
            .await
            .unwrap();
        transition(&db, done, JobState::Uploading, JobState::Printing, None, 3)
            .await
            .unwrap();
        let job = get(&db, done).await.unwrap().unwrap();
        finish(&db, &job, JobState::Done, 100, None, 4)
            .await
            .unwrap();

        let stopped = queued_job(&db, sam).await;
        store_gcode_info(&db, stopped, &info(&[50.0]), Some(red))
            .await
            .unwrap();
        transition(&db, stopped, JobState::Queued, JobState::Uploading, None, 5)
            .await
            .unwrap();
        transition(
            &db,
            stopped,
            JobState::Uploading,
            JobState::Printing,
            None,
            6,
        )
        .await
        .unwrap();
        let job = get(&db, stopped).await.unwrap().unwrap();
        finish(&db, &job, JobState::Cancelled, 40, None, 7)
            .await
            .unwrap();

        let left = |id| {
            let db = db.clone();
            async move {
                inventory::spool(&db, id)
                    .await
                    .unwrap()
                    .unwrap()
                    .remaining_grams
            }
        };
        assert!((left(red).await - (1000.0 - 30.0 - 20.0)).abs() < 1e-9);
        assert!((left(blue).await - 990.0).abs() < 1e-9);
        let history = inventory::history(&db, red).await.unwrap();
        assert_eq!(
            history.iter().map(|e| e.kind).collect::<Vec<_>>(),
            [ConsumptionKind::Estimate, ConsumptionKind::Print]
        );
        assert_eq!(history[0].note, "cube.gcode");

        priced.price_cents = Some(9900);
        inventory::update_spool(&db, blue, &priced, None)
            .await
            .unwrap();
        let blue_use = crate::stats::load(&db, 0)
            .await
            .unwrap()
            .uses
            .into_iter()
            .find(|entry| entry.grams == 10.0)
            .unwrap();
        assert_eq!(
            (blue_use.owner_id, blue_use.value_cents),
            (Some(sam), Some(20.0)),
            "the entry keeps the owner and price from when the print finished"
        );
        assert!(matches!(
            finish(&db, &job, JobState::Done, 100, None, 8).await,
            Err(JobError::StateChanged { .. })
        ));
    }

    #[tokio::test]
    async fn bed_starts_not_clear() {
        let (db, sam) = db_with_user().await;
        assert!(!bed_clear(&db).await.unwrap());
        set_bed_clear(&db, true, Some(sam), 5).await.unwrap();
        assert!(bed_clear(&db).await.unwrap());
    }

    fn snapshot(machine_status: i64, trays: &[(u32, i64)]) -> PrinterSnapshot {
        PrinterSnapshot {
            link: LinkState::Registered,
            status: Some(StatusView {
                machine_status: MachineStatus {
                    status: machine_status,
                    ..MachineStatus::default()
                },
                ..StatusView::default()
            }),
            canvas: Some(CanvasInfo {
                canvas_list: vec![Canvas {
                    canvas_id: 0,
                    connected: 1,
                    tray_list: trays
                        .iter()
                        .map(|(tray_id, status)| Tray {
                            tray_id: *tray_id,
                            status: *status,
                            ..Tray::default()
                        })
                        .collect(),
                }],
                ..CanvasInfo::default()
            }),
            attributes: None,
        }
    }

    fn binding(spool_id: i64, tray_id: i64, remaining: f64) -> Binding {
        Binding {
            canvas_id: 0,
            tray_id,
            spool: Spool {
                id: spool_id,
                material: "PLA".into(),
                brand: String::new(),
                color_name: String::new(),
                color_hex: "#000000".into(),
                owner: None,
                price_cents: None,
                initial_grams: 1000.0,
                remaining_grams: remaining,
                notes: String::new(),
                archived: false,
                created_at: 0,
            },
        }
    }

    fn job(estimated_seconds: Option<i64>) -> Job {
        Job {
            id: 1,
            owner: None,
            name: "cube.gcode".into(),
            source: Source::Gcode,
            state: JobState::Queued,
            position: 1,
            process_profile: None,
            filament_profile: None,
            supports: None,
            infill_percent: None,
            estimated_seconds,
            layers: None,
            printer_task_uuid: None,
            progress: 0,
            error: String::new(),
            created_at: 0,
            started_at: None,
            finished_at: None,
            nozzle_mm: None,
            plate: None,
        }
    }

    fn tool(index: u32, grams: f64, spool_id: Option<i64>) -> JobTool {
        JobTool {
            tool_index: index,
            material: "PLA".into(),
            color_hex: "#000000".into(),
            grams,
            spool_id,
            canvas_id: None,
            tray_id: None,
        }
    }

    #[test]
    fn start_checks_in_order() {
        let tz = TimeZone::UTC;
        let now: Timestamp = "2026-09-15T12:00:00Z".parse().unwrap();
        let idle = snapshot(1, &[(0, 1), (1, 1), (2, 0)]);
        let bindings = [
            binding(10, 0, 500.0),
            binding(11, 1, 20.0),
            binding(12, 2, 900.0),
        ];
        let quiet = [Rule {
            kind: RuleKind::Deny,
            days: 0b111_1111,
            start_minute: 13 * 60,
            end_minute: 14 * 60,
            must_finish_before: true,
            label: "lunch".into(),
        }];
        let ctx = |snapshot, bed_clear, other_job_active| StartContext {
            snapshot,
            bindings: &bindings,
            bed_clear,
            nozzle: Nozzle::Mm04,
            other_job_active,
            rules: &quiet,
            now,
            tz: &tz,
            estimate_margin: 1.5,
        };
        let check = |job: &Job, tools: &[JobTool], ctx: StartContext| check_start(job, tools, &ctx);
        let one_hour = job(Some(3600));
        let half_hour = job(Some(30 * 60));
        let ok_tools = [
            tool(0, 100.0, Some(10)),
            tool(1, 15.0, Some(11)),
            tool(2, 0.0, None),
        ];

        let offline = PrinterSnapshot {
            link: LinkState::Connecting,
            ..idle.clone()
        };
        let printing = snapshot(2, &[]);
        assert_eq!(
            check(&half_hour, &ok_tools, ctx(&offline, true, false)),
            Err(BlockReason::PrinterOffline)
        );
        assert_eq!(
            check(&half_hour, &ok_tools, ctx(&idle, true, true)),
            Err(BlockReason::AnotherJobActive)
        );
        assert_eq!(
            check(&half_hour, &ok_tools, ctx(&printing, true, false)),
            Err(BlockReason::PrinterBusy)
        );
        let for_other_nozzle = Job {
            nozzle_mm: Some(0.6),
            ..half_hour.clone()
        };
        assert_eq!(
            check(&for_other_nozzle, &ok_tools, ctx(&idle, false, false)),
            Err(BlockReason::NozzleMismatch {
                sliced_mm: 0.6,
                mounted: Nozzle::Mm04
            })
        );
        assert_eq!(
            check(&half_hour, &ok_tools, ctx(&idle, false, false)),
            Err(BlockReason::BedNotClear)
        );
        assert_eq!(
            check(&half_hour, &[tool(0, 5.0, None)], ctx(&idle, true, false)),
            Err(BlockReason::SpoolNotChosen { tool: 0 })
        );
        assert_eq!(
            check(
                &half_hour,
                &[tool(0, 5.0, Some(99))],
                ctx(&idle, true, false)
            ),
            Err(BlockReason::SpoolNotLoaded { tool: 0 })
        );
        assert_eq!(
            check(
                &half_hour,
                &[tool(3, 5.0, Some(12))],
                ctx(&idle, true, false)
            ),
            Err(BlockReason::TrayEmpty {
                tool: 3,
                canvas_id: 0,
                tray_id: 2
            })
        );
        assert_eq!(
            check(
                &half_hour,
                &[tool(0, 15.0, Some(11)), tool(1, 10.0, Some(11))],
                ctx(&idle, true, false)
            ),
            Err(BlockReason::NotEnoughFilament {
                tool: 1,
                needed: 25.0,
                left: 20.0
            }),
            "tools sharing a spool add up"
        );
        // 30 minutes × 1.5 ends 12:45, before lunch; an hour × 1.5 does not.
        assert_eq!(
            check(&half_hour, &ok_tools, ctx(&idle, true, false)),
            Ok(vec![
                SlotMapEntry {
                    t: 0,
                    canvas_id: 0,
                    tray_id: 0
                },
                SlotMapEntry {
                    t: 1,
                    canvas_id: 0,
                    tray_id: 1
                },
            ])
        );
        assert!(matches!(
            check(&one_hour, &ok_tools, ctx(&idle, true, false)),
            Err(BlockReason::Schedule(ScheduleBlock::WouldOverrun { .. }))
        ));
        assert_eq!(
            BlockReason::TrayEmpty {
                tool: 3,
                canvas_id: 0,
                tray_id: 2
            }
            .describe(&tz),
            "Tray A3 for filament 4 is empty."
        );
    }

    #[tokio::test]
    async fn the_nozzle_is_assumed_until_recorded_and_jobs_keep_theirs() {
        let (db, sam) = db_with_user().await;
        assert_eq!(
            mounted_nozzle(&db, Nozzle::Mm04).await.unwrap(),
            MountedNozzle {
                nozzle: Nozzle::Mm04,
                recorded: None
            }
        );
        set_mounted_nozzle(&db, Nozzle::Mm06, sam, 50)
            .await
            .unwrap();
        assert_eq!(
            mounted_nozzle(&db, Nozzle::Mm04).await.unwrap(),
            MountedNozzle {
                nozzle: Nozzle::Mm06,
                recorded: Some(NozzleRecord {
                    by: Some("sam".into()),
                    at: 50
                })
            }
        );

        let id = create(&db, &gcode_job(sam), 60).await.unwrap();
        let sliced = GcodeInfo {
            nozzle: "0.4".into(),
            plate: "High Temp Plate".into(),
            ..info(&[5.0])
        };
        store_gcode_info(&db, id, &sliced, None).await.unwrap();
        let job = get(&db, id).await.unwrap().unwrap();
        assert_eq!(job.nozzle_mm, Some(0.4));
        assert_eq!(job.plate.as_deref(), Some("High Temp Plate"));
    }
}
