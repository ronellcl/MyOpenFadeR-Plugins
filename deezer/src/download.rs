//! The download: the bytes of the signed URL in parallel ranges, decrypted on the way to the file.
//!
//! The CDN is asked for the file in pieces (`Range`), each piece in a thread of its own: the parts of the file are
//! written where they belong (the file is made as big as the answer says before the first byte arrives), so there
//! is no joining step and no copy. The pieces start at a multiple of the pattern (6144 bytes), so each thread can
//! decrypt its own stripes without knowing anything of the others.

use crate::crypto::{self, Decryptor, PATTERN};
use crate::http::Http;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread;

/// The bytes read at a time from the network.
const CHUNK: usize = 64 * 1024;

/// The file of a track, downloaded and decrypted.
pub struct Downloaded {
    /// Where the audio is (the file is complete: it was renamed into place).
    pub file: PathBuf,
    /// How many bytes it has.
    #[allow(dead_code)]
    pub bytes: u64,
}

/// Downloads `url` into `<dir>/<name>.<ext>` and gives back the file.
///
/// The temporary file (`.part`) is renamed at the end: the app only takes a file for a track when it is complete.
pub fn fetch(
    http: &Http,
    url: &str,
    key: &[u8; 16],
    dir: &Path,
    name: &str,
    extension: &str,
    connections: usize,
) -> Result<Downloaded, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let final_path = dir.join(format!("{name}.{extension}"));
    let temporary = dir.join(format!("{name}.{extension}.part"));
    let _ = std::fs::remove_file(&temporary);

    // The first question is whether the CDN serves ranges: the answer is in the status of a one byte request.
    let probe = http.get_with(url, ("Range", "bytes=0-0"))?;
    let ranged = probe.status() == ureq::http::StatusCode::PARTIAL_CONTENT;
    if ranged {
        let total = total_of(probe.headers())?;
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)
            .map_err(|e| format!("{}: {e}", temporary.display()))?;
        file.set_len(total)
            .map_err(|e| format!("{}: {e}", temporary.display()))?;
        drop(file);
        parallel(http, url, key, &temporary, total, connections.max(1))?;
    } else {
        // No ranges: the answer of the probe is the whole file.
        let total = probe
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        single(http, probe, key, &temporary, total)?;
    }

    let size = std::fs::metadata(&temporary)
        .map_err(|e| format!("{}: {e}", temporary.display()))?
        .len();
    std::fs::rename(&temporary, &final_path)
        .map_err(|e| format!("{}: {e}", final_path.display()))?;
    Ok(Downloaded {
        file: final_path,
        bytes: size,
    })
}

/// The file in pieces, one thread per piece.
fn parallel(
    http: &Http,
    url: &str,
    key: &[u8; 16],
    path: &Path,
    total: u64,
    connections: usize,
) -> Result<(), String> {
    let units = total.div_ceil(PATTERN as u64); // the pieces of the pattern the file holds
    let per = units.div_ceil(connections as u64).max(1);
    let mut workers = Vec::new();
    for index in 0..connections {
        let first = index as u64 * per;
        if first >= units {
            break;
        }
        let last = ((first + per).min(units) * PATTERN as u64).min(total);
        let start = first * PATTERN as u64;
        if start >= last {
            break;
        }
        // The pieces start at a multiple of the pattern: each thread then decrypts its own stripes.
        debug_assert!(!crypto::is_encrypted(start) || start % PATTERN as u64 == 0);
        let http = http.clone();
        let url = url.to_owned();
        let key = *key;
        let path = path.to_path_buf();
        workers.push(thread::spawn(move || {
            piece(&http, &url, &key, &path, start, last)
        }));
    }
    let mut failure = None;
    for worker in workers {
        match worker.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                failure.get_or_insert(e);
            }
            Err(_) => {
                failure.get_or_insert("un hilo de la descarga se cayó".to_owned());
            }
        }
    }
    match failure {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// One piece of the file: the range asked for, decrypted and written where it belongs.
fn piece(
    http: &Http,
    url: &str,
    key: &[u8; 16],
    path: &Path,
    start: u64,
    end: u64,
) -> Result<(), String> {
    let range = format!("bytes={}-{}", start, end - 1);
    let response = http.get_with(url, ("Range", &range))?;
    if response.status() != ureq::http::StatusCode::PARTIAL_CONTENT {
        return Err("Deezer no devolvió el rango pedido".into());
    }
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.seek(SeekFrom::Start(start))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut decryptor = Decryptor::new(key)?;
    let mut reader = response.into_body().into_reader();
    let mut buffer = vec![0u8; CHUNK];
    let mut written = 0u64;
    loop {
        let read = reader.read(&mut buffer).map_err(|e| format!("leer: {e}"))?;
        if read == 0 {
            break;
        }
        let plain = decryptor.push(&buffer[..read]);
        if !plain.is_empty() {
            file.write_all(&plain).map_err(|e| format!("escribir: {e}"))?;
            written += plain.len() as u64;
        }
    }
    let plain = decryptor.finish();
    if !plain.is_empty() {
        file.write_all(&plain).map_err(|e| format!("escribir: {e}"))?;
        written += plain.len() as u64;
    }
    if written != end - start {
        return Err(format!(
            "el rango {}-{} llegó corto: {written} de {} bytes",
            start,
            end - 1,
            end - start
        ));
    }
    Ok(())
}

/// The whole file in one answer (the CDN does not serve ranges).
fn single(
    _http: &Http,
    response: ureq::http::Response<ureq::Body>,
    key: &[u8; 16],
    path: &Path,
    total: u64,
) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut decryptor = Decryptor::new(key)?;
    let mut reader = response.into_body().into_reader();
    let mut buffer = vec![0u8; CHUNK];
    let mut written = 0u64;
    loop {
        let read = reader.read(&mut buffer).map_err(|e| format!("leer: {e}"))?;
        if read == 0 {
            break;
        }
        let plain = decryptor.push(&buffer[..read]);
        if !plain.is_empty() {
            file.write_all(&plain).map_err(|e| format!("escribir: {e}"))?;
            written += plain.len() as u64;
        }
    }
    let plain = decryptor.finish();
    if !plain.is_empty() {
        file.write_all(&plain).map_err(|e| format!("escribir: {e}"))?;
        written += plain.len() as u64;
    }
    if total > 0 && written != total {
        return Err(format!("la descarga llegó corta: {written} de {total} bytes"));
    }
    Ok(())
}

/// The size of the file, from the `Content-Range` of a range answer (`bytes 0-0/12345`).
fn total_of(headers: &ureq::http::HeaderMap) -> Result<u64, String> {
    let range = headers
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .ok_or("Deezer no dijo el tamaño del archivo")?;
    range
        .rsplit('/')
        .next()
        .and_then(|total| total.trim().parse::<u64>().ok())
        .ok_or(format!("Deezer no dijo el tamaño del archivo ({range})"))
}
