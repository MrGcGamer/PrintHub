//! The print queue: uploading and confirming jobs, cancelling and reordering them, the bed-clear
//! gate, and the admin's print windows.

use std::{collections::HashMap, path::Path};

use askama::Template;
use axum::{
    Form,
    extract::{Multipart, Path as UrlPath, Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use super::{
    AppState,
    error::{AppError, render},
    session::{AdminUser, CurrentUser},
    views,
};
use crate::{
    accounts::User,
    auth,
    cc2::model::Tray,
    dispatcher::{self, Readiness},
    gcode,
    inventory::{self, Spool},
    jobs::{self, Job, JobState, NewJob, Source},
    schedule::{self, Rule, RuleKind},
    slicer::{self, SliceSettings},
    store,
};

/// Finished jobs listed under the queue.
const RECENT_FINISHED: i64 = 20;
const DEFAULT_INFILL: &str = "15";

fn can_manage(user: &User, job: &Job) -> bool {
    user.is_admin() || job.owner_id() == Some(user.id)
}

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
    bed_clear: bool,
    camera_enabled: bool,
    notice: Option<&'static str>,
}

struct JobRow {
    id: i64,
    name: String,
    owner: String,
    state: &'static str,
    detail: String,
    estimate: String,
    printing: bool,
    can_cancel: bool,
    can_move: bool,
    can_requeue: bool,
}

pub async fn jobs_page(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(outcome): Query<Outcome>,
) -> Result<Response, AppError> {
    let readiness = Readiness::load(&state).await?;
    let mut rows = Vec::new();
    for job in jobs::list(&state.db, RECENT_FINISHED).await? {
        let detail = match job.state {
            JobState::Queued => {
                let tools = jobs::tools(&state.db, job.id).await?;
                match readiness.check(&state, &job, &tools) {
                    Ok(_) => "Starting shortly.".to_owned(),
                    Err(reason) => reason.describe(&state.config.timezone),
                }
            }
            JobState::Printing => format!("{}% done", job.progress),
            JobState::Failed => job.error.clone(),
            JobState::Done | JobState::Cancelled => {
                job.finished_at.map(views::format_time).unwrap_or_default()
            }
            _ => String::new(),
        };
        let manage = can_manage(&current.user, &job);
        rows.push(JobRow {
            id: job.id,
            owner: owner_name(&job),
            state: job.state.label(),
            estimate: estimate(&job),
            printing: job.state == JobState::Printing,
            can_cancel: manage && !job.state.is_finished() && job.state != JobState::Uploading,
            can_move: current.user.is_admin() && job.state == JobState::Queued,
            can_requeue: manage && job.state == JobState::Failed && job.estimated_seconds.is_some(),
            detail,
            name: job.name,
        });
    }
    let page = JobsPage {
        user: Some(current.user),
        rows,
        bed_clear: readiness.bed_clear,
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

#[derive(Template)]
#[template(path = "job_new.html")]
struct NewJobPage {
    user: Option<User>,
    stl_enabled: bool,
    /// The mounted nozzle, which models are sliced for.
    nozzle: &'static str,
    max_mb: u64,
    spools: Vec<Choice>,
    processes: Vec<Choice>,
    filaments: Vec<Choice>,
    supports: bool,
    infill: String,
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
            let choices = |names: Vec<String>, wanted: &str| -> Vec<Choice> {
                names
                    .into_iter()
                    .map(|name| Choice {
                        selected: name == wanted,
                        value: name.clone(),
                        name,
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
        processes,
        filaments,
        supports: fields.contains_key("supports"),
        infill: if field("infill").is_empty() {
            DEFAULT_INFILL.to_owned()
        } else {
            field("infill").to_owned()
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
    };
    let id = jobs::create(&state.db, &new, store::now()).await?;
    place(temp, &jobs::gcode_path(&state.config.data_dir, id)).await?;
    jobs::store_gcode_info(&state.db, id, &info, None).await?;
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

    // Read once, so the profiles checked here are the ones the model is sliced with.
    let nozzle = state.nozzle.borrow().nozzle;
    let process = field("process");
    if !slicer.processes(nozzle).iter().any(|name| name == process) {
        return Err(user_problem("Choose a print profile."));
    }
    let filaments = slicer.filaments(nozzle);
    let filament = if field("filament").is_empty() {
        default_filament(&filaments, &spool).ok_or_else(|| {
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

    let new = NewJob {
        owner_id: user.id,
        name: &upload.file_name,
        source: Source::Stl,
        process_profile: Some(process),
        filament_profile: Some(&filament),
        supports: Some(supports),
        infill_percent: Some(infill),
    };
    let id = jobs::create(&state.db, &new, store::now()).await?;
    place(temp, &jobs::model_path(&state.config.data_dir, id)).await?;
    let settings = SliceSettings {
        nozzle,
        process: process.to_owned(),
        filament,
        color_hex: spool.color_hex.clone(),
        supports,
        infill_percent: infill,
    };
    tokio::spawn(dispatcher::slice_job(state.clone(), id, settings, spool.id));
    Ok(id)
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
    can_manage: bool,
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
    settings: Option<String>,
    estimate: String,
    error: String,
    progress: i64,
    slicing: bool,
    printing: bool,
    can_cancel: bool,
    can_requeue: bool,
    created: String,
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
    let manage = can_manage(&user, &job);
    let confirm = manage && job.state == JobState::AwaitingConfirm;
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
            "{}, {}, {}% infill, supports {}",
            job.process_profile.as_deref().unwrap_or_default(),
            job.filament_profile.as_deref().unwrap_or_default(),
            job.infill_percent.unwrap_or_default(),
            if job.supports == Some(true) {
                "on"
            } else {
                "off"
            },
        )
    });
    let status = if error.is_some() {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::OK
    };
    let page = JobPage {
        user: Some(user),
        job: JobView {
            id: job.id,
            owner: owner_name(&job),
            state: job.state.label(),
            sliced_for: job.nozzle_mm.map(|mm| format!("{mm} mm")),
            source: match job.source {
                Source::Stl => "Model, sliced here",
                Source::Gcode => "G-code",
            },
            settings,
            estimate: estimate(&job),
            progress: job.progress,
            slicing: job.state == JobState::Slicing,
            printing: job.state == JobState::Printing,
            can_cancel: manage && !job.state.is_finished() && job.state != JobState::Uploading,
            can_requeue: manage && job.state == JobState::Failed && !tools.is_empty(),
            created: views::format_time(job.created_at),
            error: job.error,
            name: job.name,
        },
        tools: tool_rows,
        can_manage: manage,
        confirm,
        blocked,
        notice,
        error,
    };
    Ok((status, render(&page)?).into_response())
}

async fn managed_job(state: &AppState, user: &User, id: i64) -> Result<Job, AppError> {
    let job = jobs::get(&state.db, id).await?.ok_or(AppError::NotFound)?;
    if !can_manage(user, &job) {
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
    let job = managed_job(&state, &current.user, id).await?;
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
        JobState::Uploading => Err(AppError::BadRequest(
            "The job is being sent to the printer. Stop it once it prints.".into(),
        )),
        finished if finished.is_finished() => {
            Err(AppError::BadRequest("This job has already ended.".into()))
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
    let has_gcode = tokio::fs::try_exists(jobs::gcode_path(&state.config.data_dir, id))
        .await
        .unwrap_or(false);
    if job.state != JobState::Failed || !has_gcode || jobs::tools(&state.db, id).await?.is_empty() {
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

#[derive(Template)]
#[template(path = "schedule.html")]
struct SchedulePage {
    user: Option<User>,
    rules: Vec<RuleRow>,
    timezone: String,
    days: [&'static str; 7],
    notice: Option<&'static str>,
    error: Option<String>,
}

struct RuleRow {
    id: i64,
    kind: &'static str,
    label: String,
    days: String,
    hours: String,
    finish: bool,
}

pub async fn schedule_page(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Query(outcome): Query<Outcome>,
) -> Result<Response, AppError> {
    let notice = outcome.done.as_deref().and_then(|code| match code {
        "added" => Some("Rule added."),
        "deleted" => Some("Rule deleted."),
        _ => None,
    });
    schedule_view(&state, admin.user, notice, None).await
}

async fn schedule_view(
    state: &AppState,
    user: User,
    notice: Option<&'static str>,
    error: Option<String>,
) -> Result<Response, AppError> {
    let rules = schedule::rules(&state.db)
        .await?
        .into_iter()
        .map(|(id, rule)| RuleRow {
            id,
            kind: match rule.kind {
                RuleKind::Allow => "Allow",
                RuleKind::Deny => "Deny",
            },
            days: day_summary(rule.days),
            hours: format!(
                "{}–{}",
                schedule::format_time(rule.start_minute),
                schedule::format_time(rule.end_minute)
            ),
            finish: rule.must_finish_before,
            label: rule.label,
        })
        .collect();
    let status = if error.is_some() {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::OK
    };
    let page = SchedulePage {
        user: Some(user),
        rules,
        timezone: state
            .config
            .timezone
            .iana_name()
            .unwrap_or("UTC")
            .to_owned(),
        days: schedule::DAY_NAMES,
        notice,
        error,
    };
    Ok((status, render(&page)?).into_response())
}

fn day_summary(days: u8) -> String {
    match days {
        0b111_1111 => "Every day".into(),
        0b001_1111 => "Weekdays".into(),
        0b110_0000 => "Weekends".into(),
        _ => schedule::DAY_NAMES
            .iter()
            .enumerate()
            .filter(|(bit, _)| days & (1 << bit) != 0)
            .map(|(_, name)| *name)
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// Checkboxes are sent only when ticked, and repeated keys do not deserialize into a list, so
/// each day has its own field.
#[derive(Default, Deserialize)]
#[serde(default)]
pub struct RuleForm {
    kind: String,
    label: String,
    start: String,
    end: String,
    must_finish_before: Option<String>,
    day0: Option<String>,
    day1: Option<String>,
    day2: Option<String>,
    day3: Option<String>,
    day4: Option<String>,
    day5: Option<String>,
    day6: Option<String>,
}

impl RuleForm {
    fn rule(&self) -> Result<Rule, String> {
        let kind = RuleKind::parse(&self.kind).ok_or("Choose allow or deny.")?;
        let days = [
            &self.day0, &self.day1, &self.day2, &self.day3, &self.day4, &self.day5, &self.day6,
        ]
        .iter()
        .enumerate()
        .filter(|(_, ticked)| ticked.is_some())
        .fold(0u8, |days, (bit, _)| days | 1 << bit);
        if days == 0 {
            return Err("Tick at least one day.".into());
        }
        let start = schedule::parse_time(&self.start).ok_or("Enter the start as HH:MM.")?;
        let end = schedule::parse_time(&self.end).ok_or("Enter the end as HH:MM.")?;
        let label = self.label.trim();
        if label.chars().count() > inventory::TEXT_MAX {
            return Err(format!(
                "Names are limited to {} characters.",
                inventory::TEXT_MAX
            ));
        }
        Ok(Rule {
            kind,
            days,
            start_minute: start,
            end_minute: end,
            must_finish_before: self.must_finish_before.is_some(),
            label: if label.is_empty() {
                format!(
                    "{} {}–{}",
                    if kind == RuleKind::Allow {
                        "Allow"
                    } else {
                        "Deny"
                    },
                    schedule::format_time(start),
                    schedule::format_time(end)
                )
            } else {
                label.to_owned()
            },
        })
    }
}

pub async fn add_rule(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Form(form): Form<RuleForm>,
) -> Result<Response, AppError> {
    let rule = match form.rule() {
        Ok(rule) => rule,
        Err(error) => return schedule_view(&state, admin.user, None, Some(error)).await,
    };
    schedule::add_rule(&state.db, &rule, store::now()).await?;
    state.queue_changed.notify_one();
    Ok(Redirect::to("/admin/schedule?done=added").into_response())
}

pub async fn delete_rule(
    State(state): State<AppState>,
    AdminUser(_admin): AdminUser,
    UrlPath(id): UrlPath<i64>,
) -> Result<Response, AppError> {
    schedule::delete_rule(&state.db, id).await?;
    state.queue_changed.notify_one();
    Ok(Redirect::to("/admin/schedule?done=deleted").into_response())
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

    #[test]
    fn day_summaries() {
        assert_eq!(day_summary(0b111_1111), "Every day");
        assert_eq!(day_summary(0b001_1111), "Weekdays");
        assert_eq!(day_summary(0b000_0101), "Mon, Wed");
    }

    #[test]
    fn rule_form_validation() {
        let form = RuleForm {
            kind: "deny".into(),
            start: "22:00".into(),
            end: "07:00".into(),
            day4: Some("on".into()),
            day5: Some("on".into()),
            must_finish_before: Some("on".into()),
            ..RuleForm::default()
        };
        let rule = form.rule().unwrap();
        assert_eq!(rule.days, 0b011_0000);
        assert_eq!((rule.start_minute, rule.end_minute), (1320, 420));
        assert!(rule.must_finish_before);
        assert_eq!(rule.label, "Deny 22:00–07:00");

        assert!(
            RuleForm {
                day4: None,
                day5: None,
                ..form
            }
            .rule()
            .is_err()
        );
    }
}
