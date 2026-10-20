//! The plugin protocol: the requests the app sends and the replies it expects (`docs/plugins.md`).

use crate::{catalog, crypto, download, gw, http::Http, media, session::Session};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The engine: the HTTP client and the session of the account.
pub struct Engine {
    http: Http,
    /// The session of the ARL in use (the licence and the token of the gateway).
    session: Mutex<Option<Session>>,
}

impl Engine {
    /// An engine without a session (it is opened with the first download).
    pub fn new() -> Self {
        Self {
            http: Http::new(),
            session: Mutex::new(None),
        }
    }

    /// The reply to one request (a line of JSON): one line of JSON in return.
    pub fn answer(&self, line: &str) -> String {
        let request: Value = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(e) => return json!({"error": format!("la petición no es JSON: {e}")}).to_string(),
        };
        let id = request.get("id").cloned();
        let mut reply = self.handle(&request);
        if let (Some(id), Some(object)) = (id, reply.as_object_mut()) {
            object.insert("id".into(), id);
        }
        reply.to_string()
    }

    /// What to answer, by the kind of request.
    fn handle(&self, request: &Value) -> Value {
        let settings = Settings::read(request);
        let op = request["op"].as_str().unwrap_or_default();
        let answer = match op {
            "search" => self.search(request, &settings),
            "playlists" => Ok(json!({"playlists": []})),
            "tracks" => Err("esta fuente solo lista resultados de búsqueda".into()),
            "fetch" => self.fetch(request, &settings),
            "action" => self.action(request, &settings),
            other => Err(format!("operación desconocida {other:?}")),
        };
        match answer {
            Ok(reply) => reply,
            Err(e) => json!({"error": e}),
        }
    }

    /// The search of the browser: the public API, no ARL needed.
    fn search(&self, request: &Value, settings: &Settings) -> Result<Value, String> {
        let query = request["query"].as_str().unwrap_or_default().trim();
        if query.is_empty() {
            return Ok(json!({"tracks": []}));
        }
        let tracks = catalog::search(&self.http, query, settings.results)?;
        Ok(json!({"tracks": tracks}))
    }

    /// The audio of a track the deck asks for.
    fn fetch(&self, request: &Value, settings: &Settings) -> Result<Value, String> {
        let dir = PathBuf::from(
            request["dir"]
                .as_str()
                .ok_or("la app no dio carpeta para descargar")?,
        );
        let name = request["name"].as_str().unwrap_or("audio").to_owned();
        let id = request["id"]
            .as_str()
            .ok_or("la petición no trae id")?
            .to_owned();
        let plugin_dir = dir.parent().map(Path::to_path_buf).unwrap_or_else(|| dir.clone());
        let arl = self.arl(settings, Some(&plugin_dir));
        if arl.is_empty() {
            return Err(
                "sin ARL: ajústalo en las preferencias del plugin (la búsqueda funciona sin él, \
                 las descargas no)"
                    .into(),
            );
        }
        let file = self.download_track(&id, &dir, &name, &arl, settings)?;
        Ok(json!({
            "file": file.file_name().and_then(|n| n.to_str()).unwrap_or_default(),
        }))
    }

    /// The actions of the menu of the plugin.
    fn action(&self, request: &Value, settings: &Settings) -> Result<Value, String> {
        let action = request["action"].as_str().unwrap_or_default();
        let dir = PathBuf::from(
            request["context"]
                .get("dir")
                .and_then(Value::as_str)
                .ok_or("la app no dio carpeta")?,
        );
        match action {
            "arl" => self.action_arl(request, &dir),
            "check" => self.action_check(settings, &dir),
            "download" => self.action_download(request, settings, &dir),
            other => Err(format!("acción desconocida {other:?}")),
        }
    }

    /// The ARL the user pasted: it is kept in the plugin's data folder (the app's settings are a plain text file).
    fn action_arl(&self, request: &Value, dir: &Path) -> Result<Value, String> {
        let arl = request["input"].as_str().unwrap_or_default().trim().to_owned();
        if arl.is_empty() {
            return Err("no se pegó ningún ARL".into());
        }
        if arl.len() < 64 {
            return Err("eso no parece un ARL (demasiado corto): pega el valor de la cookie".into());
        }
        store_arl(dir, &arl)?;
        Ok(json!({"commands": [
            {"cmd": "setting", "id": "arl", "value": arl},
            {"cmd": "toast", "text": "deezer: ARL guardado (descargas activadas)"}
        ]}))
    }

    /// What the account may do: the session opens and says what it may stream.
    fn action_check(&self, settings: &Settings, dir: &Path) -> Result<Value, String> {
        let arl = self.arl(settings, Some(dir));
        let state = if arl.is_empty() {
            "sin ARL (las descargas están desactivadas)".to_owned()
        } else {
            match Session::login(&self.http, &arl) {
                Ok(session) => format!(
                    "ARL bueno ({}): {}",
                    if session.country.is_empty() {
                        "sin país".to_owned()
                    } else {
                        session.country.clone()
                    },
                    if session.lossless {
                        "FLAC y MP3 320"
                    } else if session.hq {
                        "MP3 320"
                    } else {
                        "MP3 128"
                    }
                ),
                Err(e) => format!("el ARL no vale: {e}"),
            }
        };
        Ok(json!({"commands": [{"cmd": "toast", "text": format!("deezer: {state}")}]}))
    }

    /// The download of an address: a track, an album or a playlist.
    fn action_download(
        &self,
        request: &Value,
        settings: &Settings,
        dir: &Path,
    ) -> Result<Value, String> {
        let address = request["input"].as_str().unwrap_or_default().trim().to_owned();
        if address.is_empty() {
            return Err("no hay dirección".into());
        }
        let arl = self.arl(settings, Some(dir));
        if arl.is_empty() {
            return Err("sin ARL: ajústalo en las preferencias del plugin".into());
        }
        let (kind, id) = parse_address(&address)?;
        let tracks = match kind.as_str() {
            "track" => {
                let track = catalog::track(&self.http, &id)?;
                vec![track]
            }
            "album" => catalog::album(&self.http, &id)?,
            "playlist" => catalog::playlist(&self.http, &id)?,
            other => return Err(format!("no sé qué es esto: {other:?}")),
        };
        if tracks.is_empty() {
            return Err("no hay pistas que bajar".into());
        }
        let folder = download_folder(settings, dir);
        let mut files = Vec::new();
        for track in tracks {
            let id = track["id"].as_str().unwrap_or_default().to_owned();
            if id.is_empty() {
                continue;
            }
            let name = file_name(&track);
            let file = self.download_track(&id, &folder, &name, &arl, settings)?;
            files.push(file.to_string_lossy().to_string());
        }
        if files.is_empty() {
            return Err("no se pudo bajar ninguna pista".into());
        }
        let count = files.len();
        Ok(json!({"commands": [
            {"cmd": "import", "files": files},
            {"cmd": "toast", "text": format!("deezer: {count} archivo(s) añadido(s) a la colección")},
        ]}))
    }

    /// One track: its page, the URL of its audio and the bytes.
    fn download_track(
        &self,
        id: &str,
        dir: &Path,
        name: &str,
        arl: &str,
        settings: &Settings,
    ) -> Result<PathBuf, String> {
        let mut clock = Clock::new();
        let session = self.session(arl)?;
        clock.add("session");
        let page = self.track_page(id, arl, &session)?;
        clock.add("página");
        let token = field(&page, "TRACK_TOKEN")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if token.is_empty() {
            return Err(format!("Deezer no dio un token para la pista {id}"));
        }
        let chain = formats(&page, &session, &settings.quality);
        if chain.is_empty() {
            return Err(format!(
                "Deezer lista la pista {id} pero esta cuenta no puede streamedla en ninguna calidad"
            ));
        }
        let media = media::track_url(&self.http, &session.license_token, &token, &chain)?;
        clock.add("URL");
        let key = crypto::key(id);
        let extension = media::extension(&media.format);
        let done = download::fetch(
            &self.http,
            &media.url,
            &key,
            dir,
            name,
            extension,
            settings.connections,
        )?;
        clock.add("descarga");
        eprintln!(
            "deezer: {id} · {} · {:.1} s · {}",
            media.format,
            clock.total(),
            clock.report()
        );
        Ok(done.file)
    }

    /// The page of a track, with a fresh token when the gateway rejects the one of the session.
    ///
    /// Deezer rotates the CSRF token of the gateway: when it answers "Invalid CSRF token", a new session is opened
    /// (the login gives a new one) and the call is made again, which is what the other downloaders do.
    fn track_page(&self, id: &str, arl: &str, session: &Session) -> Result<Value, String> {
        match gw::track_page(&self.http, &session.api_token, id) {
            Ok(page) => Ok(page),
            Err(e) if is_token_error(&e) => {
                let session = self.new_session(arl)?;
                gw::track_page(&self.http, &session.api_token, id)
            }
            Err(e) => Err(e),
        }
    }

    /// The session of the ARL (it is opened once and kept).
    fn session(&self, arl: &str) -> Result<Session, String> {
        let mut slot = self.session.lock().expect("the session is not poisoned");
        if let Some(session) = slot.as_ref() {
            if session.arl == arl {
                return Ok(session.clone());
            }
        }
        let session = Session::login(&self.http, arl)?;
        *slot = Some(session.clone());
        Ok(session)
    }

    /// A new session for the ARL: it replaces the one that is kept (the token of the old one was rejected).
    fn new_session(&self, arl: &str) -> Result<Session, String> {
        let session = Session::login(&self.http, arl)?;
        *self.session.lock().expect("the session is not poisoned") = Some(session.clone());
        Ok(session)
    }

    /// The ARL: the one of the settings or the one kept in the plugin's data folder.
    fn arl(&self, settings: &Settings, dir: Option<&Path>) -> String {
        if !settings.arl.trim().is_empty() {
            return settings.arl.trim().to_owned();
        }
        dir.and_then(|dir| std::fs::read_to_string(arl_file(dir)).ok())
            .map(|arl| arl.trim().to_owned())
            .unwrap_or_default()
    }
}

