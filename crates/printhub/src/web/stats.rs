//! Statistics pages: who prints how much and with whose filament, and the balances that leaves.
//! Everyone sees everyone's numbers.

use std::collections::{BTreeSet, HashMap};

use askama::Template;
use axum::{
    Form,
    extract::{Path, Query, State},
    response::{IntoResponse, Redirect, Response},
};
use jiff::civil::Date;
use serde::Deserialize;

use super::{
    AppState,
    chart::{self, BarChart, BarInput, Series},
    error::{AppError, render},
    session::{AdminUser, CurrentUser},
    views,
};
use crate::{
    accounts::{self, User},
    inventory,
    stats::{self, Debt, Granularity, Pair, Period, Person, Summary, Whose},
    store,
};

const RECENT_PAYMENTS: usize = 20;
const RECENT_PRINTS: i64 = 20;

#[derive(Deserialize)]
pub struct StatsQuery {
    period: Option<String>,
    done: Option<String>,
    problem: Option<String>,
}

impl StatsQuery {
    fn period(&self) -> Period {
        self.period
            .as_deref()
            .and_then(Period::parse)
            .unwrap_or(Period::AllTime)
    }
}

struct PeriodLink {
    href: String,
    label: &'static str,
    current: bool,
}

struct Tile {
    label: &'static str,
    value: String,
}

struct PersonRow {
    href: String,
    name: String,
    prints: u32,
    unfinished: u32,
    time: String,
    filament: String,
    own: String,
    shared: String,
    others: String,
    others_value: String,
    wasted: String,
}

struct Matrix {
    owners: Vec<String>,
    rows: Vec<MatrixRow>,
}

struct MatrixRow {
    name: String,
    cells: Vec<String>,
}

struct DebtRow {
    debtor: String,
    creditor: String,
    debtor_id: i64,
    creditor_id: i64,
    amount: String,
    can_record: bool,
}

struct PaymentRow {
    id: i64,
    when: String,
    from: String,
    to: String,
    amount: String,
    recorded_by: String,
}

struct AmountRow {
    label: String,
    grams: String,
    value: String,
}

#[derive(Template)]
#[template(path = "stats.html")]
struct StatsPage {
    user: Option<User>,
    periods: Vec<PeriodLink>,
    tiles: Vec<Tile>,
    people: Vec<PersonRow>,
    unit: &'static str,
    chart: Option<BarChart>,
    matrix: Matrix,
    debts: Vec<DebtRow>,
    payments: Vec<PaymentRow>,
    can_delete_payments: bool,
    materials: Vec<AmountRow>,
    unaccounted: Vec<AmountRow>,
    notice: Option<&'static str>,
    error: Option<&'static str>,
}

/// Everyone who ever had an account, in id order, which is also the order chart colours are
/// handed out in.
struct People {
    users: Vec<User>,
    names: HashMap<i64, String>,
}

impl People {
    async fn load(state: &AppState) -> Result<Self, AppError> {
        let mut users = accounts::users(&state.db).await?;
        users.sort_by_key(|user| user.id);
        let names = users
            .iter()
            .map(|user| (user.id, user.username.clone()))
            .collect();
        Ok(Self { users, names })
    }

    fn name(&self, id: i64) -> String {
        self.names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| "Unknown user".to_owned())
    }

    fn owner(&self, owner: Option<i64>) -> String {
        owner.map_or_else(|| "Shared".to_owned(), |id| self.name(id))
    }
}

struct Loaded {
    period: Period,
    since: i64,
    ledger: stats::Ledger,
    summary: Summary,
    people: People,
    payments: Vec<stats::Settlement>,
    debts: Vec<Debt>,
}

async fn load(state: &AppState, period: Period) -> Result<Loaded, AppError> {
    let tz = &state.config.timezone;
    let now = store::now();
    let since = period.since(now, tz);
    let ledger = stats::load(&state.db, since).await?;
    let summary = stats::summarize(&ledger, period, since, now, tz);
    let payments = stats::settlements(&state.db).await?;
    let debts = stats::balances(&stats::owed(&state.db).await?, &payments);
    Ok(Loaded {
        period,
        since,
        ledger,
        summary,
        people: People::load(state).await?,
        payments,
        debts,
    })
}

