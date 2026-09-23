//! The web interface end to end: the real router on a local port, an in-memory database and
//! the fake printer.

use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use fakeprinter::{FakePrinter, Options};
use futures::StreamExt;
use printhub::{
    accounts::{self, Role},
    auth,
    camera::{self, CameraHub},
    cc2::{ClientConfig, PrinterClient, Timing},
    config::Config,
    dispatcher, gcode, inventory,
    jobs::{self, JobState},
    printer::PrinterLink,
    schedule,
    slicer::Slicer,
    store::{self, Db},
    web::{self, AppState},
};
use reqwest::{
    StatusCode,
    header::{CONTENT_TYPE, LOCATION, SET_COOKIE},
};
use tokio::{net::TcpListener, time::timeout};

const ADMIN_PASSWORD: &str = "admin-password";
const MEMBER_PASSWORD: &str = "member-password";
const WAIT: Duration = Duration::from_secs(10);
const CUBE_GCODE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/orca-2.4.2-cc2-pla-cube.gcode"
);
const PROCESS: &str = "0.20mm Standard @Elegoo CC2 0.4 nozzle";

struct App {
    base: String,
    http: reqwest::Client,
    printer: FakePrinter,
    db: Db,
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "printhub-web-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn app() -> App {
    app_with(None).await
}

async fn app_with(slicer: Option<Slicer>) -> App {
    let printer = FakePrinter::start(Options::default()).await.unwrap();
    let env = HashMap::from([
        ("PRINTER_HOST", "127.0.0.1".to_owned()),
        ("PRINTER_SN", printer.serial.clone()),
        ("PRINTER_ACCESS_CODE", printer.password.clone()),
        ("PRINTER_MQTT_PORT", printer.mqtt_addr.port().to_string()),
        (
            "PRINTER_UPLOAD_PORT",
            printer.upload_addr.port().to_string(),
        ),
        (
            "PRINTER_CAMERA_PORT",
            printer.camera_addr.port().to_string(),
        ),
        ("DATA_DIR", scratch("data").display().to_string()),
    ]);
    let config = Config::from_lookup(|var| env.get(var).cloned()).unwrap();

    let db = store::open_in_memory().await.unwrap();
    let hash = auth::hash_password(ADMIN_PASSWORD.into()).await.unwrap();
    accounts::create_user(&db, "admin", &hash, Role::Admin, store::now())
        .await
        .unwrap();

    let camera = CameraHub::new(
        reqwest::Client::new(),
        web::camera_url(&config),
        Duration::from_millis(300),
        camera::STALL_TIMEOUT,
    );
    let link = PrinterLink::start(&config);
    let state = AppState::new(db.clone(), config, link, Some(camera), slicer)
        .await
        .unwrap();
    tokio::spawn(web::unbind_emptied_trays(state.clone()));
    tokio::spawn(dispatcher::run(state.clone()));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, web::router(state)).await });

    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    App {
        base,
        http,
        printer,
        db,
    }
}

fn form_body(fields: &[(&str, &str)]) -> String {
    let encode = |s: &str| {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect::<String>()
    };
    fields
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn multipart_body(fields: &[(&str, &str)], file_name: &str, file: &[u8]) -> (String, Vec<u8>) {
    const BOUNDARY: &str = "printhub-test-boundary";
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend_from_slice(
            format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(file);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={BOUNDARY}"), body)
}

fn session_cookie(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("printhub_session=") && !v.starts_with("printhub_session=;"))
        .and_then(|v| v.split(';').next())
        .map(str::to_owned)
}

fn spool_fields<'a>(
    material: &'a str,
    color_name: &'a str,
    color_hex: &'a str,
) -> Vec<(&'a str, &'a str)> {
    vec![
        ("material", material),
        ("brand", "Elegoo"),
        ("color_name", color_name),
        ("color_hex", color_hex),
        ("initial_grams", "1000"),
        ("price", "19,99"),
    ]
}

/// The id at the end of a path like `/spools/3?done=created`.
fn id_of(path: &str) -> String {
    path.split('?')
        .next()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned()
}

impl App {
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    async fn get(&self, path: &str, cookie: Option<&str>) -> reqwest::Response {
        let mut request = self.http.get(self.url(path));
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        request.send().await.unwrap()
    }

