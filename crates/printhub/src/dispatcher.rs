//! Moves jobs along: slices uploaded models, starts the first queued job that can run, and
//! follows the running print to its end.

use std::{collections::HashSet, path::PathBuf, time::Duration};

use jiff::Timestamp;
use thiserror::Error;

use crate::{
    cc2::{
        CommandError, LinkState, PrinterSnapshot,
        methods::{SlotMapEntry, error_code},
        model::{MachineState, printing_sub_status as sub, task_status},
        upload::UploadError,
    },
    config::Nozzle,
    gcode,
    inventory::Binding,
    jobs::{self, BlockReason, Job, JobError, JobState, JobTool, StartContext},
    schedule::{self, Rule},
    slicer::SliceSettings,
    store,
    web::{AppState, FileLayers},
};

const TICK: Duration = Duration::from_secs(30);
/// Status deltas arrive about once a second; there is no point deciding more often.
const SETTLE: Duration = Duration::from_millis(500);
/// After a start the printer can report idle for a while before it reports printing, and a
/// job whose printing state was never seen (PrintHub restarted) only ends after this long.
const START_GRACE_SECS: i64 = 120;
/// Enough history to find a print that just ended, even with a few prints started elsewhere.
const HISTORY_PAGE: i64 = 10;

/// Everything `jobs::check_start` needs besides the job, loaded once for a pass over the queue.
pub struct Readiness {
    snapshot: PrinterSnapshot,
    bindings: Vec<Binding>,
    pub bed_clear: bool,
    nozzle: Nozzle,
    rules: Vec<Rule>,
    other_job_active: bool,
}

impl Readiness {
    pub async fn load(state: &AppState) -> Result<Self, JobError> {
        let other_job_active = !jobs::in_state(&state.db, JobState::Printing)
            .await?
            .is_empty()
            || !jobs::in_state(&state.db, JobState::Uploading)
                .await?
                .is_empty();
        let bed_clear = jobs::bed_clear(&state.db).await?;
        let rules = schedule::rules(&state.db)
            .await?
            .into_iter()
            .map(|(_, rule)| rule)
            .collect();
        // Taken after the awaits: a watch borrow cannot be held across one.
        let bindings = state.bindings.borrow().clone();
        let nozzle = state.nozzle.borrow().nozzle;
        Ok(Self {
            snapshot: state.printer.snapshot(),
            bindings,
            bed_clear,
            nozzle,
            rules,
            other_job_active,
        })
    }

    pub fn check(
        &self,
        state: &AppState,
        job: &Job,
        tools: &[JobTool],
    ) -> Result<Vec<SlotMapEntry>, BlockReason> {
        jobs::check_start(
            job,
            tools,
            &StartContext {
                snapshot: &self.snapshot,
                bindings: &self.bindings,
                bed_clear: self.bed_clear,
                nozzle: self.nozzle,
                other_job_active: self.other_job_active,
                rules: &self.rules,
                now: Timestamp::now(),
                tz: &state.config.timezone,
                estimate_margin: state.config.estimate_margin,
            },
        )
    }
}

pub async fn run(state: AppState) {
    recover(&state).await;
    let mut printer = state.printer.subscribe();
    let mut tick = tokio::time::interval(TICK);
    let mut seen_printing = HashSet::new();
    loop {
        if let Err(err) = step(&state, &mut seen_printing).await {
            tracing::warn!(%err, "queue step failed");
        }
        tokio::select! {
            changed = printer.changed() => {
                if changed.is_err() {
                    std::future::pending::<()>().await;
                }
            }
            () = state.queue_changed.notified() => {}
            _ = tick.tick() => {}
        }
        tokio::time::sleep(SETTLE).await;
    }
}

/// Jobs caught mid-step by a restart: a slice cannot resume, an upload can be retried.
async fn recover(state: &AppState) {
    let now = store::now();
    let recovered = async {
        for job in jobs::in_state(&state.db, JobState::Slicing).await? {
            jobs::transition(
                &state.db,
                job.id,
                JobState::Slicing,
                JobState::Failed,
                Some("PrintHub restarted while slicing. Upload the model again."),
                now,
            )
            .await?;
        }
        for job in jobs::in_state(&state.db, JobState::Uploading).await? {
            jobs::transition(
                &state.db,
                job.id,
                JobState::Uploading,
                JobState::Queued,
                None,
                now,
            )
            .await?;
        }
        Ok::<_, JobError>(())
    };
    if let Err(err) = recovered.await {
        tracing::warn!(%err, "recovering interrupted jobs failed");
    }
}

