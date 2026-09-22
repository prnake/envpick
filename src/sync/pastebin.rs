//! HTTP client for the `pastebin-worker` API.
//!
//! | Op     | Request                        |
//! |--------|--------------------------------|
//! | create | `POST /`                       |
//! | update | `PUT /~<name>:<passwd>`        |
//! | fetch  | `GET /~<name>`                 |
//! | delete | `DELETE /~<name>:<passwd>`     |
//!
//! `POST` is the only one that can require authentication (the upstream
//! `BASIC_AUTH` deployment option); `PUT`/`DELETE` authenticate with the
//! password already in the URL.
//!
//! # There is no metadata endpoint
//!
//! Upstream documents `GET /m/<name>` as returning JSON metadata
//! (`lastModifiedAt`, `expireAt`, ...). The deployment this tool targets runs a
//! different version, where `/m/<name>` serves the paste *content* — verified
//! against the live service, for both custom and random names. So this client
//! does not ask for metadata at all: the change it needs to detect is the
//! revision inside the (encrypted) document, which `fetch` already returns.
//!
//! That is not merely a workaround. Comparing the remote's own revision is a
//! stronger signal than a server timestamp: it cannot be confused by a rewrite
//! that happens to land in the same second, and it needs no clock agreement
//! between machines.

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde::Deserialize;

use crate::text::errors;

/// Failures the sync engine needs to *recognise*, as opposed to merely report.
/// Without these, `push` would have to match on message text to decide whether
/// a failed `PUT` should be retried as a `POST`.
#[derive(Debug, thiserror::Error)]
pub enum PastebinError {
    #[error("{}", errors::REMOTE_NOT_FOUND)]
    NotFound,
    #[error("{}", errors::REMOTE_NAME_TAKEN)]
    NameTaken,
    #[error("{}", errors::REMOTE_WRONG_PASSWORD)]
    WrongPassword,
    #[error("{0}")]
    Other(String),
}

/// Receipt of `POST /` and `PUT /~<name>:<passwd>`.
///
/// Field names differ between pastebin-worker versions: upstream master
/// documents `manageUrl`/`expirationSeconds`, while the deployment we target
/// answers with `admin`/`expire`. Both spellings are accepted, so the receipt
/// stays readable if the instance is ever upgraded or moved.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct UploadResponse {
    #[serde(default)]
    pub url: Option<String>,

    /// URL that can modify or delete the paste. Not used for anything the tool
    /// does (it builds the manage URL itself from the derived password), but
    /// kept because it is the receipt's own account of where the paste lives.
    #[serde(default, alias = "manageUrl")]
    pub admin: Option<String>,

    /// Lifetime the server actually granted, in seconds.
    ///
    /// Deliberately untyped: a version that sends this as a string, or omits it,
    /// must not be able to fail an upload that already succeeded. See
    /// [`Self::expiration_seconds`].
    #[serde(default, alias = "expirationSeconds")]
    pub expire: Option<serde_json::Value>,
}