pub async fn stats_page(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(query): Query<StatsQuery>,
) -> Result<Response, AppError> {
    let Loaded {
        period,
        summary,
        people,
        payments,
        debts,
        ..
    } = load(&state, query.period()).await?;
    let viewer = &current.user;

    let rows = summary
        .people
        .iter()
        .map(|person| PersonRow {
            href: format!("/stats/users/{}?period={}", person.user_id, period.as_str()),
            name: people.name(person.user_id),
            prints: person.prints(),
            unfinished: person.failed + person.cancelled,
            time: duration(person.print_seconds),
            filament: weight(person.grams()),
            own: weight(person.own_grams),
            shared: weight(person.shared_grams),
            others: format!(
                "{} ({})",
                weight(person.others_grams),
                percent(person.others_grams, person.grams())
            ),
            others_value: money(person.others_value_cents),
            wasted: weight(person.wasted_grams),
        })
        .collect();

    let page = StatsPage {
        periods: period_links("/stats", period),
        tiles: tiles(summary.people.iter(), None),
        people: rows,
        unit: unit(summary.granularity),
        chart: people_chart(&summary, &people),
        matrix: matrix(&summary, &people),
        debts: debt_rows(&debts, &people, Some(viewer)),
        payments: payments
            .iter()
            .take(RECENT_PAYMENTS)
            .map(|payment| PaymentRow {
                id: payment.id,
                when: views::format_time(payment.created_at),
                from: payment.from.1.clone(),
                to: payment.to.1.clone(),
                amount: inventory::format_price(payment.amount_cents),
                recorded_by: payment.recorded_by.clone().unwrap_or_default(),
            })
            .collect(),
        can_delete_payments: viewer.is_admin(),
        materials: materials(&summary),
        unaccounted: summary
            .unaccounted
            .iter()
            .map(|entry| AmountRow {
                label: people.owner(entry.owner_id),
                grams: if entry.grams >= 0.0 {
                    format!("{} missing", weight(entry.grams))
                } else {
                    format!("{} more than recorded", weight(-entry.grams))
                },
                value: entry
                    .value_cents
                    .map(|cents| money(cents.abs()))
                    .unwrap_or_default(),
            })
            .collect(),
        notice: query.done.as_deref().and_then(|code| match code {
            "paid" => Some("Payment recorded."),
            "payment-deleted" => Some("Payment deleted."),
            _ => None,
        }),
        error: query.problem.as_deref().and_then(|code| match code {
            "payment-amount" => Some("Enter the amount paid, like 4.20."),
            "payment-people" => Some("A payment needs two different people."),
            _ => None,
        }),
        user: Some(current.user),
    };
    Ok(render(&page)?.into_response())
}

#[derive(Template)]
#[template(path = "stats_user.html")]
struct UserStatsPage {
    user: Option<User>,
    name: String,
    overview: String,
    periods: Vec<PeriodLink>,
    tiles: Vec<Tile>,
    debts: Vec<DebtRow>,
    unit: &'static str,
    chart: Option<BarChart>,
    used: Vec<AmountRow>,
    used_by: Vec<AmountRow>,
    materials: Vec<AmountRow>,
    prints: Vec<PrintRow>,
}

struct PrintRow {
    job_id: i64,
    when: String,
    name: String,
    outcome: &'static str,
    grams: String,
    others_value: String,
}

