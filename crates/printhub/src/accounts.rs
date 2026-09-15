//! Users, sessions and invitations. Functions take timestamps rather than reading the clock,
//! so expiry is testable.

use serde::Serialize;
use thiserror::Error;

use crate::store::Db;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Admin,
    Member,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Member => "member",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "admin" => Some(Self::Admin),
            "member" => Some(Self::Member),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub role: Role,
    pub disabled: bool,
    pub created_at: i64,
}

impl User {
    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    pub id: i64,
    pub role: Role,
    /// Set for a password reset link: the account whose password it replaces.
    pub reset_user: Option<(i64, String)>,
    pub created_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Error)]
pub enum AccountError {
    #[error("that username is taken")]
    UsernameTaken,
    #[error("the last active admin cannot be demoted or disabled")]
    LastAdmin,
    #[error("this link is invalid, expired or already used")]
    InviteInvalid,
    #[error("no such user")]
    NotFound,
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
}

pub const USERNAME_MAX: usize = 32;
pub const PASSWORD_MIN: usize = 10;
/// Argon2 cost does not depend on length, but an unbounded input is still an unbounded
/// amount of work per request.
pub const PASSWORD_MAX: usize = 256;

pub fn validate_username(username: &str) -> Result<(), String> {
    let valid_chars = username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    if username.is_empty() || username.len() > USERNAME_MAX || !valid_chars {
        return Err(format!(
            "usernames are 1 to {USERNAME_MAX} letters, digits, dots, dashes or underscores"
        ));
    }
    Ok(())
}

pub fn validate_password(password: &str) -> Result<(), String> {
    let chars = password.chars().count();
    if chars < PASSWORD_MIN || password.len() > PASSWORD_MAX {
        return Err(format!(
            "passwords need at least {PASSWORD_MIN} characters (and at most {PASSWORD_MAX} bytes)"
        ));
    }
    Ok(())
}

fn user_from(id: i64, username: String, role: &str, disabled: i64, created_at: i64) -> User {
    User {
        id,
        username,
        // The column's CHECK constraint admits only the two values `Role::parse` knows.
        role: Role::parse(role).unwrap_or(Role::Member),
        disabled: disabled != 0,
        created_at,
    }
}

pub async fn active_admin_count(db: &Db) -> Result<i64, AccountError> {
    let row = sqlx::query!(
        r#"SELECT COUNT(*) AS "count!: i64" FROM users WHERE role = 'admin' AND disabled = 0"#
    )
    .fetch_one(db)
    .await?;
    Ok(row.count)
}

pub async fn create_user(
    db: impl sqlx::SqliteExecutor<'_>,
    username: &str,
    password_hash: &str,
    role: Role,
    now: i64,
) -> Result<i64, AccountError> {
    let role = role.as_str();
    let inserted = sqlx::query!(
        "INSERT INTO users (username, password_hash, role, created_at) VALUES (?, ?, ?, ?)",
        username,
        password_hash,
        role,
        now,
    )
    .execute(db)
    .await;
    match inserted {
        Ok(result) => Ok(result.last_insert_rowid()),
        Err(sqlx::Error::Database(err)) if err.is_unique_violation() => {
            Err(AccountError::UsernameTaken)
        }
        Err(err) => Err(err.into()),
    }
}

/// The user and password hash for a login attempt; disabled users are included so the
/// caller can still spend the same time verifying.
pub async fn login_record(db: &Db, username: &str) -> Result<Option<(User, String)>, AccountError> {
    let row = sqlx::query!(
        r#"SELECT id AS "id!", username, password_hash, role, disabled, created_at
           FROM users WHERE username = ?"#,
        username,
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| {
        (
            user_from(r.id, r.username, &r.role, r.disabled, r.created_at),
            r.password_hash,
        )
    }))
}

pub async fn user(db: &Db, id: i64) -> Result<Option<User>, AccountError> {
    let row = sqlx::query!(
        r#"SELECT id AS "id!", username, role, disabled, created_at FROM users WHERE id = ?"#,
        id,
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| user_from(r.id, r.username, &r.role, r.disabled, r.created_at)))
}

pub async fn users(db: &Db) -> Result<Vec<User>, AccountError> {
    let rows = sqlx::query!(
        r#"SELECT id AS "id!", username, role, disabled, created_at
           FROM users ORDER BY username COLLATE NOCASE"#
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| user_from(r.id, r.username, &r.role, r.disabled, r.created_at))
        .collect())
}

