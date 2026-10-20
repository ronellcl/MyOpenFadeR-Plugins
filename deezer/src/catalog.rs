//! The public API of Deezer (`api.deezer.com`): the search and the listings of an album or a playlist.
//!
//! It needs no ARL, so the browser of the library works without a session (and the search never waits for one).

use crate::http::Http;
use serde_json::Value;

/// The public API.
const API: &str = "https://api.deezer.com";

/// The tracks of a search.
pub fn search(http: &Http, query: &str, limit: u32) -> Result<Vec<Value>, String> {
    let url = format!("{API}/search?q={}&limit={limit}", escape(query));
    let answer = get(http, &url)?;
    let rows = answer
        .get("data")
        .and_then(Value::as_array)
        .ok_or("Deezer no devolvió resultados")?;
    Ok(rows.iter().filter_map(shape).collect())
}

/// One track of the public API.
pub fn track(http: &Http, id: &str) -> Result<Value, String> {
    let answer = get(http, &format!("{API}/track/{id}"))?;
    shape(&answer).ok_or_else(|| format!("Deezer no conoce la pista {id}"))
}

/// The tracks of an album.
pub fn album(http: &Http, id: &str) -> Result<Vec<Value>, String> {
    let answer = get(http, &format!("{API}/album/{id}"))?;
    let rows = answer
        .get("tracks")
        .and_then(|tracks| tracks.get("data"))
        .and_then(Value::as_array)
        .ok_or("Deezer no devolvió las pistas del álbum")?;
    Ok(rows.iter().filter_map(shape).collect())
}

/// The tracks of a playlist.
pub fn playlist(http: &Http, id: &str) -> Result<Vec<Value>, String> {
    let answer = get(http, &format!("{API}/playlist/{id}"))?;
    let rows = answer
        .get("data")
        .and_then(Value::as_array)
        .ok_or("Deezer no devolvió las pistas de la lista")?;
    Ok(rows.iter().filter_map(shape).collect())
}

/// One track of the public API, in the shape the app expects.
fn shape(track: &Value) -> Option<Value> {
    let id = track.get("id")?;
    let title = track.get("title")?.as_str()?;
    let artist = track
        .get("artist")
        .and_then(|artist| artist.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let album = track
        .get("album")
        .and_then(|album| album.get("title"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut row = serde_json::json!({
        "id": id.to_string(),
        "title": title,
        "artist": artist,
        "url": format!("https://www.deezer.com/track/{id}"),
    });
    if !album.is_empty() {
        row["album"] = Value::String(album.to_owned());
    }
    if let Some(length) = track.get("duration").and_then(Value::as_f64) {
        row["length"] = Value::from(length.round() as i64);
    }
    if let Some(bpm) = track.get("bpm").and_then(Value::as_f64) {
        if bpm > 0.0 {
            row["bpm"] = Value::from(bpm);
        }
    }
    Some(row)
}

/// A GET of the public API, as JSON.
fn get(http: &Http, url: &str) -> Result<Value, String> {
    http.get(url)
        .map_err(|e| format!("Deezer: {e}"))?
        .body_mut()
        .read_json()
        .map_err(|e| format!("Deezer: la respuesta no es JSON: {e}"))
}

/// A text as a query parameter.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