/// Whether the gateway refused the token of the session (it rotates it).
fn is_token_error(error: &str) -> bool {
    error.contains("Invalid CSRF token") || error.contains("invalid api token")
}

/// The clock of a download: what each step took (the plugin writes it to stderr, where the app shows it).
struct Clock {
    start: std::time::Instant,
    steps: Vec<(&'static str, std::time::Duration)>,
    last: std::time::Instant,
}

impl Clock {
    /// A clock that starts now.
    fn new() -> Self {
        let now = std::time::Instant::now();
        Self {
            start: now,
            steps: Vec::new(),
            last: now,
        }
    }

    /// The step that just ended.
    fn add(&mut self, name: &'static str) {
        let now = std::time::Instant::now();
        self.steps.push((name, now - self.last));
        self.last = now;
    }

    /// The seconds since the clock started.
    fn total(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    /// The steps, one after the other (`session 90 ms · descarga 6.4 s`).
    fn report(&self) -> String {
        self.steps
            .iter()
            .map(|(name, took)| format!("{name} {:.0} ms", took.as_secs_f64() * 1000.0))
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// A field of the track: the gateway puts the track under `DATA` (the page of the track).
fn field<'a>(page: &'a Value, name: &str) -> Option<&'a Value> {
    page.get("DATA")
        .and_then(|data| data.get(name))
        .or_else(|| page.get(name))
}

/// The settings of the plugin (they come with every request).
struct Settings {    /// The ARL, when the user pasted it in the preferences.
    arl: String,
    /// The quality the user wants (`FLAC`, `MP3_320`, `MP3_128`).
    quality: String,
    /// How many results a search gives.
    results: u32,
    /// How many connections a download uses.
    connections: usize,
    /// Where the action of downloading leaves the files.
    folder: String,
}

impl Settings {
    /// The settings of a request (the ones of the plugin, with their defaults).
    fn read(request: &Value) -> Self {
        let settings = &request["settings"];
        let text = |name: &str| settings[name].as_str().unwrap_or_default().to_owned();
        Self {
            arl: text("arl"),
            quality: text("quality"),
            results: settings["results"].as_u64().unwrap_or(25).clamp(1, 100) as u32,
            connections: settings["connections"].as_u64().unwrap_or(4).clamp(1, 16) as usize,
            folder: text("folder"),
        }
    }
}

/// The formats to ask for, in order of preference: the one the user wants and the ones below it.
///
/// A format is left out when the account may not stream it or when the track has no file of that size.
fn formats(page: &Value, session: &Session, quality: &str) -> Vec<String> {
    let wanted = quality.trim().to_uppercase();
    let order: &[&str] = match wanted.as_str() {
        "MP3_128" | "128" => &["MP3_128"],
        "MP3_320" | "320" | "MP3" => &["MP3_320", "MP3_128"],
        _ => &["FLAC", "MP3_320", "MP3_128"],
    };
    order
        .iter()
        .filter(|format| allowed(page, session, format))
        .map(|format| format.to_string())
        .collect()
}

/// Whether the account may stream `format` and the track has it.
fn allowed(page: &Value, session: &Session, format: &str) -> bool {
    let size = match format {
        "FLAC" => field(page, "FILESIZE_FLAC"),
        "MP3_320" => field(page, "FILESIZE_MP3_320"),
        _ => field(page, "FILESIZE_MP3_128"),
    };
    if !number(size).is_some_and(|size| size > 0.0) {
        return false;
    }
    match format {
        "FLAC" => session.lossless,
        "MP3_320" => session.hq,
        _ => true,
    }
}

/// A number of the answer (Deezer sends them as numbers or as text).
fn number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// Where the ARL is kept: `<data dir>/plugins/deezer/config/.arl`.
fn arl_file(dir: &Path) -> PathBuf {
    dir.join("config").join(".arl")
}

/// Keeps the ARL in the plugin's data folder.
fn store_arl(dir: &Path, arl: &str) -> Result<(), String> {
    let file = arl_file(dir);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(&file, arl).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(())
}

/// Where the action of downloading leaves the files.
fn download_folder(settings: &Settings, dir: &Path) -> PathBuf {
    let custom = settings.folder.trim().trim_matches('"');
    let folder = if custom.is_empty() {
        dir.join("downloads")
    } else {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(custom)
    };
    let folder = folder
        .to_string_lossy()
        .replace("~", &std::env::var("HOME").unwrap_or_default());
    PathBuf::from(folder)
}

/// What an address of Deezer is: its kind and its id.
fn parse_address(address: &str) -> Result<(String, String), String> {
    let without_scheme = address
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(address);
    let path = without_scheme
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .trim_end_matches('/');
    let mut parts = path.split('/').rev().filter(|part| !part.is_empty());
    let id = parts
        .next()
        .ok_or("esa dirección no lleva el id de la pista")?;
    if !id.chars().all(|c| c.is_ascii_digit()) {
        return Err("esa dirección no lleva el id de la pista".into());
    }
    let kind = parts
        .next()
        .unwrap_or("track")
        .split(['?', '#'])
        .next()
        .unwrap_or("track")
        .to_owned();
    match kind.as_str() {
        "track" | "album" | "playlist" => Ok((kind, id.to_owned())),
        other => Err(format!("no sé qué es esto: {other:?}")),
    }
}

/// The name of a file of the action of downloading: its id and its title.
fn file_name(track: &Value) -> String {
    let id = track["id"].as_str().unwrap_or_default();
    let artist = track["artist"].as_str().unwrap_or_default();
    let title = track["title"].as_str().unwrap_or_default();
    let text = if artist.is_empty() {
        title.to_owned()
    } else if title.is_empty() {
        artist.to_owned()
    } else {
        format!("{artist} - {title}")
    };
    let safe: String = text
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let safe = safe.trim().to_owned();
    if id.is_empty() {
        safe
    } else {
        format!("{id} {safe}")
    }
}
