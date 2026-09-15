//! Password hashing, bearer tokens, cookies and the request checks around them.

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use anyhow::anyhow;
use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
use axum::http::{HeaderMap, header};
use sha2::{Digest, Sha256};

pub const SESSION_COOKIE: &str = "printhub_session";
pub const SESSION_TTL: Duration = Duration::from_secs(30 * 24 * 3600);
pub const INVITE_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

pub async fn hash_password(password: String) -> anyhow::Result<String> {
    tokio::task::spawn_blocking(move || {
        Argon2::default()
            .hash_password(password.as_bytes())
            .map(|hash| hash.to_string())
            .map_err(|err| anyhow!("hashing password: {err}"))
    })
    .await?
}

/// `None` stands for "no such user": a fixed hash is verified anyway, so a login for an unknown
/// name takes as long as one with a wrong password.
pub async fn verify_password(password: String, hash: Option<String>) -> bool {
    let known = hash.is_some();
    let hash = match hash {
        Some(hash) => hash,
        None => dummy_hash().await,
    };
    let matches = tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash).is_ok_and(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
    })
    .await
    .unwrap_or(false);
    known && matches
}

async fn dummy_hash() -> String {
    static DUMMY: OnceLock<String> = OnceLock::new();
    if let Some(hash) = DUMMY.get() {
        return hash.clone();
    }
    let hash = hash_password("printhub-timing-equaliser".into())
        .await
        .unwrap_or_default();
    DUMMY.get_or_init(|| hash).clone()
}

/// A random bearer token and the SHA-256 digest that is stored in its place.
pub fn new_token() -> anyhow::Result<(String, Vec<u8>)> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|err| anyhow!("system randomness: {err}"))?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let hash = token_hash(&token);
    Ok((token, hash))
}

pub fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

pub fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| {
            let (key, value) = pair.trim().split_once('=')?;
            (key == name).then_some(value)
        })
}

pub fn session_cookie(token: &str, secure: bool) -> String {
    format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}",
        SESSION_TTL.as_secs(),
        if secure { "; Secure" } else { "" }
    )
}

pub fn clear_session_cookie(secure: bool) -> String {
    format!(
        "{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{}",
        if secure { "; Secure" } else { "" }
    )
}

/// The host the browser addressed: `X-Forwarded-Host` behind a trusted proxy, `Host` otherwise.
pub fn request_host(headers: &HeaderMap, trust_proxy: bool) -> Option<&str> {
    let forwarded = trust_proxy
        .then(|| first_value(headers, "x-forwarded-host"))
        .flatten();
    forwarded.or_else(|| headers.get(header::HOST)?.to_str().ok())
}

pub fn is_https(headers: &HeaderMap, trust_proxy: bool) -> bool {
    trust_proxy
        && first_value(headers, "x-forwarded-proto")
            .is_some_and(|p| p.eq_ignore_ascii_case("https"))
}

/// Proxies append to these headers, so only the first entry is the client-facing one.
fn first_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let value = headers.get(name)?.to_str().ok()?;
    value.split(',').next().map(str::trim)
}

/// Whether a state-changing request came from a page on this same host. `SameSite=Lax`
/// already withholds the session cookie from cross-site form posts; this also rejects other
/// origins on the same site, such as another service on the same LAN hostname.
pub fn same_origin(headers: &HeaderMap, trust_proxy: bool) -> bool {
    let Some(host) = request_host(headers, trust_proxy) else {
        return false;
    };
    headers
        .get(header::ORIGIN)
        .or_else(|| headers.get(header::REFERER))
        .and_then(|value| value.to_str().ok())
        .and_then(authority)
        .is_some_and(|authority| authority.eq_ignore_ascii_case(host))
}

fn authority(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    rest.split(['/', '?', '#']).next()
}

/// Failed logins per username. Past `FREE_ATTEMPTS`, each attempt must wait twice as long as
/// the previous one, up to `MAX_LOCKOUT`.
#[derive(Default)]
pub struct LoginLimiter {
    failures: Mutex<HashMap<String, Failures>>,
}

struct Failures {
    count: u32,
    last: Instant,
}

const FREE_ATTEMPTS: u32 = 5;
const MAX_LOCKOUT: Duration = Duration::from_secs(15 * 60);
const MAX_TRACKED: usize = 1024;

