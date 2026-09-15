use axum::{
    extract::FromRequestParts,
    http::{HeaderMap, Method, StatusCode, request::Parts},
    response::{IntoResponse, Redirect, Response},
};

use super::{AppState, error::AppError};
use crate::{
    accounts::{self, User},
    auth, store,
};

pub struct CurrentUser {
    pub user: User,
    pub token_hash: Vec<u8>,
}

pub async fn current_user(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<CurrentUser>, AppError> {
    let Some(token) = auth::cookie_value(headers, auth::SESSION_COOKIE) else {
        return Ok(None);
    };
    let token_hash = auth::token_hash(token);
    let user = accounts::session_user(&state.db, &token_hash, store::now()).await?;
    Ok(user.map(|user| CurrentUser { user, token_hash }))
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        match current_user(state, &parts.headers).await {
            Ok(Some(current)) => Ok(current),
            Ok(None) => Err(login_required(parts)),
            Err(err) => Err(err.into_response()),
        }
    }
}

/// A page load is sent to the login page. An htmx request gets `HX-Redirect` instead, because
/// following a plain redirect would swap the login page into a fragment of the current one.
fn login_required(parts: &Parts) -> Response {
    if parts.headers.contains_key("hx-request") {
        (StatusCode::UNAUTHORIZED, [("HX-Redirect", "/login")]).into_response()
    } else if parts.method == Method::GET {
        Redirect::to("/login").into_response()
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

pub struct AdminUser(pub CurrentUser);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        let current = CurrentUser::from_request_parts(parts, state).await?;
        if !current.user.is_admin() {
            return Err(AppError::Forbidden.into_response());
        }
        Ok(Self(current))
    }
}
