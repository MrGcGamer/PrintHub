use askama::Template;
use axum::{
    Form,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};
use serde::Deserialize;

use super::{
    AppState,
    error::{AppError, render},
    session::AdminUser,
    views::format_time,
};
use crate::{
    accounts::{self, AccountError, Role, User},
    auth, store,
};

#[derive(Template)]
#[template(path = "admin_users.html")]
struct UsersPage {
    user: Option<User>,
    users: Vec<UserRow>,
    invites: Vec<InviteRow>,
    link: Option<LinkNotice>,
    notice: Option<&'static str>,
    error: Option<String>,
}

struct UserRow {
    id: i64,
    username: String,
    role: &'static str,
    is_admin: bool,
    disabled: bool,
    joined: String,
    is_me: bool,
}

struct InviteRow {
    id: i64,
    kind: String,
    expires: String,
}

struct LinkNotice {
    what: String,
    url: String,
}

/// Outcomes are passed as fixed codes rather than free text, so a crafted link cannot make
/// the page display a message of the sender's choosing.
#[derive(Deserialize)]
pub struct Outcome {
    done: Option<String>,
    problem: Option<String>,
}

fn done_message(code: &str) -> Option<&'static str> {
    match code {
        "role" => Some("Role changed."),
        "disabled" => Some("Account disabled and logged out everywhere."),
        "enabled" => Some("Account enabled."),
        "revoked" => Some("Link revoked."),
        _ => None,
    }
}

fn problem_message(code: &str) -> Option<&'static str> {
    match code {
        "last-admin" => Some("The last active admin cannot be demoted or disabled."),
        "self-disable" => Some("You cannot disable your own account."),
        _ => None,
    }
}

async fn page(
    state: &AppState,
    admin: User,
    link: Option<LinkNotice>,
    notice: Option<&'static str>,
    error: Option<String>,
) -> Result<Response, AppError> {
    let now = store::now();
    let users = accounts::users(&state.db)
        .await?
        .into_iter()
        .map(|user| UserRow {
            id: user.id,
            is_me: user.id == admin.id,
            role: user.role.as_str(),
            is_admin: user.is_admin(),
            disabled: user.disabled,
            joined: format_time(user.created_at),
            username: user.username,
        })
        .collect();
    let invites = accounts::pending_invites(&state.db, now)
        .await?
        .into_iter()
        .map(|invite| InviteRow {
            id: invite.id,
            kind: match invite.reset_user {
                Some((_, username)) => format!("Password reset for {username}"),
                None => format!("Invite as {}", invite.role.as_str()),
            },
            expires: format_time(invite.expires_at),
        })
        .collect();
    let page = UsersPage {
        user: Some(admin),
        users,
        invites,
        link,
        notice,
        error,
    };
    Ok(render(&page)?.into_response())
}

pub async fn users_page(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Query(outcome): Query<Outcome>,
) -> Result<Response, AppError> {
    let notice = outcome.done.as_deref().and_then(done_message);
    let error = outcome
        .problem
        .as_deref()
        .and_then(problem_message)
        .map(str::to_owned);
    page(&state, admin.user, None, notice, error).await
}

#[derive(Deserialize)]
pub struct RoleForm {
    role: String,
}

pub async fn set_role(
    State(state): State<AppState>,
    AdminUser(_admin): AdminUser,
    Path(id): Path<i64>,
    Form(form): Form<RoleForm>,
) -> Result<Response, AppError> {
    let role = Role::parse(&form.role)
        .ok_or_else(|| AppError::BadRequest(format!("unknown role {:?}", form.role)))?;
    Ok(match accounts::set_role(&state.db, id, role).await {
        Ok(()) => Redirect::to("/admin/users?done=role"),
        Err(AccountError::LastAdmin) => Redirect::to("/admin/users?problem=last-admin"),
        Err(err) => return Err(err.into()),
    }
    .into_response())
}

#[derive(Deserialize)]
pub struct DisabledForm {
    disabled: bool,
}

pub async fn set_disabled(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Path(id): Path<i64>,
    Form(form): Form<DisabledForm>,
) -> Result<Response, AppError> {
    if form.disabled && id == admin.user.id {
        return Ok(Redirect::to("/admin/users?problem=self-disable").into_response());
    }
    Ok(
        match accounts::set_disabled(&state.db, id, form.disabled).await {
            Ok(()) if form.disabled => Redirect::to("/admin/users?done=disabled"),
            Ok(()) => Redirect::to("/admin/users?done=enabled"),
            Err(AccountError::LastAdmin) => Redirect::to("/admin/users?problem=last-admin"),
            Err(err) => return Err(err.into()),
        }
        .into_response(),
    )
}

#[derive(Deserialize)]
pub struct InviteForm {
    role: String,
}

/// Rendered in the POST response rather than redirected to: the link is shown exactly once
/// and must not end up in a URL.
pub async fn create_invite(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    headers: HeaderMap,
    Form(form): Form<InviteForm>,
) -> Result<Response, AppError> {
    let role = Role::parse(&form.role)
        .ok_or_else(|| AppError::BadRequest(format!("unknown role {:?}", form.role)))?;
    let url = issue_link(&state, &headers, &admin.user, role, None).await?;
    let link = LinkNotice {
        what: format!("Invite link for a new {}.", role.as_str()),
        url,
    };
    page(&state, admin.user, Some(link), None, None).await
}

pub async fn reset_link(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let target = accounts::user(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let url = issue_link(&state, &headers, &admin.user, target.role, Some(target.id)).await?;
    let link = LinkNotice {
        what: format!("Password reset link for {}.", target.username),
        url,
    };
    page(&state, admin.user, Some(link), None, None).await
}

async fn issue_link(
    state: &AppState,
    headers: &HeaderMap,
    admin: &User,
    role: Role,
    reset_user: Option<i64>,
) -> Result<String, AppError> {
    let (token, token_hash) = auth::new_token()?;
    let now = store::now();
    let expires = now + auth::INVITE_TTL.as_secs() as i64;
    accounts::create_invite(
        &state.db,
        &token_hash,
        role,
        reset_user,
        admin.id,
        now,
        expires,
    )
    .await?;
    let trust_proxy = state.config.trust_proxy;
    let scheme = if auth::is_https(headers, trust_proxy) {
        "https"
    } else {
        "http"
    };
    let host = auth::request_host(headers, trust_proxy).unwrap_or("localhost");
    Ok(format!("{scheme}://{host}/invite/{token}"))
}

pub async fn revoke_invite(
    State(state): State<AppState>,
    AdminUser(_admin): AdminUser,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    accounts::revoke_invite(&state.db, id).await?;
    Ok(Redirect::to("/admin/users?done=revoked").into_response())
}
