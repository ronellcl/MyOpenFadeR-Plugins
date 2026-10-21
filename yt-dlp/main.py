#!/usr/bin/env python3
"""OpenFadeR plugin: YouTube and SoundCloud through the yt-dlp program (protocol: docs/plugins.md).

One request as JSON on stdin, one reply as JSON on stdout. Needs python3; yt-dlp
comes from the plugin's data folder, else from the `bin` folder next to this
script (drop the official binary there: yt-dlp.exe / yt-dlp / yt-dlp_macos), else
from PATH, else it is downloaded once into the data folder. The installer ships a
binary next to the script and the plugin is read-only under Program Files, so the
first use copies it into the data folder, where it can update itself. ffmpeg turns
streams the decks cannot play (opus, webm) into AAC, merges the mp4 video
download, and writes mp3.

Track ids: the video id on YouTube, the path of the track page (`artist/track`)
on SoundCloud. Playlists: the file playlists.json next to this script lists
addresses to show under each source; edit it freely.

Self-maintenance (docs: the host kills a script call after 900 s; our inner
timeouts stay clearly below):
* `auto_update`: on the first request of the day it runs `yt-dlp -U --update-to
  <channel>` ( Silence when yt-dlp is already new or managed by pip). The stamp
  lives in the plugin's data folder, else next to this script when writable.
* when an operation fails with an extractor-signature message (YouTube changed
  its player, the usual breakage), it updates yt-dlp and retries once, so the
  fix lands at the moment it is needed.
"""

import json
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlparse
from urllib.request import urlopen

HERE = os.path.dirname(os.path.abspath(__file__))
DIR = None  # the plugin's folder in the data dir, when the request says where
# The plugin's settings (plugin manager), set from each request:
# "results" (tracks a search or a playlist shows), "format" (m4a, mp3, mp4 for the
# VIDEO view, or "best": the best stream the decks play, never converted),
# "maxres" (video cap), "auto_update" and "channel" (the self-maintenance).
SETTINGS = {"results": 25, "format": "m4a", "maxres": "1080",
            "auto_update": True, "channel": "stable"}

SOURCES = {
    "youtube": {"search": "ytsearch{n}:{q}", "page": "https://www.youtube.com/watch?v={id}"},
    "soundcloud": {"search": "scsearch{n}:{q}", "page": "https://soundcloud.com/{id}"},
}

# The official binary: the data folder's copy wins when there is one (see seed_bundled), so the update never needs to
# write under Program Files.
BINARY = {"Windows": "yt-dlp.exe", "Darwin": "yt-dlp_macos"}.get(platform.system(), "yt-dlp")

# The official build per channel, for when there is no binary anywhere (a plugin installed by hand from the
# repository ships none): it fetches one once into its data folder.
RELEASE = {
    "stable": "https://github.com/yt-dlp/yt-dlp/releases/latest/download/{name}",
    "nightly": "https://github.com/yt-dlp/yt-dlp-nightly-builds/releases/latest/download/{name}",
}

# Streams the decks decode, best first, for when ffmpeg cannot convert the others.
PLAYABLE = "bestaudio[ext=m4a]/bestaudio[ext=mp3]"

# The app starts us with no console, and the helpers run console programs of their own (yt-dlp its ffmpeg): started
# detached they would have no console, so Windows would create a brand-new one for the grandchild, with its window.
# A hidden console (`CREATE_NO_WINDOW`) is inherited by the whole chain instead, so nothing is ever shown.
NO_WINDOW = getattr(subprocess, "CREATE_NO_WINDOW", 0)

# Messages that mean "the extractor no longer understands YouTube", the case a fresh
# yt-dlp fixes. A missing format or a private video is not one of them.
EXTRACTOR_BUGS = ("unable to extract", "Signature extraction failed",
                  "nsig extraction failed", "player response")

# Timeouts, minutes below the host's 900 s per call.
SEARCH_TIMEOUT = 120
FETCH_TIMEOUT = 600
ACTION_TIMEOUT = 1800
UPDATE_TIMEOUT = 180