impl UploadResponse {
    /// The granted lifetime, whether the receipt spelled it as a number or a
    /// string. `None` means "the server didn't say", never "zero".
    pub fn expiration_seconds(&self) -> Option<u64> {
        match self.expire.as_ref()? {
            serde_json::Value::Number(n) => n.as_u64(),
            serde_json::Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    }
}

pub struct PastebinClient {
    endpoint: String,
    auth_header: Option<String>,
    /// A configured agent rather than ureq's shared default: the default has
    /// no overall timeout, so a server that accepts a connection and then stops
    /// talking would hang the caller — which in the TUI means a frozen screen
    /// with no way out.
    agent: ureq::Agent,
}

impl PastebinClient {
    pub fn new(endpoint: &str, auth: Option<&str>) -> Result<Self> {
        let endpoint = endpoint.trim_end_matches('/').to_string();
        if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
            bail!("endpoint 必须以 http:// 或 https:// 开头: {endpoint}");
        }
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(20)))
            .timeout_connect(Some(std::time::Duration::from_secs(10)))
            .build();
        Ok(Self {
            endpoint,
            auth_header: auth.map(normalize_auth),
            agent: ureq::Agent::new_with_config(config),
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.endpoint, path)
    }

    /// Attach the optional `Authorization` header. Generic over the request's
    /// typestate because that is the only way to name ureq's builder without
    /// repeating the header logic in all four verbs.
    fn with_auth<B>(&self, req: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        match &self.auth_header {
            Some(auth) => req.header("Authorization", auth.as_str()),
            None => req,
        }
    }

    /// `GET /~<name>` — raw content, or `None` when the paste doesn't exist.
    pub fn fetch(&self, name: &str) -> Result<Option<String>> {
        match self
            .with_auth(self.agent.get(self.url(&paste_path(name))))
            .call()
        {
            Ok(mut resp) => Ok(Some(
                resp.body_mut()
                    .read_to_string()
                    .context("读取远端内容失败")?,
            )),
            Err(ureq::Error::StatusCode(404)) => Ok(None),
            Err(e) => Err(map_ureq_error(e, "读取远端")),
        }
    }

    /// `POST /` — create a new paste under a name we choose.
    pub fn create(
        &self,
        name: &str,
        password: &str,
        content: &str,
        expire: &str,
    ) -> Result<UploadResponse> {
        let body = multipart_body(&[
            ("c", content),
            ("n", name),
            ("s", password),
            ("e", expire),
            ("encryption-scheme", "AES-GCM"),
        ]);
        let resp = self
            .with_auth(self.agent.post(self.url("/")))
            .header("Content-Type", multipart_content_type())
            .send(body.as_slice())
            .map_err(|e| map_ureq_error(e, "创建远端 paste"))?;
        parse_upload(resp)
    }

    /// `PUT /~<name>:<passwd>` — overwrite an existing paste.
    pub fn update(
        &self,
        name: &str,
        password: &str,
        content: &str,
        expire: &str,
    ) -> Result<UploadResponse> {
        let body = multipart_body(&[
            ("c", content),
            ("s", password),
            ("e", expire),
            ("encryption-scheme", "AES-GCM"),
        ]);
        let url = self.url(&paste_manage_path(name, password));
        let resp = self
            .with_auth(self.agent.put(url))
            .header("Content-Type", multipart_content_type())
            .send(body.as_slice())
            .map_err(|e| map_ureq_error(e, "更新远端 paste"))?;
        parse_upload(resp)
    }

    /// `DELETE /~<name>:<passwd>`.
    pub fn delete(&self, name: &str, password: &str) -> Result<()> {
        let url = self.url(&paste_manage_path(name, password));
        self.with_auth(self.agent.delete(url))
            .call()
            .map_err(|e| map_ureq_error(e, "删除远端 paste"))?;
        Ok(())
    }
}

/// Read the receipt of a write that has already happened.
///
/// A 200 means the paste was stored. Anything the body then fails to tell us is
/// a missing extra, not a failed operation — so an unparseable receipt degrades
/// to "the server didn't say" instead of reporting an upload that succeeded as
/// an error the user would retry.
fn parse_upload(mut resp: ureq::http::Response<ureq::Body>) -> Result<UploadResponse> {
    let body = resp
        .body_mut()
        .read_to_string()
        .context("读取远端响应失败")?;
    Ok(serde_json::from_str(&body).unwrap_or_default())
}

/// A user-chosen paste name is fetched with a `~` marker: a paste created with
/// name `ep-x` lives at `/~ep-x`.
pub fn paste_path(name: &str) -> String {
    debug_assert!(
        name.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "paste name {name} needs percent-encoding; validate_sync_id should have caught this"
    );
    format!("/~{name}")
}

/// For `PUT`/`DELETE`, the password is appended after a colon.
fn paste_manage_path(name: &str, password: &str) -> String {
    debug_assert!(
        password
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "manage password must not need escaping"
    );
    format!("{}:{password}", paste_path(name))
}

/// A `user:pass` value is turned into Basic auth; an explicit scheme is passed
/// through untouched.
fn normalize_auth(value: &str) -> String {
    let looks_like_scheme = ["Basic ", "Bearer ", "Token "]
        .iter()
        .any(|p| value.starts_with(p));
    if looks_like_scheme {
        value.to_string()
    } else {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(value)
        )
    }
}

