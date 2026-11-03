//! Minimal HTTP seam so the auth logic can be tested without a network.

use std::time::Duration;

pub struct Response {
    pub status: u16,
    pub body: String,
}

pub trait Http: Send {
    /// POST `application/x-www-form-urlencoded`. `basic` = (user, password).
    /// Non-2xx statuses are returned as `Ok`; only transport failures are `Err`.
    fn post_form(&self, url: &str, form: &[(&str, &str)], basic: Option<(&str, &str)>) -> Result<Response, String>;
}

pub struct UreqHttp {
    agent: ureq::Agent,
}

impl UreqHttp {
    pub fn new() -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(20))
            .user_agent("tide-tv/0.1")
            .build();
        Self { agent }
    }
}

impl Default for UreqHttp {
    fn default() -> Self {
        Self::new()
    }
}

impl Http for UreqHttp {
    fn post_form(&self, url: &str, form: &[(&str, &str)], basic: Option<(&str, &str)>) -> Result<Response, String> {
        let mut req = self.agent.post(url);
        if let Some((u, p)) = basic {
            req = req.set("Authorization", &format!("Basic {}", base64(format!("{u}:{p}").as_bytes())));
        }
        match req.send_form(form) {
            Ok(r) => Ok(Response { status: r.status(), body: r.into_string().map_err(|e| e.to_string())? }),
            Err(ureq::Error::Status(code, r)) => {
                Ok(Response { status: code, body: r.into_string().unwrap_or_default() })
            }
            // Deliberately not including the URL/form: nothing secret in error text.
            Err(ureq::Error::Transport(t)) => Err(format!("network error: {}", t.kind())),
        }
    }
}

/// Standard base64 (RFC 4648) with padding.
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// One-shot local HTTP server: captures the request, replies with `status`/`body`.
    fn serve_once(status: &'static str, body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/oauth2/token", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                let n = c.read(&mut tmp).unwrap();
                buf.extend_from_slice(&tmp[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(i) = text.find("\r\n\r\n") {
                    let len = text[..i]
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                        .unwrap_or(0);
                    if buf.len() >= i + 4 + len {
                        break;
                    }
                }
            }
            let resp = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            c.write_all(resp.as_bytes()).unwrap();
            String::from_utf8_lossy(&buf).to_string()
        });
        (url, h)
    }

    #[test]
    fn ureq_sends_form_and_basic_auth_and_returns_error_statuses_as_ok() {
        let (url, h) = serve_once("400 Bad Request", r#"{"error":"authorization_pending"}"#);
        let r = UreqHttp::new()
            .post_form(&url, &[("client_id", "cid"), ("scope", "r_usr w_usr"), ("grant_type", "x:y")], Some(("cid", "csecret")))
            .unwrap();
        assert_eq!(r.status, 400);
        assert!(r.body.contains("authorization_pending"));
        let req = h.join().unwrap();
        let lower = req.to_ascii_lowercase();
        assert!(lower.starts_with("post /v1/oauth2/token"));
        assert!(lower.contains("content-type: application/x-www-form-urlencoded"));
        assert!(req.contains(&format!("Basic {}", base64(b"cid:csecret"))));
        assert!(req.contains("client_id=cid"));
        assert!(req.contains("scope=r_usr%20w_usr") || req.contains("scope=r_usr+w_usr"));
        assert!(req.contains("grant_type=x%3Ay") || req.contains("grant_type=x:y"));
    }

    #[test]
    fn ureq_transport_error_does_not_leak_request_details() {
        // Port 1 on localhost: connection refused.
        let e = UreqHttp::new().post_form("http://127.0.0.1:1/token", &[("client_secret", "TOPSECRET")], None).err().unwrap();
        assert!(e.starts_with("network error"));
        assert!(!e.contains("TOPSECRET"));
    }
}
