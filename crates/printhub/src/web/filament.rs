//! Filament pages: the spool list, tray bindings, and each spool's weigh-ins.

use askama::Template;
use axum::{
    Form,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
};
use serde::Deserialize;

use super::{
    AppState,
    error::{AppError, render},
    session::CurrentUser,
    views::{self, TrayView},
};
use crate::{
    accounts::{self, User},
    cc2::model::Tray,
    inventory::{self, Binding, ConsumptionKind, Spool, SpoolFields},
    store,
};

#[derive(Template)]
#[template(path = "inventory.html")]
struct InventoryPage {
    user: Option<User>,
    trays: Vec<TrayRow>,
    spools: Vec<SpoolRow>,
    show_archived: bool,
    notice: Option<&'static str>,
    error: Option<&'static str>,
}

struct TrayRow {
    tray: TrayView,
    choices: Vec<Choice>,
    /// Filament in the tray that no spool is recorded for: the printer knows enough about it
    /// to fill in a new spool.
    unrecorded: bool,
}

struct Choice {
    id: i64,
    name: String,
    selected: bool,
}

struct SpoolRow {
    id: i64,
    name: String,
    color: String,
    owner: String,
    remaining: String,
    initial: String,
    /// Empty when no price was entered.
    price: String,
    tray: String,
    archived: bool,
}

/// Outcomes are fixed codes, as on the users page, so a crafted link cannot put text of the
/// sender's choosing on the page.
#[derive(Deserialize)]
pub struct InventoryQuery {
    #[serde(default)]
    archived: bool,
    done: Option<String>,
    problem: Option<String>,
}

pub async fn inventory_page(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(query): Query<InventoryQuery>,
) -> Result<Response, AppError> {
    let spools = inventory::spools(&state.db, query.archived).await?;
    let snapshot = state.printer.snapshot();
    let bindings = state.bindings.borrow().clone();
    let loadable: Vec<Spool> = spools
        .iter()
        .filter(|spool| !spool.archived)
        .cloned()
        .collect();

    let trays = views::trays(&snapshot, &bindings)
        .into_iter()
        .map(|tray| {
            let reported = snapshot
                .canvas
                .as_ref()
                .and_then(|canvas| inventory::find_tray(canvas, tray.canvas_id, tray.tray_id));
            let ranked = match reported {
                Some(reported) => inventory::rank_for_tray(loadable.clone(), reported),
                None => loadable.clone(),
            };
            let bound = tray.spool.as_ref().map(|spool| spool.id);
            let choices = ranked
                .iter()
                .enumerate()
                .map(|(rank, spool)| Choice {
                    id: spool.id,
                    name: views::spool_name(spool),
                    selected: bound.map_or(rank == 0, |id| id == spool.id),
                })
                .collect();
            let unrecorded = tray.loaded && tray.spool.is_none();
            TrayRow {
                tray,
                choices,
                unrecorded,
            }
        })
        .collect();

    let spools = spools
        .iter()
        .map(|spool| SpoolRow {
            id: spool.id,
            name: views::spool_name(spool),
            color: views::safe_color(&spool.color_hex),
            owner: owner_name(spool),
            remaining: views::grams(spool.remaining_grams),
            initial: views::grams(spool.initial_grams),
            price: spool
                .price_cents
                .map(inventory::format_price)
                .unwrap_or_default(),
            tray: tray_of(&bindings, spool.id).unwrap_or_default(),
            archived: spool.archived,
        })
        .collect();

    let page = InventoryPage {
        user: Some(current.user),
        trays,
        spools,
        show_archived: query.archived,
        notice: query.done.as_deref().and_then(|code| match code {
            "bound" => Some("Spool set."),
            "unbound" => Some("Tray cleared."),
            "refreshed" => Some("Trays refreshed."),
            _ => None,
        }),
        error: query.problem.as_deref().and_then(|code| match code {
            "offline" => Some("The printer did not answer, so the trays were not refreshed."),
            _ => None,
        }),
    };
    Ok(render(&page)?.into_response())
}