class Failure(Exception):
    """Sent back as {"error": …}."""


def own_bin():
    """The plugin's `bin` folder: the official binary shipped next to the script lives here."""
    return os.path.join(HERE, "bin")


def data_bin():
    """The `bin` folder in the plugin's data folder (per user, writable): ours after a download."""
    return os.path.join(DIR, "bin") if DIR else None


def seed_bundled():
    """Copies the binary shipped next to the script into the data folder, once, when it cannot update itself.

    The installer puts the plugin under Program Files, where the shipped `bin\\yt-dlp.exe` is read-only: `yt-dlp -U`
    would fail on it. The first time it is used, it is copied to `<data>/bin` (writable) and that copy is run from then
    on, so the daily update works. Without an installer (a checkout, the data folder is the same or missing), this does
    nothing.
    """
    own, data = own_bin(), data_bin()
    if not own or not data:
        return
    src = os.path.join(own, BINARY)
    dst = os.path.join(data, BINARY)
    if not os.path.isfile(src) or os.path.isfile(dst) or os.access(src, os.W_OK):
        return
    try:
        os.makedirs(data, exist_ok=True)
        part = dst + ".part"
        shutil.copyfile(src, part)
        os.replace(part, dst)
    except OSError:
        pass  # no writable folder either: the shipped copy still works, it just stays as it is


def found_binary():
    """An yt-dlp program already in place, or None: the data folder's, the plugin's, then PATH."""
    seed_bundled()
    for d in (data_bin(), own_bin()):
        if d:
            p = os.path.join(d, BINARY)
            if os.path.isfile(p):
                return p
    return shutil.which("yt-dlp")


def fetch_binary():
    """Downloads the official build into a `bin` we can write (the data folder first, else next to the script)."""
    url = RELEASE.get(SETTINGS.get("channel") or "stable", RELEASE["stable"]).format(name=BINARY)
    where = None
    for d in (data_bin(), own_bin()):
        if not d:
            continue
        try:
            os.makedirs(d, exist_ok=True)  # a search carries no context: its `bin` may not exist yet
            where = d
            break
        except OSError:
            continue
    if not where:
        raise Failure(f"no hay carpeta donde guardar yt-dlp (descárgalo a mano a {own_bin()})")
    dest = os.path.join(where, BINARY)
    part = dest + ".part"
    try:
        with urlopen(url, timeout=FETCH_TIMEOUT) as src, open(part, "wb") as out:
            shutil.copyfileobj(src, out)
        os.replace(part, dest)  # it only appears under its name once it is complete
        return dest
    except OSError as e:
        try:
            os.remove(part)
        except OSError:
            pass
        raise Failure(f"no se pudo descargar yt-dlp de {url}: {e} "
                      f"(descárgalo a mano a {where})")


def ytdlp():
    return found_binary() or fetch_binary()


def results():
    return max(1, min(500, int(SETTINGS.get("results") or 25)))


def auto():
    """The `auto_update` setting, whatever the JSON type it arrived as."""
    return SETTINGS.get("auto_update") in (True, 1, "true", "1", "yes")


def convert_args(download=False):
    """yt-dlp arguments that produce the chosen format, or pick a playable one without ffmpeg."""
    fmt = SETTINGS.get("format", "m4a")
    if fmt == "mp4 (video)":
        if not shutil.which("ffmpeg"):
            raise Failure("el formato mp4 (video) necesita ffmpeg (fusiona el vídeo y el audio)")
        res = (SETTINGS.get("maxres") or "1080").strip() or "1080"
        # H.264 + AAC: the streams every player and the app's VIDEO view decode. The best VP9/Opus streams merged into
        # an .mp4 look like "audio only" (no picture) in most players, so they are the fallback, never the choice.
        # The metadata goes into the file: the app reads the title and the artist from it (the file is named after the
        # id, so without the tags the browser would only show the id).
        return ["-f", f"bv*[vcodec^=avc1][height<={res}]+ba[acodec^=mp4a]/b[ext=mp4]/bv*+ba/b",
                "-S", f"res:{res},vcodec:h264,acodec:aac",
                "--merge-output-format", "mp4",
                "--embed-metadata"]
    if fmt == "best" or not shutil.which("ffmpeg"):
        return ["-f", PLAYABLE]
    quality = [] if download else ["--audio-quality", "192K"]
    # The metadata goes into the file (see above): the title and the artist are read from it.
    return ["-f", "bestaudio/best", "-x", "--audio-format", fmt, *quality,
            "--embed-metadata"]


