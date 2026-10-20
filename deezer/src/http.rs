//! The HTTP client: the headers Deezer's web player sends and its cookies (the ARL is one of them).

use std::sync::{Arc, Mutex};
use std::time::Duration;
use ureq::{Agent, http::Response};

/// The browser Deezer expects: its private API refuses requests without one.
pub const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// The client: one agent (its connections are pooled and shared by the download workers) and the cookies.
#[derive(Clone)]
pub struct Http {
    agent: Agent,
    cookies: Arc<Mutex<Vec<(String, String)>>>,
}

impl Http {
    /// A client with a cookie jar of its own.
    pub fn new() -> Self {
        let config = Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(120)))
            .build();
        Self {
            agent: config.into(),
            cookies: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Sets a cookie (the ARL when the session starts).
    pub fn set_cookie(&self, name: &str, value: &str) {
        let mut cookies = self.cookies.lock().expect("the cookies are not poisoned");
        match cookies.iter_mut().find(|(n, _)| n == name) {
            Some(entry) => entry.1 = value.to_owned(),
            None => cookies.push((name.to_owned(), value.to_owned())),
        }
    }

    /// The `Cookie` header the requests carry (empty when there is no cookie).
    pub fn cookie_header(&self) -> Option<String> {
        let cookies = self.cookies.lock().expect("the cookies are not poisoned");
        if cookies.is_empty() {
            return None;
        }
        Some(
            cookies
                .iter()
                .map(|(n, v)| format!("{n}={v}"))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    /// A GET with the browser's headers.
    pub fn get(&self, url: &str) -> Result<Response<ureq::Body>, String> {
        let mut request = self.agent.get(url).header("User-Agent", USER_AGENT);
        if let Some(cookies) = self.cookie_header() {
            request = request.header("Cookie", cookies);
        }
        let response = request.call().map_err(|e| describe(&e))?;
        self.remember_cookies(&response);
        Ok(response)
    }

    /// A GET with the browser's headers and one more header (a byte range).
    pub fn get_with(&self, url: &str, header: (&str, &str)) -> Result<Response<ureq::Body>, String> {
        let mut request = self
            .agent
            .get(url)
            .header("User-Agent", USER_AGENT)
            .header(header.0, header.1);
        if let Some(cookies) = self.cookie_header() {
            request = request.header("Cookie", cookies);
        }
        let response = request.call().map_err(|e| describe(&e))?;
        self.remember_cookies(&response);
        Ok(response)
    }

    /// A POST with a JSON body (the private API of Deezer takes its arguments that way).
    pub fn post_json(&self, url: &str, body: &serde_json::Value) -> Result<Response<ureq::Body>, String> {
        let mut request = self.agent.post(url).header("User-Agent", USER_AGENT);
        if let Some(cookies) = self.cookie_header() {
            request = request.header("Cookie", cookies);
        }
        let response = request.send_json(body).map_err(|e| describe(&e))?;
        self.remember_cookies(&response);
        Ok(response)
    }

    /// Keeps the cookies the server sets (`sid`, `dzr_uniq_id`...), so the next calls carry them.
    ///
    /// The CSRF token of the gateway belongs to that session: without the cookies, Deezer answers "Invalid CSRF
    /// token" to every call after the login.
    fn remember_cookies(&self, response: &Response<ureq::Body>) {
        for header in response.headers().get_all("set-cookie") {
            let Ok(value) = header.to_str() else { continue };
            let Some(pair) = value.split(';').next() else { continue };
            let Some((name, val)) = pair.split_once('=') else { continue };
            let (name, val) = (name.trim(), val.trim());
            if name.is_empty() || val.is_empty() {
                continue;
            }
            let mut cookies = self.cookies.lock().expect("the cookies are not poisoned");
            match cookies.iter_mut().find(|(n, _)| n == name) {
                Some(entry) => entry.1 = val.to_owned(),
                None => cookies.push((name.to_owned(), val.to_owned())),
            }
        }
    }
}

/// What to tell the user when a request fails.
fn describe(error: &ureq::Error) -> String {
    match error {
        ureq::Error::StatusCode(code) => format!("Deezer respondió {code}"),
        ureq::Error::Timeout(_) => "la petición tardó demasiado".to_owned(),
        other => format!("la petición falló: {other}"),
    }
}
