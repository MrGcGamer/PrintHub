//! Who printed how much, with whose filament, and what is owed for it. Queries return plain
//! rows; the aggregation is pure. Like `accounts`, functions take timestamps rather than reading
//! the clock.

use std::collections::{BTreeMap, HashMap};

use jiff::{Timestamp, ToSpan, civil::Date, tz::TimeZone};

use crate::{inventory::ConsumptionKind, jobs::JobState, store::Db};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    Last30Days,
    Last12Months,
    AllTime,
}

impl Period {
    pub const ALL: [Self; 3] = [Self::Last30Days, Self::Last12Months, Self::AllTime];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Last30Days => "30d",
            Self::Last12Months => "12m",
            Self::AllTime => "all",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|period| period.as_str() == raw)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Last30Days => "Last 30 days",
            Self::Last12Months => "Last 12 months",
            Self::AllTime => "All time",
        }
    }

    /// The first second that counts, 0 for all time. Twelve months start on the first of a
    /// month, so the chart's first bar covers a whole one.
    pub fn since(self, now: i64, tz: &TimeZone) -> i64 {
        match self {
            Self::Last30Days => now - 30 * 86_400,
            Self::Last12Months => local_date(now, tz)
                .first_of_month()
                .checked_sub(11.months())
                .ok()
                .and_then(|month| month.to_zoned(tz.clone()).ok())
                .map_or(0, |start| start.timestamp().as_second()),
            Self::AllTime => 0,
        }
    }

    pub fn granularity(self) -> Granularity {
        match self {
            Self::Last30Days => Granularity::Day,
            Self::Last12Months | Self::AllTime => Granularity::Month,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    Day,
    Month,
}

impl Granularity {
    fn start(self, date: Date) -> Date {
        match self {
            Self::Day => date,
            Self::Month => date.first_of_month(),
        }
    }

    fn next(self, start: Date) -> Option<Date> {
        match self {
            Self::Day => start.tomorrow().ok(),
            Self::Month => start.checked_add(1.month()).ok(),
        }
    }
}

pub fn local_date(unix: i64, tz: &TimeZone) -> Date {
    Timestamp::from_second(unix)
        .unwrap_or(Timestamp::UNIX_EPOCH)
        .to_zoned(tz.clone())
        .date()
}

/// Whose spool filament came from, seen from whoever used it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Whose {
    Own,
    Shared,
    Others,
}

impl Whose {
    pub const ALL: [Self; 3] = [Self::Own, Self::Shared, Self::Others];

    fn index(self) -> usize {
        match self {
            Self::Own => 0,
            Self::Shared => 1,
            Self::Others => 2,
        }
    }
}

/// One ledger entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Use {
    /// Whoever printed, or for a weigh-in whoever weighed.
    pub user_id: Option<i64>,
    /// The spool's owner when the entry was written; `None` for a shared spool.
    pub owner_id: Option<i64>,
    pub kind: ConsumptionKind,
    pub grams: f64,
    /// `None` when the spool had no price.
    pub value_cents: Option<f64>,
    pub material: String,
    pub at: i64,
}

impl Use {
    /// `None` for weigh-ins, which nobody printed.
    pub fn whose(&self) -> Option<Whose> {
        if self.kind == ConsumptionKind::WeighIn {
            return None;
        }
        let user = self.user_id?;
        Some(match self.owner_id {
            None => Whose::Shared,
            Some(owner) if owner == user => Whose::Own,
            Some(_) => Whose::Others,
        })
    }
}