# --- self-maintenance ------------------------------------------------------


def stamp():
    """A writable place for the update stamp: the data folder when known, else next to this script."""
    for d in _stamp_candidates():
        if not d or not os.path.isdir(d):
            continue
        p = os.path.join(d, "update.stamp")
        try:
            with open(p, "a"):
                pass
        except OSError:
            continue
        return p
    return None


def _stamp_candidates():
    out = []
    if DIR:
        out.append(os.path.dirname(DIR))  # <data>/plugins/<plugin>
        out.append(DIR)
    out.append(HERE)
    return out


def stale(hours):
    p = stamp()
    if not p:  # nowhere writable: the self-repair still works on failure
        return False
    try:
        return time.time() - os.path.getmtime(p) > hours * 3600
    except OSError:
        return True


def update_ytdlp():
    """`yt-dlp -U`; True when a newer build (or the current one) is in place."""
    try:
        done = run_raw([ytdlp(), "-U", "--update-to",
                        SETTINGS.get("channel") or "stable"], UPDATE_TIMEOUT)
    except Failure:
        return False
    text = ((done.stdout or "") + (done.stderr or "")).lower()
    ok = done.returncode == 0 or "up to date" in text or "updated" in text
    if ok:
        marked = stamp()
        if marked:
            try:
                os.utime(marked)
            except OSError:
                pass
    return ok


def extractor_bug(text):
    low = text.lower()
    return any(p.lower() in low for p in EXTRACTOR_BUGS)


def attempt(fn, *args, **kw):
    """Run one op; on an extractor-signature failure update yt-dlp and retry once."""
    try:
        return fn(*args, **kw)
    except Failure as e:
        if not extractor_bug(str(e)) or not auto():
            raise
        if not stale(6):  # repaired recently; a fresh yt-dlp did not help
            raise
        if not update_ytdlp():
            raise Failure(f"{e} (actualizar yt-dlp no lo arregló)")
        return fn(*args, **kw)


def maybe_update():
    """The daily check, on any request while the stamp is old."""
    if auto() and stale(24):
        try:
            update_ytdlp()
        except Failure:
            pass


# --- yt-dlp plumbing -------------------------------------------------------


def js_runtime():
    """`--js-runtimes` for the JavaScript runtime found on PATH. YouTube's player challenge needs one: without it
    yt-dlp warns that formats go missing and YouTube answers "sign in to confirm you're not a bot". Deno is yt-dlp's
    own default, so when it is there nothing is added."""
    if shutil.which("deno"):
        return []
    for name in ("node", "bun"):
        if shutil.which(name):
            return ["--js-runtimes", name]
    return []


def run_raw(args, timeout):
    # Global options go right after the program, so every call gets them without repeating them (also `-U`).
    args = [*args[:1], *js_runtime(), *args[1:]]
    # `errors="replace"`: ffmpeg writes its messages in the console code page, and a title with a curly quote would
    # make the reader thread raise UnicodeDecodeError (the download then never finishes).
    try:
        return subprocess.run(args, capture_output=True, text=True, encoding="utf-8", errors="replace",
                              timeout=timeout, creationflags=NO_WINDOW)
    except subprocess.TimeoutExpired:
        raise Failure(f"yt-dlp no respondió en {timeout} s")


def last_line(text):
    lines = [l for l in (text or "").strip().splitlines() if l.strip()]
    return lines[-1] if lines else "sin mensaje"


def explain(text):
    """A short message for the answers YouTube gives without a signed-in session: the raw line is long and says
    nothing about what to do."""
    if "Sign in to confirm" in text:
        return ("YouTube pide iniciar sesión desde esta red (protección contra bots) y no es un fallo del plugin: "
                "espera unos minutos y reinténtalo, o abre youtube.com en el navegador e inicia sesión")
    return text


