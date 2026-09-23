//! The print queue: uploading and confirming jobs, cancelling and reordering them, and the
//! bed-clear gate.

use std::{collections::HashMap, path::Path};

use askama::Template;
use axum::{
    Form,
    body::Body,
    extract::{Multipart, Path as UrlPath, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{
    AppState,
    error::{AppError, render},
    session::{AdminUser, CurrentUser},
    views,
};
use crate::{
    accounts::{self, Permission, User},
    auth,
    cc2::model::Tray,
    dispatcher::{self, Readiness},
    gcode,
    inventory::{self, Spool},
    jobs::{self, Job, JobState, NewJob, Source},
    preview,
    slicer::{self, Plate, SliceSettings},
    store,
};

/// Finished jobs listed under the queue.
const RECENT_FINISHED: i64 = 20;
const DEFAULT_INFILL: &str = "15";
const DEFAULT_SCALE: &str = "100";

fn owner_name(job: &Job) -> String {
    job.owner
        .as_ref()
        .map_or_else(String::new, |(_, name)| name.clone())
}

fn estimate(job: &Job) -> String {
    job.estimated_seconds
        .map_or_else(String::new, views::human_duration)
}

/// Outcomes are fixed codes, as on the users page, so a crafted link cannot put text of the
/// sender's choosing on the page.
#[derive(Deserialize)]
pub struct Outcome {
    done: Option<String>,
}

#[derive(Template)]
#[template(path = "jobs.html")]
struct JobsPage {
    user: Option<User>,
    rows: Vec<JobRow>,
    /// Leaves out previews and actions, as the dashboard does; the shared rows need it.
    compact: bool,
    bed_clear: bool,
    printer_busy: bool,
    camera_enabled: bool,
    notice: Option<&'static str>,
}

pub(super) struct JobRow {
    pub(super) id: i64,
    pub(super) name: String,
    pub(super) owner: String,
    pub(super) state: &'static str,
    pub(super) detail: String,
    pub(super) estimate: String,
    /// Place in the queue, counting from 1; only queued jobs have one.
    pub(super) position: Option<usize>,
    pub(super) filament: Vec<FilamentChip>,
    /// Whether there is G-code to draw a preview from.
    pub(super) preview: bool,
    pub(super) printing: bool,
    pub(super) can_cancel: bool,
    pub(super) can_move: bool,
    pub(super) can_requeue: bool,
}

pub(super) struct FilamentChip {
    /// Always a valid `#RRGGBB`: it is written into an SVG attribute.
    pub(super) color: String,
    pub(super) label: String,
}

pub async fn jobs_page(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(outcome): Query<Outcome>,
) -> Result<Response, AppError> {
    let readiness = Readiness::load(&state).await?;
    let jobs = jobs::list(&state.db, RECENT_FINISHED).await?;
    let rows = job_rows(&state, &current.user, &readiness, jobs).await?;
    let page = JobsPage {
        user: Some(current.user),
        rows,
        compact: false,
        bed_clear: readiness.bed_clear,
        printer_busy: readiness.printer_busy(),
        camera_enabled: state.camera.is_some(),
        notice: outcome.done.as_deref().and_then(|code| match code {
            "queued" => Some("Job added to the queue."),
            "cancelled" => Some("Job cancelled."),
            "bed" => Some("Bed marked clear."),
            "requeued" => Some("Job queued again."),
            _ => None,
        }),
    };
    Ok(render(&page)?.into_response())
}

/// Unfinished jobs for the dashboard's preview of the queue, in queue order.
pub(super) async fn upcoming_rows(
    state: &AppState,
    user: &User,
    limit: usize,
) -> Result<(Vec<JobRow>, usize), AppError> {
    let readiness = Readiness::load(state).await?;
    let mut jobs = jobs::list(&state.db, 0).await?;
    let total = jobs.len();
    jobs.truncate(limit);
    Ok((job_rows(state, user, &readiness, jobs).await?, total))
}

async fn job_rows(
    state: &AppState,
    user: &User,
    readiness: &Readiness,
    jobs: Vec<Job>,
) -> Result<Vec<JobRow>, AppError> {
    let control_any = accounts::has_permission(&state.db, user, Permission::ControlPrint).await?;
    let bindings = state.bindings.borrow().clone();
    let spools = inventory::spools(&state.db, true).await?;
    let spools: HashMap<i64, &Spool> = spools.iter().map(|spool| (spool.id, spool)).collect();
    let mut queued = 0;
    let mut rows = Vec::new();
    for job in jobs {
        let tools = jobs::tools(&state.db, job.id).await?;
        let position = (job.state == JobState::Queued).then(|| {
            queued += 1;
            queued
        });
        let detail = match job.state {
            JobState::Queued => match readiness.check(state, &job, &tools) {
                Ok(_) => "Starting shortly.".to_owned(),
                Err(reason) => reason.describe(&state.config.timezone),
            },
            JobState::Printing => format!("{}% done", job.progress),
            JobState::Failed => job.error.clone(),
            JobState::Done | JobState::Cancelled => {
                job.finished_at.map(views::format_time).unwrap_or_default()
            }
            _ => String::new(),
        };
        let filament = tools
            .iter()
            .map(|tool| {
                let tray = match (tool.canvas_id, tool.tray_id) {
                    (Some(canvas), Some(tray)) => Some((canvas, tray)),
                    _ => bindings
                        .iter()
                        .find(|binding| Some(binding.spool.id) == tool.spool_id)
                        .map(|binding| (binding.canvas_id, binding.tray_id)),
                };
                // The chosen spool is what will print; the G-code only names what it was sliced for.
                let (color, material) = match tool.spool_id.and_then(|id| spools.get(&id)) {
                    Some(spool) => (&spool.color_hex, &spool.material),
                    None => (&tool.color_hex, &tool.material),
                };
                FilamentChip {
                    color: views::safe_color(color),
                    label: match tray {
                        Some((canvas, tray)) => {
                            format!("{} {material}", views::tray_label(canvas, tray))
                        }
                        None => material.clone(),
                    },
                }
            })
            .collect();
        let has_gcode = exists(&jobs::gcode_path(&state.config.data_dir, job.id)).await;
        rows.push(JobRow {
            id: job.id,
            owner: owner_name(&job),
            state: job.state.label(),
            estimate: estimate(&job),
            position,
            filament,
            preview: has_gcode,
            printing: job.state == JobState::Printing,
            can_cancel: job.cancellable_by(user, control_any),
            can_move: user.is_admin() && job.state == JobState::Queued,
            can_requeue: job.retryable_by(user, &tools, has_gcode),
            detail,
            name: job.name,
        });
    }
    Ok(rows)
}

#[derive(Template)]
#[template(path = "job_new.html")]
struct NewJobPage {
    user: Option<User>,
    stl_enabled: bool,
    /// The mounted nozzle, which models are sliced for.
    nozzle: &'static str,
    max_mb: u64,
    spools: Vec<Choice>,
    plates: Vec<Choice>,
    processes: Vec<Choice>,
    filaments: Vec<Choice>,
    supports: bool,
    auto_orient: bool,
    infill: String,
    scale_percent: String,
    error: Option<String>,
}

struct Choice {
    value: String,
    name: String,
    selected: bool,
}

pub async fn new_job_page(
    State(state): State<AppState>,
    current: CurrentUser,
) -> Result<Response, AppError> {
    new_job_form(&state, current.user, &HashMap::new(), None).await
}

async fn new_job_form(
    state: &AppState,
    user: User,
    fields: &HashMap<String, String>,
    error: Option<String>,
) -> Result<Response, AppError> {
    let field = |name: &str| fields.get(name).map(String::as_str).unwrap_or_default();
    let nozzle = state.nozzle.borrow().nozzle;
    let (processes, filaments) = match &state.slicer {
        Some(slicer) => {
            let default_process = slicer::default_process(nozzle);
            let wanted = if field("process").is_empty() {
                default_process.as_str()
            } else {
                field("process")
            };
            let choices = |names: &[String], wanted: &str| -> Vec<Choice> {
                names
                    .iter()
                    .map(|name| Choice {
                        selected: name == wanted,
                        value: name.clone(),
                        name: name.clone(),
                    })
                    .collect()
            };
            (
                choices(slicer.processes(nozzle), wanted),
                choices(slicer.filaments(nozzle), field("filament")),
            )
        }
        None => (Vec::new(), Vec::new()),
    };
    let spools = inventory::spools(&state.db, false)
        .await?
        .iter()
        .map(|spool| Choice {
            value: spool.id.to_string(),
            name: format!(
                "{} ({} left)",
                views::spool_name(spool),
                views::grams(spool.remaining_grams)
            ),
            selected: field("spool_id") == spool.id.to_string(),
        })
        .collect();
    let plates = Plate::ALL
        .into_iter()
        .enumerate()
        .map(|(i, plate)| Choice {
            value: plate.as_str().to_owned(),
            name: plate.as_str().to_owned(),
            selected: match Plate::parse(field("plate")) {
                Some(wanted) => wanted == plate,
                None => i == 0,
            },
        })
        .collect();
    let status = if error.is_some() {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::OK
    };
    let page = NewJobPage {
        user: Some(user),
        stl_enabled: state.slicer.is_some(),
        nozzle: nozzle.as_str(),
        max_mb: state.config.max_upload_bytes / (1024 * 1024),
        spools,
        plates,
        processes,
        filaments,
        supports: fields.contains_key("supports"),
        auto_orient: fields.contains_key("auto_orient"),
        infill: if field("infill").is_empty() {
            DEFAULT_INFILL.to_owned()
        } else {
            field("infill").to_owned()
        },
        scale_percent: if field("scale_percent").is_empty() {
            DEFAULT_SCALE.to_owned()
        } else {
            field("scale_percent").to_owned()
        },
        error,
    };
    Ok((status, render(&page)?).into_response())
}

/// Something the uploader can fix, shown on the form, or a server fault.
enum Problem {
    User(String),
    App(AppError),
}

impl From<AppError> for Problem {
    fn from(err: AppError) -> Self {
        Self::App(err)
    }
}

impl From<jobs::JobError> for Problem {
    fn from(err: jobs::JobError) -> Self {
        Self::App(err.into())
    }
}

impl From<inventory::InventoryError> for Problem {
    fn from(err: inventory::InventoryError) -> Self {
        Self::App(err.into())
    }
}

fn user_problem(message: impl Into<String>) -> Problem {
    Problem::User(message.into())
}

#[derive(Default)]
struct Upload {
    file_name: String,
    bytes: u64,
    fields: HashMap<String, String>,
}

pub async fn create_job(
    State(state): State<AppState>,
    current: CurrentUser,
    mut multipart: Multipart,
) -> Result<Response, AppError> {
    let uploads = state.config.data_dir.join("uploads");
    tokio::fs::create_dir_all(&uploads)
        .await
        .map_err(|err| AppError::Internal(err.into()))?;
    let (token, _) = auth::new_token()?;
    let temp = uploads.join(format!("{token}.part"));

    let mut upload = Upload::default();
    let received = receive(&state, &mut multipart, &temp, &mut upload).await;
    let created = match received {
        Ok(()) => create_from_upload(&state, &current.user, &upload, &temp).await,
        Err(problem) => Err(problem),
    };
    // Gone already when the job took the file.
    let _ = tokio::fs::remove_file(&temp).await;

    match created {
        Ok(id) => {
            tracing::info!(username = %current.user.username, job = id, name = %upload.file_name, "job uploaded");
            Ok(Redirect::to(&format!("/jobs/{id}")).into_response())
        }
        Err(Problem::User(message)) => {
            new_job_form(&state, current.user, &upload.fields, Some(message)).await
        }
        Err(Problem::App(err)) => Err(err),
    }
}

async fn receive(
    state: &AppState,
    multipart: &mut Multipart,
    temp: &Path,
    upload: &mut Upload,
) -> Result<(), Problem> {
    let unreadable = |err: axum::extract::multipart::MultipartError| {
        user_problem(format!("The upload was interrupted: {err}"))
    };
    let mut got_file = false;
    while let Some(mut field) = multipart.next_field().await.map_err(unreadable)? {
        let name = field.name().unwrap_or_default().to_owned();
        if name != "file" {
            let value = field.text().await.map_err(unreadable)?;
            upload.fields.insert(name, value);
            continue;
        }
        upload.file_name = field.file_name().map(clean_file_name).unwrap_or_default();
        let mut file = tokio::fs::File::create(temp)
            .await
            .map_err(|err| AppError::Internal(err.into()))?;
        while let Some(chunk) = field.chunk().await.map_err(unreadable)? {
            upload.bytes += chunk.len() as u64;
            if upload.bytes > state.config.max_upload_bytes {
                return Err(user_problem(format!(
                    "The file is larger than {} MB.",
                    state.config.max_upload_bytes / (1024 * 1024)
                )));
            }
            file.write_all(&chunk)
                .await
                .map_err(|err| AppError::Internal(err.into()))?;
        }
        file.flush()
            .await
            .map_err(|err| AppError::Internal(err.into()))?;
        got_file = upload.bytes > 0;
    }
    if !got_file {
        return Err(user_problem("Choose a file to upload."));
    }
    Ok(())
}

/// The last path component, for display only: files are stored under the job's id.
fn clean_file_name(raw: &str) -> String {
    let name = raw.rsplit(['/', '\\']).next().unwrap_or(raw).trim();
    name.chars().take(120).collect()
}

async fn create_from_upload(
    state: &AppState,
    user: &User,
    upload: &Upload,
    temp: &Path,
) -> Result<i64, Problem> {
    let lower = upload.file_name.to_ascii_lowercase();
    if lower.ends_with(".gcode") {
        create_gcode_job(state, user, upload, temp).await
    } else if lower.ends_with(".stl") {
        create_stl_job(state, user, upload, temp).await
    } else {
        Err(user_problem(
            "Upload an .stl model or a .gcode file sliced for the Centauri Carbon 2.",
        ))
    }
}

async fn create_gcode_job(
    state: &AppState,
    user: &User,
    upload: &Upload,
    temp: &Path,
) -> Result<i64, Problem> {
    let info = gcode::read(temp)
        .await
        .map_err(|err| user_problem(capitalise(&err.to_string())))?;
    if !info.printer_model.is_empty() && !info.printer_model.contains("Centauri Carbon 2") {
        return Err(user_problem(format!(
            "This file was sliced for {}, not the Centauri Carbon 2.",
            info.printer_model
        )));
    }
    if info.total_grams() <= 0.0 {
        return Err(user_problem(
            "The file does not say how much filament it uses, so it cannot be matched to spools.",
        ));
    }
    let new = NewJob {
        owner_id: user.id,
        name: &upload.file_name,
        source: Source::Gcode,
        process_profile: None,
        filament_profile: None,
        supports: None,
        infill_percent: None,
        scale_percent: None,
    };
    let id = jobs::create(&state.db, &new, store::now()).await?;
    place(temp, &jobs::gcode_path(&state.config.data_dir, id)).await?;
    jobs::store_gcode_info(&state.db, id, &info, None).await?;
    preview::prepare(state.config.data_dir.clone(), id);
    Ok(id)
}

async fn create_stl_job(
    state: &AppState,
    user: &User,
    upload: &Upload,
    temp: &Path,
) -> Result<i64, Problem> {
    let slicer = state.slicer.as_ref().ok_or_else(|| {
        user_problem(
            "This server cannot slice models. Upload G-code sliced for the Centauri Carbon 2.",
        )
    })?;
    let field = |name: &str| {
        upload
            .fields
            .get(name)
            .map(|value| value.trim())
            .unwrap_or_default()
    };
    let spool = match field("spool_id").parse::<i64>() {
        Ok(id) => inventory::spool(&state.db, id).await?,
        Err(_) => None,
    }
    .filter(|spool| !spool.archived)
    .ok_or_else(|| user_problem("Choose the spool to print with."))?;
    let plate =
        Plate::parse(field("plate")).ok_or_else(|| user_problem("Choose the build plate."))?;

    // Read once, so the profiles checked here are the ones the model is sliced with.
    let nozzle = state.nozzle.borrow().nozzle;
    let process = field("process");
    if !slicer.processes(nozzle).iter().any(|name| name == process) {
        return Err(user_problem("Choose a print profile."));
    }
    let filaments = slicer.filaments(nozzle);
    let filament = if field("filament").is_empty() {
        default_filament(filaments, &spool).ok_or_else(|| {
            user_problem(format!(
                "No filament profile matches {}. Choose one.",
                spool.material
            ))
        })?
    } else {
        field("filament").to_owned()
    };
    if !filaments.contains(&filament) {
        return Err(user_problem("Choose a filament profile."));
    }
    let infill = field("infill")
        .trim_end_matches('%')
        .parse::<u8>()
        .ok()
        .filter(|percent| *percent <= 100)
        .ok_or_else(|| user_problem("Infill is a percentage from 0 to 100."))?;
    let supports = upload.fields.contains_key("supports");
    let auto_orient = upload.fields.contains_key("auto_orient");
    let scale_percent = parse_scale(field("scale_percent"))?;

    let new = NewJob {
        owner_id: user.id,
        name: &upload.file_name,
        source: Source::Stl,
        process_profile: Some(process),
        filament_profile: Some(&filament),
        supports: Some(supports),
        infill_percent: Some(infill),
        scale_percent: (scale_percent != 100.0).then_some(scale_percent),
    };
    let id = jobs::create(&state.db, &new, store::now()).await?;
    place(temp, &jobs::model_path(&state.config.data_dir, id)).await?;
    let settings = SliceSettings {
        nozzle,
        plate,
        process: process.to_owned(),
        filament,
        color_hex: spool.color_hex.clone(),
        supports,
        infill_percent: infill,
        scale_percent,
        auto_orient,
    };
    tokio::spawn(dispatcher::slice_job(state.clone(), id, settings, spool.id));
    Ok(id)
}

/// Percent, blank meaning 100. Kept inside the slicer's own bounds, which today stop at 100:
/// see `slicer::SCALE_MAX`.
fn parse_scale(raw: &str) -> Result<f64, Problem> {
    if raw.is_empty() {
        return Ok(100.0);
    }
    raw.trim_end_matches('%')
        .replace(',', ".")
        .parse::<f64>()
        .ok()
        .filter(|percent| {
            percent.is_finite()
                && *percent >= slicer::SCALE_MIN * 100.0
                && *percent <= slicer::SCALE_MAX * 100.0
        })
        .ok_or_else(|| {
            user_problem(format!(
                "Scale is a percentage from {:.0} to {:.0}.",
                slicer::SCALE_MIN * 100.0,
                slicer::SCALE_MAX * 100.0
            ))
        })
}

/// Elegoo's profile for the spool's material.
fn default_filament(filaments: &[String], spool: &Spool) -> Option<String> {
    let wanted = format!("Elegoo {} @ECC2", spool.material.trim());
    filaments
        .iter()
        .find(|name| name.eq_ignore_ascii_case(&wanted))
        .cloned()
}

async fn place(temp: &Path, target: &Path) -> Result<(), Problem> {
    let io = |err: std::io::Error| Problem::App(AppError::Internal(err.into()));
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(io)?;
    }
    tokio::fs::rename(temp, target).await.map_err(io)
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => format!("{}{}.", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

#[derive(Template)]
#[template(path = "job.html")]
struct JobPage {
    user: Option<User>,
    job: JobView,
    tools: Vec<ToolRow>,
    confirm: bool,
    blocked: Option<String>,
    notice: Option<&'static str>,
    error: Option<String>,
}

struct JobView {
    id: i64,
    name: String,
    owner: String,
    state: &'static str,
    source: &'static str,
    sliced_for: Option<String>,
    plate: Option<String>,
    settings: Option<String>,
    estimate: String,
    error: String,
    progress: i64,
    slicing: bool,
    printing: bool,
    can_cancel: bool,
    can_requeue: bool,
    created: String,
    /// Downloads, named as they would be saved. Absent while the file is not on disk yet.
    model_file: Option<String>,
    gcode_file: Option<String>,
}

struct ToolRow {
    index: u32,
    label: String,
    material: String,
    color: String,
    grams: String,
    spool: String,
    tray: String,
    choices: Vec<Choice>,
}

pub async fn job_page(
    State(state): State<AppState>,
    current: CurrentUser,
    UrlPath(id): UrlPath<i64>,
    Query(outcome): Query<Outcome>,
) -> Result<Response, AppError> {
    let notice = outcome.done.as_deref().and_then(|code| match code {
        "stopping" => Some("Stop sent to the printer."),
        _ => None,
    });
    job_detail(&state, current.user, id, notice, None).await
}

async fn job_detail(
    state: &AppState,
    user: User,
    id: i64,
    notice: Option<&'static str>,
    error: Option<String>,
) -> Result<Response, AppError> {
    let job = jobs::get(&state.db, id).await?.ok_or(AppError::NotFound)?;
    let tools = jobs::tools(&state.db, id).await?;
    let control_any = accounts::has_permission(&state.db, &user, Permission::ControlPrint).await?;
    let confirm = job.managed_by(&user) && job.state == JobState::AwaitingConfirm;
    let blocked = if job.state == JobState::Queued {
        let readiness = Readiness::load(state).await?;
        readiness
            .check(state, &job, &tools)
            .err()
            .map(|reason| reason.describe(&state.config.timezone))
    } else {
        None
    };

    let spools = inventory::spools(&state.db, true).await?;
    let by_id: HashMap<i64, &Spool> = spools.iter().map(|spool| (spool.id, spool)).collect();
    let loadable: Vec<Spool> = spools.iter().filter(|s| !s.archived).cloned().collect();
    let tool_rows = tools
        .iter()
        .map(|tool| {
            let choices = if confirm {
                let wanted = Tray {
                    filament_type: tool.material.clone(),
                    filament_color: tool.color_hex.clone(),
                    ..Tray::default()
                };
                inventory::rank_for_tray(loadable.clone(), &wanted)
                    .iter()
                    .map(|spool| Choice {
                        value: spool.id.to_string(),
                        name: format!(
                            "{} ({} left)",
                            views::spool_name(spool),
                            views::grams(spool.remaining_grams)
                        ),
                        selected: tool.spool_id == Some(spool.id),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            ToolRow {
                index: tool.tool_index,
                label: format!("Filament {}", tool.tool_index + 1),
                material: tool.material.clone(),
                color: views::safe_color(&tool.color_hex),
                grams: views::grams(tool.grams),
                spool: tool
                    .spool_id
                    .and_then(|id| by_id.get(&id))
                    .map_or_else(|| "Not chosen".to_owned(), |spool| views::spool_name(spool)),
                tray: match (tool.canvas_id, tool.tray_id) {
                    (Some(canvas), Some(tray)) => views::tray_label(canvas, tray),
                    _ => String::new(),
                },
                choices,
            }
        })
        .collect();

    let settings = (job.source == Source::Stl).then(|| {
        format!(
            "{}, {}, {}% infill, supports {}{}",
            job.process_profile.as_deref().unwrap_or_default(),
            job.filament_profile.as_deref().unwrap_or_default(),
            job.infill_percent.unwrap_or_default(),
            if job.supports == Some(true) {
                "on"
            } else {
                "off"
            },
            job.scale_percent
                .map(|percent| format!(", scaled to {percent}%"))
                .unwrap_or_default(),
        )
    });
    let status = if error.is_some() {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::OK
    };
    let has_gcode = exists(&jobs::gcode_path(&state.config.data_dir, id)).await;
    let page = JobPage {
        job: JobView {
            id: job.id,
            owner: owner_name(&job),
            state: job.state.label(),
            sliced_for: job.nozzle_mm.map(|mm| format!("{mm} mm")),
            plate: job.plate.clone(),
            source: match job.source {
                Source::Stl => "Model, sliced here",
                Source::Gcode => "G-code",
            },
            settings,
            estimate: estimate(&job),
            progress: job.progress,
            slicing: job.state == JobState::Slicing,
            printing: job.state == JobState::Printing,
            can_cancel: job.cancellable_by(&user, control_any),
            can_requeue: job.retryable_by(&user, &tools, has_gcode),
            created: views::format_time(job.created_at),
            model_file: exists(&jobs::model_path(&state.config.data_dir, id))
                .await
                .then(|| download_name(&job.name, job.id, "stl")),
            gcode_file: has_gcode.then(|| download_name(&job.name, job.id, "gcode")),
            error: job.error,
            name: job.name,
        },
        user: Some(user),
        tools: tool_rows,
        confirm,
        blocked,
        notice,
        error,
    };
    Ok((status, render(&page)?).into_response())
}

/// A job's own files, for anyone logged in: everybody can already see every job.
pub async fn job_file(
    State(state): State<AppState>,
    _current: CurrentUser,
    UrlPath((id, kind)): UrlPath<(i64, String)>,
) -> Result<Response, AppError> {
    let job = jobs::get(&state.db, id).await?.ok_or(AppError::NotFound)?;
    if kind == "preview.png" {
        let png = preview::for_job(&state.config.data_dir, id)
            .await
            .map_err(anyhow::Error::from)?
            .ok_or(AppError::NotFound)?;
        return Ok((
            [
                (header::CONTENT_TYPE, "image/png"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            png,
        )
            .into_response());
    }
    let (path, extension, content_type) = match kind.as_str() {
        "model.stl" => (
            jobs::model_path(&state.config.data_dir, id),
            "stl",
            "model/stl",
        ),
        "job.gcode" => (
            jobs::gcode_path(&state.config.data_dir, id),
            "gcode",
            "text/x.gcode",
        ),
        _ => return Err(AppError::NotFound),
    };
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| AppError::NotFound)?;
    let length = file.metadata().await.map(|meta| meta.len()).unwrap_or(0);
    let name = download_name(&job.name, job.id, extension);
    Ok((
        [
            (header::CONTENT_TYPE, content_type.to_owned()),
            (header::CONTENT_LENGTH, length.to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        stream_file(file),
    )
        .into_response())
}

/// Reads in chunks: a sliced G-code runs to hundreds of megabytes, and the Pi5 serves it while
/// a slice may be running.
fn stream_file(file: tokio::fs::File) -> Body {
    Body::from_stream(futures::stream::try_unfold(file, |mut file| async move {
        let mut chunk = vec![0u8; 64 * 1024];
        let read = file.read(&mut chunk).await?;
        if read == 0 {
            return Ok::<_, std::io::Error>(None);
        }
        chunk.truncate(read);
        Ok(Some((bytes::Bytes::from(chunk), file)))
    }))
}

/// The uploaded name with `extension` forced on it, reduced to characters that need no quoting
/// inside a `Content-Disposition` filename.
fn download_name(uploaded: &str, id: i64, extension: &str) -> String {
    let stem = uploaded
        .rsplit_once('.')
        .map_or(uploaded, |(stem, _)| stem)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let stem = stem.trim();
    if stem.is_empty() {
        format!("printhub-{id}.{extension}")
    } else {
        format!("{stem}.{extension}")
    }
}

pub(super) async fn exists(path: &Path) -> bool {
    tokio::fs::try_exists(path).await.unwrap_or(false)
}

async fn managed_job(state: &AppState, user: &User, id: i64) -> Result<Job, AppError> {
    let job = jobs::get(&state.db, id).await?.ok_or(AppError::NotFound)?;
    if !job.managed_by(user) {
        return Err(AppError::Forbidden);
    }
    Ok(job)
}

/// The form carries one `spool_<tool>` field per tool.
pub async fn confirm_job(
    State(state): State<AppState>,
    current: CurrentUser,
    UrlPath(id): UrlPath<i64>,
    Form(fields): Form<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let job = managed_job(&state, &current.user, id).await?;
    if job.state != JobState::AwaitingConfirm {
        return Err(AppError::BadRequest(
            "This job is not waiting for confirmation.".into(),
        ));
    }
    let loadable: Vec<i64> = inventory::spools(&state.db, false)
        .await?
        .iter()
        .map(|spool| spool.id)
        .collect();
    let mut choices = Vec::new();
    for tool in jobs::tools(&state.db, id).await? {
        let chosen = fields
            .get(&format!("spool_{}", tool.tool_index))
            .and_then(|raw| raw.parse::<i64>().ok())
            .filter(|spool| loadable.contains(spool));
        match chosen {
            Some(spool) => choices.push((tool.tool_index, spool)),
            None => {
                let message = format!("Choose a spool for filament {}.", tool.tool_index + 1);
                return job_detail(&state, current.user, id, None, Some(message)).await;
            }
        }
    }
    jobs::choose_spools(&state.db, id, &choices).await?;
    jobs::transition(
        &state.db,
        id,
        JobState::AwaitingConfirm,
        JobState::Queued,
        Some(""),
        store::now(),
    )
    .await?;
    state.queue_changed.notify_one();
    Ok(Redirect::to("/jobs?done=queued").into_response())
}

pub async fn cancel_job(
    State(state): State<AppState>,
    current: CurrentUser,
    UrlPath(id): UrlPath<i64>,
) -> Result<Response, AppError> {
    let job = jobs::get(&state.db, id).await?.ok_or(AppError::NotFound)?;
    let control_any =
        accounts::has_permission(&state.db, &current.user, Permission::ControlPrint).await?;
    if !job.cancellable_by(&current.user, control_any) {
        return Err(match job.state {
            JobState::Uploading => AppError::BadRequest(
                "The job is being sent to the printer. Stop it once it prints.".into(),
            ),
            state if state.is_finished() => {
                AppError::BadRequest("This job has already ended.".into())
            }
            _ => AppError::Forbidden,
        });
    }
    match job.state {
        JobState::Printing => {
            let client = state
                .printer
                .client()
                .ok_or_else(|| AppError::Unavailable("The printer is not connected.".into()))?;
            client
                .stop()
                .await
                .map_err(|err| AppError::Unavailable(format!("The printer did not stop: {err}")))?;
            tracing::info!(username = %current.user.username, job = id, "stop sent");
            Ok(Redirect::to(&format!("/jobs/{id}?done=stopping")).into_response())
        }
        other => {
            jobs::transition(
                &state.db,
                id,
                other,
                JobState::Cancelled,
                None,
                store::now(),
            )
            .await?;
            state.queue_changed.notify_one();
            Ok(Redirect::to("/jobs?done=cancelled").into_response())
        }
    }
}

pub async fn requeue_job(
    State(state): State<AppState>,
    current: CurrentUser,
    UrlPath(id): UrlPath<i64>,
) -> Result<Response, AppError> {
    let job = managed_job(&state, &current.user, id).await?;
    let has_gcode = exists(&jobs::gcode_path(&state.config.data_dir, id)).await;
    let tools = jobs::tools(&state.db, id).await?;
    if !job.retryable_by(&current.user, &tools, has_gcode) {
        return Err(AppError::BadRequest(
            "Only a failed job with sliced G-code can be retried.".into(),
        ));
    }
    jobs::transition(
        &state.db,
        id,
        JobState::Failed,
        JobState::Queued,
        Some(""),
        store::now(),
    )
    .await?;
    state.queue_changed.notify_one();
    Ok(Redirect::to("/jobs?done=requeued").into_response())
}

#[derive(Deserialize)]
pub struct MoveForm {
    direction: String,
}

pub async fn move_job(
    State(state): State<AppState>,
    AdminUser(_admin): AdminUser,
    UrlPath(id): UrlPath<i64>,
    Form(form): Form<MoveForm>,
) -> Result<Response, AppError> {
    jobs::move_queued(&state.db, id, form.direction == "up").await?;
    state.queue_changed.notify_one();
    Ok(Redirect::to("/jobs").into_response())
}

pub async fn mark_bed_clear(
    State(state): State<AppState>,
    current: CurrentUser,
) -> Result<Response, AppError> {
    jobs::set_bed_clear(&state.db, true, Some(current.user.id), store::now()).await?;
    tracing::info!(username = %current.user.username, "bed marked clear");
    state.queue_changed.notify_one();
    Ok(Redirect::to("/jobs?done=bed").into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_lose_their_directories() {
        assert_eq!(clean_file_name("C:\\models\\benchy.stl"), "benchy.stl");
        assert_eq!(clean_file_name("../../etc/passwd"), "passwd");
        assert_eq!(clean_file_name(&"a".repeat(300)).len(), 120);
    }
}
