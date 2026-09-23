//! What jobs keep on disk, for admins to see and delete.

use askama::Template;
use axum::{
    extract::{Path, Query, State},
    response::{IntoResponse, Redirect, Response},
};
use serde::Deserialize;

use super::{
    AppState,
    error::{AppError, render},
    queue::exists,
    session::AdminUser,
    views,
};
use crate::{accounts::User, jobs, store};

#[derive(Template)]
#[template(path = "storage.html")]
struct StoragePage {
    user: Option<User>,
    rows: Vec<StoredRow>,
    total: String,
    notice: Option<&'static str>,
    error: Option<&'static str>,
}

struct StoredRow {
    id: i64,
    /// `None` when the directory outlived its job.
    name: Option<String>,
    owner: String,
    state: &'static str,
    uploaded: String,
    age: String,
    size: String,
    preview: bool,
    /// Files of a job still in the queue or printing are not offered for deletion.
    removable: bool,
}

/// Outcomes are fixed codes, so a crafted link cannot put text of the sender's choosing on the
/// page.
#[derive(Deserialize)]
pub struct Outcome {
    done: Option<String>,
    problem: Option<String>,
}

pub async fn storage_page(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Query(outcome): Query<Outcome>,
) -> Result<Response, AppError> {
    let data_dir = &state.config.data_dir;
    let mut stored = jobs::stored(data_dir)
        .await
        .map_err(|err| AppError::Internal(err.into()))?;
    stored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let now = store::now();
    let mut rows = Vec::new();
    for (id, bytes) in &stored {
        let job = jobs::get(&state.db, *id).await?;
        let has_preview = exists(&jobs::preview_path(data_dir, *id)).await
            || exists(&jobs::gcode_path(data_dir, *id)).await;
        rows.push(match job {
            Some(job) => StoredRow {
                id: *id,
                owner: job.owner.map_or_else(String::new, |(_, username)| username),
                state: job.state.label(),
                uploaded: views::format_time(job.created_at),
                age: age(now - job.created_at),
                size: size(*bytes),
                preview: has_preview,
                removable: job.state.is_finished(),
                name: Some(job.name),
            },
            None => StoredRow {
                id: *id,
                name: None,
                owner: String::new(),
                state: "",
                uploaded: String::new(),
                age: String::new(),
                size: size(*bytes),
                preview: false,
                removable: true,
            },
        });
    }
    let page = StoragePage {
        user: Some(admin.user),
        rows,
        total: size(stored.iter().map(|(_, bytes)| bytes).sum()),
        notice: outcome.done.as_deref().and_then(|code| match code {
            "deleted" => Some("Files deleted."),
            _ => None,
        }),
        error: outcome.problem.as_deref().and_then(|code| match code {
            "active" => Some("That job is still in the queue or printing, so its files stay."),
            _ => None,
        }),
    };
    Ok(render(&page)?.into_response())
}

pub async fn delete_files(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    if let Some(job) = jobs::get(&state.db, id).await?
        && !job.state.is_finished()
    {
        return Ok(Redirect::to("/admin/storage?problem=active").into_response());
    }
    jobs::remove_files(&state.config.data_dir, id)
        .await
        .map_err(|err| AppError::Internal(err.into()))?;
    tracing::info!(username = %admin.user.username, job = id, "deleted a job's files");
    Ok(Redirect::to("/admin/storage?done=deleted").into_response())
}

/// Binary units, as `MAX_UPLOAD_MB` counts them.
fn size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let bytes = bytes as f64;
    if bytes < KB {
        format!("{bytes} B")
    } else if bytes < KB * KB {
        format!("{:.0} KB", bytes / KB)
    } else if bytes < KB * KB * KB {
        format!("{:.1} MB", bytes / (KB * KB))
    } else {
        format!("{:.2} GB", bytes / (KB * KB * KB))
    }
}

fn age(seconds: i64) -> String {
    match seconds / 86_400 {
        ..=0 => "today".to_owned(),
        1 => "1 day".to_owned(),
        days => format!("{days} days"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_ages_read_naturally() {
        assert_eq!(size(512), "512 B");
        assert_eq!(size(40 * 1024), "40 KB");
        assert_eq!(size(34 * 1024 * 1024 + 300 * 1024), "34.3 MB");
        assert_eq!(size(3 * 1024 * 1024 * 1024), "3.00 GB");
        assert_eq!(age(3600), "today");
        assert_eq!(age(86_400 + 5), "1 day");
        assert_eq!(age(30 * 86_400), "30 days");
    }
}
