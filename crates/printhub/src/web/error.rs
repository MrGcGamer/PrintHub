use askama::Template;
use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};

use crate::{
    accounts::{AccountError, User},
    inventory::InventoryError,
    jobs::JobError,
};

#[derive(Debug)]
pub enum AppError {
    NotFound,
    Forbidden,
    BadRequest(String),
    Unavailable(String),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        Self::Internal(err)
    }
}

impl From<AccountError> for AppError {
    fn from(err: AccountError) -> Self {
        match err {
            AccountError::NotFound => Self::NotFound,
            AccountError::Db(err) => Self::Internal(err.into()),
            other => Self::BadRequest(other.to_string()),
        }
    }
}

impl From<InventoryError> for AppError {
    fn from(err: InventoryError) -> Self {
        match err {
            InventoryError::NotFound => Self::NotFound,
            InventoryError::Db(err) => Self::Internal(err.into()),
            other => Self::BadRequest(other.to_string()),
        }
    }
}

impl From<JobError> for AppError {
    fn from(err: JobError) -> Self {
        match err {
            JobError::NotFound => Self::NotFound,
            JobError::Inventory(err) => err.into(),
            JobError::Db(err) => Self::Internal(err.into()),
            other => Self::BadRequest(other.to_string()),
        }
    }
}

impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        Self::Internal(err.into())
    }
}

impl From<askama::Error> for AppError {
    fn from(err: askama::Error) -> Self {
        Self::Internal(anyhow::anyhow!("rendering template: {err}"))
    }
}

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage<'a> {
    user: Option<User>,
    status: u16,
    message: &'a str,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            Self::NotFound => (StatusCode::NOT_FOUND, "Not found.".to_owned()),
            Self::Forbidden => (
                StatusCode::FORBIDDEN,
                "You are not allowed to do that.".to_owned(),
            ),
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message.clone()),
            Self::Unavailable(message) => (StatusCode::SERVICE_UNAVAILABLE, message.clone()),
            Self::Internal(err) => {
                tracing::error!(error = ?err, "request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Something went wrong on the server.".to_owned(),
                )
            }
        };
        let page = ErrorPage {
            user: None,
            status: status.as_u16(),
            message: &message,
        };
        match page.render() {
            Ok(html) => (status, Html(html)).into_response(),
            Err(_) => (status, message).into_response(),
        }
    }
}

pub fn render(template: &impl Template) -> Result<Html<String>, AppError> {
    Ok(Html(template.render()?))
}