fn owner_name(spool: &Spool) -> String {
    spool
        .owner
        .as_ref()
        .map_or_else(|| "Shared".to_owned(), |(_, name)| name.clone())
}

fn tray_of(bindings: &[Binding], spool_id: i64) -> Option<String> {
    bindings
        .iter()
        .find(|binding| binding.spool.id == spool_id)
        .map(|binding| views::tray_label(binding.canvas_id, binding.tray_id))
}

#[derive(Template)]
#[template(path = "spool.html")]
struct SpoolPage {
    user: Option<User>,
    spool: SpoolDetail,
    history: Vec<HistoryRow>,
    can_edit: bool,
    weigh_value: String,
    notice: Option<&'static str>,
    error: Option<String>,
}

struct SpoolDetail {
    id: i64,
    name: String,
    color: String,
    owner: String,
    remaining: String,
    initial: String,
    price: Option<String>,
    notes: String,
    archived: bool,
    tray: String,
    added: String,
}

struct HistoryRow {
    when: String,
    what: String,
    change: String,
    who: String,
}

#[derive(Deserialize)]
pub struct SpoolOutcome {
    done: Option<String>,
}

pub async fn spool_page(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Query(outcome): Query<SpoolOutcome>,
) -> Result<Response, AppError> {
    let notice = outcome.done.as_deref().and_then(|code| match code {
        "created" => Some("Spool added."),
        "created-bound" => Some("Spool added and set in its tray."),
        "saved" => Some("Changes saved."),
        "weighed" => Some("Weigh-in recorded."),
        "archived" => Some("Spool archived."),
        "restored" => Some("Spool restored."),
        _ => None,
    });
    let detail = Detail {
        notice,
        error: None,
        weigh_value: String::new(),
        status: StatusCode::OK,
    };
    detail_page(&state, current.user, id, detail).await
}

struct Detail {
    notice: Option<&'static str>,
    error: Option<String>,
    weigh_value: String,
    status: StatusCode,
}