pub async fn user_stats_page(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<i64>,
    Query(query): Query<StatsQuery>,
) -> Result<Response, AppError> {
    let subject = accounts::user(&state.db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let Loaded {
        period,
        since,
        ledger,
        summary,
        people,
        debts,
        ..
    } = load(&state, query.period()).await?;
    let tz = &state.config.timezone;
    let own = stats::summarize(&ledger.of_user(id), period, since, store::now(), tz);
    let person = own.people.first();

    let pair_row = |label: String, pair: &Pair, show_value: bool| AmountRow {
        label,
        grams: weight(pair.grams),
        value: pair
            .value_cents
            .filter(|_| show_value)
            .map(money)
            .unwrap_or_default(),
    };
    let used = summary
        .pairs
        .iter()
        .filter(|pair| pair.user_id == id)
        .map(|pair| {
            pair_row(
                people.owner(pair.owner_id),
                pair,
                pair.owner_id.is_some_and(|owner| owner != id),
            )
        })
        .collect();
    let used_by = summary
        .pairs
        .iter()
        .filter(|pair| pair.owner_id == Some(id) && pair.user_id != id)
        .map(|pair| pair_row(people.name(pair.user_id), pair, true))
        .collect();

    let prints = stats::recent_prints(&state.db, id, since, RECENT_PRINTS)
        .await?
        .into_iter()
        .map(|print| PrintRow {
            job_id: print.job_id,
            when: views::format_time(print.finished_at),
            name: print.name,
            outcome: print.state.label(),
            grams: weight(print.grams),
            others_value: print.others_value_cents.map(money).unwrap_or_default(),
        })
        .collect();

    let involved: Vec<Debt> = debts
        .into_iter()
        .filter(|debt| debt.debtor == id || debt.creditor == id)
        .collect();
    let page = UserStatsPage {
        name: subject.username,
        overview: format!("/stats?period={}", period.as_str()),
        periods: period_links(&format!("/stats/users/{id}"), period),
        tiles: tiles(person.into_iter(), person),
        debts: debt_rows(&involved, &people, None),
        unit: unit(own.granularity),
        chart: whose_chart(&own),
        used,
        used_by,
        materials: materials(&own),
        prints,
        user: Some(current.user),
    };
    Ok(render(&page)?.into_response())
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct PaymentForm {
    from_user: String,
    to_user: String,
    amount: String,
}

pub async fn record_payment(
    State(state): State<AppState>,
    current: CurrentUser,
    Form(form): Form<PaymentForm>,
) -> Result<Response, AppError> {
    let parse = |raw: &str| {
        raw.trim()
            .parse::<i64>()
            .map_err(|_| AppError::BadRequest("Choose who paid whom.".to_owned()))
    };
    let (from, to) = (parse(&form.from_user)?, parse(&form.to_user)?);
    // Whoever received the money says so, or an admin: nobody clears their own debt.
    if current.user.id != to && !current.user.is_admin() {
        return Err(AppError::Forbidden);
    }
    if from == to {
        return Ok(Redirect::to("/stats?problem=payment-people#balances").into_response());
    }
    for id in [from, to] {
        accounts::user(&state.db, id)
            .await?
            .ok_or_else(|| AppError::BadRequest("That person does not exist.".to_owned()))?;
    }
    let amount = match inventory::parse_price(&form.amount) {
        Ok(Some(cents)) if cents > 0 => cents,
        _ => return Ok(Redirect::to("/stats?problem=payment-amount#balances").into_response()),
    };
    stats::record_settlement(&state.db, from, to, amount, current.user.id, store::now()).await?;
    tracing::info!(
        username = %current.user.username,
        from,
        to,
        amount_cents = amount,
        "recorded a payment"
    );
    Ok(Redirect::to("/stats?done=paid#balances").into_response())
}

pub async fn delete_payment(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    if !stats::delete_settlement(&state.db, id).await? {
        return Err(AppError::NotFound);
    }
    tracing::info!(username = %admin.user.username, payment = id, "deleted a payment");
    Ok(Redirect::to("/stats?done=payment-deleted#balances").into_response())
}

fn period_links(base: &str, current: Period) -> Vec<PeriodLink> {
    Period::ALL
        .into_iter()
        .map(|period| PeriodLink {
            href: format!("{base}?period={}", period.as_str()),
            label: period.label(),
            current: period == current,
        })
        .collect()
}

/// With `subject`, the last tile is that person's share of other people's filament rather than
/// the group's success rate.
fn tiles<'a>(people: impl Iterator<Item = &'a Person>, subject: Option<&Person>) -> Vec<Tile> {
    let (mut prints, mut done, mut seconds, mut grams) = (0, 0, 0, 0.0);
    for person in people {
        prints += person.prints();
        done += person.done;
        seconds += person.print_seconds;
        grams += person.grams();
    }
    let last = match subject {
        Some(person) => Tile {
            label: "From other people's spools",
            value: percent(person.others_grams, person.grams()),
        },
        None => Tile {
            label: "Finished successfully",
            value: percent(f64::from(done), f64::from(prints)),
        },
    };
    vec![
        Tile {
            label: "Prints",
            value: prints.to_string(),
        },
        Tile {
            label: "Print time",
            value: duration(seconds),
        },
        Tile {
            label: "Filament used",
            value: weight(grams),
        },
        last,
    ]
}

/// A colour stays with a person whatever the period: slots follow the order of all accounts,
/// and everyone past the last slot shares one series.
fn people_chart(summary: &Summary, people: &People) -> Option<BarChart> {
    let present: BTreeSet<i64> = summary
        .buckets
        .iter()
        .flat_map(|bucket| bucket.by_user.keys().copied())
        .collect();
    let mut series = Vec::new();
    let mut column: HashMap<i64, usize> = HashMap::new();
    let mut everyone_else = None;
    for (index, user) in people.users.iter().enumerate() {
        if !present.contains(&user.id) {
            continue;
        }
        let at = match chart::SLOTS.get(index) {
            Some(slot) => {
                series.push(Series {
                    name: user.username.clone(),
                    slot,
                });
                series.len() - 1
            }
            None => *everyone_else.get_or_insert_with(|| {
                series.push(Series {
                    name: "Everyone else".to_owned(),
                    slot: chart::OTHER_SLOT,
                });
                series.len() - 1
            }),
        };
        column.insert(user.id, at);
    }
    let bars = summary
        .buckets
        .iter()
        .enumerate()
        .map(|(index, bucket)| {
            let mut values = vec![0.0; series.len()];
            for (user, grams) in &bucket.by_user {
                if let Some(at) = column.get(user) {
                    values[*at] += grams;
                }
            }
            bar_input(bucket.start, summary.granularity, index == 0, values)
        })
        .collect();
    chart::stacked(
        format!("Filament used per {}, by person", unit(summary.granularity)),
        series,
        bars,
        weight,
    )
}

fn whose_chart(summary: &Summary) -> Option<BarChart> {
    let series = Whose::ALL
        .into_iter()
        .zip(chart::SLOTS)
        .map(|(whose, slot)| Series {
            name: match whose {
                Whose::Own => "Own spools",
                Whose::Shared => "Shared spools",
                Whose::Others => "Other people's spools",
            }
            .to_owned(),
            slot,
        })
        .collect();
    let bars = summary
        .buckets
        .iter()
        .enumerate()
        .map(|(index, bucket)| {
            let values = Whose::ALL.map(|whose| bucket.whose(whose)).to_vec();
            bar_input(bucket.start, summary.granularity, index == 0, values)
        })
        .collect();
    chart::stacked(
        format!(
            "Filament used per {}, by whose spool",
            unit(summary.granularity)
        ),
        series,
        bars,
        weight,
    )
}

/// The first bar and every January name the year, so a long range stays readable.
fn bar_input(start: Date, granularity: Granularity, first: bool, values: Vec<f64>) -> BarInput {
    let (short_label, label) = match granularity {
        Granularity::Month if first || start.month() == 1 => (
            start.strftime("%b %Y").to_string(),
            start.strftime("%B %Y").to_string(),
        ),
        Granularity::Month => (
            start.strftime("%b").to_string(),
            start.strftime("%B %Y").to_string(),
        ),
        Granularity::Day => (
            start.strftime("%-d %b").to_string(),
            start.strftime("%-d %B %Y").to_string(),
        ),
    };
    BarInput {
        short_label,
        label,
        values,
    }
}

/// Rows print, columns own the spools, with shared spools last.
fn matrix(summary: &Summary, people: &People) -> Matrix {
    let mut owners: Vec<Option<i64>> = summary.pairs.iter().map(|pair| pair.owner_id).collect();
    owners.sort_by_key(|owner| {
        (
            owner.is_none(),
            owner.map(|id| people.name(id).to_lowercase()),
        )
    });
    owners.dedup();
    let rows = summary
        .people
        .iter()
        .filter(|person| person.grams() > 0.0)
        .map(|person| MatrixRow {
            name: people.name(person.user_id),
            cells: owners
                .iter()
                .map(|owner| {
                    summary
                        .pairs
                        .iter()
                        .find(|pair| pair.user_id == person.user_id && pair.owner_id == *owner)
                        .map(|pair| match (pair.owner_id, pair.value_cents) {
                            (Some(owner), Some(value)) if owner != pair.user_id => {
                                format!("{} ({})", weight(pair.grams), money(value))
                            }
                            _ => weight(pair.grams),
                        })
                        .unwrap_or_default()
                })
                .collect(),
        })
        .collect();
    Matrix {
        owners: owners.iter().map(|owner| people.owner(*owner)).collect(),
        rows,
    }
}

fn debt_rows(debts: &[Debt], people: &People, viewer: Option<&User>) -> Vec<DebtRow> {
    debts
        .iter()
        .map(|debt| DebtRow {
            debtor: people.name(debt.debtor),
            creditor: people.name(debt.creditor),
            debtor_id: debt.debtor,
            creditor_id: debt.creditor,
            amount: inventory::format_price(debt.cents),
            can_record: viewer.is_some_and(|user| user.is_admin() || user.id == debt.creditor),
        })
        .collect()
}

fn materials(summary: &Summary) -> Vec<AmountRow> {
    summary
        .materials
        .iter()
        .map(|(material, grams)| AmountRow {
            label: material.clone(),
            grams: weight(*grams),
            value: String::new(),
        })
        .collect()
}

fn unit(granularity: Granularity) -> &'static str {
    match granularity {
        Granularity::Day => "day",
        Granularity::Month => "month",
    }
}