pub async fn set_role(db: &Db, id: i64, role: Role) -> Result<(), AccountError> {
    let mut tx = db.begin().await?;
    let target = target_for_admin_change(&mut tx, id).await?;
    if target.is_admin() && !target.disabled && role != Role::Admin {
        ensure_other_admin(&mut tx).await?;
    }
    let role = role.as_str();
    sqlx::query!("UPDATE users SET role = ? WHERE id = ?", role, id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn set_disabled(db: &Db, id: i64, disabled: bool) -> Result<(), AccountError> {
    let mut tx = db.begin().await?;
    let target = target_for_admin_change(&mut tx, id).await?;
    if disabled && target.is_admin() && !target.disabled {
        ensure_other_admin(&mut tx).await?;
    }
    let flag = i64::from(disabled);
    sqlx::query!("UPDATE users SET disabled = ? WHERE id = ?", flag, id)
        .execute(&mut *tx)
        .await?;
    if disabled {
        sqlx::query!("DELETE FROM sessions WHERE user_id = ?", id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn target_for_admin_change(
    tx: &mut sqlx::SqliteConnection,
    id: i64,
) -> Result<User, AccountError> {
    let row = sqlx::query!(
        r#"SELECT id AS "id!", username, role, disabled, created_at FROM users WHERE id = ?"#,
        id,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AccountError::NotFound)?;
    Ok(user_from(
        row.id,
        row.username,
        &row.role,
        row.disabled,
        row.created_at,
    ))
}

async fn ensure_other_admin(tx: &mut sqlx::SqliteConnection) -> Result<(), AccountError> {
    let row = sqlx::query!(
        r#"SELECT COUNT(*) AS "count!: i64" FROM users WHERE role = 'admin' AND disabled = 0"#
    )
    .fetch_one(&mut *tx)
    .await?;
    if row.count <= 1 {
        return Err(AccountError::LastAdmin);
    }
    Ok(())
}

/// Replaces the password and ends every session of the user except `keep_session`.
pub async fn set_password(
    db: &Db,
    id: i64,
    password_hash: &str,
    keep_session: Option<&[u8]>,
) -> Result<(), AccountError> {
    let mut tx = db.begin().await?;
    let updated = sqlx::query!(
        "UPDATE users SET password_hash = ? WHERE id = ?",
        password_hash,
        id,
    )
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(AccountError::NotFound);
    }
    let keep = keep_session.unwrap_or_default();
    sqlx::query!(
        "DELETE FROM sessions WHERE user_id = ? AND token_hash != ?",
        id,
        keep,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn create_session(
    db: &Db,
    token_hash: &[u8],
    user_id: i64,
    now: i64,
    expires_at: i64,
) -> Result<(), AccountError> {
    sqlx::query!(
        "INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES (?, ?, ?, ?)",
        token_hash,
        user_id,
        now,
        expires_at,
    )
    .execute(db)
    .await?;
    Ok(())
}

pub async fn session_user(
    db: &Db,
    token_hash: &[u8],
    now: i64,
) -> Result<Option<User>, AccountError> {
    let row = sqlx::query!(
        r#"SELECT u.id AS "id!", u.username, u.role, u.disabled, u.created_at
           FROM sessions s JOIN users u ON u.id = s.user_id
           WHERE s.token_hash = ? AND s.expires_at > ? AND u.disabled = 0"#,
        token_hash,
        now,
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| user_from(r.id, r.username, &r.role, r.disabled, r.created_at)))
}

pub async fn delete_session(db: &Db, token_hash: &[u8]) -> Result<(), AccountError> {
    sqlx::query!("DELETE FROM sessions WHERE token_hash = ?", token_hash)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn purge_expired(db: &Db, now: i64) -> Result<(), AccountError> {
    sqlx::query!("DELETE FROM sessions WHERE expires_at <= ?", now)
        .execute(db)
        .await?;
    sqlx::query!(
        "DELETE FROM invites WHERE expires_at <= ? OR used_at IS NOT NULL",
        now
    )
    .execute(db)
    .await?;
    Ok(())
}

pub async fn create_invite(
    db: &Db,
    token_hash: &[u8],
    role: Role,
    reset_user: Option<i64>,
    created_by: i64,
    now: i64,
    expires_at: i64,
) -> Result<(), AccountError> {
    let role = role.as_str();
    sqlx::query!(
        "INSERT INTO invites (token_hash, role, reset_user, created_by, created_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?)",
        token_hash,
        role,
        reset_user,
        created_by,
        now,
        expires_at,
    )
    .execute(db)
    .await?;
    Ok(())
}

/// A usable invite: unused and unexpired.
pub async fn invite(db: &Db, token_hash: &[u8], now: i64) -> Result<Option<Invite>, AccountError> {
    let row = sqlx::query!(
        r#"SELECT i.id AS "id!", i.role, i.reset_user, u.username AS "reset_username?",
                  i.created_at, i.expires_at
           FROM invites i LEFT JOIN users u ON u.id = i.reset_user
           WHERE i.token_hash = ? AND i.used_at IS NULL AND i.expires_at > ?"#,
        token_hash,
        now,
    )
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| Invite {
        id: r.id,
        role: Role::parse(&r.role).unwrap_or(Role::Member),
        reset_user: r.reset_user.zip(r.reset_username),
        created_at: r.created_at,
        expires_at: r.expires_at,
    }))
}

pub async fn pending_invites(db: &Db, now: i64) -> Result<Vec<Invite>, AccountError> {
    let rows = sqlx::query!(
        r#"SELECT i.id AS "id!", i.role, i.reset_user, u.username AS "reset_username?",
                  i.created_at, i.expires_at
           FROM invites i LEFT JOIN users u ON u.id = i.reset_user
           WHERE i.used_at IS NULL AND i.expires_at > ?
           ORDER BY i.created_at DESC"#,
        now,
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Invite {
            id: r.id,
            role: Role::parse(&r.role).unwrap_or(Role::Member),
            reset_user: r.reset_user.zip(r.reset_username),
            created_at: r.created_at,
            expires_at: r.expires_at,
        })
        .collect())
}

pub async fn revoke_invite(db: &Db, id: i64) -> Result<(), AccountError> {
    sqlx::query!("DELETE FROM invites WHERE id = ?", id)
        .execute(db)
        .await?;
    Ok(())
}

/// Redeems an invite: creates the account, or for a reset link replaces the password and
/// ends that user's sessions. Returns the user id. `username` is ignored for reset links.
pub async fn redeem_invite(
    db: &Db,
    token_hash: &[u8],
    username: &str,
    password_hash: &str,
    now: i64,
) -> Result<i64, AccountError> {
    let mut tx = db.begin().await?;
    // Marking the invite used first, conditionally, makes a concurrent second redemption of
    // the same link update zero rows instead of racing past a separate check.
    let claimed = sqlx::query!(
        r#"UPDATE invites SET used_at = ?
           WHERE token_hash = ? AND used_at IS NULL AND expires_at > ?
           RETURNING role, reset_user"#,
        now,
        token_hash,
        now,
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AccountError::InviteInvalid)?;

    let user_id = match claimed.reset_user {
        Some(user_id) => {
            sqlx::query!(
                "UPDATE users SET password_hash = ? WHERE id = ?",
                password_hash,
                user_id,
            )
            .execute(&mut *tx)
            .await?;
            sqlx::query!("DELETE FROM sessions WHERE user_id = ?", user_id)
                .execute(&mut *tx)
                .await?;
            user_id
        }
        None => {
            let role = Role::parse(&claimed.role).unwrap_or(Role::Member);
            create_user(&mut *tx, username, password_hash, role, now).await?
        }
    };
    tx.commit().await?;
    Ok(user_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;

    async fn db_with_admin() -> (Db, i64) {
        let db = store::open_in_memory().await.unwrap();
        let admin = create_user(&db, "alex", "hash-a", Role::Admin, 100)
            .await
            .unwrap();
        (db, admin)
    }

    #[tokio::test]
    async fn usernames_are_unique_case_insensitively() {
        let (db, _) = db_with_admin().await;
        let err = create_user(&db, "ALEX", "h", Role::Member, 1)
            .await
            .unwrap_err();
        assert!(matches!(err, AccountError::UsernameTaken));
        let (user, hash) = login_record(&db, "Alex").await.unwrap().unwrap();
        assert_eq!(user.username, "alex");
        assert_eq!(hash, "hash-a");
    }

    #[tokio::test]
    async fn last_admin_is_protected() {
        let (db, admin) = db_with_admin().await;
        assert!(matches!(
            set_role(&db, admin, Role::Member).await,
            Err(AccountError::LastAdmin)
        ));
        assert!(matches!(
            set_disabled(&db, admin, true).await,
            Err(AccountError::LastAdmin)
        ));

        let second = create_user(&db, "sam", "h", Role::Admin, 1).await.unwrap();
        set_role(&db, admin, Role::Member).await.unwrap();
        assert!(matches!(
            set_disabled(&db, second, true).await,
            Err(AccountError::LastAdmin)
        ));
    }

    #[tokio::test]
    async fn sessions_expire_and_die_with_disable() {
        let (db, admin) = db_with_admin().await;
        let member = create_user(&db, "sam", "h", Role::Member, 1).await.unwrap();
        create_session(&db, b"tok", member, 10, 20).await.unwrap();

        assert_eq!(
            session_user(&db, b"tok", 15).await.unwrap().unwrap().id,
            member
        );
        assert!(session_user(&db, b"tok", 20).await.unwrap().is_none());

        create_session(&db, b"tok2", member, 10, 1000)
            .await
            .unwrap();
        set_disabled(&db, member, true).await.unwrap();
        assert!(session_user(&db, b"tok2", 15).await.unwrap().is_none());
        set_disabled(&db, member, false).await.unwrap();
        assert!(
            session_user(&db, b"tok2", 15).await.unwrap().is_none(),
            "disabling ended the session for good"
        );
        let _ = admin;
    }

    #[tokio::test]
    async fn password_change_keeps_only_the_current_session() {
        let (db, admin) = db_with_admin().await;
        create_session(&db, b"here", admin, 1, 1000).await.unwrap();
        create_session(&db, b"elsewhere", admin, 1, 1000)
            .await
            .unwrap();
        set_password(&db, admin, "new", Some(b"here"))
            .await
            .unwrap();
        assert!(session_user(&db, b"here", 2).await.unwrap().is_some());
        assert!(session_user(&db, b"elsewhere", 2).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn invite_creates_account_once() {
        let (db, admin) = db_with_admin().await;
        create_invite(&db, b"inv", Role::Member, None, admin, 10, 100)
            .await
            .unwrap();
        assert!(invite(&db, b"inv", 50).await.unwrap().is_some());

        let id = redeem_invite(&db, b"inv", "sam", "h", 50).await.unwrap();
        let user = user(&db, id).await.unwrap().unwrap();
        assert_eq!((user.username.as_str(), user.role), ("sam", Role::Member));

        assert!(matches!(
            redeem_invite(&db, b"inv", "kim", "h", 51).await,
            Err(AccountError::InviteInvalid)
        ));
        assert!(invite(&db, b"inv", 51).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn expired_invite_is_refused() {
        let (db, admin) = db_with_admin().await;
        create_invite(&db, b"old", Role::Member, None, admin, 10, 100)
            .await
            .unwrap();
        assert!(matches!(
            redeem_invite(&db, b"old", "sam", "h", 100).await,
            Err(AccountError::InviteInvalid)
        ));
    }

    #[tokio::test]
    async fn taken_username_leaves_invite_usable() {
        let (db, admin) = db_with_admin().await;
        create_invite(&db, b"inv", Role::Member, None, admin, 10, 100)
            .await
            .unwrap();
        assert!(matches!(
            redeem_invite(&db, b"inv", "alex", "h", 50).await,
            Err(AccountError::UsernameTaken)
        ));
        redeem_invite(&db, b"inv", "sam", "h", 51).await.unwrap();
    }

    #[tokio::test]
    async fn reset_link_replaces_password_and_ends_sessions() {
        let (db, admin) = db_with_admin().await;
        let member = create_user(&db, "sam", "old", Role::Member, 1)
            .await
            .unwrap();
        create_session(&db, b"s", member, 1, 1000).await.unwrap();
        create_invite(&db, b"reset", Role::Member, Some(member), admin, 10, 100)
            .await
            .unwrap();

        let invite = invite(&db, b"reset", 20).await.unwrap().unwrap();
        assert_eq!(invite.reset_user, Some((member, "sam".to_owned())));

        assert_eq!(
            redeem_invite(&db, b"reset", "ignored", "new", 20)
                .await
                .unwrap(),
            member
        );
        assert_eq!(login_record(&db, "sam").await.unwrap().unwrap().1, "new");
        assert!(session_user(&db, b"s", 21).await.unwrap().is_none());
    }

    #[test]
    fn validation() {
        assert!(validate_username("sam.k-9_x").is_ok());
        assert!(validate_username("").is_err());
        assert!(validate_username("has space").is_err());
        assert!(validate_username(&"a".repeat(33)).is_err());
        assert!(validate_password("correct horse").is_ok());
        assert!(validate_password("short").is_err());
        assert!(validate_password(&"x".repeat(257)).is_err());
    }
}
