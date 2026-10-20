//! The gateway (`gw-light.php`): the private API Deezer's web player uses for the pages of the site.
//!
//! Every call carries the CSRF token of the session (`checkForm`) and answers with `results` (or with `error`).

use crate::http::Http;
use serde_json::{Value, json};

/// The gateway of the web player.
const GATEWAY: &str = "https://www.deezer.com/ajax/gw-light.php";

/// Calls `method` with `args` and gives back its `results`.
///
/// The token is the one of the session (the first call of a session asks for the user's data with `null`).
pub fn call(http: &Http, token: &str, method: &str, args: &Value) -> Result<Value, String> {
    let url = format!(
        "{GATEWAY}?api_version=1.0&api_token={token}&input=3&method={method}"
    );
    let answer: Value = http
        .post_json(&url, args)
        .map_err(|e| format!("{method}: {e}"))?
        .body_mut()
        .read_json()
        .map_err(|e| format!("{method}: la respuesta no es JSON: {e}"))?;
    if let Some(error) = answer.get("error") {
        if !error.is_null() && error.as_object().is_some_and(|o| !o.is_empty()) {
            return Err(format!("{method}: {}", compact(error)));
        }
    }
    Ok(answer.get("results").cloned().unwrap_or(Value::Null))
}

/// The page of a track: its token, its sizes and what the account may stream.
pub fn track_page(http: &Http, token: &str, id: &str) -> Result<Value, String> {
    let args = json!({"SNG_ID": id});
    let results = call(http, token, "deezer.pageTrack", &args)?;
    if results.is_null() {
        return Err(format!("Deezer no conoce la pista {id}"));
    }
    Ok(results)
}

/// The JSON of a value in one line (the errors of the gateway are objects).
fn compact(value: &Value) -> String {
    value.to_string()
}