/// Grams below a kilogram, kilograms to two decimals above.
fn weight(grams: f64) -> String {
    if grams.abs() >= 1000.0 {
        format!("{} kg", (grams / 10.0).round() / 100.0)
    } else {
        // `+ 0.0` turns the -0 that rounds from a tiny negative into 0.
        format!("{} g", grams.round() + 0.0)
    }
}

fn money(cents: f64) -> String {
    let cents = cents.round() as i64;
    if cents < 0 {
        format!("-{}", inventory::format_price(-cents))
    } else {
        inventory::format_price(cents)
    }
}

fn percent(part: f64, whole: f64) -> String {
    if whole > 0.0 {
        format!("{:.0}%", 100.0 * part / whole)
    } else {
        "–".to_owned()
    }
}

fn duration(seconds: i64) -> String {
    if seconds > 0 {
        views::human_duration(seconds)
    } else {
        "–".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_read_naturally() {
        assert_eq!(weight(412.4), "412 g");
        assert_eq!(weight(0.2), "0 g");
        assert_eq!(weight(1250.0), "1.25 kg");
        assert_eq!(weight(2000.0), "2 kg");
        assert_eq!(money(419.6), "4.20");
        assert_eq!(money(-150.0), "-1.50");
        assert_eq!(percent(1.0, 3.0), "33%");
        assert_eq!(percent(1.0, 0.0), "–");
    }

    #[test]
    fn bar_labels_name_the_year_where_it_changes() {
        let date = |raw: &str| raw.parse::<Date>().unwrap();
        let label = |raw, granularity, first| {
            let bar = bar_input(date(raw), granularity, first, Vec::new());
            (bar.short_label, bar.label)
        };
        assert_eq!(
            label("2026-03-01", Granularity::Month, false),
            ("Mar".to_owned(), "March 2026".to_owned())
        );
        assert_eq!(label("2026-01-01", Granularity::Month, false).0, "Jan 2026");
        assert_eq!(label("2025-11-01", Granularity::Month, true).0, "Nov 2025");
        assert_eq!(
            label("2026-03-05", Granularity::Day, false),
            ("5 Mar".to_owned(), "5 March 2026".to_owned())
        );
    }
}
