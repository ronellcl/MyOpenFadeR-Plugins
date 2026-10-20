//! The session: the ARL cookie -> the gateway token, the licence token and what the account may stream.

use crate::gw;
use crate::http::Http;
use serde_json::{Value, json};

/// What the session knows about the account behind the ARL.
#[derive(Clone)]
pub struct Session {
    /// The ARL the session was opened with.
    pub arl: String,
    /// The CSRF token every call of the gateway carries.
    pub api_token: String,
    /// The licence token the media API asks for.
    pub license_token: String,
    /// The account may stream 320 kbps.
    pub hq: bool,
    /// The account may stream FLAC.
    pub lossless: bool,
    /// The country of the licence (Deezer refuses some tracks outside it).
    pub country: String,
}

impl Session {
    /// Opens a session with the ARL of the user.
    ///
    /// The ARL is the cookie of the session in the browser: with it Deezer tells who the user is, what the
    /// licence allows to stream and the token the calls of the gateway need.
    pub fn login(http: &Http, arl: &str) -> Result<Self, String> {
        let arl = arl.trim();
        if arl.is_empty() {
            return Err("sin ARL: las descargas necesitan tu cookie de sesión".into());
        }
        http.set_cookie("arl", arl);
        let results = gw::call(http, "null", "deezer.getUserData", &json!({}))?;
        let user = results.get("USER").cloned().unwrap_or(Value::Null);
        let id = user.get("USER_ID").and_then(Value::as_i64).unwrap_or(0);
        if user.is_null() || id == 0 {
            return Err(
                "el ARL no fue aceptado por Deezer: cópialo de nuevo con la sesión iniciada en deezer.com \
                 (F12 > Application > Cookies > www.deezer.com > arl)"
                    .into(),
            );
        }
        let options = user.get("OPTIONS").cloned().unwrap_or(Value::Null);
        let license_token = options
            .get("license_token")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if license_token.is_empty() {
            return Err("Deezer no dio una licencia de streaming a esta cuenta".into());
        }
        Ok(Self {
            arl: arl.to_owned(),
            api_token: results
                .get("checkForm")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            license_token,
            hq: flag(&options, "web_hq") || flag(&options, "mobile_hq"),
            lossless: flag(&options, "web_lossless") || flag(&options, "mobile_lossless"),
            country: options
                .get("license_country")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        })
    }
}

/// Whether the option is on (Deezer sends a number, a boolean or a text).
fn flag(options: &Value, name: &str) -> bool {
    match options.get(name) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty() && s != "0",
        _ => false,
    }
}
