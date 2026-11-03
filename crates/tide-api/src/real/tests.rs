use super::auth::{self, AuthError, DeviceCode};
use super::config::Config;
use super::http::{Http, Response};
use super::store::{self, Session};
use super::Backend;
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tide_core::services::{Cmd, Evt};

type Logged = (String, Vec<(String, String)>, Option<(String, String)>);

#[derive(Clone, Default)]
struct Fake {
    script: Arc<Mutex<VecDeque<Result<Response, String>>>>,
    log: Arc<Mutex<Vec<Logged>>>,
}

impl Fake {
    fn push(&self, status: u16, body: &str) -> &Self {
        self.script.lock().unwrap().push_back(Ok(Response { status, body: body.into() }));
        self
    }
    fn push_err(&self, e: &str) -> &Self {
        self.script.lock().unwrap().push_back(Err(e.into()));
        self
    }
    fn requests(&self) -> Vec<Logged> {
        self.log.lock().unwrap().clone()
    }
}

impl Http for Fake {
    fn post_form(&self, url: &str, form: &[(&str, &str)], basic: Option<(&str, &str)>) -> Result<Response, String> {
        self.log.lock().unwrap().push((
            url.into(),
            form.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            basic.map(|(u, p)| (u.into(), p.into())),
        ));
        self.script.lock().unwrap().pop_front().expect("unexpected extra HTTP request")
    }
}

fn cfg() -> Config {
    Config {
        client_id: "cid".into(),
        client_secret: "csecret".into(),
        auth_base: "https://auth.test/v1/oauth2".into(),
        scope: "r_usr w_usr w_sub".into(),
    }
}

const DEVICE_JSON: &str = r#"{"deviceCode":"dev-123","userCode":"AFIDU","verificationUri":"link.example.com",
  "verificationUriComplete":"link.example.com/AFIDU","expiresIn":300,"interval":2}"#;
const PENDING: &str = r#"{"status":400,"error":"authorization_pending","sub_status":1002}"#;
const TOKEN_JSON: &str = r#"{"access_token":"AT","refresh_token":"RT","token_type":"Bearer","expires_in":604800,
  "user":{"userId":42,"countryCode":"DE"}}"#;

/// Fake clock: `sleep` advances time, so polling loops finish instantly.
fn clock() -> (Arc<AtomicU64>, impl Fn() -> u64 + Clone, impl Fn(Duration) + Clone, Arc<Mutex<Vec<u64>>>) {
    let t = Arc::new(AtomicU64::new(1_000_000));
    let sleeps = Arc::new(Mutex::new(Vec::new()));
    let (t1, t2, s2) = (t.clone(), t.clone(), sleeps.clone());
    (
        t,
        move || t1.load(Ordering::SeqCst),
        move |d: Duration| {
            s2.lock().unwrap().push(d.as_secs());
            t2.fetch_add(d.as_secs(), Ordering::SeqCst);
        },
        sleeps,
    )
}

fn device_code() -> DeviceCode {
    serde_json::from_str(DEVICE_JSON).unwrap()
}

#[test]
fn device_code_parsing_and_qr_url() {
    let dc = device_code();
    assert_eq!(dc.user_code, "AFIDU");
    assert_eq!(dc.qr_url(), "https://link.example.com/AFIDU");
    let no_complete: DeviceCode = serde_json::from_str(
        r#"{"deviceCode":"d","userCode":"ZZZ","verificationUri":"link.example.com","expiresIn":60}"#,
    )
    .unwrap();
    assert_eq!(no_complete.interval, 5);
    assert_eq!(no_complete.qr_url(), "https://link.example.com/ZZZ");
}

#[test]
fn request_device_code_sends_client_id_and_scope_without_basic_auth() {
    let f = Fake::default();
    f.push(200, DEVICE_JSON);
    let dc = auth::request_device_code(&f, &cfg()).unwrap();
    assert_eq!(dc.device_code, "dev-123");
    let (url, form, basic) = &f.requests()[0];
    assert_eq!(url, "https://auth.test/v1/oauth2/device_authorization");
    assert!(form.contains(&("client_id".into(), "cid".into())));
    assert!(form.contains(&("scope".into(), "r_usr w_usr w_sub".into())));
    assert!(basic.is_none());
}

#[test]
fn polling_waits_through_pending_then_succeeds() {
    let f = Fake::default();
    f.push(400, PENDING).push(400, PENDING).push(200, TOKEN_JSON);
    let (_, now, sleep, sleeps) = clock();
    let s = auth::poll_for_session(&f, &cfg(), &device_code(), &now, &sleep).unwrap();
    assert_eq!(s.access_token, "AT");
    assert_eq!(s.refresh_token, "RT");
    assert_eq!(s.expires_at, now() + 604800);
    assert_eq!((s.user_id, s.country_code.as_deref()), (Some(42), Some("DE")));
    assert_eq!(*sleeps.lock().unwrap(), vec![2, 2, 2]);
    let (url, form, basic) = &f.requests()[2];
    assert_eq!(url, "https://auth.test/v1/oauth2/token");
    assert!(form.contains(&("grant_type".into(), "urn:ietf:params:oauth:grant-type:device_code".into())));
    assert!(form.contains(&("device_code".into(), "dev-123".into())));
    assert_eq!(basic, &Some(("cid".into(), "csecret".into())));
}

