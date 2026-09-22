//! A tiny hand-rolled HTTP server for tests.
//!
//! The sync paths are the ones worth testing — conflict detection, create vs.
//! update, error mapping — and they should be testable without touching the
//! network or a real pastebin. This serves canned responses and records what
//! the client actually sent.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub auth: Option<String>,
    pub body: String,
}

pub struct Mock {
    pub base: String,
    pub seen: Arc<Mutex<Vec<Recorded>>>,
    stop: Arc<AtomicBool>,
}

impl Mock {
    pub fn requests(&self) -> Vec<Recorded> {
        self.seen.lock().unwrap().clone()
    }

    pub fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so it can observe the flag and exit.
        let _ = TcpStream::connect(self.base.trim_start_matches("http://"));
    }
}

/// Serve `responses` in order, one per request. Requests beyond the list get a
/// 404, so an unexpected extra round trip fails loudly instead of hanging.
pub fn mock(responses: Vec<(u16, String)>) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));

    let seen2 = Arc::clone(&seen);
    let stop2 = Arc::clone(&stop);
    std::thread::spawn(move || {
        let mut idx = 0;
        while !stop2.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    if stop2.load(Ordering::SeqCst) {
                        break;
                    }
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    if let Some(req) = read_request(&mut stream) {
                        seen2.lock().unwrap().push(req);
                        let (status, body) = responses
                            .get(idx)
                            .cloned()
                            .unwrap_or((404, "not found".into()));
                        idx += 1;
                        write_response(&mut stream, status, &body);
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });

    Mock {
        base: format!("http://{addr}"),
        seen,
        stop,
    }
}

fn read_request(stream: &mut TcpStream) -> Option<Recorded> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    let head_end = loop {
        if let Some(p) = find(&buf, b"\r\n\r\n") {
            break p + 4;
        }
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next()?.to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let mut auth = None;
    let mut content_length = 0usize;
    let mut expect_continue = false;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim();
            match k.to_ascii_lowercase().as_str() {
                "authorization" => auth = Some(v.to_string()),
                "content-length" => content_length = v.parse().unwrap_or(0),
                "expect" if v.eq_ignore_ascii_case("100-continue") => expect_continue = true,
                _ => {}
            }
        }
    }

    if expect_continue {
        let _ = stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
    }

    while buf.len() < head_end + content_length {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }

    Some(Recorded {
        method,
        path,
        auth,
        body: String::from_utf8_lossy(&buf[head_end..]).to_string(),
    })
}

fn write_response(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        _ => "Error",
    };
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain;charset=UTF-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The JSON body `POST /` and `PUT` return on success, in the shape the
/// deployment we target actually uses (`admin`/`expire`, not upstream's
/// `manageUrl`/`expirationSeconds` — copied from a live response).
pub fn upload_json(expire_seconds: u64) -> String {
    format!(
        r#"{{"url":"http://x/~ep-a","suggestUrl":null,"admin":"http://x/~ep-a:pw",
            "isPrivate":false,"expire":{expire_seconds}}}"#
    )
}