async fn step(state: &AppState, seen_printing: &mut HashSet<i64>) -> Result<(), JobError> {
    let snapshot = state.printer.snapshot();
    refresh_file_layers(state, &snapshot).await;
    for job in jobs::in_state(&state.db, JobState::Printing).await? {
        follow(state, &job, &snapshot, seen_printing).await?;
    }

    let queued = jobs::in_state(&state.db, JobState::Queued).await?;
    if queued.is_empty() {
        return Ok(());
    }
    let readiness = Readiness::load(state).await?;
    for job in queued {
        let tools = jobs::tools(&state.db, job.id).await?;
        match readiness.check(state, &job, &tools) {
            Ok(slot_map) => {
                start(state, &job, &slot_map).await?;
                return Ok(());
            }
            // Nothing else in the queue could start either.
            Err(
                BlockReason::PrinterOffline
                | BlockReason::AnotherJobActive
                | BlockReason::PrinterBusy
                | BlockReason::BedNotClear,
            ) => return Ok(()),
            Err(_) => continue,
        }
    }
    Ok(())
}

/// What the printer's history says about the job's print: its task status, by the task id seen
/// while it ran, or else by the file's name. `None` when the printer cannot be asked or has no
/// entry for it.
async fn recorded_outcome(state: &AppState, job: &Job) -> Option<i64> {
    let client = state.printer.client()?;
    let history = match client.task_history(HISTORY_PAGE).await {
        Ok(history) => history,
        Err(err) => {
            tracing::warn!(job = job.id, %err, "reading the print history failed");
            return None;
        }
    };
    let filename = job.printer_filename();
    history
        .history_task_list
        .iter()
        .filter(|task| {
            job.printer_task_uuid.as_deref() == Some(task.task_id.as_str())
                || task.task_name == filename
        })
        .max_by_key(|task| task.end_time)
        .map(|task| task.task_status)
}

/// The status stream reports `total_layer` 0, so the file's own metadata (1046) is where a
/// layer total comes from. Asked once per file, whoever started the print.
async fn refresh_file_layers(state: &AppState, snapshot: &PrinterSnapshot) {
    let filename = match (&snapshot.link, &snapshot.status) {
        (LinkState::Registered, Some(status)) => status.print_status.filename.clone(),
        _ => return,
    };
    if filename.is_empty() {
        state
            .file_layers
            .send_if_modified(|current| current.take().is_some());
        return;
    }
    let known = state
        .file_layers
        .borrow()
        .as_ref()
        .is_some_and(|file| file.filename == filename);
    if known {
        return;
    }
    let Some(client) = state.printer.client() else {
        return;
    };
    match client.file_detail(&filename).await {
        Ok(detail) => {
            if let Some(layers) = detail.layers().filter(|layers| *layers > 0) {
                state
                    .file_layers
                    .send_replace(Some(FileLayers { filename, layers }));
            }
        }
        Err(err) => tracing::debug!(%err, filename, "asking for the file's layer count failed"),
    }
}

async fn follow(
    state: &AppState,
    job: &Job,
    snapshot: &PrinterSnapshot,
    seen_printing: &mut HashSet<i64>,
) -> Result<(), JobError> {
    let status = match (&snapshot.link, &snapshot.status) {
        (LinkState::Registered, Some(status)) => status,
        // While disconnected the last status is stale; wait rather than guess.
        _ => return Ok(()),
    };
    let ours = status.print_status.filename == job.printer_filename();
    let machine = &status.machine_status;
    match machine.state() {
        MachineState::Printing if ours => {
            seen_printing.insert(job.id);
            let progress = machine.progress.clamp(0, 100);
            if progress != job.progress {
                jobs::set_progress(&state.db, job.id, progress).await?;
            }
            if job.printer_task_uuid.is_none() && !status.print_status.uuid.is_empty() {
                jobs::record_start(&state.db, job.id, &[], Some(&status.print_status.uuid)).await?;
            }
        }
        MachineState::Idle => {
            let now = store::now();
            let long_ago = job
                .started_at
                .is_some_and(|started| now - started > START_GRACE_SECS);
            if !seen_printing.contains(&job.id) && !long_ago {
                return Ok(());
            }
            let (outcome, error) = match machine.sub_status {
                sub::COMPLETED if ours => (JobState::Done, None),
                sub::STOPPED if ours => (JobState::Cancelled, None),
                // Firmware 02.01.00.00 goes to plain idle and clears the filename, so the
                // print's own history entry is the only thing left that says how it ended.
                _ => match recorded_outcome(state, job).await {
                    Some(task_status::COMPLETED) => (JobState::Done, None),
                    Some(_) => (
                        JobState::Failed,
                        Some("The printer's history says the print did not finish."),
                    ),
                    None => (
                        JobState::Failed,
                        Some("The printer stopped without finishing the print."),
                    ),
                },
            };
            let progress = if outcome == JobState::Done {
                100
            } else {
                job.progress
            };
            jobs::finish(&state.db, job, outcome, progress, error, now).await?;
            // Whatever the outcome, something is on the bed until somebody says otherwise.
            jobs::set_bed_clear(&state.db, false, None, now).await?;
            seen_printing.remove(&job.id);
            tracing::info!(
                job = job.id,
                outcome = outcome.as_str(),
                progress,
                "print ended"
            );
            state.refresh_bindings().await?;
        }
        _ => {}
    }
    Ok(())
}

