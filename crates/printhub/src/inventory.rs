//! Filament spools, the CANVAS tray each one sits in, and the ledger of grams used. Like
//! `accounts`, functions take timestamps rather than reading the clock.

use std::collections::HashMap;

use thiserror::Error;

use crate::{
    accounts::User,
    cc2::model::{CanvasInfo, Tray, TrayState},
    store::Db,
};

#[derive(Debug, Clone, PartialEq)]
pub struct Spool {
    pub id: i64,
    pub material: String,
    pub brand: String,
    pub color_name: String,
    /// `#RRGGBB` as produced by [`normalize_color`].
    pub color_hex: String,
    pub owner: Option<(i64, String)>,
    pub price_cents: Option<i64>,
    pub initial_grams: f64,
    pub remaining_grams: f64,
    pub notes: String,
    pub archived: bool,
    pub created_at: i64,
}

impl Spool {
    pub fn editable_by(&self, user: &User) -> bool {
        user.is_admin() || self.owner.as_ref().is_some_and(|(id, _)| *id == user.id)
    }
}

/// What a spool form sets. `remaining_grams` is absent on purpose: it only moves together with
/// a ledger entry.
#[derive(Debug, Clone, PartialEq)]
pub struct SpoolFields {
    pub material: String,
    pub brand: String,
    pub color_name: String,
    pub color_hex: String,
    pub price_cents: Option<i64>,
    pub initial_grams: f64,
    pub notes: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    pub canvas_id: i64,
    pub tray_id: i64,
    pub spool: Spool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumptionKind {
    Print,
    /// A stopped or failed print, charged by its progress.
    Estimate,
    WeighIn,
}

impl ConsumptionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Print => "print",
            Self::Estimate => "estimate",
            Self::WeighIn => "weigh_in",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "print" => Some(Self::Print),
            "estimate" => Some(Self::Estimate),
            "weigh_in" => Some(Self::WeighIn),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Consumption {
    pub id: i64,
    pub kind: ConsumptionKind,
    /// Positive when filament was used up, negative when a weigh-in found more than recorded.
    pub grams: f64,
    pub note: String,
    pub username: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Error)]
pub enum InventoryError {
    #[error("no such spool")]
    NotFound,
    #[error("archived spools cannot be put in a tray")]
    Archived,
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
}

pub const TEXT_MAX: usize = 64;
pub const NOTES_MAX: usize = 1000;
/// Rejects a weight typed with an extra digit rather than storing it.
pub const GRAMS_MAX: f64 = 20_000.0;

struct SpoolRow {
    id: i64,
    material: String,
    brand: String,
    color_name: String,
    color_hex: String,
    owner_id: Option<i64>,
    owner_name: Option<String>,
    price_cents: Option<i64>,
    initial_grams: f64,
    remaining_grams: f64,
    notes: String,
    archived: i64,
    created_at: i64,
}

impl From<SpoolRow> for Spool {
    fn from(row: SpoolRow) -> Self {
        Self {
            id: row.id,
            material: row.material,
            brand: row.brand,
            color_name: row.color_name,
            color_hex: row.color_hex,
            owner: row.owner_id.zip(row.owner_name),
            price_cents: row.price_cents,
            initial_grams: row.initial_grams,
            remaining_grams: row.remaining_grams,
            notes: row.notes,
            archived: row.archived != 0,
            created_at: row.created_at,
        }
    }
}

pub async fn spool(db: &Db, id: i64) -> Result<Option<Spool>, InventoryError> {
    let row = sqlx::query_as!(
        SpoolRow,
        r#"SELECT s.id AS "id!", s.material, s.brand, s.color_name, s.color_hex, s.owner_id,
                  u.username AS "owner_name?", s.price_cents, s.initial_grams, s.remaining_grams,
                  s.notes, s.archived, s.created_at
           FROM spools s LEFT JOIN users u ON u.id = s.owner_id
           WHERE s.id = ?"#,
        id,
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map(Spool::from))
}

pub async fn spools(db: &Db, include_archived: bool) -> Result<Vec<Spool>, InventoryError> {
    let include_archived = i64::from(include_archived);
    let rows = sqlx::query_as!(
        SpoolRow,
        r#"SELECT s.id AS "id!", s.material, s.brand, s.color_name, s.color_hex, s.owner_id,
                  u.username AS "owner_name?", s.price_cents, s.initial_grams, s.remaining_grams,
                  s.notes, s.archived, s.created_at
           FROM spools s LEFT JOIN users u ON u.id = s.owner_id
           WHERE s.archived = 0 OR ?
           ORDER BY s.archived, s.material COLLATE NOCASE, s.brand COLLATE NOCASE,
                    s.color_name COLLATE NOCASE, s.id"#,
        include_archived,
    )
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(Spool::from).collect())
}

