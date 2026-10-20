//! The media API: the signed URL of a track (`media.deezer.com/v1/get_url`).
//!
//! The API takes a chain of formats and answers with the best one the account may stream, so the fallback from
//! FLAC to MP3 320 costs no extra request: the tier it served is in the answer (`media.format`).

use crate::http::Http;
use serde_json::{Value, json};

/// The endpoint that signs the URLs of the catalogue.
const MEDIA: &str = "https://media.deezer.com/v1/get_url";

/// A signed URL and the format it serves.
pub struct Media {
    /// The URL of the file (the CDN serves it encrypted).
    pub url: String,
    /// The format it serves (`FLAC`, `MP3_320`, `MP3_128`).
    pub format: String,
}

/// Asks for the URL of a track, with the formats of `chain` in order of preference.
pub fn track_url(
    http: &Http,
    license_token: &str,
    track_token: &str,
    chain: &[String],
) -> Result<Media, String> {
    if chain.is_empty() {
        return Err("no hay ningún formato que esta cuenta pueda streamed".into());
    }
    let formats: Vec<Value> = chain
        .iter()
        .map(|format| json!({"cipher": "BF_CBC_STRIPE", "format": format}))
        .collect();
    let body = json!({
        "license_token": license_token,
        "media": [{"type": "FULL", "formats": formats}],
        "track_tokens": [track_token],
    });
    let answer: Value = http
        .post_json(MEDIA, &body)
        .map_err(|e| format!("la API de medios: {e}"))?
        .body_mut()
        .read_json()
        .map_err(|e| format!("la API de medios: la respuesta no es JSON: {e}"))?;
    let entry = answer
        .get("data")
        .and_then(Value::as_array)
        .and_then(|data| data.first())
        .ok_or("la API de medios no devolvió nada")?;
    if let Some(errors) = entry.get("errors").and_then(Value::as_array) {
        if let Some(first) = errors.first() {
            let code = first.get("code").and_then(Value::as_i64).unwrap_or(0);
            return Err(match code {
                2002 => "Deezer no sirve este tema en el país de la cuenta".to_owned(),
                _ => format!("Deezer rechazó la petición: {}", compact(first)),
            });
        }
    }
    let media = entry
        .get("media")
        .and_then(Value::as_array)
        .and_then(|media| media.first())
        .ok_or("Deezer no dio ninguna URL para este tema")?;
    let url = media
        .get("sources")
        .and_then(Value::as_array)
        .and_then(|sources| sources.first())
        .and_then(|source| source.get("url"))
        .and_then(Value::as_str)
        .ok_or("Deezer no dio ninguna URL para este tema")?
        .to_owned();
    let format = media
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("MP3_128")
        .to_owned();
    Ok(Media { url, format })
}

/// The extension of a format: what the decks can play.
pub fn extension(format: &str) -> &'static str {
    match format {
        "FLAC" => "flac",
        _ => "mp3",
    }
}

/// The JSON of a value in one line.
fn compact(value: &Value) -> String {
    value.to_string()
}
