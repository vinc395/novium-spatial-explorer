//! Access control for serving beyond this computer. Two modes:
//!
//! * Token (LAN): a private `?token=` link sets an HttpOnly cookie and redirects to the
//!   same URL without the token; API clients may send `Authorization: Bearer <token>`.
//!   An optional shared password, typed on the sign-in page (request body, never a URL),
//!   sets the same cookie.
//! * People (`--access-users-file`): an email list, each with its own password, stored as
//!   PBKDF2-HMAC-SHA256 hashes. The sign-in page asks for both, and each sign-in gets its
//!   own random session (in memory, 30 days). Removing someone from the file ends their
//!   sessions at their next request. Token links and a shared password are not accepted.
//!
//! Wrong attempts are limited per client address. Behind a local Cloudflare tunnel the
//! address comes from `CF-Connecting-IP`, trusted only on connections from this computer.
//! `/logout` drops the cookie (and the session); `/healthz` stays open.

use crate::API_VERSION;
use anyhow::{anyhow, bail, Context, Result};
use axum::extract::{FromRequest, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const AUTH_COOKIE: &str = "xw_token";
/// Wrong sign-ins allowed per client address within `FAILURE_WINDOW`.
const MAX_FAILURES: u32 = 10;
const FAILURE_WINDOW: Duration = Duration::from_secs(15 * 60);
const SESSION_LIFETIME: Duration = Duration::from_secs(30 * 24 * 3600);
/// PBKDF2-HMAC-SHA256 rounds for new password hashes (OWASP's 2023 recommendation).
pub const PBKDF2_ROUNDS: u32 = 600_000;
/// Compared against when an email is not on the list, so both cases take as long.
const DUMMY_HASH: &str = "pbkdf2-sha256$600000$00000000000000000000000000000000$0000000000000000000000000000000000000000000000000000000000000000";

fn ct_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    use std::io::Read;
    let mut b = [0u8; N];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        bail!("odd-length hex");
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| anyhow!("bad hex: {e}")))
        .collect()
}

/// 64 hex characters from the OS CSPRNG.
pub fn generate_token() -> Result<String> {
    Ok(hex(&random_bytes::<32>()?))
}

/// 20 random characters (100 bits) in groups of five, without look-alike characters.
pub fn generate_password() -> Result<String> {
    const ALPHABET: &[u8; 32] = b"abcdefghijkmnpqrstuvwxyz23456789";
    let chars: Vec<char> = random_bytes::<20>()?
        .iter()
        .map(|b| ALPHABET[(b & 31) as usize] as char)
        .collect();
    Ok(chars
        .chunks(5)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("-"))
}

/// Lower-cased address; rejects anything that is not a plain `local@domain.tld`.
pub fn normalize_email(s: &str) -> Result<String> {
    let e = s.trim().to_lowercase();
    let ok = e.len() <= 254
        && e.split('@').count() == 2
        && e.split_once('@').is_some_and(|(l, d)| {
            !l.is_empty() && d.contains('.') && !d.starts_with('.') && !d.ends_with('.')
        })
        && e.chars().all(|c| c.is_ascii_graphic());
    if !ok {
        bail!("{s:?} is not an email address");
    }
    Ok(e)
}

/// `pbkdf2-sha256$<rounds>$<salt hex>$<hash hex>` with a random 16-byte salt.
pub fn hash_password(password: &str) -> Result<String> {
    hash_password_rounds(password, PBKDF2_ROUNDS)
}

/// As [`hash_password`] with a chosen number of rounds (tests use few).
pub fn hash_password_rounds(password: &str, rounds: u32) -> Result<String> {
    let n = NonZeroU32::new(rounds).ok_or_else(|| anyhow!("rounds must be positive"))?;
    let salt = random_bytes::<16>()?;
    let mut out = [0u8; 32];
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        n,
        &salt,
        password.as_bytes(),
        &mut out,
    );
    Ok(format!(
        "pbkdf2-sha256${rounds}${}${}",
        hex(&salt),
        hex(&out)
    ))
}

fn parse_hash(s: &str) -> Result<(NonZeroU32, Vec<u8>, Vec<u8>)> {
    let p: Vec<&str> = s.split('$').collect();
    if p.len() != 4 || p[0] != "pbkdf2-sha256" {
        bail!("not a pbkdf2-sha256 password hash");
    }
    let rounds = NonZeroU32::new(p[1].parse()?).ok_or_else(|| anyhow!("zero rounds"))?;
    let (salt, hash) = (unhex(p[2])?, unhex(p[3])?);
    if salt.len() < 8 || hash.len() != 32 {
        bail!("bad salt or hash length");
    }
    Ok((rounds, salt, hash))
}

