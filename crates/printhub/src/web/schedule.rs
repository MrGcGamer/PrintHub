//! The admin's print windows.

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
    session::AdminUser,
};
use crate::{
    accounts::User,
    inventory,
    schedule::{self, Rule, RuleKind},
    store,
};

/// Outcomes are fixed codes, so a crafted link cannot put text of the sender's choosing on the
/// page.
#[derive(Deserialize)]
pub struct Outcome {
    done: Option<String>,
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
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    schedule::delete_rule(&state.db, id).await?;
    state.queue_changed.notify_one();
    Ok(Redirect::to("/admin/schedule?done=deleted").into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

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
