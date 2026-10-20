//! The Deezer engine of OpenFadeR: the program of the `deezer` plugin.
//!
//! It speaks the plugin protocol of the app (`docs/plugins.md`): one JSON request in, one JSON reply out. Both
//! protocols are served: the one-shot one (the request on stdin, the process ends after the reply) and the
//! persistent one (a request per line, the reply carries the same `id`).
//!
//! What it does: searches the public API of Deezer (no ARL needed) and downloads the tracks of the catalogue (the
//! ARL is needed): the signed URL of the media API, the bytes in parallel ranges and the encryption of the audio
//! undone on the way to the file.

mod catalog;
mod crypto;
mod download;
mod gw;
mod http;
mod media;
mod plugin;
mod session;
#[cfg(test)]
mod vectors;

use std::io::{BufRead, Write};

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let engine = plugin::Engine::new();
    // A request per line: the one-shot protocol writes it without a newline and closes stdin, the persistent one
    // writes a line and waits for the next.
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(e) => {
                eprintln!("deezer: no se pudo leer la petición: {e}");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let reply = engine.answer(&line);
        // The reply is one line: the persistent protocol reads it line by line.
        if writeln!(stdout, "{reply}").is_err() || stdout.flush().is_err() {
            break;
        }
    }
}