/// A job that reached the printer and ended.
#[derive(Debug, Clone, PartialEq)]
pub struct Print {
    pub owner_id: Option<i64>,
    pub state: JobState,
    /// Includes time spent paused.
    pub seconds: i64,
    pub finished_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ledger {
    pub uses: Vec<Use>,
    pub prints: Vec<Print>,
}

impl Ledger {
    /// Only `user_id`'s prints and the filament they used.
    pub fn of_user(&self, user_id: i64) -> Self {
        Self {
            uses: self
                .uses
                .iter()
                .filter(|entry| entry.whose().is_some() && entry.user_id == Some(user_id))
                .cloned()
                .collect(),
            prints: self
                .prints
                .iter()
                .filter(|print| print.owner_id == Some(user_id))
                .cloned()
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Person {
    pub user_id: i64,
    pub done: u32,
    pub failed: u32,
    pub cancelled: u32,
    pub print_seconds: i64,
    pub own_grams: f64,
    pub shared_grams: f64,
    pub others_grams: f64,
    /// What the filament from other people's spools was worth, counting priced spools only.
    pub others_value_cents: f64,
    /// Filament that went into prints that failed or were stopped.
    pub wasted_grams: f64,
}

impl Person {
    pub fn prints(&self) -> u32 {
        self.done + self.failed + self.cancelled
    }

    pub fn grams(&self) -> f64 {
        self.own_grams + self.shared_grams + self.others_grams
    }
}

/// What one person used of one owner's filament.
#[derive(Debug, Clone, PartialEq)]
pub struct Pair {
    pub user_id: i64,
    /// `None` for shared spools.
    pub owner_id: Option<i64>,
    pub grams: f64,
    /// `None` when none of the spools had a price.
    pub value_cents: Option<f64>,
}

/// What weigh-ins found missing from one owner's spools, beyond what prints recorded. Negative
/// when they found more than recorded.
#[derive(Debug, Clone, PartialEq)]
pub struct Unaccounted {
    pub owner_id: Option<i64>,
    pub grams: f64,
    pub value_cents: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bucket {
    /// The day, or the first of the month.
    pub start: Date,
    pub by_user: BTreeMap<i64, f64>,
    /// Indexed in [`Whose::ALL`] order.
    pub by_whose: [f64; 3],
}

impl Bucket {
    pub fn whose(&self, whose: Whose) -> f64 {
        self.by_whose[whose.index()]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    /// Most filament first.
    pub people: Vec<Person>,
    /// By user, then owner, with shared spools last.
    pub pairs: Vec<Pair>,
    /// Most filament first; names compare case-insensitively and keep the first spelling seen.
    pub materials: Vec<(String, f64)>,
    pub unaccounted: Vec<Unaccounted>,
    /// Consecutive, from the period's start (or the first entry, for all time) to `now`.
    pub buckets: Vec<Bucket>,
    pub granularity: Granularity,
}

pub fn summarize(ledger: &Ledger, period: Period, since: i64, now: i64, tz: &TimeZone) -> Summary {
    let mut people: HashMap<i64, Person> = HashMap::new();
    for print in &ledger.prints {
        let Some(owner) = print.owner_id else {
            continue;
        };
        let entry = person(&mut people, owner);
        match print.state {
            JobState::Done => entry.done += 1,
            JobState::Failed => entry.failed += 1,
            JobState::Cancelled => entry.cancelled += 1,
            _ => continue,
        }
        entry.print_seconds += print.seconds;
    }

    let granularity = period.granularity();
    let first = if since > 0 {
        since
    } else {
        ledger
            .uses
            .iter()
            .map(|entry| entry.at)
            .chain(ledger.prints.iter().map(|print| print.finished_at))
            .min()
            .unwrap_or(now)
    };
    let mut buckets = Vec::new();
    let last = local_date(now, tz);
    let mut start = Some(granularity.start(local_date(first, tz)));
    while let Some(date) = start.filter(|date| *date <= last) {
        buckets.push(Bucket {
            start: date,
            by_user: BTreeMap::new(),
            by_whose: [0.0; 3],
        });
        start = granularity.next(date);
    }

    let mut pairs: HashMap<(i64, Option<i64>), Pair> = HashMap::new();
    let mut materials: Vec<(String, f64)> = Vec::new();
    let mut unaccounted: HashMap<Option<i64>, Unaccounted> = HashMap::new();
    for entry in &ledger.uses {
        let (Some(user), Some(whose)) = (entry.user_id, entry.whose()) else {
            if entry.kind == ConsumptionKind::WeighIn {
                let slot = unaccounted
                    .entry(entry.owner_id)
                    .or_insert_with(|| Unaccounted {
                        owner_id: entry.owner_id,
                        grams: 0.0,
                        value_cents: None,
                    });
                slot.grams += entry.grams;
                add_value(&mut slot.value_cents, entry.value_cents);
            }
            continue;
        };

        let stats = person(&mut people, user);
        match whose {
            Whose::Own => stats.own_grams += entry.grams,
            Whose::Shared => stats.shared_grams += entry.grams,
            Whose::Others => {
                stats.others_grams += entry.grams;
                stats.others_value_cents += entry.value_cents.unwrap_or(0.0);
            }
        }
        if entry.kind == ConsumptionKind::Estimate {
            stats.wasted_grams += entry.grams;
        }

        let pair = pairs.entry((user, entry.owner_id)).or_insert_with(|| Pair {
            user_id: user,
            owner_id: entry.owner_id,
            grams: 0.0,
            value_cents: None,
        });
        pair.grams += entry.grams;
        add_value(&mut pair.value_cents, entry.value_cents);

        match materials
            .iter_mut()
            .find(|(name, _)| name.eq_ignore_ascii_case(entry.material.trim()))
        {
            Some((_, grams)) => *grams += entry.grams,
            None => materials.push((entry.material.trim().to_owned(), entry.grams)),
        }

        let day = granularity.start(local_date(entry.at, tz));
        if let Ok(index) = buckets.binary_search_by(|bucket| bucket.start.cmp(&day)) {
            let bucket = &mut buckets[index];
            *bucket.by_user.entry(user).or_default() += entry.grams;
            bucket.by_whose[whose.index()] += entry.grams;
        }
    }

    let mut people: Vec<Person> = people.into_values().collect();
    people.sort_by(|a, b| {
        b.grams()
            .total_cmp(&a.grams())
            .then(b.prints().cmp(&a.prints()))
            .then(a.user_id.cmp(&b.user_id))
    });
    let mut pairs: Vec<Pair> = pairs.into_values().collect();
    pairs.sort_by_key(|pair| (pair.user_id, pair.owner_id.is_none(), pair.owner_id));
    materials.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut unaccounted: Vec<Unaccounted> = unaccounted.into_values().collect();
    unaccounted.sort_by_key(|entry| (entry.owner_id.is_none(), entry.owner_id));

    Summary {
        people,
        pairs,
        materials,
        unaccounted,
        buckets,
        granularity,
    }
}

fn person(people: &mut HashMap<i64, Person>, id: i64) -> &mut Person {
    people.entry(id).or_insert_with(|| Person {
        user_id: id,
        ..Person::default()
    })
}

fn add_value(total: &mut Option<f64>, value: Option<f64>) {
    if let Some(value) = value {
        *total = Some(total.unwrap_or(0.0) + value);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settlement {
    pub id: i64,
    pub from: (i64, String),
    pub to: (i64, String),
    pub amount_cents: i64,
    pub recorded_by: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Debt {
    pub debtor: i64,
    pub creditor: i64,
    pub cents: i64,
}

/// Nets, for every pair of people, the filament each used of the other's spools against the
/// payments between them. `owed` holds `(user, owner, value_cents)`.
pub fn balances(owed: &[(i64, i64, f64)], paid: &[Settlement]) -> Vec<Debt> {
    // Keyed by the lower id first; positive means the lower id owes the higher.
    let mut net: BTreeMap<(i64, i64), f64> = BTreeMap::new();
    let mut add = |debtor: i64, creditor: i64, cents: f64| {
        if debtor < creditor {
            *net.entry((debtor, creditor)).or_default() += cents;
        } else if creditor < debtor {
            *net.entry((creditor, debtor)).or_default() -= cents;
        }
    };
    for (user, owner, cents) in owed {
        add(*user, *owner, *cents);
    }
    for payment in paid {
        add(payment.from.0, payment.to.0, -(payment.amount_cents as f64));
    }
    net.into_iter()
        .filter_map(|((low, high), cents)| {
            let cents = cents.round() as i64;
            match cents.signum() {
                1 => Some(Debt {
                    debtor: low,
                    creditor: high,
                    cents,
                }),
                -1 => Some(Debt {
                    debtor: high,
                    creditor: low,
                    cents: -cents,
                }),
                _ => None,
            }
        })
        .collect()
}

pub async fn load(db: &Db, since: i64) -> Result<Ledger, sqlx::Error> {
    let uses = sqlx::query!(
        "SELECT c.user_id, c.spool_owner_id, c.kind, c.grams, c.value_cents, s.material,
                c.created_at
         FROM consumption c JOIN spools s ON s.id = c.spool_id
         WHERE c.created_at >= ?
         ORDER BY c.created_at, c.id",
        since,
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .filter_map(|r| {
        Some(Use {
            user_id: r.user_id,
            owner_id: r.spool_owner_id,
            kind: ConsumptionKind::parse(&r.kind)?,
            grams: r.grams,
            value_cents: r.value_cents,
            material: r.material,
            at: r.created_at,
        })
    })
    .collect();
    let prints = sqlx::query!(
        r#"SELECT owner_id, state, started_at AS "started_at!", finished_at AS "finished_at!"
           FROM jobs
           WHERE state IN ('done', 'failed', 'cancelled')
             AND started_at IS NOT NULL AND finished_at IS NOT NULL AND finished_at >= ?"#,
        since,
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .filter_map(|r| {
        Some(Print {
            owner_id: r.owner_id,
            state: JobState::parse(&r.state)?,
            seconds: (r.finished_at - r.started_at).max(0),
            finished_at: r.finished_at,
        })
    })
    .collect();
    Ok(Ledger { uses, prints })
}

/// The value of everything each person printed from someone else's priced spools, over all
/// time, as `(user, owner, value_cents)`.
pub async fn owed(db: &Db) -> Result<Vec<(i64, i64, f64)>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT user_id AS "user_id!", spool_owner_id AS "owner_id!",
                  SUM(value_cents) AS "value_cents!: f64"
           FROM consumption
           WHERE kind IN ('print', 'estimate') AND user_id IS NOT NULL
             AND spool_owner_id IS NOT NULL AND user_id != spool_owner_id
             AND value_cents IS NOT NULL
           GROUP BY user_id, spool_owner_id"#
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.user_id, r.owner_id, r.value_cents))
        .collect())
}

/// Newest first.
pub async fn settlements(db: &Db) -> Result<Vec<Settlement>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT p.id AS "id!", p.from_user, f.username AS from_name, p.to_user,
                  t.username AS to_name, p.amount_cents, r.username AS "recorded_by?",
                  p.created_at
           FROM settlements p
           JOIN users f ON f.id = p.from_user
           JOIN users t ON t.id = p.to_user
           LEFT JOIN users r ON r.id = p.recorded_by
           ORDER BY p.created_at DESC, p.id DESC"#
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Settlement {
            id: r.id,
            from: (r.from_user, r.from_name),
            to: (r.to_user, r.to_name),
            amount_cents: r.amount_cents,
            recorded_by: r.recorded_by,
            created_at: r.created_at,
        })
        .collect())
}

pub async fn record_settlement(
    db: &Db,
    from: i64,
    to: i64,
    amount_cents: i64,
    recorded_by: i64,
    now: i64,
) -> Result<i64, sqlx::Error> {
    let inserted = sqlx::query!(
        "INSERT INTO settlements (from_user, to_user, amount_cents, recorded_by, created_at)
         VALUES (?, ?, ?, ?, ?)",
        from,
        to,
        amount_cents,
        recorded_by,
        now,
    )
    .execute(db)
    .await?;
    Ok(inserted.last_insert_rowid())
}

/// Returns whether there was such a payment.
pub async fn delete_settlement(db: &Db, id: i64) -> Result<bool, sqlx::Error> {
    let deleted = sqlx::query!("DELETE FROM settlements WHERE id = ?", id)
        .execute(db)
        .await?;
    Ok(deleted.rows_affected() > 0)
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecentPrint {
    pub job_id: i64,
    pub name: String,
    pub state: JobState,
    pub finished_at: i64,
    pub grams: f64,
    /// `None` when the print used nothing from other people's priced spools.
    pub others_value_cents: Option<f64>,
}

/// `user_id`'s prints that ended since `since`, newest first.
pub async fn recent_prints(
    db: &Db,
    user_id: i64,
    since: i64,
    limit: i64,
) -> Result<Vec<RecentPrint>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"SELECT j.id AS "id!", j.name, j.state, j.finished_at AS "finished_at!",
                  COALESCE(SUM(c.grams), 0.0) AS "grams!: f64",
                  SUM(CASE WHEN c.spool_owner_id != j.owner_id THEN c.value_cents END)
                      AS "others_value_cents?: f64"
           FROM jobs j
           LEFT JOIN consumption c ON c.job_id = j.id AND c.kind IN ('print', 'estimate')
           WHERE j.owner_id = ? AND j.state IN ('done', 'failed', 'cancelled')
             AND j.started_at IS NOT NULL AND j.finished_at >= ?
           GROUP BY j.id
           ORDER BY j.finished_at DESC, j.id DESC
           LIMIT ?"#,
        user_id,
        since,
        limit,
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(RecentPrint {
                job_id: r.id,
                name: r.name,
                state: JobState::parse(&r.state)?,
                finished_at: r.finished_at,
                grams: r.grams,
                others_value_cents: r.others_value_cents,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        accounts::{self, Role},
        inventory::{self, SpoolFields},
        store,
    };

    fn berlin() -> TimeZone {
        TimeZone::get("Europe/Berlin").unwrap()
    }

    fn unix(raw: &str) -> i64 {
        raw.parse::<jiff::civil::DateTime>()
            .unwrap()
            .to_zoned(TimeZone::UTC)
            .unwrap()
            .timestamp()
            .as_second()
    }

    fn used(user: i64, owner: Option<i64>, kind: ConsumptionKind, grams: f64, at: &str) -> Use {
        Use {
            user_id: Some(user),
            owner_id: owner,
            kind,
            grams,
            value_cents: owner.map(|_| grams * 2.0),
            material: "PLA".into(),
            at: unix(at),
        }
    }

    fn payment(from: i64, to: i64, amount_cents: i64) -> Settlement {
        Settlement {
            id: 0,
            from: (from, String::new()),
            to: (to, String::new()),
            amount_cents,
            recorded_by: None,
            created_at: 0,
        }
    }

    const SAM: i64 = 1;
    const ALEX: i64 = 2;

    #[test]
    fn filament_is_split_by_whose_spool_it_came_from() {
        let print = |state, seconds| Print {
            owner_id: Some(ALEX),
            state,
            seconds,
            finished_at: unix("2026-03-01T12:00:00"),
        };
        let mut pla = used(
            ALEX,
            None,
            ConsumptionKind::Print,
            5.0,
            "2026-03-01T12:00:00",
        );
        pla.material = " pla ".into();
        let ledger = Ledger {
            uses: vec![
                used(
                    ALEX,
                    Some(ALEX),
                    ConsumptionKind::Print,
                    10.0,
                    "2026-03-01T12:00:00",
                ),
                pla,
                used(
                    ALEX,
                    Some(SAM),
                    ConsumptionKind::Estimate,
                    20.0,
                    "2026-03-01T12:00:00",
                ),
                used(
                    SAM,
                    Some(SAM),
                    ConsumptionKind::Print,
                    1.0,
                    "2026-03-01T12:00:00",
                ),
                used(
                    SAM,
                    Some(SAM),
                    ConsumptionKind::WeighIn,
                    7.0,
                    "2026-03-01T12:00:00",
                ),
            ],
            prints: vec![print(JobState::Done, 3600), print(JobState::Cancelled, 60)],
        };
        let now = unix("2026-03-02T00:00:00");
        let summary = summarize(&ledger, Period::AllTime, 0, now, &TimeZone::UTC);

        let alex = &summary.people[0];
        assert_eq!(alex.user_id, ALEX, "most filament first");
        assert_eq!(
            (alex.own_grams, alex.shared_grams, alex.others_grams),
            (10.0, 5.0, 20.0)
        );
        assert_eq!((alex.others_value_cents, alex.wasted_grams), (40.0, 20.0));
        assert_eq!(
            (alex.done, alex.cancelled, alex.print_seconds),
            (1, 1, 3660)
        );
        assert_eq!(summary.people[1].grams(), 1.0, "a weigh-in is not a print");

        assert_eq!(
            summary
                .pairs
                .iter()
                .map(|p| (p.user_id, p.owner_id, p.grams, p.value_cents))
                .collect::<Vec<_>>(),
            [
                (SAM, Some(SAM), 1.0, Some(2.0)),
                (ALEX, Some(SAM), 20.0, Some(40.0)),
                (ALEX, Some(ALEX), 10.0, Some(20.0)),
                (ALEX, None, 5.0, None),
            ]
        );
        assert_eq!(summary.materials, [("PLA".to_owned(), 36.0)]);
        assert_eq!(
            summary.unaccounted,
            [Unaccounted {
                owner_id: Some(SAM),
                grams: 7.0,
                value_cents: Some(14.0)
            }]
        );
        assert_eq!(summary.buckets.len(), 1);
        assert_eq!(summary.buckets[0].whose(Whose::Others), 20.0);
    }

    #[test]
    fn buckets_follow_the_local_calendar() {
        let tz = berlin();
        // 23:30 UTC on 31 January is already February in Berlin.
        let ledger = Ledger {
            uses: vec![used(
                SAM,
                None,
                ConsumptionKind::Print,
                4.0,
                "2026-01-31T23:30:00",
            )],
            prints: Vec::new(),
        };
        let now = unix("2026-03-15T12:00:00");
        let since = Period::Last12Months.since(now, &tz);
        assert_eq!(
            since,
            unix("2025-03-31T22:00:00"),
            "1 April 2025, Berlin summer time"
        );
        let summary = summarize(&ledger, Period::Last12Months, since, now, &tz);
        let months: Vec<String> = summary
            .buckets
            .iter()
            .map(|bucket| bucket.start.to_string())
            .collect();
        assert_eq!(months.len(), 12);
        assert_eq!(months.first().unwrap(), "2025-04-01");
        assert_eq!(months.last().unwrap(), "2026-03-01");
        let february = &summary.buckets[10];
        assert_eq!(february.start.to_string(), "2026-02-01");
        assert_eq!(february.by_user.get(&SAM), Some(&4.0));

        let days = summarize(
            &ledger,
            Period::Last30Days,
            Period::Last30Days.since(now, &tz),
            now,
            &tz,
        );
        assert_eq!(days.buckets.len(), 31);
        assert!(days.people.iter().all(|p| p.user_id == SAM));
    }

    #[test]
    fn balances_net_both_directions_and_payments() {
        assert_eq!(
            balances(&[(ALEX, SAM, 500.0), (SAM, ALEX, 120.4)], &[]),
            [Debt {
                debtor: ALEX,
                creditor: SAM,
                cents: 380
            }]
        );
        assert_eq!(
            balances(&[(ALEX, SAM, 500.0)], &[payment(ALEX, SAM, 500)]),
            [],
            "paid in full"
        );
        assert_eq!(
            balances(&[(ALEX, SAM, 500.0)], &[payment(ALEX, SAM, 700)]),
            [Debt {
                debtor: SAM,
                creditor: ALEX,
                cents: 200
            }],
            "overpaid"
        );
        assert_eq!(balances(&[(SAM, SAM, 1.0)], &[]), []);
    }

    #[tokio::test]
    async fn the_ledger_keeps_owner_and_value_from_when_filament_was_used() {
        let db = store::open_in_memory().await.unwrap();
        let sam = accounts::create_user(&db, "sam", "h", Role::Member, 1)
            .await
            .unwrap();
        let alex = accounts::create_user(&db, "alex", "h", Role::Member, 1)
            .await
            .unwrap();
        let mut fields = SpoolFields {
            material: "PLA".into(),
            brand: String::new(),
            color_name: String::new(),
            color_hex: "#000000".into(),
            price_cents: Some(2000),
            initial_grams: 1000.0,
            notes: String::new(),
        };
        let spool = inventory::create_spool(&db, &fields, Some(sam), 1)
            .await
            .unwrap();
        inventory::weigh_in(&db, spool, alex, 900.0, 2)
            .await
            .unwrap();

        fields.price_cents = Some(9000);
        inventory::update_spool(&db, spool, &fields, Some(alex))
            .await
            .unwrap();

        let ledger = load(&db, 0).await.unwrap();
        assert_eq!(ledger.uses.len(), 1);
        assert_eq!(ledger.uses[0].owner_id, Some(sam));
        assert_eq!(ledger.uses[0].value_cents, Some(200.0));

        let id = record_settlement(&db, alex, sam, 150, sam, 3)
            .await
            .unwrap();
        let paid = settlements(&db).await.unwrap();
        assert_eq!(paid[0].from, (alex, "alex".to_owned()));
        assert_eq!(
            balances(&[(alex, sam, 200.0)], &paid),
            [Debt {
                debtor: alex,
                creditor: sam,
                cents: 50
            }]
        );
        assert!(delete_settlement(&db, id).await.unwrap());
        assert!(!delete_settlement(&db, id).await.unwrap());
    }
}