pub async fn create_spool(
    db: &Db,
    fields: &SpoolFields,
    owner_id: Option<i64>,
    now: i64,
) -> Result<i64, InventoryError> {
    let inserted = sqlx::query!(
        "INSERT INTO spools (material, brand, color_name, color_hex, owner_id, price_cents,
                             initial_grams, remaining_grams, notes, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        fields.material,
        fields.brand,
        fields.color_name,
        fields.color_hex,
        owner_id,
        fields.price_cents,
        fields.initial_grams,
        fields.initial_grams,
        fields.notes,
        now,
    )
    .execute(db)
    .await?;
    Ok(inserted.last_insert_rowid())
}

pub async fn update_spool(
    db: &Db,
    id: i64,
    fields: &SpoolFields,
    owner_id: Option<i64>,
) -> Result<(), InventoryError> {
    let updated = sqlx::query!(
        "UPDATE spools SET material = ?, brand = ?, color_name = ?, color_hex = ?, owner_id = ?,
                           price_cents = ?, initial_grams = ?, notes = ?
         WHERE id = ?",
        fields.material,
        fields.brand,
        fields.color_name,
        fields.color_hex,
        owner_id,
        fields.price_cents,
        fields.initial_grams,
        fields.notes,
        id,
    )
    .execute(db)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(InventoryError::NotFound);
    }
    Ok(())
}