impl LoginLimiter {
    pub fn retry_after(&self, username: &str) -> Option<Duration> {
        let failures = self.failures.lock().unwrap();
        let entry = failures.get(&username.to_lowercase())?;
        let wait = lockout(entry.count)?;
        let elapsed = entry.last.elapsed();
        (elapsed < wait).then(|| wait - elapsed)
    }

    pub fn record_failure(&self, username: &str) {
        let mut failures = self.failures.lock().unwrap();
        if failures.len() >= MAX_TRACKED {
            failures.retain(|_, entry| entry.last.elapsed() < MAX_LOCKOUT);
        }
        let entry = failures.entry(username.to_lowercase()).or_insert(Failures {
            count: 0,
            last: Instant::now(),
        });
        entry.count += 1;
        entry.last = Instant::now();
    }

    pub fn record_success(&self, username: &str) {
        self.failures
            .lock()
            .unwrap()
            .remove(&username.to_lowercase());
    }
}

fn lockout(failures: u32) -> Option<Duration> {
    let over = failures.checked_sub(FREE_ATTEMPTS)?;
    Some(Duration::from_secs(1u64 << over.min(20)).min(MAX_LOCKOUT))
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, HeaderValue::from_static(value));
        }
        map
    }

    #[tokio::test]
    async fn password_round_trip() {
        let hash = hash_password("correct horse".into()).await.unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse".into(), Some(hash.clone())).await);
        assert!(!verify_password("wrong horse".into(), Some(hash)).await);
        assert!(!verify_password("anything".into(), None).await);
    }

    #[test]
    fn tokens_are_random_and_hashed() {
        let (a, hash_a) = new_token().unwrap();
        let (b, _) = new_token().unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), 64);
        assert_eq!(hash_a, token_hash(&a));
        assert_eq!(hash_a.len(), 32);
    }

    #[test]
    fn cookie_parsing() {
        let map = headers(&[("cookie", "a=1; printhub_session=abc"), ("cookie", "b=2")]);
        assert_eq!(cookie_value(&map, SESSION_COOKIE), Some("abc"));
        assert_eq!(cookie_value(&map, "b"), Some("2"));
        assert_eq!(cookie_value(&map, "c"), None);
    }

    #[test]
    fn same_origin_checks() {
        assert!(same_origin(
            &headers(&[("host", "pi5:8080"), ("origin", "http://pi5:8080")]),
            false
        ));
        assert!(!same_origin(
            &headers(&[("host", "pi5:8080"), ("origin", "http://evil:8080")]),
            false
        ));
        assert!(same_origin(
            &headers(&[
                ("host", "pi5:8080"),
                ("referer", "http://pi5:8080/admin?x=1")
            ]),
            false
        ));
        assert!(!same_origin(&headers(&[("host", "pi5:8080")]), false));
        assert!(!same_origin(
            &headers(&[("host", "pi5:8080"), ("origin", "null")]),
            false
        ));

        let proxied = headers(&[
            ("host", "127.0.0.1:8080"),
            ("x-forwarded-host", "printer.tail.ts.net"),
            ("x-forwarded-proto", "https"),
            ("origin", "https://printer.tail.ts.net"),
        ]);
        assert!(same_origin(&proxied, true));
        assert!(
            !same_origin(&proxied, false),
            "forwarded host ignored unless trusted"
        );
        assert!(is_https(&proxied, true));
        assert!(!is_https(&proxied, false));
    }

    #[test]
    fn limiter_locks_after_free_attempts() {
        let limiter = LoginLimiter::default();
        for _ in 0..FREE_ATTEMPTS - 1 {
            limiter.record_failure("Sam");
        }
        assert!(limiter.retry_after("sam").is_none());
        limiter.record_failure("SAM");
        assert!(limiter.retry_after("sam").is_some());
        limiter.record_success("sam");
        assert!(limiter.retry_after("sam").is_none());
    }

    #[test]
    fn lockout_doubles_and_caps() {
        assert_eq!(lockout(4), None);
        assert_eq!(lockout(5), Some(Duration::from_secs(1)));
        assert_eq!(lockout(8), Some(Duration::from_secs(8)));
        assert_eq!(lockout(60), Some(MAX_LOCKOUT));
    }
}
