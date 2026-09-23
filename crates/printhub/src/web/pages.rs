use askama::Template;
use axum::{
    Form,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use serde::Deserialize;

use super::{
    AppState,
    error::{AppError, render},
    session::{CurrentUser, current_user},
    views::PrinterCard,
};
use crate::{
    accounts::{self, AccountError, Permission, User},
    auth,
    config::Nozzle,
    jobs, store,
};

#[derive(Template)]
#[template(path = "dashboard.html")]
struct DashboardPage {
    user: Option<User>,
    card: PrinterCard,
    camera_enabled: bool,
    /// Only for those allowed to record the nozzle.
    nozzle_choices: Option<Vec<NozzleChoice>>,
    /// The front of the queue; `queue_total` counts every unfinished job.
    rows: Vec<super::queue::JobRow>,
    queue_total: usize,
    compact: bool,
}

struct NozzleChoice {
    value: &'static str,
    selected: bool,
}

pub async fn dashboard(
    State(state): State<AppState>,
    current: CurrentUser,
) -> Result<Response, AppError> {
    let may_record_nozzle =
        accounts::has_permission(&state.db, &current.user, Permission::SetNozzle).await?;
    let bindings = state.bindings.borrow().clone();
    let mounted = state.nozzle.borrow().clone();
    let snapshot = state.printer.snapshot();
    let card = PrinterCard::new(
        &snapshot,
        &current.user,
        accounts::has_permission(&state.db, &current.user, Permission::ControlPrint).await?,
        &bindings,
        &mounted,
        super::layer_total(&state, &snapshot).await,
    );
    let nozzle_choices = may_record_nozzle.then(|| {
        Nozzle::ALL
            .into_iter()
            .map(|nozzle| NozzleChoice {
                value: nozzle.as_str(),
                selected: nozzle == mounted.nozzle,
            })
            .collect()
    });
    let (rows, queue_total) = super::queue::upcoming_rows(&state, &current.user, 5).await?;
    let page = DashboardPage {
        user: Some(current.user),
        card,
        camera_enabled: state.camera.is_some(),
        nozzle_choices,
        rows,
        queue_total,
        compact: true,
    };
    Ok(render(&page)?.into_response())
}

#[derive(Deserialize)]
pub struct NozzleForm {
    nozzle: String,
}

pub async fn set_nozzle(
    State(state): State<AppState>,
    current: CurrentUser,
    Form(form): Form<NozzleForm>,
) -> Result<Response, AppError> {
    if !accounts::has_permission(&state.db, &current.user, Permission::SetNozzle).await? {
        return Err(AppError::Forbidden);
    }
    let nozzle = Nozzle::parse(&form.nozzle)
        .ok_or_else(|| AppError::BadRequest(format!("unknown nozzle {:?}", form.nozzle)))?;
    jobs::set_mounted_nozzle(&state.db, nozzle, current.user.id, store::now()).await?;
    state.refresh_nozzle().await?;
    tracing::info!(username = %current.user.username, nozzle = nozzle.as_str(), "recorded the mounted nozzle");
    Ok(Redirect::to("/").into_response())
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage {
    user: Option<User>,
    username: String,
    error: Option<String>,
}

pub async fn login_page(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if current_user(&state, &headers).await?.is_some() {
        return Ok(Redirect::to("/").into_response());
    }
    let page = LoginPage {
        user: None,
        username: String::new(),
        error: None,
    };
    Ok(render(&page)?.into_response())
}

#[derive(Deserialize)]
pub struct LoginForm {
    username: String,
    password: String,
}

pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppError> {
    let username = form.username.trim().to_owned();
    if let Some(wait) = state.limiter.retry_after(&username) {
        let message = format!(
            "Too many failed attempts. Try again in {} seconds.",
            wait.as_secs().max(1)
        );
        return login_failed(StatusCode::TOO_MANY_REQUESTS, username, message);
    }

    let (user, hash) = match accounts::login_record(&state.db, &username).await? {
        Some((user, hash)) => (Some(user), Some(hash)),
        None => (None, None),
    };
    let password_matches = auth::verify_password(form.password, hash).await;
    let user = match user {
        Some(user) if password_matches && !user.disabled => user,
        _ => {
            state.limiter.record_failure(&username);
            return login_failed(
                StatusCode::UNAUTHORIZED,
                username,
                "Wrong username or password.".to_owned(),
            );
        }
    };
    state.limiter.record_success(&username);

    let cookie = start_session(&state, &headers, user.id).await?;
    tracing::info!(username = %user.username, "logged in");
    Ok(([(header::SET_COOKIE, cookie)], Redirect::to("/")).into_response())
}

fn login_failed(
    status: StatusCode,
    username: String,
    message: String,
) -> Result<Response, AppError> {
    let page = LoginPage {
        user: None,
        username,
        error: Some(message),
    };
    Ok((status, render(&page)?).into_response())
}

pub async fn start_session(
    state: &AppState,
    headers: &HeaderMap,
    user_id: i64,
) -> Result<String, AppError> {
    let (token, token_hash) = auth::new_token()?;
    let now = store::now();
    let expires = now + auth::SESSION_TTL.as_secs() as i64;
    accounts::create_session(&state.db, &token_hash, user_id, now, expires).await?;
    Ok(auth::session_cookie(
        &token,
        auth::is_https(headers, state.config.trust_proxy),
    ))
}

pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    current: CurrentUser,
) -> Result<Response, AppError> {
    accounts::delete_session(&state.db, &current.token_hash).await?;
    let cookie = auth::clear_session_cookie(auth::is_https(&headers, state.config.trust_proxy));
    Ok(([(header::SET_COOKIE, cookie)], Redirect::to("/login")).into_response())
}

#[derive(Template)]
#[template(path = "invite.html")]
struct InvitePage {
    user: Option<User>,
    token: String,
    reset_username: Option<String>,
    username: String,
    error: Option<String>,
}

pub async fn invite_page(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Response, AppError> {
    let invite = accounts::invite(&state.db, &auth::token_hash(&token), store::now())
        .await?
        .ok_or(AccountError::InviteInvalid)?;
    let page = InvitePage {
        user: None,
        token,
        reset_username: invite.reset_user.map(|(_, name)| name),
        username: String::new(),
        error: None,
    };
    Ok(render(&page)?.into_response())
}

#[derive(Deserialize)]
pub struct RedeemForm {
    #[serde(default)]
    username: String,
    password: String,
    confirm: String,
}

pub async fn redeem(
    State(state): State<AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
    Form(form): Form<RedeemForm>,
) -> Result<Response, AppError> {
    let token_hash = auth::token_hash(&token);
    let invite = accounts::invite(&state.db, &token_hash, store::now())
        .await?
        .ok_or(AccountError::InviteInvalid)?;
    let reset_username = invite.reset_user.map(|(_, name)| name);
    let username = form.username.trim().to_owned();

    let problem = if form.password != form.confirm {
        Some("The passwords do not match.".to_owned())
    } else if reset_username.is_none()
        && let Err(message) = accounts::validate_username(&username)
    {
        Some(message)
    } else {
        accounts::validate_password(&form.password).err()
    };
    let retry = |error: String| {
        let page = InvitePage {
            user: None,
            token: token.clone(),
            reset_username: reset_username.clone(),
            username: username.clone(),
            error: Some(error),
        };
        Ok::<_, AppError>((StatusCode::BAD_REQUEST, render(&page)?).into_response())
    };
    if let Some(error) = problem {
        return retry(error);
    }

    let password_hash = auth::hash_password(form.password).await?;
    match accounts::redeem_invite(
        &state.db,
        &token_hash,
        &username,
        &password_hash,
        store::now(),
    )
    .await
    {
        Ok(user_id) => {
            let cookie = start_session(&state, &headers, user_id).await?;
            Ok(([(header::SET_COOKIE, cookie)], Redirect::to("/")).into_response())
        }
        Err(AccountError::UsernameTaken) => retry("That username is taken.".to_owned()),
        Err(err) => Err(err.into()),
    }
}

#[derive(Template)]
#[template(path = "account.html")]
struct AccountPage {
    user: Option<User>,
    notice: Option<String>,
    error: Option<String>,
}

pub async fn account_page(current: CurrentUser) -> Result<Response, AppError> {
    let page = AccountPage {
        user: Some(current.user),
        notice: None,
        error: None,
    };
    Ok(render(&page)?.into_response())
}

#[derive(Deserialize)]
pub struct ThemeForm {
    theme: String,
}

pub async fn change_theme(
    State(state): State<AppState>,
    mut current: CurrentUser,
    Form(form): Form<ThemeForm>,
) -> Result<Response, AppError> {
    let (status, notice, error) = match accounts::Theme::parse(&form.theme) {
        Some(theme) => {
            accounts::set_theme(&state.db, current.user.id, theme).await?;
            current.user.theme = theme;
            (StatusCode::OK, Some("Theme changed.".to_owned()), None)
        }
        None => (
            StatusCode::BAD_REQUEST,
            None,
            Some("That theme does not exist.".to_owned()),
        ),
    };
    let page = AccountPage {
        user: Some(current.user),
        notice,
        error,
    };
    Ok((status, render(&page)?).into_response())
}

#[derive(Deserialize)]
pub struct PasswordForm {
    current: String,
    password: String,
    confirm: String,
}

pub async fn change_password(
    State(state): State<AppState>,
    current: CurrentUser,
    Form(form): Form<PasswordForm>,
) -> Result<Response, AppError> {
    let stored_hash = accounts::login_record(&state.db, &current.user.username)
        .await?
        .map(|(_, hash)| hash);
    let problem = if !auth::verify_password(form.current, stored_hash).await {
        Some("The current password is wrong.".to_owned())
    } else if form.password != form.confirm {
        Some("The new passwords do not match.".to_owned())
    } else {
        accounts::validate_password(&form.password).err()
    };

    let (status, notice, error) = match problem {
        Some(error) => (StatusCode::BAD_REQUEST, None, Some(error)),
        None => {
            let hash = auth::hash_password(form.password).await?;
            accounts::set_password(&state.db, current.user.id, &hash, Some(&current.token_hash))
                .await?;
            (
                StatusCode::OK,
                Some("Password changed. Other devices have been logged out.".to_owned()),
                None,
            )
        }
    };
    let page = AccountPage {
        user: Some(current.user),
        notice,
        error,
    };
    Ok((status, render(&page)?).into_response())
}