pub async fn set_archived(db: &Db, id: i64, archived: bool) -> Result<(), InventoryError> {
    let mut tx = db.begin().await?;
    let flag = i64::from(archived);
    let updated = sqlx::query!("UPDATE spools SET archived = ? WHERE id = ?", flag, id)
        .execute(&mut *tx)
        .await?;
    if updated.rows_affected() == 0 {
        return Err(InventoryError::NotFound);
    }
    if archived {
        sqlx::query!("DELETE FROM tray_bindings WHERE spool_id = ?", id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Records that `remaining_grams` are actually left, with a ledger entry for the difference.
pub async fn weigh_in(
    db: &Db,
    spool_id: i64,
    user_id: i64,
    remaining_grams: f64,
    now: i64,
) -> Result<(), InventoryError> {
    let mut tx = db.begin().await?;
    let before = sqlx::query!("SELECT remaining_grams FROM spools WHERE id = ?", spool_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(InventoryError::NotFound)?;
    sqlx::query!(
        "UPDATE spools SET remaining_grams = ? WHERE id = ?",
        remaining_grams,
        spool_id,
    )
    .execute(&mut *tx)
    .await?;
    let used = before.remaining_grams - remaining_grams;
    let kind = ConsumptionKind::WeighIn.as_str();
    sqlx::query!(
        "INSERT INTO consumption (spool_id, user_id, kind, grams, note, created_at,
                                  spool_owner_id, value_cents)
         SELECT s.id, ?, ?, ?, '', ?, s.owner_id,
                CASE WHEN s.price_cents IS NOT NULL AND s.initial_grams > 0
                     THEN ? * s.price_cents / s.initial_grams END
         FROM spools s WHERE s.id = ?",
        user_id,
        kind,
        used,
        now,
        used,
        spool_id,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Deducts what a print used, as a ledger entry tied to the job. The entry keeps the spool's
/// current owner and the filament's value, which later edits to the spool leave alone.
#[allow(clippy::too_many_arguments)]
pub async fn record_use(
    conn: &mut sqlx::SqliteConnection,
    spool_id: i64,
    user_id: Option<i64>,
    job_id: i64,
    kind: ConsumptionKind,
    grams: f64,
    note: &str,
    now: i64,
) -> Result<(), InventoryError> {
    let updated = sqlx::query!(
        "UPDATE spools SET remaining_grams = remaining_grams - ? WHERE id = ?",
        grams,
        spool_id,
    )
    .execute(&mut *conn)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(InventoryError::NotFound);
    }
    let kind = kind.as_str();
    sqlx::query!(
        "INSERT INTO consumption (spool_id, user_id, job_id, kind, grams, note, created_at,
                                  spool_owner_id, value_cents)
         SELECT s.id, ?, ?, ?, ?, ?, ?, s.owner_id,
                CASE WHEN s.price_cents IS NOT NULL AND s.initial_grams > 0
                     THEN ? * s.price_cents / s.initial_grams END
         FROM spools s WHERE s.id = ?",
        user_id,
        job_id,
        kind,
        grams,
        note,
        now,
        grams,
        spool_id,
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// `A1` for the first tray of the first CANVAS unit.
pub fn tray_label(canvas_id: i64, tray_id: i64) -> String {
    let letter = char::from(b'A' + u8::try_from(canvas_id.rem_euclid(26)).unwrap_or(0));
    format!("{letter}{}", tray_id + 1)
}

/// Newest first.
pub async fn history(db: &Db, spool_id: i64) -> Result<Vec<Consumption>, InventoryError> {
    let rows = sqlx::query!(
        r#"SELECT c.id AS "id!", c.kind, c.grams, c.note, u.username AS "username?", c.created_at
           FROM consumption c LEFT JOIN users u ON u.id = c.user_id
           WHERE c.spool_id = ?
           ORDER BY c.created_at DESC, c.id DESC"#,
        spool_id,
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Consumption {
            id: r.id,
            // The column's CHECK constraint admits only the kinds `parse` knows.
            kind: ConsumptionKind::parse(&r.kind).unwrap_or(ConsumptionKind::Print),
            grams: r.grams,
            note: r.note,
            username: r.username,
            created_at: r.created_at,
        })
        .collect())
}

pub async fn bindings(db: &Db) -> Result<Vec<Binding>, InventoryError> {
    let rows = sqlx::query!(
        "SELECT canvas_id, tray_id, spool_id FROM tray_bindings ORDER BY canvas_id, tray_id"
    )
    .fetch_all(db)
    .await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let mut spools: HashMap<i64, Spool> = spools(db, true)
        .await?
        .into_iter()
        .map(|spool| (spool.id, spool))
        .collect();
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(Binding {
                canvas_id: r.canvas_id,
                tray_id: r.tray_id,
                spool: spools.remove(&r.spool_id)?,
            })
        })
        .collect())
}

/// Puts a spool in a tray, taking it out of any other tray and replacing the tray's spool.
pub async fn bind(
    db: &Db,
    canvas_id: i64,
    tray_id: i64,
    spool_id: i64,
    now: i64,
) -> Result<(), InventoryError> {
    let mut tx = db.begin().await?;
    let spool = sqlx::query!("SELECT archived FROM spools WHERE id = ?", spool_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(InventoryError::NotFound)?;
    if spool.archived != 0 {
        return Err(InventoryError::Archived);
    }
    sqlx::query!(
        "DELETE FROM tray_bindings WHERE spool_id = ? OR (canvas_id = ? AND tray_id = ?)",
        spool_id,
        canvas_id,
        tray_id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO tray_bindings (canvas_id, tray_id, spool_id, bound_at) VALUES (?, ?, ?, ?)",
        canvas_id,
        tray_id,
        spool_id,
        now,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Takes the spools out of the given `(canvas_id, tray_id)` trays.
pub async fn unbind_trays(db: &Db, trays: &[(i64, i64)]) -> Result<(), InventoryError> {
    let mut tx = db.begin().await?;
    for (canvas_id, tray_id) in trays {
        sqlx::query!(
            "DELETE FROM tray_bindings WHERE canvas_id = ? AND tray_id = ?",
            canvas_id,
            tray_id,
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// `#RRGGBB` from `RRGGBB` or `#rrggbb`; `None` for anything else.
pub fn normalize_color(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let hex = raw.strip_prefix('#').unwrap_or(raw);
    (hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| format!("#{}", hex.to_ascii_uppercase()))
}

pub fn parse_grams(raw: &str) -> Result<f64, String> {
    let grams: f64 = raw
        .trim()
        .replace(',', ".")
        .parse()
        .map_err(|_| "Enter the weight in grams, like 850.".to_owned())?;
    if !(0.0..=GRAMS_MAX).contains(&grams) {
        return Err(format!("The weight must be between 0 and {GRAMS_MAX} g."));
    }
    Ok(grams)
}

/// Accepts `19`, `19.9` and `19,99`: a comma is the decimal separator in much of Europe.
pub fn parse_price(raw: &str) -> Result<Option<i64>, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let invalid = || "Enter a price like 19.99, or leave it empty.".to_owned();
    let (whole, fraction) = raw.split_once(['.', ',']).unwrap_or((raw, ""));
    let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    if whole.is_empty()
        || whole.len() > 6
        || fraction.len() > 2
        || !digits(whole)
        || !digits(fraction)
    {
        return Err(invalid());
    }
    let whole: i64 = whole.parse().map_err(|_| invalid())?;
    let cents: i64 = format!("{fraction:0<2}").parse().map_err(|_| invalid())?;
    Ok(Some(whole * 100 + cents))
}

pub fn format_price(cents: i64) -> String {
    format!("{}.{:02}", cents / 100, cents % 100)
}

/// Case-insensitive; a tray that reports no type matches nothing.
pub fn material_matches(tray_type: &str, material: &str) -> bool {
    let tray_type = tray_type.trim();
    !tray_type.is_empty() && tray_type.eq_ignore_ascii_case(material.trim())
}

/// Squared RGB distance, or `None` unless both are `#RRGGBB`.
pub fn color_distance(a: &str, b: &str) -> Option<u32> {
    let rgb = |raw: &str| u32::from_str_radix(&normalize_color(raw)?[1..], 16).ok();
    let (a, b) = (rgb(a)?, rgb(b)?);
    Some(
        [16, 8, 0]
            .into_iter()
            .map(|shift| {
                let d = ((a >> shift) & 0xFF).abs_diff((b >> shift) & 0xFF);
                d * d
            })
            .sum(),
    )
}

/// Orders spools for a tray's picker: matching material first, then nearest colour.
pub fn rank_for_tray(mut spools: Vec<Spool>, tray: &Tray) -> Vec<Spool> {
    spools.sort_by_key(|spool| {
        (
            !material_matches(&tray.filament_type, &spool.material),
            color_distance(&tray.filament_color, &spool.color_hex).unwrap_or(u32::MAX),
        )
    });
    spools
}

pub fn find_tray(canvas: &CanvasInfo, canvas_id: i64, tray_id: i64) -> Option<&Tray> {
    canvas
        .canvas_list
        .iter()
        .filter(|unit| i64::from(unit.canvas_id) == canvas_id)
        .flat_map(|unit| &unit.tray_list)
        .find(|tray| i64::from(tray.tray_id) == tray_id)
}

/// Bound trays that the CANVAS reports empty, as `(canvas_id, tray_id)`. Only units reporting
/// themselves connected count, so a unit that drops off keeps its bindings.
pub fn emptied_bindings(canvas: &CanvasInfo, bindings: &[Binding]) -> Vec<(i64, i64)> {
    bindings
        .iter()
        .filter(|binding| {
            canvas.canvas_list.iter().any(|unit| {
                unit.connected == 1
                    && i64::from(unit.canvas_id) == binding.canvas_id
                    && unit.tray_list.iter().any(|tray| {
                        i64::from(tray.tray_id) == binding.tray_id
                            && tray.state() == TrayState::Empty
                    })
            })
        })
        .map(|binding| (binding.canvas_id, binding.tray_id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        accounts::{self, Role},
        cc2::model::Canvas,
        store,
    };

    fn fields(material: &str, color_hex: &str) -> SpoolFields {
        SpoolFields {
            material: material.to_owned(),
            brand: "Elegoo".to_owned(),
            color_name: String::new(),
            color_hex: color_hex.to_owned(),
            price_cents: None,
            initial_grams: 1000.0,
            notes: String::new(),
        }
    }

    fn spool_with(id: i64, material: &str, color_hex: &str) -> Spool {
        Spool {
            id,
            material: material.to_owned(),
            brand: String::new(),
            color_name: String::new(),
            color_hex: color_hex.to_owned(),
            owner: None,
            price_cents: None,
            initial_grams: 1000.0,
            remaining_grams: 1000.0,
            notes: String::new(),
            archived: false,
            created_at: 0,
        }
    }

    fn tray(tray_id: u32, filament_type: &str, color: &str, status: i64) -> Tray {
        Tray {
            tray_id,
            filament_type: filament_type.to_owned(),
            filament_color: color.to_owned(),
            status,
            ..Tray::default()
        }
    }

    async fn db_with_user() -> (Db, i64) {
        let db = store::open_in_memory().await.unwrap();
        let user = accounts::create_user(&db, "sam", "h", Role::Member, 1)
            .await
            .unwrap();
        (db, user)
    }

    #[tokio::test]
    async fn remaining_grams_move_only_through_the_ledger() {
        let (db, sam) = db_with_user().await;
        let id = create_spool(&db, &fields("PLA", "#000000"), Some(sam), 10)
            .await
            .unwrap();
        weigh_in(&db, id, sam, 800.0, 20).await.unwrap();
        weigh_in(&db, id, sam, 850.0, 30).await.unwrap();

        let mut edited = fields("PLA", "#FFFFFF");
        edited.initial_grams = 750.0;
        update_spool(&db, id, &edited, Some(sam)).await.unwrap();

        let spool = spool(&db, id).await.unwrap().unwrap();
        assert_eq!(
            spool.remaining_grams, 850.0,
            "editing leaves remaining alone"
        );
        assert_eq!(spool.initial_grams, 750.0);
        assert_eq!(spool.owner, Some((sam, "sam".to_owned())));

        let history = history(&db, id).await.unwrap();
        let grams: Vec<f64> = history.iter().map(|entry| entry.grams).collect();
        assert_eq!(grams, [-50.0, 200.0]);
        assert!(
            history
                .iter()
                .all(|entry| entry.kind == ConsumptionKind::WeighIn
                    && entry.username.as_deref() == Some("sam"))
        );

        assert!(matches!(
            weigh_in(&db, 999, sam, 1.0, 40).await,
            Err(InventoryError::NotFound)
        ));
    }

    #[tokio::test]
    async fn a_tray_holds_one_spool_and_archiving_unbinds() {
        let (db, sam) = db_with_user().await;
        let a = create_spool(&db, &fields("PLA", "#000000"), Some(sam), 1)
            .await
            .unwrap();
        let b = create_spool(&db, &fields("PETG", "#FF0000"), None, 1)
            .await
            .unwrap();
        let bound = |list: Vec<Binding>| {
            list.into_iter()
                .map(|x| (x.canvas_id, x.tray_id, x.spool.id))
                .collect::<Vec<_>>()
        };

        bind(&db, 0, 0, a, 2).await.unwrap();
        bind(&db, 0, 1, a, 3).await.unwrap();
        assert_eq!(
            bound(bindings(&db).await.unwrap()),
            [(0, 1, a)],
            "binding again moves the spool"
        );

        bind(&db, 0, 1, b, 4).await.unwrap();
        assert_eq!(
            bound(bindings(&db).await.unwrap()),
            [(0, 1, b)],
            "and replaces the tray's spool"
        );

        set_archived(&db, b, true).await.unwrap();
        assert!(bindings(&db).await.unwrap().is_empty());
        assert!(matches!(
            bind(&db, 0, 1, b, 5).await,
            Err(InventoryError::Archived)
        ));
        assert_eq!(spools(&db, false).await.unwrap().len(), 1);
        assert_eq!(spools(&db, true).await.unwrap().len(), 2);
    }

    #[test]
    fn prices_accept_either_decimal_separator() {
        assert_eq!(parse_price(""), Ok(None));
        assert_eq!(parse_price("19"), Ok(Some(1900)));
        assert_eq!(parse_price("19,9"), Ok(Some(1990)));
        assert_eq!(parse_price(" 19.99 "), Ok(Some(1999)));
        assert!(parse_price("19.999").is_err());
        assert!(parse_price("-1").is_err());
        assert!(parse_price("1e3").is_err());
        assert_eq!(format_price(1905), "19.05");
    }

    #[test]
    fn grams_and_colours_are_validated() {
        assert_eq!(parse_grams("812,5"), Ok(812.5));
        assert!(parse_grams("-1").is_err());
        assert!(parse_grams("NaN").is_err());
        assert!(parse_grams("200000").is_err());
        assert_eq!(normalize_color("2850df").as_deref(), Some("#2850DF"));
        assert_eq!(normalize_color("#fff"), None);
    }

    #[test]
    fn picker_ranks_material_before_colour() {
        let tray = tray(0, "pla", "#F0F0F0", 1);
        let spools = vec![
            spool_with(1, "PETG", "#FFFFFF"),
            spool_with(2, "PLA", "#000000"),
            spool_with(3, "PLA", "#FFFFFF"),
        ];
        let ids: Vec<i64> = rank_for_tray(spools, &tray)
            .iter()
            .map(|spool| spool.id)
            .collect();
        assert_eq!(ids, [3, 2, 1]);
        assert!(!material_matches("", ""));
    }

    #[test]
    fn only_connected_units_unbind_empty_trays() {
        let binding = |canvas_id, tray_id| Binding {
            canvas_id,
            tray_id,
            spool: spool_with(1, "PLA", "#000000"),
        };
        let canvas = CanvasInfo {
            canvas_list: vec![
                Canvas {
                    canvas_id: 0,
                    connected: 1,
                    tray_list: vec![tray(0, "PLA", "#000000", 1), tray(1, "", "", 0)],
                },
                Canvas {
                    canvas_id: 1,
                    connected: 0,
                    tray_list: vec![tray(0, "", "", 0)],
                },
            ],
            ..CanvasInfo::default()
        };
        let bindings = [binding(0, 0), binding(0, 1), binding(1, 0), binding(2, 0)];
        assert_eq!(emptied_bindings(&canvas, &bindings), [(0, 1)]);
    }
}