/// Translate transport and status errors into something the user can act on.
/// The 404/409/403 cases are typed so callers can branch on them; everything
/// else is only ever displayed.
fn map_ureq_error(e: ureq::Error, action: &str) -> anyhow::Error {
    match e {
        ureq::Error::StatusCode(403) => PastebinError::WrongPassword.into(),
        ureq::Error::StatusCode(404) => PastebinError::NotFound.into(),
        ureq::Error::StatusCode(409) => PastebinError::NameTaken.into(),
        ureq::Error::StatusCode(401) => PastebinError::Other(format!(
            "{action} 失败：远端要求认证，请在 settings.toml 的 sync.auth 填写凭据"
        ))
        .into(),
        ureq::Error::StatusCode(413) => {
            PastebinError::Other(format!("{action} 失败：内容超出远端大小限制")).into()
        }
        ureq::Error::StatusCode(400) => PastebinError::Other(format!(
            "{action} 失败：请求被远端拒绝（400），可能是命名或过期时间不合法"
        ))
        .into(),
        ureq::Error::StatusCode(code) => {
            PastebinError::Other(format!("{action} 失败：远端返回 {code}")).into()
        }
        other => PastebinError::Other(format!("{action} 失败：{other}")).into(),
    }
}

const BOUNDARY: &str = "----envpickBoundary7x9Kq2";

fn multipart_content_type() -> String {
    format!("multipart/form-data; boundary={BOUNDARY}")
}