#[derive(Debug, Error)]
enum StartError {
    /// Worth retrying: the job keeps its place in the queue.
    #[error("{0}")]
    Retry(String),
    #[error("{0}")]
    Fatal(String),
}

async fn start(state: &AppState, job: &Job, slot_map: &[SlotMapEntry]) -> Result<(), JobError> {
    let now = store::now();
    match jobs::transition(
        &state.db,
        job.id,
        JobState::Queued,
        JobState::Uploading,
        Some(""),
        now,
    )
    .await
    {
        Ok(()) => {}
        // Cancelled while this step was deciding.
        Err(JobError::StateChanged { .. }) => return Ok(()),
        Err(err) => return Err(err),
    }
    tracing::info!(job = job.id, ?slot_map, "starting print");

    match send(state, job, slot_map).await {
        Ok(()) => {
            let now = store::now();
            jobs::transition(
                &state.db,
                job.id,
                JobState::Uploading,
                JobState::Printing,
                None,
                now,
            )
            .await?;
            jobs::record_start(&state.db, job.id, slot_map, None).await?;
            jobs::set_bed_clear(&state.db, false, None, now).await?;
        }
        Err(StartError::Retry(message)) => {
            tracing::warn!(job = job.id, message, "print start deferred");
            jobs::transition(
                &state.db,
                job.id,
                JobState::Uploading,
                JobState::Queued,
                Some(&message),
                store::now(),
            )
            .await?;
        }
        Err(StartError::Fatal(message)) => {
            tracing::warn!(job = job.id, message, "print start failed");
            jobs::transition(
                &state.db,
                job.id,
                JobState::Uploading,
                JobState::Failed,
                Some(&message),
                store::now(),
            )
            .await?;
        }
    }
    Ok(())
}

async fn send(state: &AppState, job: &Job, slot_map: &[SlotMapEntry]) -> Result<(), StartError> {
    let client = state
        .printer
        .client()
        .ok_or_else(|| StartError::Retry("The printer is not connected.".into()))?;
    let path = jobs::gcode_path(&state.config.data_dir, job.id);
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|err| StartError::Fatal(format!("The G-code file is missing: {err}")))?;
    let filename = job.printer_filename();

    state
        .uploader
        .upload(&filename, &bytes)
        .await
        .map_err(|err| match err {
            UploadError::Busy | UploadError::Http(_) => {
                StartError::Retry(format!("Sending the file failed: {err}"))
            }
            other => StartError::Fatal(format!("The printer refused the file: {other}")),
        })?;
    client
        .file_detail(&filename)
        .await
        .map_err(|err| command_error("The printer did not confirm the file", err))?;
    client
        .start_print(&filename, slot_map.to_vec())
        .await
        .map_err(|err| command_error("The printer did not start", err))
}

fn command_error(context: &str, err: CommandError) -> StartError {
    let message = format!("{context}: {err}");
    match err.printer_code() {
        Some(error_code::PRINTER_BUSY) | None => StartError::Retry(message),
        Some(_) => StartError::Fatal(message),
    }
}

/// Slices an uploaded model and moves the job on to confirmation. Runs as its own task.
pub async fn slice_job(state: AppState, job_id: i64, settings: SliceSettings, spool_id: i64) {
    let Some(slicer) = state.slicer.clone() else {
        return;
    };
    let data_dir = &state.config.data_dir;
    let workdir: PathBuf = jobs::dir(data_dir, job_id).join("slice");
    let sliced = async {
        let output = slicer
            .slice(&jobs::model_path(data_dir, job_id), &workdir, &settings)
            .await
            .map_err(|err| err.to_string())?;
        let target = jobs::gcode_path(data_dir, job_id);
        tokio::fs::rename(&output, &target)
            .await
            .map_err(|err| format!("keeping the G-code: {err}"))?;
        let info = gcode::read(&target).await.map_err(|err| err.to_string())?;
        if info.total_grams() <= 0.0 {
            return Err("The slicer reported no filament use.".to_owned());
        }
        Ok(info)
    };
    let result = sliced.await;
    let _ = tokio::fs::remove_dir_all(&workdir).await;

    let now = store::now();
    let recorded = match result {
        Ok(info) => {
            async {
                jobs::store_gcode_info(&state.db, job_id, &info, Some(spool_id)).await?;
                jobs::transition(
                    &state.db,
                    job_id,
                    JobState::Slicing,
                    JobState::AwaitingConfirm,
                    None,
                    now,
                )
                .await
            }
            .await
        }
        Err(message) => {
            tracing::warn!(job = job_id, message, "slicing failed");
            jobs::transition(
                &state.db,
                job_id,
                JobState::Slicing,
                JobState::Failed,
                Some(&message),
                now,
            )
            .await
        }
    };
    match recorded {
        // Cancelled while slicing.
        Ok(()) | Err(JobError::StateChanged { .. }) => {}
        Err(err) => tracing::warn!(job = job_id, %err, "recording the slice failed"),
    }
}