async fn detail_page(
    state: &AppState,
    user: User,
    id: i64,
    detail: Detail,
) -> Result<Response, AppError> {
    let spool = inventory::spool(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let history = inventory::history(&state.db, id)
        .await?
        .into_iter()
        .map(|entry| {
            let kind = match entry.kind {
                ConsumptionKind::Print => "Print",
                ConsumptionKind::Estimate => "Unfinished print, estimated",
                ConsumptionKind::WeighIn => "Weigh-in",
            };
            HistoryRow {
                when: views::format_time(entry.created_at),
                what: if entry.note.is_empty() {
                    kind.to_owned()
                } else {
                    format!("{kind}: {}", entry.note)
                },
                change: ledger_change(entry.grams),
                who: entry.username.unwrap_or_default(),
            }
        })
        .collect();
    let tray = tray_of(&state.bindings.borrow(), id);

    let page = SpoolPage {
        can_edit: spool.editable_by(&user),
        user: Some(user),
        spool: SpoolDetail {
            id: spool.id,
            name: views::spool_name(&spool),
            color: views::safe_color(&spool.color_hex),
            owner: owner_name(&spool),
            remaining: views::grams(spool.remaining_grams),
            initial: views::grams(spool.initial_grams),
            price: spool.price_cents.map(inventory::format_price),
            notes: spool.notes,
            archived: spool.archived,
            tray: tray.unwrap_or_else(|| "Not in a tray".to_owned()),
            added: views::format_time(spool.created_at),
        },
        history,
        weigh_value: detail.weigh_value,
        notice: detail.notice,
        error: detail.error,
    };
    Ok((detail.status, render(&page)?).into_response())
}

fn ledger_change(grams: f64) -> String {
    let rounded = grams.round();
    if rounded > 0.0 {
        format!("{rounded:.0} g used")
    } else if rounded < 0.0 {
        format!("{:.0} g found", -rounded)
    } else {
        "No change".to_owned()
    }
}

#[derive(Template)]
#[template(path = "spool_form.html")]
struct SpoolFormPage {
    user: Option<User>,
    action: String,
    heading: &'static str,
    submit: &'static str,
    form: SpoolForm,
    owners: Vec<OwnerChoice>,
    error: Option<String>,
}

struct OwnerChoice {
    id: i64,
    username: String,
    selected: bool,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct SpoolForm {
    material: String,
    brand: String,
    color_name: String,
    color_hex: String,
    initial_grams: String,
    price: String,
    notes: String,
    /// `None` when the form had no owner field, which keeps the owner; empty means shared.
    owner_id: Option<String>,
    /// The tray the spool was read off, carried through so it is bound on creation.
    canvas: Option<String>,
    tray: Option<String>,
}

impl SpoolForm {
    fn from_spool(spool: &Spool) -> Self {
        Self {
            material: spool.material.clone(),
            brand: spool.brand.clone(),
            color_name: spool.color_name.clone(),
            color_hex: spool.color_hex.clone(),
            initial_grams: spool.initial_grams.to_string(),
            price: spool
                .price_cents
                .map(inventory::format_price)
                .unwrap_or_default(),
            notes: spool.notes.clone(),
            owner_id: Some(
                spool
                    .owner
                    .as_ref()
                    .map(|(id, _)| id.to_string())
                    .unwrap_or_default(),
            ),
            canvas: None,
            tray: None,
        }
    }

    fn fields(&self) -> Result<SpoolFields, String> {
        let material = text("Material", &self.material, inventory::TEXT_MAX)?;
        if material.is_empty() {
            return Err("Enter the material, like PLA.".to_owned());
        }
        Ok(SpoolFields {
            material,
            brand: text("Brand", &self.brand, inventory::TEXT_MAX)?,
            color_name: text("Colour name", &self.color_name, inventory::TEXT_MAX)?,
            color_hex: inventory::normalize_color(&self.color_hex)
                .ok_or_else(|| "Pick a colour.".to_owned())?,
            price_cents: inventory::parse_price(&self.price)?,
            initial_grams: inventory::parse_grams(&self.initial_grams)?,
            notes: text("Notes", &self.notes, inventory::NOTES_MAX)?,
        })
    }
}

fn text(label: &str, raw: &str, max: usize) -> Result<String, String> {
    let value = raw.trim();
    if value.chars().count() > max {
        return Err(format!("{label} is limited to {max} characters."));
    }
    Ok(value.to_owned())
}

async fn form_page(
    state: &AppState,
    user: User,
    editing: Option<i64>,
    form: SpoolForm,
    error: Option<String>,
) -> Result<Response, AppError> {
    let owners = if user.is_admin() {
        accounts::users(&state.db)
            .await?
            .into_iter()
            .filter_map(|account| {
                let selected = form.owner_id.as_deref() == Some(account.id.to_string().as_str());
                (selected || !account.disabled).then_some(OwnerChoice {
                    id: account.id,
                    username: account.username,
                    selected,
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    let status = if error.is_some() {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::OK
    };
    let page = SpoolFormPage {
        user: Some(user),
        action: editing.map_or_else(|| "/spools".to_owned(), |id| format!("/spools/{id}")),
        heading: if editing.is_some() {
            "Edit spool"
        } else {
            "Add a spool"
        },
        submit: if editing.is_some() {
            "Save changes"
        } else {
            "Add spool"
        },
        form,
        owners,
        error,
    };
    Ok((status, render(&page)?).into_response())
}

/// Members cannot reassign a spool, and what they add is theirs.
async fn chosen_owner(
    state: &AppState,
    user: &User,
    form: &SpoolForm,
    existing: Option<&Spool>,
) -> Result<Option<i64>, AppError> {
    let unchanged = match existing {
        Some(spool) => spool.owner.as_ref().map(|(id, _)| *id),
        None => Some(user.id),
    };
    let raw = match form.owner_id.as_deref().map(str::trim) {
        Some(raw) if user.is_admin() => raw,
        _ => return Ok(unchanged),
    };
    if raw.is_empty() {
        return Ok(None);
    }
    let unknown = || AppError::BadRequest("That owner does not exist.".to_owned());
    let id = raw.parse().map_err(|_| unknown())?;
    accounts::user(&state.db, id).await?.ok_or_else(unknown)?;
    Ok(Some(id))
}

async fn editable(state: &AppState, user: &User, id: i64) -> Result<Spool, AppError> {
    let spool = inventory::spool(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    if !spool.editable_by(user) {
        return Err(AppError::Forbidden);
    }
    Ok(spool)
}

#[derive(Deserialize)]
pub struct FromTray {
    canvas: Option<i64>,
    tray: Option<i64>,
}

pub async fn new_spool_page(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(from): Query<FromTray>,
) -> Result<Response, AppError> {
    let mut form = SpoolForm {
        color_hex: "#FFFFFF".to_owned(),
        initial_grams: "1000".to_owned(),
        owner_id: Some(current.user.id.to_string()),
        ..SpoolForm::default()
    };
    if let (Some(canvas_id), Some(tray_id)) = (from.canvas, from.tray) {
        let snapshot = state.printer.snapshot();
        let reported = snapshot
            .canvas
            .as_ref()
            .and_then(|canvas| inventory::find_tray(canvas, canvas_id, tray_id))
            .filter(|tray| tray.has_filament());
        if let Some(tray) = reported {
            // Everything else stays at its default: the printer reports no weight or price, and
            // `filament_name` is a material name, not a colour name.
            form.material = tray.filament_type.clone();
            form.brand = tray.brand.clone();
            form.color_hex = inventory::normalize_color(&tray.filament_color)
                .unwrap_or_else(|| form.color_hex.clone());
            form.canvas = Some(canvas_id.to_string());
            form.tray = Some(tray_id.to_string());
        }
    }
    form_page(&state, current.user, None, form, None).await
}

pub async fn create_spool(
    State(state): State<AppState>,
    current: CurrentUser,
    Form(form): Form<SpoolForm>,
) -> Result<Response, AppError> {
    let fields = match form.fields() {
        Ok(fields) => fields,
        Err(error) => return form_page(&state, current.user, None, form, Some(error)).await,
    };
    let owner_id = chosen_owner(&state, &current.user, &form, None).await?;
    let id = inventory::create_spool(&state.db, &fields, owner_id, store::now()).await?;
    tracing::info!(username = %current.user.username, spool = id, "added a spool");
    // Built from a tray's own filament, so it belongs in that tray. A tray emptied in the
    // meantime just leaves the spool unbound.
    if let (Some(canvas_id), Some(tray_id)) = (
        form.canvas.as_deref().and_then(|raw| raw.parse().ok()),
        form.tray.as_deref().and_then(|raw| raw.parse().ok()),
    ) && bind_if_loaded(&state, canvas_id, tray_id, id).await?
    {
        return Ok(Redirect::to(&format!("/spools/{id}?done=created-bound")).into_response());
    }
    Ok(Redirect::to(&format!("/spools/{id}?done=created")).into_response())
}

pub async fn edit_spool_page(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let spool = editable(&state, &current.user, id).await?;
    form_page(
        &state,
        current.user,
        Some(id),
        SpoolForm::from_spool(&spool),
        None,
    )
    .await
}

pub async fn update_spool(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Form(form): Form<SpoolForm>,
) -> Result<Response, AppError> {
    let spool = editable(&state, &current.user, id).await?;
    let fields = match form.fields() {
        Ok(fields) => fields,
        Err(error) => return form_page(&state, current.user, Some(id), form, Some(error)).await,
    };
    let owner_id = chosen_owner(&state, &current.user, &form, Some(&spool)).await?;
    inventory::update_spool(&state.db, id, &fields, owner_id).await?;
    state.refresh_bindings().await?;
    Ok(Redirect::to(&format!("/spools/{id}?done=saved")).into_response())
}

#[derive(Deserialize)]
pub struct ArchivedForm {
    archived: bool,
}

pub async fn set_archived(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Form(form): Form<ArchivedForm>,
) -> Result<Response, AppError> {
    editable(&state, &current.user, id).await?;
    inventory::set_archived(&state.db, id, form.archived).await?;
    state.refresh_bindings().await?;
    let done = if form.archived {
        "archived"
    } else {
        "restored"
    };
    Ok(Redirect::to(&format!("/spools/{id}?done={done}")).into_response())
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct WeighForm {
    grams: String,
}

pub async fn weigh_in(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Form(form): Form<WeighForm>,
) -> Result<Response, AppError> {
    let grams = match inventory::parse_grams(&form.grams) {
        Ok(grams) => grams,
        Err(error) => {
            let detail = Detail {
                notice: None,
                error: Some(error),
                weigh_value: form.grams,
                status: StatusCode::BAD_REQUEST,
            };
            return detail_page(&state, current.user, id, detail).await;
        }
    };
    inventory::weigh_in(&state.db, id, current.user.id, grams, store::now()).await?;
    state.refresh_bindings().await?;
    Ok(Redirect::to(&format!("/spools/{id}?done=weighed")).into_response())
}

/// The client polls the CANVAS only every 30 s, too slow to confirm a spool just loaded.
pub async fn refresh_trays(State(state): State<AppState>, _current: CurrentUser) -> Response {
    let refreshed = match state.printer.client() {
        Some(client) => match client.canvas().await {
            Ok(_) => true,
            Err(err) => {
                tracing::warn!(%err, "refreshing trays failed");
                false
            }
        },
        None => false,
    };
    let to = if refreshed {
        "/inventory?done=refreshed"
    } else {
        "/inventory?problem=offline"
    };
    Redirect::to(to).into_response()
}

#[derive(Deserialize)]
pub struct BindForm {
    spool_id: i64,
}

/// Only a tray with filament can take a spool: an empty tray's binding would be removed again
/// by the next status update.
/// Binds `spool_id` into the tray when the printer still reports filament in it. `false`
/// means it does not, and nothing was written.
async fn bind_if_loaded(
    state: &AppState,
    canvas_id: i64,
    tray_id: i64,
    spool_id: i64,
) -> Result<bool, AppError> {
    let snapshot = state.printer.snapshot();
    let loaded = snapshot
        .canvas
        .as_ref()
        .and_then(|canvas| inventory::find_tray(canvas, canvas_id, tray_id))
        .is_some_and(Tray::has_filament);
    if !loaded {
        return Ok(false);
    }
    inventory::bind(&state.db, canvas_id, tray_id, spool_id, store::now()).await?;
    state.refresh_bindings().await?;
    Ok(true)
}

pub async fn bind_tray(
    State(state): State<AppState>,
    current: CurrentUser,
    Path((canvas_id, tray_id)): Path<(i64, i64)>,
    Form(form): Form<BindForm>,
) -> Result<Response, AppError> {
    if !bind_if_loaded(&state, canvas_id, tray_id, form.spool_id).await? {
        return Err(AppError::BadRequest(
            "That tray has no filament in it.".to_owned(),
        ));
    }
    tracing::info!(
        username = %current.user.username,
        spool = form.spool_id,
        tray = %views::tray_label(canvas_id, tray_id),
        "set tray spool"
    );
    Ok(Redirect::to("/inventory?done=bound").into_response())
}

pub async fn unbind_tray(
    State(state): State<AppState>,
    _current: CurrentUser,
    Path((canvas_id, tray_id)): Path<(i64, i64)>,
) -> Result<Response, AppError> {
    inventory::unbind_trays(&state.db, &[(canvas_id, tray_id)]).await?;
    state.refresh_bindings().await?;
    Ok(Redirect::to("/inventory?done=unbound").into_response())
}