/// The worker rejects anything that isn't `multipart/form-data`, so the body is
/// assembled by hand — the format is a handful of lines and this avoids pulling
/// in a multipart crate for one request shape.
fn multipart_body(fields: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, value) in fields {
        out.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        out.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::mock::{mock, upload_json};

    /// What the deployment grants for a `90d` request.
    const EXP_SECS: u64 = 7_776_000;

    fn client(base: &str) -> PastebinClient {
        PastebinClient::new(base, None).unwrap()
    }

    #[test]
    fn create_sends_multipart_with_name_password_and_scheme() {
        let m = mock(vec![(200, upload_json(EXP_SECS))]);
        let r = client(&m.base)
            .create("ep-sync1", "pw0123456789", "CIPHERTEXT", "90d")
            .unwrap();

        assert_eq!(r.expiration_seconds(), Some(EXP_SECS));

        let seen = m.seen.lock().unwrap();
        let req = &seen[0];
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/");
        for needle in [
            "name=\"c\"",
            "CIPHERTEXT",
            "name=\"n\"",
            "ep-sync1",
            "name=\"s\"",
            "pw0123456789",
            "name=\"e\"",
            "90d",
            "encryption-scheme",
        ] {
            assert!(
                req.body.contains(needle),
                "body missing {needle}:\n{}",
                req.body
            );
        }
    }

    #[test]
    fn create_body_is_valid_multipart() {
        let body = String::from_utf8(multipart_body(&[("a", "1"), ("b", "2")])).unwrap();
        assert!(body.starts_with(&format!("--{BOUNDARY}\r\n")));
        assert!(body.ends_with(&format!("--{BOUNDARY}--\r\n")));
        assert_eq!(body.matches(&format!("--{BOUNDARY}\r\n")).count(), 2);
    }

    #[test]
    fn update_puts_to_the_manage_url_and_omits_the_name() {
        let m = mock(vec![(200, upload_json(EXP_SECS))]);
        client(&m.base)
            .update("ep-sync1", "pw0123456789", "NEW", "90d")
            .unwrap();

        let seen = m.seen.lock().unwrap();
        let req = &seen[0];
        assert_eq!(req.method, "PUT");
        assert_eq!(req.path, "/~ep-sync1:pw0123456789");
        assert!(req.body.contains("NEW"));
        // The name is immutable via PUT; sending it is a 400.
        assert!(
            !req.body.contains("name=\"n\""),
            "PUT must not send n:\n{}",
            req.body
        );
    }

    #[test]
    fn fetch_returns_content() {
        let m = mock(vec![(200, "HELLO".into())]);
        assert_eq!(
            client(&m.base).fetch("ep-sync1").unwrap().as_deref(),
            Some("HELLO")
        );
        assert_eq!(m.seen.lock().unwrap()[0].path, "/~ep-sync1");
    }

    #[test]
    fn fetch_returns_none_when_absent() {
        let m = mock(vec![(404, "Error 404: paste of name 'x' not found".into())]);
        assert!(client(&m.base).fetch("ep-sync1").unwrap().is_none());
    }

    /// The deployment we target answers with `admin`/`expire`; upstream master
    /// documents `manageUrl`/`expirationSeconds`. Both spellings are accepted,
    /// so a server upgrade in either direction cannot break the receipt.
    #[test]
    fn both_receipt_spellings_are_understood() {
        let upstream = r#"{"url":"http://x/~ep-a","manageUrl":"http://x/~ep-a:pw",
                           "expirationSeconds":7776000}"#;
        let m = mock(vec![(200, upstream.into())]);
        let r = client(&m.base)
            .create("ep-a", "pw0123456789", "x", "90d")
            .unwrap();
        assert_eq!(r.expiration_seconds(), Some(7_776_000));
        assert_eq!(r.admin.as_deref(), Some("http://x/~ep-a:pw"));
    }

    /// The lifetime spelled as a string, which is a plausible thing for another
    /// version to do. `None` would be a silent lie here: the server did say.
    #[test]
    fn a_string_lifetime_is_read_as_a_number() {
        let m = mock(vec![(200, r#"{"expire":"7776000"}"#.into())]);
        let r = client(&m.base)
            .create("ep-a", "pw0123456789", "x", "90d")
            .unwrap();
        assert_eq!(r.expiration_seconds(), Some(7_776_000));
    }

    /// A 200 means the paste is stored. Whatever the body then fails to tell us
    /// is a missing extra — reporting it as an error would have the user retry
    /// an upload that already succeeded.
    #[test]
    fn an_unreadable_receipt_does_not_fail_a_stored_upload() {
        let m = mock(vec![(200, "<html>surprise</html>".into())]);
        let r = client(&m.base)
            .create("ep-a", "pw0123456789", "x", "90d")
            .unwrap();
        assert_eq!(r.expiration_seconds(), None);
    }

    #[test]
    fn wrong_password_is_reported_clearly() {
        let m = mock(vec![(403, "wrong password".into())]);
        let err = client(&m.base)
            .update("ep-sync1", "bad", "x", "90d")
            .unwrap_err()
            .to_string();
        assert!(err.contains("管理密码不正确"), "got: {err}");
    }

    #[test]
    fn name_conflict_is_reported_clearly() {
        let m = mock(vec![(409, "name used".into())]);
        let err = client(&m.base)
            .create("ep-sync1", "pw0123456789", "x", "90d")
            .unwrap_err()
            .to_string();
        assert!(err.contains("同名 paste"), "got: {err}");
    }

    #[test]
    fn auth_required_is_reported_with_a_hint() {
        let m = mock(vec![(401, "unauthorized".into())]);
        let err = client(&m.base)
            .create("ep-sync1", "pw0123456789", "x", "90d")
            .unwrap_err()
            .to_string();
        assert!(err.contains("sync.auth"), "got: {err}");
    }

    #[test]
    fn delete_uses_the_manage_path() {
        let m = mock(vec![(200, "the paste will be deleted in seconds".into())]);
        client(&m.base).delete("ep-sync1", "pw0123456789").unwrap();
        let seen = m.seen.lock().unwrap();
        assert_eq!(seen[0].method, "DELETE");
        assert_eq!(seen[0].path, "/~ep-sync1:pw0123456789");
    }

    #[test]
    fn user_pass_auth_becomes_basic() {
        let m = mock(vec![(200, upload_json(EXP_SECS))]);
        let c = PastebinClient::new(&m.base, Some("user:pass")).unwrap();
        c.create("ep-a", "pw0123456789", "x", "90d").unwrap();
        let seen = m.seen.lock().unwrap();
        // base64("user:pass")
        assert_eq!(seen[0].auth.as_deref(), Some("Basic dXNlcjpwYXNz"));
    }

    #[test]
    fn explicit_auth_scheme_is_passed_through() {
        let m = mock(vec![(200, upload_json(EXP_SECS))]);
        let c = PastebinClient::new(&m.base, Some("Bearer abc123")).unwrap();
        c.create("ep-a", "pw0123456789", "x", "90d").unwrap();
        assert_eq!(
            m.seen.lock().unwrap()[0].auth.as_deref(),
            Some("Bearer abc123")
        );
    }

    #[test]
    fn trailing_slash_on_endpoint_is_tolerated() {
        let m = mock(vec![(200, "HI".into())]);
        let c = PastebinClient::new(&format!("{}/", m.base), None).unwrap();
        assert_eq!(c.fetch("ep-a").unwrap().as_deref(), Some("HI"));
    }

    #[test]
    fn endpoint_must_be_http() {
        assert!(PastebinClient::new("pb.pka.moe", None).is_err());
        assert!(PastebinClient::new("ftp://x", None).is_err());
    }

    #[test]
    fn paths_match_the_documented_routes() {
        assert_eq!(paste_path("ep-abc"), "/~ep-abc");
        assert_eq!(paste_manage_path("ep-abc", "pw"), "/~ep-abc:pw");
    }
}