def entries(target):
    """The entries yt-dlp lists for a search or a playlist address, without resolving each one."""
    done = run_raw([ytdlp(), "--no-warnings", "--flat-playlist", "--playlist-end",
                    str(results()), "--dump-json", "--ignore-errors", target],
                   SEARCH_TIMEOUT)
    rows = []
    for line in done.stdout.splitlines():
        try:
            rows.append(json.loads(line))
        except ValueError:
            pass
    if not rows and done.returncode != 0:
        raise Failure(last_line(done.stderr))
    return rows


def track_id(source, e):
    if source == "youtube":
        return e.get("id")
    page = e.get("webpage_url") or e.get("url") or ""
    return urlparse(page).path.strip("/") if "soundcloud.com" in page else None


def track(source, e):
    ident = track_id(source, e)
    if not ident or not e.get("title"):
        return None
    title = e["title"]
    artist = (e.get("uploader") or e.get("channel") or "").removesuffix(" - Topic")
    if source == "soundcloud":
        artist = artist or ident.split("/")[0]  # flat playlist entries have no uploader: the user's name in the address
    elif " - " in title:  # "Artist - Title" is how music is named on YouTube
        artist, title = title.split(" - ", 1)
    out = {"id": ident, "title": title, "artist": artist}
    if e.get("duration"):
        out["length"] = e["duration"]
    if e.get("genres"):
        out["genre"] = e["genres"][0]
    if e.get("webpage_url") or e.get("url"):
        out["url"] = e.get("webpage_url") or e["url"]
    return out


def tracks(source, target):
    return {"tracks": [t for t in (track(source, e) for e in entries(target)) if t]}


def playlists(source):
    try:
        with open(os.path.join(HERE, "playlists.json"), encoding="utf-8") as f:
            return {"playlists": json.load(f).get(source, [])}
    except FileNotFoundError:
        return {"playlists": []}


def search(source, query):
    return tracks(source, SOURCES[source]["search"].format(n=results(), q=query))


def info(source, ids):
    """The title and the artist of these tracks, by their id: the file of a download is named after the id, so a file
    downloaded before the plugin wrote its tags shows the id and nothing else. yt-dlp answers without downloading."""
    if not ids:
        return {"tracks": []}
    urls = [SOURCES[source]["page"].format(id=i) for i in ids]
    done = run_raw([ytdlp(), "--no-warnings", "--skip-download", "--ignore-errors",
                    "--print", "%(id)s\t%(title)s\t%(uploader)s", *urls], FETCH_TIMEOUT)
    out = []
    for line in done.stdout.splitlines():
        parts = line.split("\t")
        if len(parts) < 2 or not parts[1].strip():
            continue
        title = parts[1].strip()
        artist = parts[2].strip() if len(parts) > 2 else ""
        if " - " in title and not artist:  # "Artist - Title" is how music is named on YouTube
            artist, title = title.split(" - ", 1)
        out.append({"id": parts[0].strip(), "title": title, "artist": artist})
    return {"tracks": out}


def clean_parts(dest):
    """Removes the work folders of downloads that never finished (the app was killed while yt-dlp ran): they are
    hidden and hold a full video. Only an hour old or more, so a download running right now is left alone."""
    try:
        old_names = os.listdir(dest)
    except OSError:
        return  # the folder may not exist yet (the first download)
    for old in old_names:
        if not old.startswith(".part-"):
            continue
        path = os.path.join(dest, old)
        try:
            if time.time() - os.path.getmtime(path) > 3600:
                shutil.rmtree(path, ignore_errors=True)
        except OSError:
            pass