    async fn post(
        &self,
        path: &str,
        cookie: Option<&str>,
        fields: &[(&str, &str)],
    ) -> reqwest::Response {
        let mut request = self
            .http
            .post(self.url(path))
            .header("origin", &self.base)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form_body(fields));
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        request.send().await.unwrap()
    }

    async fn upload(
        &self,
        cookie: &str,
        fields: &[(&str, &str)],
        file_name: &str,
        file: &[u8],
    ) -> reqwest::Response {
        let (content_type, body) = multipart_body(fields, file_name, file);
        self.http
            .post(self.url("/jobs"))
            .header("origin", &self.base)
            .header("cookie", cookie)
            .header(CONTENT_TYPE, content_type)
            .body(body)
            .send()
            .await
            .unwrap()
    }

    async fn login(&self, username: &str, password: &str) -> String {
        let response = self
            .post(
                "/login",
                None,
                &[("username", username), ("password", password)],
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::SEE_OTHER,
            "login as {username}"
        );
        session_cookie(&response).expect("session cookie")
    }

    async fn member(&self, username: &str) -> String {
        let hash = auth::hash_password(MEMBER_PASSWORD.into()).await.unwrap();
        accounts::create_user(&self.db, username, &hash, Role::Member, store::now())
            .await
            .unwrap();
        self.login(username, MEMBER_PASSWORD).await
    }

    /// Adds a spool and returns the path of its page.
    async fn add_spool(&self, cookie: &str, fields: &[(&str, &str)]) -> String {
        let response = self.post("/spools", Some(cookie), fields).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response.headers()[LOCATION].to_str().unwrap();
        location.split('?').next().unwrap().to_owned()
    }

    /// Tray contents arrive shortly after the app registers with the printer, and binding
    /// is refused until then.
    async fn bind(&self, cookie: &str, tray: u32, spool_id: &str) {
        timeout(WAIT, async {
            loop {
                let response = self
                    .post(
                        &format!("/trays/0/{tray}/bind"),
                        Some(cookie),
                        &[("spool_id", spool_id)],
                    )
                    .await;
                if response.status() == StatusCode::SEE_OTHER {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("the tray accepts a spool");
    }

    async fn wait_for_job(&self, id: i64, done: impl Fn(&jobs::Job) -> bool) -> jobs::Job {
        timeout(WAIT, async {
            loop {
                let job = jobs::get(&self.db, id).await.unwrap().unwrap();
                if done(&job) {
                    return job;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!("job {id} never got there");
        })
    }

    /// Polls until `done` holds, for anything the printer reports back asynchronously.
    async fn wait_for<F: Future<Output = bool>>(&self, done: impl Fn() -> F) -> () {
        timeout(WAIT, async {
            while !done().await {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("never got there")
    }

    /// Reads a streaming response until `needle` appears in what has arrived.
    async fn read_until(response: reqwest::Response, needle: &str) -> String {
        let mut stream = response.bytes_stream();
        let mut seen = Vec::new();
        timeout(WAIT, async {
            while let Some(chunk) = stream.next().await {
                seen.extend_from_slice(&chunk.unwrap());
                if String::from_utf8_lossy(&seen).contains(needle) {
                    return;
                }
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{needle:?} never arrived; got {:?}",
                String::from_utf8_lossy(&seen)
            )
        });
        String::from_utf8_lossy(&seen).into_owned()
    }
}

#[tokio::test]
async fn anonymous_visitors_are_sent_to_login() {
    let app = app().await;
    let response = app.get("/", None).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()[LOCATION], "/login");

    let login = app.get("/login", None).await;
    assert_eq!(login.status(), StatusCode::OK);
    assert!(login.text().await.unwrap().contains("Log in"));

    assert_eq!(app.get("/healthz", None).await.status(), StatusCode::OK);
    for path in ["/events/printer", "/inventory", "/jobs"] {
        assert_eq!(
            app.get(path, None).await.status(),
            StatusCode::SEE_OTHER,
            "{path}"
        );
    }
}

#[tokio::test]
async fn login_logout_round_trip() {
    let app = app().await;
    let cookie = app.login("admin", ADMIN_PASSWORD).await;

    let dashboard = app.get("/", Some(&cookie)).await;
    assert_eq!(dashboard.status(), StatusCode::OK);
    let html = dashboard.text().await.unwrap();
    assert!(
        html.contains(r#"href="/admin/users""#),
        "admin sees the users link"
    );
    assert!(html.contains(r#"src="/camera/stream""#));

    assert_eq!(
        app.post("/logout", Some(&cookie), &[]).await.status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        app.get("/", Some(&cookie)).await.status(),
        StatusCode::SEE_OTHER
    );
}

#[tokio::test]
async fn a_picked_theme_recolours_every_page_for_that_user_only() {
    let app = app().await;
    let sam = app.member("sam").await;
    let admin = app.login("admin", ADMIN_PASSWORD).await;
    let login = app.get("/login", None).await.text().await.unwrap();
    assert!(login.contains(r#"data-theme="printhub""#), "{login}");

    let picked = app
        .post("/account/theme", Some(&sam), &[("theme", "docker")])
        .await;
    assert_eq!(picked.status(), StatusCode::OK);
    assert!(
        picked
            .text()
            .await
            .unwrap()
            .contains(r#"value="docker" selected"#)
    );

    let jobs = app.get("/jobs", Some(&sam)).await.text().await.unwrap();
    assert!(jobs.contains(r#"data-theme="docker""#), "{jobs}");
    assert!(
        jobs.contains(r#"class="logo-tile""#),
        "the logo is inline so it can take the theme"
    );
    assert!(jobs.contains(r##"<meta name="theme-color" content="#10151b""##));
    let theirs = app.get("/jobs", Some(&admin)).await.text().await.unwrap();
    assert!(theirs.contains(r#"data-theme="printhub""#));

    assert_eq!(
        app.post("/account/theme", Some(&sam), &[("theme", "neon")])
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let still = app.get("/", Some(&sam)).await.text().await.unwrap();
    assert!(still.contains(r#"data-theme="docker""#));
}

#[tokio::test]
async fn failed_logins_are_rate_limited() {
    let app = app().await;
    for _ in 0..5 {
        let response = app
            .post(
                "/login",
                None,
                &[("username", "admin"), ("password", "wrong-password")],
            )
            .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let locked = app
        .post(
            "/login",
            None,
            &[("username", "admin"), ("password", ADMIN_PASSWORD)],
        )
        .await;
    assert_eq!(locked.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn cross_origin_posts_are_refused() {
    let app = app().await;
    let body = form_body(&[("username", "admin"), ("password", ADMIN_PASSWORD)]);
    let send = |origin: Option<&'static str>| {
        let mut request = app
            .http
            .post(app.url("/login"))
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body.clone());
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        request.send()
    };
    assert_eq!(send(None).await.unwrap().status(), StatusCode::FORBIDDEN);
    assert_eq!(
        send(Some("http://evil.example")).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn security_headers_are_set() {
    let app = app().await;
    let response = app.get("/login", None).await;
    let headers = response.headers();
    assert!(
        headers["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("default-src 'self'")
    );
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers["x-frame-options"], "DENY");
}

#[tokio::test]
async fn every_linked_icon_is_served_without_login() {
    let app = app().await;
    let page = app.get("/login", None).await.text().await.unwrap();
    let manifest: serde_json::Value = app
        .get("/static/manifest.webmanifest", None)
        .await
        .json()
        .await
        .unwrap();

    let attribute = |name: &'static str| page.split(name).skip(1);
    let mut paths: Vec<String> = attribute("href=\"")
        .chain(attribute("src=\""))
        .filter_map(|rest| rest.split('"').next())
        .filter(|path| path.contains("icon") || path.contains("logo"))
        .map(str::to_owned)
        .collect();
    assert!(paths.contains(&"/favicon.ico".to_owned()), "{paths:?}");
    assert!(page.contains("rel=\"manifest\""));
    paths.extend(
        manifest["icons"]
            .as_array()
            .unwrap()
            .iter()
            .map(|icon| icon["src"].as_str().unwrap().to_owned()),
    );

    for path in paths {
        let response = app.get(&path, None).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let content_type = response.headers()[CONTENT_TYPE].to_str().unwrap();
        assert!(content_type.starts_with("image/"), "{path}: {content_type}");
    }
}

#[tokio::test]
async fn help_pages_render_search_and_show_their_photos() {
    let app = app().await;
    let anonymous = app.get("/wiki/queue", None).await;
    assert_eq!(anonymous.status(), StatusCode::SEE_OTHER);
    let sam = app.member("sam").await;

    let home = app.get("/wiki", Some(&sam)).await;
    assert_eq!(home.status(), StatusCode::OK);
    let home = home.text().await.unwrap();
    assert!(home.contains("<h1>How PrintHub works</h1>"), "{home}");
    assert!(home.contains(r#"href="/wiki/filament/materials/pla""#));

    let pla = app
        .get("/wiki/filament/materials/pla", Some(&sam))
        .await
        .text()
        .await
        .unwrap();
    assert!(pla.contains(r#"<nav class="crumbs""#));
    assert!(pla.contains("<table>"));
    let photo = pla
        .split(r#"<img src="/static/wiki/"#)
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .map(|file| format!("/static/wiki/{file}"))
        .expect("the PLA page shows a photo");
    assert!(pla.contains("CC BY 2.0"), "the photo is credited");
    let image = app.get(&photo, None).await;
    assert_eq!(image.status(), StatusCode::OK, "{photo}");
    assert_eq!(image.headers()[CONTENT_TYPE], "image/jpeg");

    let results = app
        .get("/wiki/search?q=bed+CLEAR", Some(&sam))
        .await
        .text()
        .await
        .unwrap();
    assert!(
        results
            .contains(r#"href="/wiki/queue/waiting#nobody-has-confirmed-that-the-bed-is-clear""#),
        "{results}"
    );
    assert!(results.contains("<mark>"));

    assert_eq!(
        app.get("/wiki/no/such/page", Some(&sam)).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.get("/static/wiki/missing.jpg", None).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn invite_creates_a_member_without_admin_rights() {
    let app = app().await;
    let admin = app.login("admin", ADMIN_PASSWORD).await;

    let created = app
        .post("/admin/invites", Some(&admin), &[("role", "member")])
        .await;
    assert_eq!(created.status(), StatusCode::OK);
    let html = created.text().await.unwrap();
    let start = html.find("/invite/").expect("invite link shown") + "/invite/".len();
    let token: String = html[start..]
        .chars()
        .take_while(char::is_ascii_hexdigit)
        .collect();
    assert_eq!(token.len(), 64);
    let invite_path = format!("/invite/{token}");

    assert_eq!(app.get(&invite_path, None).await.status(), StatusCode::OK);

    let mismatch = app
        .post(
            &invite_path,
            None,
            &[
                ("username", "sam"),
                ("password", "sam-password-1"),
                ("confirm", "different-one"),
            ],
        )
        .await;
    assert_eq!(mismatch.status(), StatusCode::BAD_REQUEST);

    let redeemed = app
        .post(
            &invite_path,
            None,
            &[
                ("username", "sam"),
                ("password", "sam-password-1"),
                ("confirm", "sam-password-1"),
            ],
        )
        .await;
    assert_eq!(redeemed.status(), StatusCode::SEE_OTHER);
    let sam = session_cookie(&redeemed).expect("new member is logged in");

    let dashboard = app.get("/", Some(&sam)).await.text().await.unwrap();
    assert!(!dashboard.contains(r#"href="/admin/users""#));
    assert_eq!(
        app.get("/admin/users", Some(&sam)).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.post("/printer/pause", Some(&sam), &[]).await.status(),
        StatusCode::FORBIDDEN,
        "a member with no running job cannot control the printer"
    );

    let reused = app
        .post(
            &invite_path,
            None,
            &[
                ("username", "kim"),
                ("password", "kim-password-1"),
                ("confirm", "kim-password-1"),
            ],
        )
        .await;
    assert_eq!(reused.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn status_card_streams_over_sse() {
    let app = app().await;
    let cookie = app.login("admin", ADMIN_PASSWORD).await;
    let response = app.get("/events/printer", Some(&cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let seen = App::read_until(response, "Idle").await;
    assert!(seen.contains("event: printer"));
    assert!(seen.contains("Connected"));
}

#[tokio::test]
async fn camera_viewers_share_one_upstream() {
    let app = app().await;
    let cookie = app.login("admin", ADMIN_PASSWORD).await;

    let mut viewers = Vec::new();
    for _ in 0..3 {
        let response = app.get("/camera/stream", Some(&cookie)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()[CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("multipart/x-mixed-replace")
        );
        let mut stream = response.bytes_stream();
        timeout(WAIT, stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        viewers.push(stream);
    }
    assert_eq!(app.printer.camera_connections(), 1);
    assert_eq!(app.printer.camera_connects_total(), 1);

    drop(viewers);
    timeout(WAIT, async {
        while app.printer.camera_connections() > 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("upstream closes after the last viewer leaves");
}

#[tokio::test]
async fn snapshot_is_a_jpeg() {
    let app = app().await;
    let cookie = app.login("admin", ADMIN_PASSWORD).await;
    let response = app.get("/camera/snapshot.jpg", Some(&cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CONTENT_TYPE], "image/jpeg");
    assert!(response.bytes().await.unwrap().starts_with(&[0xFF, 0xD8]));
}

#[tokio::test]
async fn admin_can_pause_a_running_print() {
    let app = app().await;
    let cookie = app.login("admin", ADMIN_PASSWORD).await;

    let idle = timeout(WAIT, async {
        loop {
            let text = app
                .post("/printer/pause", Some(&cookie), &[])
                .await
                .text()
                .await
                .unwrap();
            if !text.contains("not connected") {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the app connects to the printer");
    assert!(
        idle.contains("refused"),
        "pausing an idle printer is refused: {idle}"
    );

    app.printer.add_file("cube.gcode", b"G28\n");
    let (other, _supervisor) = PrinterClient::start(ClientConfig {
        host: "127.0.0.1".into(),
        port: app.printer.mqtt_addr.port(),
        serial: app.printer.serial.clone(),
        password: app.printer.password.clone(),
        timing: Timing::default(),
    });
    other.wait_until_registered(WAIT).await.unwrap();
    other.start_print("cube.gcode", vec![]).await.unwrap();

    let paused = app
        .post("/printer/pause", Some(&cookie), &[])
        .await
        .text()
        .await
        .unwrap();
    assert!(paused.contains("Pause sent."), "{paused}");
}

#[tokio::test]
async fn members_edit_only_their_own_spools() {
    let app = app().await;
    let admin = app.login("admin", ADMIN_PASSWORD).await;
    let sam = app.member("sam").await;
    let kim = app.member("kim").await;

    let invalid = app
        .post(
            "/spools",
            Some(&sam),
            &[
                ("material", " "),
                ("color_hex", "#000000"),
                ("initial_grams", "1000"),
            ],
        )
        .await;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

    let spool = app
        .add_spool(&sam, &spool_fields("PLA", "Black", "#000000"))
        .await;
    let page = app.get(&spool, Some(&kim)).await.text().await.unwrap();
    assert!(page.contains("Elegoo PLA Black"), "{page}");
    assert!(page.contains("<dd>sam</dd>"), "sam owns what sam added");
    assert!(page.contains("19.99"));
    assert!(!page.contains("/edit"), "kim cannot edit sam's spool");

    let edit = spool_fields("PLA", "Galaxy Black", "#000000");
    assert_eq!(
        app.post(&spool, Some(&kim), &edit).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.post(
            &format!("{spool}/archived"),
            Some(&kim),
            &[("archived", "true")]
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.post(&spool, Some(&admin), &edit).await.status(),
        StatusCode::SEE_OTHER,
        "admins edit any spool, and leaving out the owner field keeps the owner"
    );

    assert_eq!(
        app.post(&format!("{spool}/weigh"), Some(&kim), &[("grams", "812")])
            .await
            .status(),
        StatusCode::SEE_OTHER,
        "anyone can weigh in"
    );
    let page = app.get(&spool, Some(&sam)).await.text().await.unwrap();
    assert!(page.contains("Elegoo PLA Galaxy Black"));
    assert!(page.contains("<dd>sam</dd>"));
    assert!(page.contains("812 g of 1000 g"), "{page}");
    assert!(page.contains("188 g used"));
    assert!(page.contains(&format!(r#"href="{spool}/edit""#)));
}

#[tokio::test]
async fn tray_spools_show_on_the_card_and_unbind_when_emptied() {
    let app = app().await;
    let admin = app.login("admin", ADMIN_PASSWORD).await;
    let petg = id_of(
        &app.add_spool(&admin, &spool_fields("PETG", "Red", "#FF0000"))
            .await,
    );
    let pla = id_of(
        &app.add_spool(&admin, &spool_fields("PLA", "White", "#FFFFFF"))
            .await,
    );

    app.bind(&admin, 1, &pla).await;
    app.bind(&admin, 0, &petg).await;
    assert_eq!(
        app.post(
            "/trays/0/3/bind",
            Some(&admin),
            &[("spool_id", petg.as_str())]
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST,
        "tray A4 is empty"
    );

    let page = app
        .get("/inventory", Some(&admin))
        .await
        .text()
        .await
        .unwrap();
    assert!(page.contains("Elegoo PETG Red"));
    assert_eq!(page.matches("Material mismatch").count(), 1, "{page}");

    let response = app.get("/events/printer", Some(&admin)).await;
    let card = App::read_until(response, "Material mismatch").await;
    assert!(card.contains("Elegoo PLA White"));

    app.printer.set_tray(1, "", "", 0);
    let refreshed = app.post("/trays/refresh", Some(&admin), &[]).await;
    assert_eq!(refreshed.headers()[LOCATION], "/inventory?done=refreshed");
    let bindings = timeout(WAIT, async {
        loop {
            let bindings = inventory::bindings(&app.db).await.unwrap();
            if bindings.len() == 1 {
                return bindings;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the emptied tray loses its spool");
    assert_eq!(bindings[0].spool.material, "PETG");
}

#[tokio::test]
async fn gcode_job_prints_from_its_spool_and_deducts_filament() {
    let app = app().await;
    let admin = app.login("admin", ADMIN_PASSWORD).await;
    let spool = id_of(
        &app.add_spool(&admin, &spool_fields("PLA", "Orange", "#F2754E"))
            .await,
    );
    app.bind(&admin, 0, &spool).await;

    let refused = app.upload(&admin, &[], "notes.txt", b"hello").await;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let not_sliced = app.upload(&admin, &[], "raw.gcode", b"G28\nG1 X10\n").await;
    assert_eq!(not_sliced.status(), StatusCode::BAD_REQUEST);

    let gcode = std::fs::read(CUBE_GCODE).unwrap();
    let uploaded = app.upload(&admin, &[], "cube.gcode", &gcode).await;
    assert_eq!(uploaded.status(), StatusCode::SEE_OTHER);
    let job_path = uploaded.headers()[LOCATION].to_str().unwrap().to_owned();
    let job_id: i64 = id_of(&job_path).parse().unwrap();
    let page = app.get(&job_path, Some(&admin)).await.text().await.unwrap();
    assert!(page.contains("Filament 1: PLA"), "{page}");
    assert!(page.contains("Add to the queue"));

    let confirmed = app
        .post(
            &format!("{job_path}/confirm"),
            Some(&admin),
            &[("spool_0", spool.as_str())],
        )
        .await;
    assert_eq!(confirmed.headers()[LOCATION], "/jobs?done=queued");
    let queue = app.get("/jobs", Some(&admin)).await.text().await.unwrap();
    assert!(
        queue.contains("Nobody has confirmed that the bed is clear."),
        "{queue}"
    );
    let dashboard = app.get("/", Some(&admin)).await.text().await.unwrap();
    assert!(
        dashboard.contains(&format!(r#"href="/jobs/{job_id}""#)),
        "the dashboard previews the queue: {dashboard}"
    );
    assert!(dashboard.contains(r#"<td class="num position">1</td>"#));
    assert!(dashboard.contains("</svg>A1 PLA</span>"), "{dashboard}");
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        app.printer.started_prints().is_empty(),
        "nothing starts before the bed is clear"
    );

    app.post("/printer/bed-clear", Some(&admin), &[]).await;
    // The task id is recorded once the dispatcher has seen the print running.
    app.wait_for_job(job_id, |job| {
        job.state == JobState::Printing && job.printer_task_uuid.is_some()
    })
    .await;
    let started = app.printer.started_prints();
    assert_eq!(started[0]["filename"], format!("cube-{job_id}.gcode"));
    assert_eq!(
        started[0]["config"]["slot_map"],
        serde_json::json!([{"t": 0, "canvas_id": 0, "tray_id": 0}])
    );
    assert_eq!(app.printer.uploads()[0].bytes, gcode);
    assert!(
        !jobs::bed_clear(&app.db).await.unwrap(),
        "a started print leaves the bed not clear"
    );
    let printing = app.get("/jobs", Some(&admin)).await.text().await.unwrap();
    assert!(
        !printing.contains("The bed is clear</button>"),
        "the bed cannot be cleared under a running print: {printing}"
    );

    app.printer.complete_print();
    app.wait_for_job(job_id, |job| job.state == JobState::Done)
        .await;
    let left = inventory::spool(&app.db, spool.parse().unwrap())
        .await
        .unwrap()
        .unwrap()
        .remaining_grams;
    assert!((left - (1000.0 - 3.54)).abs() < 1e-9, "{left}");
}

/// A stand-in OrcaSlicer that copies the cube fixture to `--outputdir` and keeps the process and
/// filament profiles it was given.
#[cfg(unix)]
fn stub_slicer() -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("stub");
    let path = dir.join("orca-slicer");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\n\
             while [ $# -gt 0 ]; do\n\
               case \"$1\" in --outputdir) out=$2;; --load-settings) set=$2;; --load-filaments) fil=$2;; esac\n\
               shift\n\
             done\n\
             cp \"${{set##*;}}\" \"$(dirname \"$0\")/process.json\"\n\
             cp \"$fil\" \"$(dirname \"$0\")/filament.json\"\n\
             cp '{CUBE_GCODE}' \"$out/plate_1.gcode\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn stub_profiles() -> PathBuf {
    let dir = scratch("profiles");
    let machine = "Elegoo Centauri Carbon 2 0.4 nozzle";
    for (file, profile) in [
        (
            "machine.json",
            serde_json::json!({"name": machine, "type": "machine", "instantiation": "true"}),
        ),
        (
            "process.json",
            serde_json::json!({"name": PROCESS, "type": "process", "instantiation": "true", "compatible_printers": [machine]}),
        ),
        (
            "pla.json",
            serde_json::json!({"name": "Elegoo PLA @ECC2", "type": "filament", "instantiation": "true", "compatible_printers": [machine]}),
        ),
    ] {
        std::fs::write(dir.join(file), profile.to_string()).unwrap();
    }
    dir
}

#[cfg(unix)]
#[tokio::test]
async fn stl_uploads_are_sliced_for_the_chosen_spool() {
    let binary = stub_slicer();
    let slicer = Slicer::new(binary.clone(), &stub_profiles(), WAIT).unwrap();
    let app = app_with(Some(slicer)).await;
    let admin = app.login("admin", ADMIN_PASSWORD).await;
    let sam = app.member("sam").await;
    let spool = id_of(
        &app.add_spool(&admin, &spool_fields("PLA", "Blue", "#2850DF"))
            .await,
    );
    let model = b"solid cube\nendsolid cube\n";

    let form = app.get("/jobs/new", Some(&sam)).await.text().await.unwrap();
    assert!(form.contains(PROCESS), "{form}");
    assert!(
        form.contains(r#"<option value="Textured PEI Plate" selected>"#),
        "{form}"
    );

    let no_spool = app
        .upload(
            &sam,
            &[("process", PROCESS), ("infill", "20")],
            "cube.stl",
            model,
        )
        .await;
    assert_eq!(no_spool.status(), StatusCode::BAD_REQUEST);
    let no_plate = app
        .upload(
            &sam,
            &[
                ("spool_id", spool.as_str()),
                ("process", PROCESS),
                ("infill", "20"),
            ],
            "cube.stl",
            model,
        )
        .await;
    assert_eq!(no_plate.status(), StatusCode::BAD_REQUEST);
    assert!(
        no_plate
            .text()
            .await
            .unwrap()
            .contains("Choose the build plate.")
    );

    let uploaded = app
        .upload(
            &sam,
            &[
                ("spool_id", spool.as_str()),
                ("plate", "High Temp Plate"),
                ("process", PROCESS),
                ("filament", ""),
                ("infill", "20"),
                ("supports", "on"),
            ],
            "cube.stl",
            model,
        )
        .await;
    assert_eq!(uploaded.status(), StatusCode::SEE_OTHER);
    let job_id: i64 = id_of(uploaded.headers()[LOCATION].to_str().unwrap())
        .parse()
        .unwrap();

    let job = app
        .wait_for_job(job_id, |job| job.state == JobState::AwaitingConfirm)
        .await;
    assert_eq!(
        job.filament_profile.as_deref(),
        Some("Elegoo PLA @ECC2"),
        "chosen by the spool's material"
    );
    assert_eq!((job.supports, job.infill_percent), (Some(true), Some(20)));
    let tools = jobs::tools(&app.db, job_id).await.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].spool_id, Some(spool.parse().unwrap()));
    let filament: serde_json::Value =
        serde_json::from_slice(&std::fs::read(binary.with_file_name("filament.json")).unwrap())
            .unwrap();
    assert_eq!(filament["filament_colour"], serde_json::json!(["#2850DF"]));
    let process: serde_json::Value =
        serde_json::from_slice(&std::fs::read(binary.with_file_name("process.json")).unwrap())
            .unwrap();
    assert_eq!(process["curr_bed_type"], "High Temp Plate");
    assert_eq!(
        job.plate.as_deref(),
        Some("Cool Plate"),
        "read from the G-code, here the stub's fixture"
    );

    let kim = app.member("kim").await;
    let cancel = format!("/jobs/{job_id}/cancel");
    assert_eq!(
        app.post(&cancel, Some(&kim), &[]).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.post(&cancel, Some(&sam), &[]).await.status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        jobs::get(&app.db, job_id).await.unwrap().unwrap().state,
        JobState::Cancelled
    );
}

#[tokio::test]
async fn job_files_download_under_the_uploaded_name() {
    let app = app().await;
    let admin = app.login("admin", ADMIN_PASSWORD).await;
    let gcode = std::fs::read(CUBE_GCODE).unwrap();
    let uploaded = app.upload(&admin, &[], "a cube!.gcode", &gcode).await;
    assert_eq!(uploaded.status(), StatusCode::SEE_OTHER);
    let job_path = uploaded.headers()[LOCATION].to_str().unwrap().to_owned();

    let page = app.get(&job_path, Some(&admin)).await.text().await.unwrap();
    assert!(page.contains("a cube_.gcode"), "{page}");
    assert!(
        !page.contains("files/model.stl"),
        "a G-code job has no model: {page}"
    );

    let download = app
        .get(&format!("{job_path}/files/job.gcode"), Some(&admin))
        .await;
    assert_eq!(download.status(), StatusCode::OK);
    assert_eq!(
        download.headers()["content-disposition"],
        "attachment; filename=\"a cube_.gcode\"",
        "the shell characters an upload may carry are dropped"
    );
    assert_eq!(download.bytes().await.unwrap().as_ref(), gcode.as_slice());

    let queue = app.get("/jobs", Some(&admin)).await.text().await.unwrap();
    let preview_path = format!("{job_path}/files/preview.png");
    assert!(queue.contains(&preview_path), "{queue}");
    // The second request is served from the file the first one wrote.
    for _ in 0..2 {
        let preview = app.get(&preview_path, Some(&admin)).await;
        assert_eq!(preview.status(), StatusCode::OK);
        assert_eq!(preview.headers()["content-type"], "image/png");
        let png = preview.bytes().await.unwrap();
        assert!(png.starts_with(b"\x89PNG"));
    }

    assert_eq!(
        app.get(&format!("{job_path}/files/model.stl"), Some(&admin))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let anonymous = app.get(&format!("{job_path}/files/job.gcode"), None).await;
    assert_eq!(anonymous.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        anonymous.headers()[LOCATION],
        "/login",
        "sent to the login page rather than served"
    );
}

#[tokio::test]
async fn anyone_logged_in_switches_the_light() {
    let app = app().await;
    let sam = app.member("sam").await;

    let dashboard = app.get("/", Some(&sam)).await.text().await.unwrap();
    assert!(dashboard.contains("Turn the light on"), "{dashboard}");

    let on = app.post("/printer/light-on", Some(&sam), &[]).await;
    assert_eq!(on.status(), StatusCode::OK);
    assert!(on.text().await.unwrap().contains("Light on."));
    assert!(app.printer.requests_seen().contains(&1029));

    app.wait_for(|| async {
        app.get("/", Some(&sam))
            .await
            .text()
            .await
            .unwrap()
            .contains("Turn the light off")
    })
    .await;

    let off = app.post("/printer/light-off", Some(&sam), &[]).await;
    assert!(off.text().await.unwrap().contains("Light off."));
    assert_eq!(
        app.post("/printer/pause", Some(&sam), &[]).await.status(),
        StatusCode::FORBIDDEN,
        "the light is the only control a member holds with no job printing"
    );
}

#[tokio::test]
async fn only_permitted_members_record_the_mounted_nozzle() {
    let app = app().await;
    let admin = app.login("admin", ADMIN_PASSWORD).await;
    let sam = app.member("sam").await;
    let sam_id = accounts::login_record(&app.db, "sam")
        .await
        .unwrap()
        .unwrap()
        .0
        .id;
    let permissions = format!("/admin/users/{sam_id}/permissions");
    let record = |cookie: &str, nozzle: &str| {
        let cookie = cookie.to_owned();
        let nozzle = nozzle.to_owned();
        let app = &app;
        async move {
            app.post("/printer/nozzle", Some(&cookie), &[("nozzle", &nozzle)])
                .await
                .status()
        }
    };

    let dashboard = app.get("/", Some(&sam)).await.text().await.unwrap();
    assert!(dashboard.contains("0.4 mm"), "{dashboard}");
    assert!(
        !dashboard.contains(r#"action="/printer/nozzle""#),
        "no form without the permission"
    );
    assert_eq!(record(&sam, "0.6").await, StatusCode::FORBIDDEN);

    assert_eq!(
        app.post(&permissions, Some(&sam), &[("permission", "set_nozzle")])
            .await
            .status(),
        StatusCode::FORBIDDEN,
        "members cannot grant themselves"
    );
    let granted = app
        .post(&permissions, Some(&admin), &[("permission", "set_nozzle")])
        .await;
    assert_eq!(granted.headers()[LOCATION], "/admin/users?done=permissions");
    let dashboard = app.get("/", Some(&sam)).await.text().await.unwrap();
    assert!(
        dashboard.contains(r#"action="/printer/nozzle""#),
        "{dashboard}"
    );
    assert_eq!(record(&sam, "0.6").await, StatusCode::SEE_OTHER);
    let dashboard = app.get("/", Some(&admin)).await.text().await.unwrap();
    assert!(dashboard.contains("recorded by sam"), "{dashboard}");

    app.post(&permissions, Some(&admin), &[]).await;
    assert_eq!(record(&sam, "0.4").await, StatusCode::FORBIDDEN);
    assert_eq!(
        record(&admin, "0.8").await,
        StatusCode::SEE_OTHER,
        "admins need no grant"
    );
    assert_eq!(record(&admin, "0.5").await, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_spool_list_shows_prices() {
    let app = app().await;
    let admin = app.login("admin", ADMIN_PASSWORD).await;
    app.add_spool(&admin, &spool_fields("PLA", "Blue", "#2850DF"))
        .await;
    let page = app
        .get("/inventory", Some(&admin))
        .await
        .text()
        .await
        .unwrap();
    assert!(page.contains("<td>19.99</td>"), "{page}");
}

#[tokio::test]
async fn admins_cannot_be_made_members() {
    let app = app().await;
    let admin = app.login("admin", ADMIN_PASSWORD).await;
    app.member("sam").await;
    let users = app
        .get("/admin/users", Some(&admin))
        .await
        .text()
        .await
        .unwrap();
    assert!(!users.contains("Make member"), "{users}");
    assert!(users.contains("Make admin"), "offered for the member");

    let admin_id = accounts::login_record(&app.db, "admin")
        .await
        .unwrap()
        .unwrap()
        .0
        .id;
    let refused = app
        .post(
            &format!("/admin/users/{admin_id}/role"),
            Some(&admin),
            &[("role", "member")],
        )
        .await;
    assert_eq!(
        refused.headers()[LOCATION],
        "/admin/users?problem=admin-role"
    );
    assert!(
        accounts::user(&app.db, admin_id)
            .await
            .unwrap()
            .unwrap()
            .is_admin()
    );
}

#[tokio::test]
async fn print_windows_are_admin_only() {
    let app = app().await;
    let admin = app.login("admin", ADMIN_PASSWORD).await;
    let sam = app.member("sam").await;
    assert_eq!(
        app.get("/admin/schedule", Some(&sam)).await.status(),
        StatusCode::FORBIDDEN
    );

    let no_days = app
        .post(
            "/admin/schedule",
            Some(&admin),
            &[("kind", "deny"), ("start", "22:00"), ("end", "07:00")],
        )
        .await;
    assert_eq!(no_days.status(), StatusCode::BAD_REQUEST);

    let added = app
        .post(
            "/admin/schedule",
            Some(&admin),
            &[
                ("kind", "deny"),
                ("label", "Quiet hours"),
                ("start", "22:00"),
                ("end", "07:00"),
                ("day0", "on"),
                ("must_finish_before", "on"),
            ],
        )
        .await;
    assert_eq!(added.headers()[LOCATION], "/admin/schedule?done=added");
    let page = app
        .get("/admin/schedule", Some(&admin))
        .await
        .text()
        .await
        .unwrap();
    assert!(page.contains("Deny: Quiet hours"), "{page}");
    assert!(page.contains("22:00–07:00"));

    let rules = schedule::rules(&app.db).await.unwrap();
    assert_eq!(rules.len(), 1);
    assert!(rules[0].1.must_finish_before);
    app.post(
        &format!("/admin/schedule/{}/delete", rules[0].0),
        Some(&admin),
        &[],
    )
    .await;
    assert!(schedule::rules(&app.db).await.unwrap().is_empty());
}

#[tokio::test]
async fn statistics_show_whose_filament_was_used_and_what_is_owed() {
    let app = app().await;
    let admin = app.login("admin", ADMIN_PASSWORD).await;
    let sam = app.member("sam").await;
    let alex = app.member("alex").await;
    let user_id = |name: &'static str| {
        let db = app.db.clone();
        async move {
            accounts::login_record(&db, name)
                .await
                .unwrap()
                .unwrap()
                .0
                .id
                .to_string()
        }
    };
    let (sam_id, alex_id) = (user_id("sam").await, user_id("alex").await);
    let spool_path = app
        .add_spool(&sam, &spool_fields("PLA", "Blue", "#2850DF"))
        .await;
    let spool: i64 = id_of(&spool_path).parse().unwrap();

    // Alex prints 100 g from Sam's spool, which cost 19.99 for 1000 g.
    let now = store::now();
    let job = jobs::create(
        &app.db,
        &jobs::NewJob {
            owner_id: alex_id.parse().unwrap(),
            name: "benchy.gcode",
            source: jobs::Source::Gcode,
            process_profile: None,
            filament_profile: None,
            supports: None,
            infill_percent: None,
            scale_percent: None,
        },
        now - 3600,
    )
    .await
    .unwrap();
    let info = gcode::GcodeInfo {
        generator: String::new(),
        printer_model: String::new(),
        nozzle: String::new(),
        plate: String::new(),
        estimated_seconds: Some(1800),
        layers: Some(10),
        tools: vec![gcode::ToolUse {
            index: 0,
            material: "PLA".into(),
            color: "#2850DF".into(),
            grams: 100.0,
            profile: String::new(),
        }],
    };
    jobs::store_gcode_info(&app.db, job, &info, Some(spool))
        .await
        .unwrap();
    for (from, to) in [
        (JobState::AwaitingConfirm, JobState::Queued),
        (JobState::Queued, JobState::Uploading),
        (JobState::Uploading, JobState::Printing),
    ] {
        jobs::transition(&app.db, job, from, to, None, now - 1800)
            .await
            .unwrap();
    }
    let printing = jobs::get(&app.db, job).await.unwrap().unwrap();
    jobs::finish(&app.db, &printing, JobState::Done, 100, None, now)
        .await
        .unwrap();

    // Handing the spool to everyone and repricing it later does not rewrite the print.
    let mut shared = spool_fields("PLA", "Blue", "#2850DF");
    shared.retain(|(name, _)| *name != "price");
    shared.extend([("price", "99"), ("owner_id", "")]);
    let edited = app.post(&spool_path, Some(&admin), &shared).await;
    assert_eq!(edited.status(), StatusCode::SEE_OTHER);

    let page = app.get("/stats", Some(&alex)).await.text().await.unwrap();
    assert!(page.contains("100 g (100%)"), "{page}");
    assert!(
        page.contains(r#"<td class="num">100 g (2.00)</td>"#),
        "{page}"
    );
    assert!(page.contains("alex owes sam"), "{page}");
    assert!(
        page.contains(r#"class="series-3""#),
        "alex holds the third account's colour"
    );
    assert!(
        !page.contains("Record payment"),
        "a debtor is not offered to record their payment"
    );

    let payment = [
        ("from_user", alex_id.as_str()),
        ("to_user", sam_id.as_str()),
        ("amount", "2"),
    ];
    let refused = app.post("/stats/settlements", Some(&alex), &payment).await;
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);

    let sam_page = app.get("/stats", Some(&sam)).await.text().await.unwrap();
    assert!(sam_page.contains("Record payment"));
    let no_amount = app
        .post(
            "/stats/settlements",
            Some(&sam),
            &[payment[0], payment[1], ("amount", "0")],
        )
        .await;
    assert_eq!(
        no_amount.headers()[LOCATION],
        "/stats?problem=payment-amount#balances"
    );
    let paid = app.post("/stats/settlements", Some(&sam), &payment).await;
    assert_eq!(paid.headers()[LOCATION], "/stats?done=paid#balances");
    let settled = app.get("/stats", Some(&sam)).await.text().await.unwrap();
    assert!(
        settled.contains("Nobody owes anybody anything."),
        "{settled}"
    );
    assert!(settled.contains("alex paid sam"));

    let alex_page = app
        .get(&format!("/stats/users/{alex_id}?period=30d"), Some(&sam))
        .await
        .text()
        .await
        .unwrap();
    assert!(
        alex_page.contains("Whose filament alex used"),
        "{alex_page}"
    );
    assert!(alex_page.contains("benchy.gcode"));
    assert!(
        alex_page.contains(r#"<td>sam</td><td class="num">100 g</td><td class="num">2.00</td>"#)
    );
    assert_eq!(
        app.get("/stats/users/9999", Some(&sam)).await.status(),
        StatusCode::NOT_FOUND
    );

    let payment_id = printhub::stats::settlements(&app.db).await.unwrap()[0].id;
    let delete = format!("/stats/settlements/{payment_id}/delete");
    assert_eq!(
        app.post(&delete, Some(&sam), &[]).await.status(),
        StatusCode::FORBIDDEN
    );
    app.post(&delete, Some(&admin), &[]).await;
    let reopened = app.get("/stats", Some(&sam)).await.text().await.unwrap();
    assert!(reopened.contains("alex owes sam"));
}

#[tokio::test]
async fn a_slicing_job_polls_until_it_can_be_confirmed() {
    let app = app().await;
    let sam = app.member("sam").await;
    let sam_id = accounts::login_record(&app.db, "sam")
        .await
        .unwrap()
        .unwrap()
        .0
        .id;
    let id = jobs::create(
        &app.db,
        &jobs::NewJob {
            owner_id: sam_id,
            name: "cube.stl",
            source: jobs::Source::Stl,
            process_profile: Some(PROCESS),
            filament_profile: Some("Elegoo PLA @ECC2"),
            supports: Some(false),
            infill_percent: Some(20),
            scale_percent: None,
        },
        store::now(),
    )
    .await
    .unwrap();

    let path = format!("/jobs/{id}");
    let slicing = app.get(&path, Some(&sam)).await.text().await.unwrap();
    assert!(
        slicing.contains(&format!(r#"hx-get="/jobs/{id}""#)),
        "a slicing job reloads itself: {slicing}"
    );

    jobs::transition(
        &app.db,
        id,
        JobState::Slicing,
        JobState::AwaitingConfirm,
        None,
        store::now(),
    )
    .await
    .unwrap();
    let confirm = app.get(&path, Some(&sam)).await.text().await.unwrap();
    assert!(
        !confirm.contains("hx-trigger"),
        "and stops once it is confirmable: {confirm}"
    );
}

#[tokio::test]
async fn controlling_another_member_s_print_needs_the_permission() {
    let app = app().await;
    let admin = app.login("admin", ADMIN_PASSWORD).await;
    let sam = app.member("sam").await;
    let kim = app.member("kim").await;
    let user_id = |name: &'static str| {
        let db = app.db.clone();
        async move {
            accounts::login_record(&db, name)
                .await
                .unwrap()
                .unwrap()
                .0
                .id
        }
    };
    let (sam_id, admin_id, kim_id) = (
        user_id("sam").await,
        user_id("admin").await,
        user_id("kim").await,
    );

    let now = store::now();
    let id = jobs::create(
        &app.db,
        &jobs::NewJob {
            owner_id: sam_id,
            name: "benchy.gcode",
            source: jobs::Source::Gcode,
            process_profile: None,
            filament_profile: None,
            supports: None,
            infill_percent: None,
            scale_percent: None,
        },
        now,
    )
    .await
    .unwrap();
    for (from, to) in [
        (JobState::AwaitingConfirm, JobState::Queued),
        (JobState::Queued, JobState::Uploading),
        (JobState::Uploading, JobState::Printing),
    ] {
        jobs::transition(&app.db, id, from, to, None, now)
            .await
            .unwrap();
    }
    let job_path = format!("/jobs/{id}");
    let cancel = format!("{job_path}/cancel");

    let owner_page = app.get(&job_path, Some(&sam)).await.text().await.unwrap();
    assert!(
        owner_page.contains("Stop the print"),
        "one can always stop their own print: {owner_page}"
    );
    let stranger = app.get(&job_path, Some(&kim)).await.text().await.unwrap();
    assert!(!stranger.contains("Stop the print"), "{stranger}");
    assert!(!stranger.contains("/printer/pause"), "{stranger}");
    assert_eq!(
        app.post(&cancel, Some(&kim), &[]).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.post("/printer/stop", Some(&kim), &[]).await.status(),
        StatusCode::FORBIDDEN
    );

    accounts::set_permissions(
        &app.db,
        kim_id,
        &[accounts::Permission::ControlPrint],
        admin_id,
        store::now(),
    )
    .await
    .unwrap();

    let granted = app.get(&job_path, Some(&kim)).await.text().await.unwrap();
    assert!(granted.contains("Stop the print"), "{granted}");
    assert!(
        granted.contains("/printer/pause"),
        "pausing goes with stopping: {granted}"
    );
    assert_ne!(
        app.post(&cancel, Some(&kim), &[]).await.status(),
        StatusCode::FORBIDDEN,
        "the queue's own stop button honours the permission too"
    );
    assert_eq!(
        app.post("/printer/stop", Some(&kim), &[]).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        app.post("/printer/pause", Some(&kim), &[]).await.status(),
        StatusCode::OK
    );

    let users = app
        .get("/admin/users", Some(&admin))
        .await
        .text()
        .await
        .unwrap();
    assert!(users.contains("Pause or stop any print"), "{users}");
}
