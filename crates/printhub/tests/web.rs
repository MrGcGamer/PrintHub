//! The web interface end to end: the real router on a local port, an in-memory database and
//! the fake printer.

use std::{collections::HashMap, time::Duration};

use fakeprinter::{FakePrinter, Options};
use futures::StreamExt;
use printhub::{
    accounts::{self, Role},
    auth,
    camera::CameraHub,
    cc2::{ClientConfig, PrinterClient, Timing},
    config::Config,
    inventory,
    printer::PrinterLink,
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

struct App {
    base: String,
    http: reqwest::Client,
    printer: FakePrinter,
    db: Db,
}

async fn app() -> App {
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
    );
    let link = PrinterLink::start(&config);
    let state = AppState::new(db.clone(), config, link, Some(camera))
        .await
        .unwrap();
    tokio::spawn(web::unbind_emptied_trays(state.clone()));

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
    assert_eq!(
        app.get("/events/printer", None).await.status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        app.get("/inventory", None).await.status(),
        StatusCode::SEE_OTHER
    );
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
        StatusCode::FORBIDDEN
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
    let id_of = |path: &str| path.rsplit('/').next().unwrap().to_owned();
    let petg = id_of(
        &app.add_spool(&admin, &spool_fields("PETG", "Red", "#FF0000"))
            .await,
    );
    let pla = id_of(
        &app.add_spool(&admin, &spool_fields("PLA", "White", "#FFFFFF"))
            .await,
    );

    // Tray contents arrive shortly after the app registers with the printer.
    timeout(WAIT, async {
        loop {
            let response = app
                .post(
                    "/trays/0/1/bind",
                    Some(&admin),
                    &[("spool_id", pla.as_str())],
                )
                .await;
            if response.status() == StatusCode::SEE_OTHER {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("tray A2 accepts a spool");
    assert_eq!(
        app.post(
            "/trays/0/0/bind",
            Some(&admin),
            &[("spool_id", petg.as_str())]
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
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
    timeout(WAIT, async {
        loop {
            let bindings = inventory::bindings(&app.db).await.unwrap();
            if bindings.len() == 1 {
                return bindings;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map(|bindings| assert_eq!(bindings[0].spool.material, "PETG"))
    .expect("the emptied tray loses its spool");
}