def fetch(source, ident, dest, name):
    """Downloads the track into `dest` as `<name>.<ext>`; the file appears there only when it is complete."""
    clean_parts(dest)
    work = tempfile.mkdtemp(prefix=f".part-{name}-", dir=dest)
    try:
        convert = convert_args()
        done = run_raw(
            [ytdlp(), "--no-playlist", "--no-progress", *convert,
             "-o", os.path.join(work, name + ".%(ext)s"), SOURCES[source]["page"].format(id=ident)],
            FETCH_TIMEOUT,
        )
        made = [f for f in os.listdir(work) if f.startswith(name + ".") and not f.endswith((".part", ".ytdl"))]
        if done.returncode != 0 or not made:
            hint = " (sin ffmpeg solo se pueden bajar streams m4a y mp3)" if "-x" not in convert else ""
            raise Failure(explain(last_line(done.stderr)) + hint)
        os.replace(os.path.join(work, made[0]), os.path.join(dest, made[0]))
        return {"file": made[0]}
    finally:
        shutil.rmtree(work, ignore_errors=True)


def download(address, folder):
    """The action: downloads a track or playlist into `folder` and has the app import it."""
    if not folder:
        raise Failure("la app no dio carpeta para descargar")
    os.makedirs(folder, exist_ok=True)
    convert = convert_args(download=True)
    done = run_raw(
        [ytdlp(), "--no-progress", "--yes-playlist", *convert,
         "-o", os.path.join(folder, "%(uploader)s - %(title)s.%(ext)s"),
         "--print", "after_move:filepath", address],
        ACTION_TIMEOUT,
    )
    files = [l for l in done.stdout.splitlines() if os.path.isfile(l)]
    if not files:
        raise Failure(explain(last_line(done.stderr)))
    return {"commands": [
        {"cmd": "import", "files": files},
        {"cmd": "toast",
         "text": f"yt-dlp: {len(files)} pista{'' if len(files) == 1 else 's'} añadida{'' if len(files) == 1 else 's'} a la colección"},
    ]}


def action_update():
    """Manual update from the Plugins menu, with the result in a toast."""
    before = run_raw([ytdlp(), "--version"], SEARCH_TIMEOUT).stdout.strip()
    ok = update_ytdlp()
    if ok:
        text = f"yt-dlp: {before}; contra el canal {SETTINGS.get('channel') or 'stable'}: está al día"
    else:
        text = (f"yt-dlp: la actualización falló, {before or '?'} se queda "
                "(un yt-dlp gestionado por otra herramienta, como pip, se actualiza desde ahí)")
    return {"commands": [{"cmd": "toast", "text": text}]}


# --- the protocol ----------------------------------------------------------


def handle(req):
    global DIR
    op = req.get("op")
    SETTINGS.update(req.get("settings") or {})
    context = req.get("context") or {}
    DIR = context.get("dir")
    maybe_update()
    if op == "action":
        if req.get("action") == "update":
            return action_update()
        if req.get("action") != "download":
            raise Failure(f"acción desconocida {req.get('action')!r}")
        if not (req.get("input") or "").strip():
            raise Failure("no hay dirección")
        where = DIR and os.path.join(DIR, "downloads")
        return attempt(download, req["input"].strip(), where)
    source = req.get("source")
    if source not in SOURCES:
        raise Failure(f"fuente desconocida {source!r}")
    if op == "playlists":
        return attempt(playlists, source)
    if op == "tracks":
        return attempt(tracks, source, req["playlist"])
    if op == "search":
        return attempt(search, source, req["query"])
    if op == "info":
        return attempt(info, source, req.get("ids") or [])
    if op == "fetch":
        DIR = os.path.dirname(req["dir"])
        return attempt(fetch, source, req["id"], req["dir"], req["name"])
    raise Failure(f"operación desconocida {op!r}")


if __name__ == "__main__":
    try:
        reply = handle(json.load(sys.stdin))
    except Failure as e:
        reply = {"error": str(e)}
    except KeyError as e:
        reply = {"error": f"missing {e}"}
    except Exception as e:
        # A plugin that dies leaves the app waiting for a reply that never comes: an unexpected error is answered like
        # any other, so the deck says what went wrong instead of "the plugin process exited".
        reply = {"error": f"{type(e).__name__}: {e}"}
    json.dump(reply, sys.stdout)