#[test]
fn slow_down_increases_interval() {
    let f = Fake::default();
    f.push(400, r#"{"error":"slow_down"}"#).push(200, TOKEN_JSON);
    let (_, now, sleep, sleeps) = clock();
    auth::poll_for_session(&f, &cfg(), &device_code(), &now, &sleep).unwrap();
    assert_eq!(*sleeps.lock().unwrap(), vec![2, 7]);
}

#[test]
fn expiry_denial_and_unknown_errors() {
    // Never approved: the clock passes expiresIn.
    let f = Fake::default();
    for _ in 0..400 {
        f.push(400, PENDING);
    }
    let (_, now, sleep, _) = clock();
    assert_eq!(auth::poll_for_session(&f, &cfg(), &device_code(), &now, &sleep), Err(AuthError::Expired));

    let f = Fake::default();
    f.push(400, r#"{"error":"expired_token"}"#);
    let (_, now, sleep, _) = clock();
    assert_eq!(auth::poll_for_session(&f, &cfg(), &device_code(), &now, &sleep), Err(AuthError::Expired));

    let f = Fake::default();
    f.push(400, r#"{"error":"access_denied"}"#);
    let (_, now, sleep, _) = clock();
    assert_eq!(auth::poll_for_session(&f, &cfg(), &device_code(), &now, &sleep), Err(AuthError::Denied));

    let f = Fake::default();
    f.push(500, "oops");
    let (_, now, sleep, _) = clock();
    assert!(matches!(auth::poll_for_session(&f, &cfg(), &device_code(), &now, &sleep), Err(AuthError::Other(_))));
}

#[test]
fn polling_tolerates_a_few_network_blips_but_not_many() {
    let f = Fake::default();
    f.push_err("net").push_err("net").push(200, TOKEN_JSON);
    let (_, now, sleep, _) = clock();
    assert!(auth::poll_for_session(&f, &cfg(), &device_code(), &now, &sleep).is_ok());

    let f = Fake::default();
    for _ in 0..5 {
        f.push_err("net");
    }
    let (_, now, sleep, _) = clock();
    assert!(matches!(auth::poll_for_session(&f, &cfg(), &device_code(), &now, &sleep), Err(AuthError::Network(_))));
}

fn old_session() -> Session {
    Session { access_token: "OLD".into(), refresh_token: "RT0".into(), expires_at: 1_000_100, user_id: Some(7), country_code: Some("BR".into()) }
}

#[test]
fn refresh_keeps_refresh_token_and_user_when_reply_omits_them() {
    let f = Fake::default();
    f.push(200, r#"{"access_token":"NEW","token_type":"Bearer","expires_in":604800}"#);
    let s = auth::refresh(&f, &cfg(), &old_session(), 1_000_000).unwrap();
    assert_eq!((s.access_token.as_str(), s.refresh_token.as_str()), ("NEW", "RT0"));
    assert_eq!((s.user_id, s.country_code.as_deref()), (Some(7), Some("BR")));
    assert_eq!(s.expires_at, 1_000_000 + 604800);
    let (_, form, basic) = &f.requests()[0];
    assert!(form.contains(&("grant_type".into(), "refresh_token".into())));
    assert!(form.contains(&("refresh_token".into(), "RT0".into())));
    assert!(basic.is_some());
}

#[test]
fn refresh_error_classification() {
    for (status, rejected) in [(400u16, true), (401, true), (500, false), (503, false)] {
        let f = Fake::default();
        f.push(status, "{}");
        let r = auth::refresh(&f, &cfg(), &old_session(), 1_000_000);
        assert_eq!(matches!(r, Err(AuthError::Rejected)), rejected, "status {status}");
    }
    let f = Fake::default();
    f.push_err("offline");
    assert!(matches!(auth::refresh(&f, &cfg(), &old_session(), 1), Err(AuthError::Network(_))));
}

// ---- Backend-level flows ----

struct Harness {
    be: Backend,
    fake: Fake,
    path: PathBuf,
    time: Arc<AtomicU64>,
}

fn harness(name: &str, cfg: Result<Config, String>) -> Harness {
    let fake = Fake::default();
    let (time, now, sleep, _) = clock();
    let path = std::env::temp_dir().join(format!("tide-be-{name}-{}", std::process::id())).join("session.json");
    let be = Backend::new(Box::new(fake.clone()), cfg, path.clone(), Box::new(now), Box::new(sleep));
    Harness { be, fake, path, time }
}

fn run(h: &mut Harness, cmd: Cmd) -> Vec<Evt> {
    let out = std::cell::RefCell::new(Vec::new());
    h.be.handle(cmd, &|e| out.borrow_mut().push(e));
    out.into_inner()
}

#[test]
fn boot_without_config_or_session() {
    let mut h = harness("noconf", Err("Missing client credentials".into()));
    let ev = run(&mut h, Cmd::Boot);
    assert!(matches!(&ev[..], [Evt::LoginFailed(m)] if m.contains("Missing")));

    let mut h = harness("nosess", Ok(cfg()));
    assert!(matches!(&run(&mut h, Cmd::Boot)[..], [Evt::LoginRequired]));
}

#[test]
fn boot_with_valid_session_makes_no_requests() {
    let mut h = harness("valid", Ok(cfg()));
    let now = h.time.load(Ordering::SeqCst);
    store::save(&h.path, &Session { expires_at: now + 86_400, ..old_session() }).unwrap();
    assert!(matches!(&run(&mut h, Cmd::Boot)[..], [Evt::LoggedIn]));
    assert!(h.fake.requests().is_empty());
}

#[test]
fn boot_refreshes_a_nearly_expired_session_and_persists_it() {
    let mut h = harness("refresh", Ok(cfg()));
    store::save(&h.path, &old_session()).unwrap(); // expires in 100 s
    h.fake.push(200, r#"{"access_token":"NEW","expires_in":604800}"#);
    assert!(matches!(&run(&mut h, Cmd::Boot)[..], [Evt::LoggedIn]));
    assert_eq!(store::load(&h.path).unwrap().access_token, "NEW");
}

#[test]
fn boot_with_rejected_refresh_deletes_session() {
    let mut h = harness("rejected", Ok(cfg()));
    store::save(&h.path, &old_session()).unwrap();
    h.fake.push(400, r#"{"error":"invalid_grant"}"#);
    assert!(matches!(&run(&mut h, Cmd::Boot)[..], [Evt::LoginRequired]));
    assert!(store::load(&h.path).is_none());
}

#[test]
fn boot_offline_keeps_still_valid_session() {
    let mut h = harness("offline", Ok(cfg()));
    store::save(&h.path, &old_session()).unwrap(); // not expired yet, but inside refresh window
    h.fake.push_err("network error: Dns");
    let ev = run(&mut h, Cmd::Boot);
    assert!(matches!(&ev[..], [Evt::Error(_), Evt::LoggedIn]));
    assert!(store::load(&h.path).is_some());
}

#[test]
fn full_login_flow_emits_code_then_logs_in_and_saves() {
    let mut h = harness("login", Ok(cfg()));
    h.fake.push(200, DEVICE_JSON).push(400, PENDING).push(200, TOKEN_JSON);
    let ev = run(&mut h, Cmd::StartLogin);
    match &ev[..] {
        [Evt::LoginCode { user_code, qr_url, expires_in, .. }, Evt::LoggedIn] => {
            assert_eq!(user_code, "AFIDU");
            assert_eq!(qr_url, "https://link.example.com/AFIDU");
            assert_eq!(*expires_in, 300);
        }
        other => panic!("unexpected events: {other:?}"),
    }
    assert_eq!(store::load(&h.path).unwrap().refresh_token, "RT");
}

#[test]
fn expired_code_is_replaced_automatically() {
    let mut h = harness("regen", Ok(cfg()));
    h.fake
        .push(200, DEVICE_JSON)
        .push(400, r#"{"error":"expired_token"}"#)
        .push(200, DEVICE_JSON)
        .push(200, TOKEN_JSON);
    let ev = run(&mut h, Cmd::StartLogin);
    assert!(matches!(&ev[..], [Evt::LoginCode { .. }, Evt::LoginCode { .. }, Evt::LoggedIn]));
}

#[test]
fn denied_login_reports_failure() {
    let mut h = harness("denied", Ok(cfg()));
    h.fake.push(200, DEVICE_JSON).push(400, r#"{"error":"access_denied"}"#);
    let ev = run(&mut h, Cmd::StartLogin);
    assert!(matches!(&ev[..], [Evt::LoginCode { .. }, Evt::LoginFailed(m)] if m.contains("denied")));
    assert!(store::load(&h.path).is_none());
}

#[test]
fn logout_deletes_session() {
    let mut h = harness("logout", Ok(cfg()));
    store::save(&h.path, &old_session()).unwrap();
    assert!(matches!(&run(&mut h, Cmd::Logout)[..], [Evt::LoginRequired]));
    assert!(store::load(&h.path).is_none());
}