fn verify_password(password: &str, stored: &str) -> bool {
    parse_hash(stored).is_ok_and(|(rounds, salt, hash)| {
        ring::pbkdf2::verify(
            ring::pbkdf2::PBKDF2_HMAC_SHA256,
            rounds,
            &salt,
            password.as_bytes(),
            &hash,
        )
        .is_ok()
    })
}

/// Parse an access list: `email<TAB>hash` per line, `#` starts a comment line.
pub fn parse_people(text: &str) -> Result<BTreeMap<String, String>> {
    let mut m = BTreeMap::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (email, hash) = line
            .split_once('\t')
            .ok_or_else(|| anyhow!("line {}: expected email<TAB>password hash", i + 1))?;
        let email = normalize_email(email).with_context(|| format!("line {}", i + 1))?;
        parse_hash(hash.trim()).with_context(|| format!("line {}", i + 1))?;
        m.insert(email, hash.trim().to_string());
    }
    Ok(m)
}

fn read_people(path: &Path) -> Result<BTreeMap<String, String>> {
    match std::fs::read_to_string(path) {
        Ok(t) => parse_people(&t).with_context(|| format!("{}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

/// Replace the file atomically, readable by its owner only.
fn write_people(path: &Path, people: &BTreeMap<String, String>) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut text = String::from(
        "# NOVIUM Spatial Explorer access list: email<TAB>password hash (manage with `xenium-web users`)\n",
    );
    for (e, h) in people {
        text.push_str(&format!("{e}\t{h}\n"));
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.tmp", generate_token()?));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

/// Add `email` with a newly generated password, or give an existing entry a new one.
/// Returns the password; only its hash is stored.
pub fn people_add(path: &Path, email: &str) -> Result<String> {
    let password = generate_password()?;
    people_add_with_password(path, email, &password)?;
    Ok(password)
}

/// Add `email` (or update their entry) with a given password, e.g. a master password
/// shared by everyone on the list.
pub fn people_add_with_password(path: &Path, email: &str, password: &str) -> Result<()> {
    let email = normalize_email(email)?;
    check_password(password)?;
    let mut people = read_people(path)?;
    people.insert(email, hash_password(password)?);
    write_people(path, &people)
}

/// Give everyone on the list `password` (fresh salt each); returns how many entries.
pub fn people_reset_all(path: &Path, password: &str) -> Result<usize> {
    check_password(password)?;
    let mut people = read_people(path)?;
    for hash in people.values_mut() {
        *hash = hash_password(password)?;
    }
    write_people(path, &people)?;
    Ok(people.len())
}

fn check_password(password: &str) -> Result<()> {
    if password.trim() != password || password.chars().count() < 8 {
        bail!("a password needs at least 8 characters and no leading or trailing spaces");
    }
    Ok(())
}

/// Remove `email`; returns whether it was on the list.
pub fn people_remove(path: &Path, email: &str) -> Result<bool> {
    let email = normalize_email(email)?;
    let mut people = read_people(path)?;
    let found = people.remove(&email).is_some();
    if found {
        write_people(path, &people)?;
    }
    Ok(found)
}

pub fn people_list(path: &Path) -> Result<Vec<String>> {
    Ok(read_people(path)?.into_keys().collect())
}

/// The access list as last read, re-read whenever the file changes. An unreadable or
/// invalid file admits nobody until it is fixed.
struct People {
    path: PathBuf,
    stamp: Option<(std::time::SystemTime, u64, u64)>,
    hashes: BTreeMap<String, String>,
}

impl People {
    fn current(&mut self) -> &BTreeMap<String, String> {
        use std::os::unix::fs::MetadataExt;
        let stamp = std::fs::metadata(&self.path)
            .ok()
            .and_then(|m| Some((m.modified().ok()?, m.len(), m.ino())));
        if stamp.is_none() || stamp != self.stamp {
            match read_people(&self.path) {
                Ok(h) if stamp.is_some() => self.hashes = h,
                Ok(_) => self.hashes.clear(),
                Err(e) => {
                    if !self.hashes.is_empty() || self.stamp.is_some() {
                        tracing::warn!("{e:#}; nobody can sign in until the access list is fixed");
                    }
                    self.hashes.clear();
                }
            }
            self.stamp = stamp;
        }
        &self.hashes
    }
}

/// Shared-secret or per-person access control (see the module documentation).
#[derive(Clone)]
pub struct Auth {
    token: Option<Arc<String>>,
    password: Option<Arc<String>>,
    people: Option<Arc<Mutex<People>>>,
    /// session id -> (email, started)
    sessions: Arc<Mutex<HashMap<String, (String, Instant)>>>,
    /// Caps the CPU spent on password hashing at two sign-ins at a time.
    hashing: Arc<tokio::sync::Semaphore>,
    secure_cookie: bool,
    behind_cloudflare: bool,
    failures: Arc<Mutex<HashMap<IpAddr, (u32, Instant)>>>,
}

impl Auth {
    fn base(secure_cookie: bool) -> Self {
        Auth {
            token: None,
            password: None,
            people: None,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            hashing: Arc::new(tokio::sync::Semaphore::new(2)),
            secure_cookie,
            behind_cloudflare: false,
            failures: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Token mode: the private link (and bearer token).
    pub fn new(token: String, secure_cookie: bool) -> Result<Self> {
        if token.len() < 32 || !token.bytes().all(|b| b.is_ascii_alphanumeric()) {
            bail!("access token must be at least 32 alphanumeric characters");
        }
        Ok(Auth {
            token: Some(Arc::new(token)),
            ..Self::base(secure_cookie)
        })
    }

    /// Token mode: also accept a shared `password` on the sign-in page.
    pub fn with_password(mut self, password: &str) -> Result<Self> {
        let p = password.trim();
        if p.chars().count() < 8 {
            bail!("the password must have at least 8 characters");
        }
        self.password = Some(Arc::new(p.to_string()));
        Ok(self)
    }

    /// People mode: the email list with password hashes in `users_file`.
    pub fn people(users_file: &Path, secure_cookie: bool) -> Result<Self> {
        let mut people = People {
            path: users_file.to_path_buf(),
            stamp: None,
            hashes: BTreeMap::new(),
        };
        read_people(users_file)?; // fail at start-up on a broken file
        people.current();
        Ok(Auth {
            people: Some(Arc::new(Mutex::new(people))),
            ..Self::base(secure_cookie)
        })
    }

    /// Reached only through a local Cloudflare tunnel (HTTPS at Cloudflare): cookies are
    /// Secure, and wrong sign-ins are counted per visitor (`CF-Connecting-IP`).
    pub fn behind_cloudflare(mut self) -> Self {
        self.behind_cloudflare = true;
        self.secure_cookie = true;
        self
    }

    /// Number of people on the access list (people mode).
    pub fn people_count(&self) -> Option<usize> {
        self.people
            .as_ref()
            .map(|p| p.lock().unwrap().current().len())
    }

    fn matches_token(&self, candidate: &str) -> bool {
        self.token.as_ref().is_some_and(|t| ct_eq(candidate, t))
    }

    fn matches_password(&self, candidate: &str) -> bool {
        self.password.as_ref().is_some_and(|p| ct_eq(candidate, p))
    }

    fn client_ip(&self, req: &axum::extract::Request) -> IpAddr {
        let peer = req
            .extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|c| c.0.ip())
            .unwrap_or(IpAddr::from([0, 0, 0, 0]));
        if self.behind_cloudflare && (peer.is_loopback() || peer.is_unspecified()) {
            if let Some(ip) = req
                .headers()
                .get("cf-connecting-ip")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse().ok())
            {
                return ip;
            }
        }
        peer
    }

    /// Remaining lockout for an address with too many recent wrong attempts.
    fn locked(&self, ip: IpAddr) -> Option<Duration> {
        let f = self.failures.lock().unwrap();
        let (n, since) = f.get(&ip)?;
        let left = FAILURE_WINDOW.checked_sub(since.elapsed())?;
        (*n >= MAX_FAILURES).then_some(left)
    }

    fn record_failure(&self, ip: IpAddr) {
        let mut f = self.failures.lock().unwrap();
        f.retain(|_, (_, t)| t.elapsed() < FAILURE_WINDOW);
        let e = f.entry(ip).or_insert((0, Instant::now()));
        e.0 += 1;
    }

    fn clear(&self, ip: IpAddr) {
        self.failures.lock().unwrap().remove(&ip);
    }

    /// The person of a live session whose email is still on the list.
    fn session_user(&self, id: &str) -> Option<String> {
        let mut s = self.sessions.lock().unwrap();
        let (email, started) = s.get(id)?.clone();
        let listed = self
            .people
            .as_ref()
            .is_some_and(|p| p.lock().unwrap().current().contains_key(&email));
        if started.elapsed() > SESSION_LIFETIME || !listed {
            s.remove(id);
            return None;
        }
        Some(email)
    }

    /// Who sent this request: `Some(None)` for the token, `Some(Some(email))` for a person.
    fn identify(&self, headers: &HeaderMap) -> Option<Option<String>> {
        let cookie = cookie_token(headers);
        if self.people.is_some() {
            return cookie.and_then(|c| self.session_user(&c)).map(Some);
        }
        let bearer = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        (bearer.is_some_and(|t| self.matches_token(t))
            || cookie.is_some_and(|c| self.matches_token(&c)))
        .then_some(None)
    }

    /// Set the session cookie and go to `location`: the token itself in token mode, a new
    /// random session in people mode.
    fn start_session(&self, person: Option<&str>, location: &str) -> Response {
        let value = match (person, &self.token) {
            (Some(email), _) => {
                let Ok(id) = generate_token() else {
                    return (StatusCode::INTERNAL_SERVER_ERROR, "no randomness").into_response();
                };
                let mut s = self.sessions.lock().unwrap();
                s.retain(|_, (_, t)| t.elapsed() < SESSION_LIFETIME);
                s.insert(id.clone(), (email.to_string(), Instant::now()));
                id
            }
            (None, Some(t)) => t.to_string(),
            (None, None) => return (StatusCode::FORBIDDEN, "no access configured").into_response(),
        };
        let cookie = format!(
            "{AUTH_COOKIE}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
            SESSION_LIFETIME.as_secs(),
            if self.secure_cookie { "; Secure" } else { "" }
        );
        let mut h = HeaderMap::new();
        if let (Ok(c), Ok(l)) = (
            HeaderValue::from_str(&cookie),
            HeaderValue::from_str(location),
        ) {
            h.insert(header::SET_COOKIE, c);
            h.insert(header::LOCATION, l);
        }
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        (StatusCode::SEE_OTHER, h).into_response()
    }

    /// Drop this browser's cookie (and its session) and return to the sign-in page. A
    /// token stays valid, so shared links keep working.
    fn sign_out(&self, headers: &HeaderMap) -> Response {
        if let Some(id) = cookie_token(headers) {
            self.sessions.lock().unwrap().remove(&id);
        }
        let cookie = format!(
            "{AUTH_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{}",
            if self.secure_cookie { "; Secure" } else { "" }
        );
        let mut h = HeaderMap::new();
        if let Ok(c) = HeaderValue::from_str(&cookie) {
            h.insert(header::SET_COOKIE, c);
        }
        h.insert(header::LOCATION, HeaderValue::from_static("/"));
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        (StatusCode::SEE_OTHER, h).into_response()
    }

    async fn sign_in(&self, req: axum::extract::Request, ip: IpAddr) -> Response {
        if let Some(left) = self.locked(ip) {
            return too_many(self, "/", left);
        }
        let Ok(axum::Form(f)) = axum::Form::<LoginForm>::from_request(req, &()).await else {
            return login_page(
                self,
                StatusCode::BAD_REQUEST,
                "/",
                Some("Please fill in the form."),
            );
        };
        let next = safe_next(&f.next);
        let secret = f.secret.trim().to_string();
        let person = if let Some(people) = &self.people {
            let email = normalize_email(&f.email).ok();
            let stored = email
                .as_ref()
                .and_then(|e| people.lock().unwrap().current().get(e).cloned());
            let Ok(_permit) = self.hashing.acquire().await else {
                return (StatusCode::SERVICE_UNAVAILABLE, "shutting down").into_response();
            };
            let known = stored.is_some();
            let ok = tokio::task::spawn_blocking(move || {
                verify_password(&secret, stored.as_deref().unwrap_or(DUMMY_HASH))
            })
            .await
            .unwrap_or(false);
            (ok && known).then_some(email)
        } else {
            (self.matches_password(&secret) || self.matches_token(&secret)).then_some(None)
        };
        match person {
            Some(person) => {
                self.clear(ip);
                if let Some(email) = &person {
                    tracing::info!("signed in: {email} from {ip}");
                }
                self.start_session(person.as_deref(), &next)
            }
            None => {
                self.record_failure(ip);
                tracing::warn!("rejected a sign-in attempt from {ip}");
                tokio::time::sleep(Duration::from_millis(500)).await;
                let msg = if self.people.is_some() {
                    "Wrong email or password. Please try again."
                } else {
                    "Wrong password. Please try again."
                };
                login_page(self, StatusCode::UNAUTHORIZED, &next, Some(msg))
            }
        }
    }
}

/// The NOVIUM Spatial Explorer logo for the sign-in page (from `scripts/brand_assets.py`).
const LOGO_SVG: &str = include_str!("novium-logo.svg");

/// Only same-site paths are followed after signing in.
fn safe_next(s: &str) -> String {
    if s.starts_with('/') && !s.starts_with("//") && !s.contains('\\') && s.len() <= 2048 {
        s.to_string()
    } else {
        "/".to_string()
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn login_page(auth: &Auth, status: StatusCode, next: &str, error: Option<&str>) -> Response {
    let fields = if auth.people.is_some() {
        r#"<label>Email<br><input name="email" type="email" autocomplete="username" required autofocus></label><label>Password<br><input name="secret" type="password" autocomplete="current-password" required></label>"#
    } else if auth.password.is_some() {
        r#"<label>Password<br><input name="secret" type="password" autocomplete="current-password" required autofocus></label>"#
    } else {
        r#"<label>Access token<br><input name="secret" type="password" autocomplete="current-password" required autofocus></label>"#
    };
    let hint = if auth.people.is_some() {
        "Access is limited to listed people. Ask the person running NOVIUM Spatial Explorer to add you."
    } else {
        "Ask the person running NOVIUM Spatial Explorer for the password or link."
    };
    let error = error
        .map(|e| format!("<p class=\"err\" role=\"alert\">{}</p>", html_escape(e)))
        .unwrap_or_default();
    let page = format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>NOVIUM Spatial Explorer – sign in</title>
<style>body{{font:15px system-ui,sans-serif;display:grid;place-items:center;min-height:100vh;margin:0;background:#f6f7f9;color:#1d2330}}
form{{background:#fff;padding:24px 28px;border-radius:10px;box-shadow:0 4px 20px rgba(0,0,0,.08);display:grid;gap:10px;min-width:300px;max-width:360px}}
input{{font:inherit;width:100%;box-sizing:border-box;padding:6px 8px;border:1px solid #ccd;border-radius:6px}}button{{font:inherit;padding:6px;border-radius:6px;border:0;background:#2f6fde;color:#fff;cursor:pointer}}
.err{{color:#c62828;margin:0}}small{{color:#6b7280}}
.logo{{width:300px;max-width:100%;margin:0 auto 6px}}.logo svg{{display:block;width:100%;height:auto}}
@media (prefers-color-scheme:dark){{body{{background:#14161a;color:#e6e8ec}}form{{background:#1c1f25;box-shadow:none;border:1px solid #2c3038}}input{{background:#14161a;color:#e6e8ec;border-color:#3a3f48}}button{{background:#6b9cff;color:#0b1020}}.err{{color:#ff7b7b}}small{{color:#a3aab6}}.logo .ink{{fill:#e3eaff}}.logo .ink-text{{fill:#e3eaff;stroke:#e3eaff}}}}</style></head>
<body><form method="post" action="/login"><div class="logo">{logo}</div>{error}{fields}<input type="hidden" name="next" value="{next}"><button>Sign in</button><small>{hint}</small></form></body></html>"#,
        next = html_escape(next),
        logo = LOGO_SVG,
    );
    let mut h = HeaderMap::new();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // This page is answered before the router, so it sets its own security headers.
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
        ),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    (status, h, page).into_response()
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == AUTH_COOKIE)
        .map(|(_, v)| v.to_string())
}

#[derive(Deserialize)]
struct LoginForm {
    #[serde(default)]
    email: String,
    secret: String,
    #[serde(default)]
    next: String,
}

fn too_many(auth: &Auth, next: &str, left: Duration) -> Response {
    let mins = left.as_secs().div_ceil(60).max(1);
    login_page(
        auth,
        StatusCode::TOO_MANY_REQUESTS,
        next,
        Some(&format!(
            "Too many wrong attempts from this computer. Try again in {mins} minute{}.",
            if mins == 1 { "" } else { "s" }
        )),
    )
}

async fn auth_layer(
    State(auth): State<Auth>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = req.uri().path().to_string();
    if path == "/healthz" {
        return next.run(req).await;
    }
    // Signing out needs no credentials: it only clears this browser's cookie and session.
    if path == "/logout" {
        return auth.sign_out(req.headers());
    }
    let ip = auth.client_ip(&req);
    // Sign-in form: email and password, password, or token, posted in the body.
    if path == "/login" && req.method() == axum::http::Method::POST {
        return auth.sign_in(req, ip).await;
    }
    if let Some(person) = auth.identify(req.headers()) {
        // Lets the viewer offer "Sign out" only when there is something to sign out of.
        if path
            .strip_prefix("/api/")
            .and_then(|p| p.strip_prefix(API_VERSION))
            == Some("/session")
        {
            let password = auth.password.is_some() || auth.people.is_some();
            return Json(json!({"auth": true, "password": password, "user": person}))
                .into_response();
        }
        return next.run(req).await;
    }
    // One-time link: ?token=... -> cookie + redirect without the token (token mode only).
    let query = req.uri().query().unwrap_or("");
    let mut rest = vec![];
    let mut supplied = None;
    for kv in query.split('&').filter(|s| !s.is_empty()) {
        match kv.split_once('=') {
            Some(("token", v)) => supplied = Some(v.to_string()),
            _ => rest.push(kv),
        }
    }
    let here = if rest.is_empty() {
        path.clone()
    } else {
        format!("{path}?{}", rest.join("&"))
    };
    if let (Some(t), true) = (supplied, auth.people.is_none()) {
        if let Some(left) = auth.locked(ip) {
            return too_many(&auth, &here, left);
        }
        if auth.matches_token(&t) {
            auth.clear(ip);
            return auth.start_session(None, &here);
        }
        auth.record_failure(ip);
        tracing::warn!("rejected an invalid access token from {ip}");
    }
    if path.starts_with("/api/") {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "authentication required"})),
        )
            .into_response();
    }
    login_page(&auth, StatusCode::UNAUTHORIZED, &safe_next(&here), None)
}

/// Wrap a router with access control.
pub fn with_auth(app: Router, auth: Auth) -> Router {
    app.layer(axum::middleware::from_fn_with_state(auth, auth_layer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_hash_and_verify() {
        let h = hash_password_rounds("correct horse", 1000).unwrap();
        assert!(h.starts_with("pbkdf2-sha256$1000$"));
        assert!(verify_password("correct horse", &h));
        assert!(!verify_password("correct horsf", &h));
        assert!(!verify_password("x", "pbkdf2-sha256$0$00$00"));
        assert_ne!(
            h,
            hash_password_rounds("correct horse", 1000).unwrap(),
            "salted"
        );
        let p = generate_password().unwrap();
        assert_eq!(p.len(), 23);
        assert!(p.split('-').all(|g| g.len() == 5));
        assert!(!p.contains(['l', 'o', '0', '1']));
    }

    #[test]
    fn emails_are_normalized_or_rejected() {
        assert_eq!(
            normalize_email(" A.B@Example.org ").unwrap(),
            "a.b@example.org"
        );
        for bad in [
            "", "a", "a@b", "@b.c", "a@@b.c", "a b@c.d", "a@.c", "a@c.", "a\t@b.c",
        ] {
            assert!(normalize_email(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn access_list_file_round_trip() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("people/users.tsv");
        assert!(people_list(&f).unwrap().is_empty());
        let pw = people_add(&f, "B@x.org").unwrap();
        people_add(&f, "a@x.org").unwrap();
        assert_eq!(people_list(&f).unwrap(), ["a@x.org", "b@x.org"]);
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(!text.contains(&pw), "only the hash is stored");
        assert_eq!(
            std::fs::metadata(&f).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let m = parse_people(&text).unwrap();
        assert!(verify_password(&pw, &m["b@x.org"]));
        assert!(people_remove(&f, "b@X.org").unwrap());
        assert!(!people_remove(&f, "b@x.org").unwrap());
        assert_eq!(people_list(&f).unwrap(), ["a@x.org"]);
        // A shared (master) password for some or all entries.
        people_add_with_password(&f, "c@x.org", "master-pass-1").unwrap();
        assert_eq!(people_list(&f).unwrap(), ["a@x.org", "c@x.org"]);
        assert_eq!(people_reset_all(&f, "master-pass-2").unwrap(), 2);
        let m = parse_people(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert!(m.values().all(|h| verify_password("master-pass-2", h)));
        assert_ne!(m["a@x.org"], m["c@x.org"], "each entry has its own salt");
        assert!(people_add_with_password(&f, "d@x.org", "short").is_err());
        assert!(parse_people("a@x.org no-tab-hash\n").is_err());
        assert!(parse_people("a@x.org\tplain\n").is_err());
    }
}
