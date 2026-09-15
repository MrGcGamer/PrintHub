//! Weekly print windows. Rules are wall-clock times in the configured zone, turned into real
//! instants per day, so a window keeps its local start and end across a DST change.
//!
//! Windows are judged one occurrence at a time: two allow windows that touch (Monday 18:00–24:00
//! and Tuesday 00:00–08:00) do not add up for `must_finish_before`.

use jiff::{Timestamp, ToSpan, civil::Date, tz::TimeZone};

use crate::store::Db;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    Allow,
    Deny,
}

impl RuleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "allow" => Some(Self::Allow),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub kind: RuleKind,
    /// Bit 0 is Monday, bit 6 Sunday.
    pub days: u8,
    pub start_minute: u16,
    /// At or before `start_minute`, the window ends the next day.
    pub end_minute: u16,
    /// For a deny rule: a job must end before the window's next start. For an allow rule: before
    /// the active window ends.
    pub must_finish_before: bool,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleBlock {
    DenyActive {
        label: String,
        until: Timestamp,
    },
    /// Allow windows exist and none is open; `next` is when the next one opens.
    OutsideAllowed {
        next: Option<Timestamp>,
    },
    WouldOverrun {
        label: String,
        deadline: Timestamp,
    },
    /// A `must_finish_before` rule applies and the job has no time estimate.
    NoEstimate {
        label: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Window {
    start: Timestamp,
    end: Timestamp,
}

/// Whether a job may start at `now`, ending at `estimated_end`.
pub fn check(
    rules: &[Rule],
    now: Timestamp,
    estimated_end: Option<Timestamp>,
    tz: &TimeZone,
) -> Result<(), ScheduleBlock> {
    let today = now.to_zoned(tz.clone()).date();
    let active = |rule: &Rule| {
        windows(rule, today, tz)
            .into_iter()
            .find(|w| w.start <= now && now < w.end)
    };
    let next_start = |rule: &Rule| {
        windows(rule, today, tz)
            .into_iter()
            .map(|w| w.start)
            .filter(|start| *start > now)
            .min()
    };
    let (allows, denies): (Vec<&Rule>, Vec<&Rule>) =
        rules.iter().partition(|rule| rule.kind == RuleKind::Allow);

    for rule in &denies {
        if let Some(window) = active(rule) {
            return Err(ScheduleBlock::DenyActive {
                label: rule.label.clone(),
                until: window.end,
            });
        }
    }

    if !allows.is_empty() {
        let open: Vec<(&Rule, Window)> = allows
            .iter()
            .filter_map(|rule| active(rule).map(|window| (*rule, window)))
            .collect();
        if open.is_empty() {
            return Err(ScheduleBlock::OutsideAllowed {
                next: allows.iter().filter_map(|rule| next_start(rule)).min(),
            });
        }
        let fits = |(rule, window): &(&Rule, Window)| {
            !rule.must_finish_before || estimated_end.is_some_and(|end| end <= window.end)
        };
        if !open.iter().any(fits) {
            let (rule, window) = open[0];
            return Err(match estimated_end {
                None => ScheduleBlock::NoEstimate {
                    label: rule.label.clone(),
                },
                Some(_) => ScheduleBlock::WouldOverrun {
                    label: rule.label.clone(),
                    deadline: window.end,
                },
            });
        }
    }

    for rule in denies.iter().filter(|rule| rule.must_finish_before) {
        let Some(deadline) = next_start(rule) else {
            continue;
        };
        match estimated_end {
            None => {
                return Err(ScheduleBlock::NoEstimate {
                    label: rule.label.clone(),
                });
            }
            Some(end) if end > deadline => {
                return Err(ScheduleBlock::WouldOverrun {
                    label: rule.label.clone(),
                    deadline,
                });
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// Occurrences starting from yesterday (whose overnight window may still be open) through a
/// week ahead.
fn windows(rule: &Rule, today: Date, tz: &TimeZone) -> Vec<Window> {
    (-1..=7)
        .filter_map(|offset| {
            let date = today.checked_add(offset.days()).ok()?;
            let weekday = date.weekday().to_monday_zero_offset();
            if rule.days & (1 << weekday) == 0 {
                return None;
            }
            let end_date = if rule.end_minute > rule.start_minute {
                date
            } else {
                date.tomorrow().ok()?
            };
            Some(Window {
                start: instant(date, rule.start_minute, tz)?,
                end: instant(end_date, rule.end_minute, tz)?,
            })
        })
        .collect()
}

/// A local time that falls in a DST gap resolves to the instant after the gap.
fn instant(date: Date, minute: u16, tz: &TimeZone) -> Option<Timestamp> {
    let hour = i8::try_from(minute / 60).ok()?;
    let minute = i8::try_from(minute % 60).ok()?;
    date.at(hour, minute, 0, 0)
        .to_zoned(tz.clone())
        .ok()
        .map(|zoned| zoned.timestamp())
}

/// `HH:MM`, 24-hour.
pub fn parse_time(raw: &str) -> Option<u16> {
    let (hour, minute) = raw.trim().split_once(':')?;
    let (hour, minute): (u16, u16) = (hour.parse().ok()?, minute.parse().ok()?);
    (hour < 24 && minute < 60 && raw.trim().len() == 5).then_some(hour * 60 + minute)
}

pub fn format_time(minute: u16) -> String {
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

pub const DAY_NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

pub async fn rules(db: &Db) -> Result<Vec<(i64, Rule)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT id AS "id!", kind, days, start_minute, end_minute, must_finish_before, label
           FROM schedule_rules ORDER BY kind, start_minute, id"#
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.id,
                Rule {
                    // The column's CHECK constraint admits only the kinds `parse` knows.
                    kind: RuleKind::parse(&r.kind).unwrap_or(RuleKind::Deny),
                    days: u8::try_from(r.days).unwrap_or(0),
                    start_minute: u16::try_from(r.start_minute).unwrap_or(0),
                    end_minute: u16::try_from(r.end_minute).unwrap_or(0),
                    must_finish_before: r.must_finish_before != 0,
                    label: r.label,
                },
            )
        })
        .collect())
}

pub async fn add_rule(db: &Db, rule: &Rule, now: i64) -> Result<i64, sqlx::Error> {
    let kind = rule.kind.as_str();
    let finish = i64::from(rule.must_finish_before);
    let inserted = sqlx::query!(
        "INSERT INTO schedule_rules
             (kind, days, start_minute, end_minute, must_finish_before, label, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        kind,
        rule.days,
        rule.start_minute,
        rule.end_minute,
        finish,
        rule.label,
        now,
    )
    .execute(db)
    .await?;
    Ok(inserted.last_insert_rowid())
}

pub async fn delete_rule(db: &Db, id: i64) -> Result<(), sqlx::Error> {
    sqlx::query!("DELETE FROM schedule_rules WHERE id = ?", id)
        .execute(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERY_DAY: u8 = 0b111_1111;

    fn berlin() -> TimeZone {
        TimeZone::get("Europe/Berlin").unwrap()
    }

    /// A local Berlin time.
    fn at(raw: &str) -> Timestamp {
        raw.parse::<jiff::civil::DateTime>()
            .unwrap()
            .to_zoned(berlin())
            .unwrap()
            .timestamp()
    }

    fn rule(kind: RuleKind, days: u8, start: &str, end: &str, finish: bool) -> Rule {
        Rule {
            kind,
            days,
            start_minute: parse_time(start).unwrap(),
            end_minute: parse_time(end).unwrap(),
            must_finish_before: finish,
            label: format!("{} {start}-{end}", kind.as_str()),
        }
    }

    fn check_at(rules: &[Rule], now: &str, end: Option<&str>) -> Result<(), ScheduleBlock> {
        check(rules, at(now), end.map(at), &berlin())
    }

    #[test]
    fn no_rules_allow_anything() {
        assert_eq!(check_at(&[], "2026-09-15T03:00", None), Ok(()));
    }

    #[test]
    fn overnight_deny_is_active_after_midnight() {
        // Friday 22:00 to Saturday 07:00.
        let quiet = [rule(RuleKind::Deny, 1 << 4, "22:00", "07:00", false)];
        assert_eq!(check_at(&quiet, "2026-09-18T21:59", None), Ok(()));
        assert_eq!(
            check_at(&quiet, "2026-09-19T03:00", None),
            Err(ScheduleBlock::DenyActive {
                label: "deny 22:00-07:00".into(),
                until: at("2026-09-19T07:00"),
            })
        );
        assert_eq!(check_at(&quiet, "2026-09-19T07:00", None), Ok(()));
        // Saturday night itself is not a Friday.
        assert_eq!(check_at(&quiet, "2026-09-19T23:00", None), Ok(()));
    }

    #[test]
    fn allow_rules_close_everything_else() {
        let weekdays = [rule(RuleKind::Allow, 0b001_1111, "08:00", "20:00", false)];
        assert_eq!(check_at(&weekdays, "2026-09-15T12:00", None), Ok(()));
        // Friday evening: the next window is Monday morning.
        assert_eq!(
            check_at(&weekdays, "2026-09-18T21:00", None),
            Err(ScheduleBlock::OutsideAllowed {
                next: Some(at("2026-09-21T08:00"))
            })
        );
    }

    #[test]
    fn deny_wins_over_an_overlapping_allow() {
        let rules = [
            rule(RuleKind::Allow, EVERY_DAY, "06:00", "23:00", false),
            rule(RuleKind::Deny, EVERY_DAY, "12:00", "13:00", false),
        ];
        assert!(matches!(
            check_at(&rules, "2026-09-15T12:30", None),
            Err(ScheduleBlock::DenyActive { .. })
        ));
        assert_eq!(check_at(&rules, "2026-09-15T13:30", None), Ok(()));
    }

    #[test]
    fn deny_toggle_requires_finishing_before_the_next_start() {
        let off = [rule(RuleKind::Deny, EVERY_DAY, "22:00", "07:00", false)];
        let on = [rule(RuleKind::Deny, EVERY_DAY, "22:00", "07:00", true)];
        let now = "2026-09-15T20:00";
        assert_eq!(check_at(&on, now, Some("2026-09-15T21:59")), Ok(()));
        assert_eq!(
            check_at(&on, now, Some("2026-09-15T22:30")),
            Err(ScheduleBlock::WouldOverrun {
                label: "deny 22:00-07:00".into(),
                deadline: at("2026-09-15T22:00"),
            })
        );
        assert_eq!(check_at(&off, now, Some("2026-09-16T05:00")), Ok(()));
        assert_eq!(
            check_at(&on, now, None),
            Err(ScheduleBlock::NoEstimate {
                label: "deny 22:00-07:00".into()
            })
        );
    }

    #[test]
    fn allow_toggle_requires_finishing_inside_the_window() {
        let on = [rule(RuleKind::Allow, EVERY_DAY, "08:00", "18:00", true)];
        let off = [rule(RuleKind::Allow, EVERY_DAY, "08:00", "18:00", false)];
        let now = "2026-09-15T17:00";
        assert_eq!(check_at(&on, now, Some("2026-09-15T18:00")), Ok(()));
        assert_eq!(
            check_at(&on, now, Some("2026-09-15T18:30")),
            Err(ScheduleBlock::WouldOverrun {
                label: "allow 08:00-18:00".into(),
                deadline: at("2026-09-15T18:00"),
            })
        );
        assert_eq!(check_at(&off, now, Some("2026-09-15T23:00")), Ok(()));
    }

    #[test]
    fn windows_keep_local_times_across_dst_changes() {
        // Berlin springs forward on 2026-03-29 at 02:00 and falls back on 2026-10-25 at 03:00.
        let night = [rule(RuleKind::Deny, EVERY_DAY, "01:00", "05:00", false)];
        let Err(ScheduleBlock::DenyActive { until, .. }) =
            check_at(&night, "2026-03-29T04:30", None)
        else {
            panic!("the window is open at 04:30 local time");
        };
        assert_eq!(until, "2026-03-29T03:00:00Z".parse().unwrap());
        assert_eq!(check_at(&night, "2026-10-25T05:00", None), Ok(()));
        assert!(check_at(&night, "2026-10-25T04:59", None).is_err());

        // 02:30 does not exist on the spring-forward day.
        let gap = [rule(RuleKind::Allow, EVERY_DAY, "02:30", "06:00", false)];
        assert_eq!(
            check_at(&gap, "2026-03-29T01:00", None),
            Err(ScheduleBlock::OutsideAllowed {
                next: Some("2026-03-29T01:30:00Z".parse().unwrap())
            })
        );
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("07:05"), Some(425));
        assert_eq!(parse_time("23:59"), Some(1439));
        assert_eq!(parse_time("24:00"), None);
        assert_eq!(parse_time("7:05"), None);
        assert_eq!(format_time(425), "07:05");
    }
}
