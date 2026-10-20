#!/usr/bin/env python3
"""OpenFadeR plugin: Deezer through the deemix CLI (protocol: docs/plugins.md).

One request as JSON on stdin, one reply as JSON on stdout. Needs python3 and the
deemix CLI (https://github.com/bambanah/deemix); downloading needs an ARL (the
user's own Deezer session cookie), which the plugin keeps in its data folder
(`<data>/plugins/deezer/config/.arl`, written from the setting) and hands to
deemix through `--portable`. Search uses Deezer's public API and needs no ARL.

Track ids: the Deezer track id (`3135556`), also handed back by `fetch`.
Albums and playlists go through the Download action (the source lists tracks).
"""

import base64
import json
import os
import platform
import shutil
import subprocess
import sys
import tempfile
import urllib.parse
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
# The plugin's settings (plugin manager), set from each request. `command`
# defaults to the CLI on PATH; quality: FLAC while the space allows it.
SETTINGS = {"command": "deemix", "arl": "", "quality": "FLAC", "results": 25,
            "folder": ""}

# Formats the decks play, what `fetch` may deliver.
AUDIO = (".flac", ".mp3", ".m4a", ".wav")
# deemix may also write the cover, a log and subtitles next to the audio.
IGNORE = (".jpg", ".jpeg", ".png", ".log", ".lrc", ".txt")

# The CLI's own timeouts stay below the host's 900 s for `fetch` (fetch.rs) and
# below whatever the action takes: the work belongs to the download, not to a
# stuck program.
SEARCH_TIMEOUT = 30
FETCH_TIMEOUT = 600
ACTION_TIMEOUT = 1800

# The app starts us with no console, and deemix is a `.cmd` shim that runs node: started detached, node would get
# no console at all and Windows would create a brand-new one for it (a terminal window on every download). A hidden
# console (`CREATE_NO_WINDOW`) is inherited by the whole chain instead, so nothing is ever shown. The flag is zero on
# the other systems.
NO_WINDOW = getattr(subprocess, "CREATE_NO_WINDOW", 0)
# PowerShell shows its dialogs only while it has a console: a fully detached one (`DETACHED_PROCESS`) runs the dialog
# code and exits at once without showing anything, so it keeps a hidden console just the same.
PS_FLAGS = getattr(subprocess, "CREATE_NO_WINDOW", 0)


class Failure(Exception):
    """Sent back as {"error": …}."""


def settings_dict():
    return {k: SETTINGS.get(k) for k in ("command", "arl", "quality", "results", "folder")}


def bitrate():
    """The CLI's `-b` values (packages/cli/src/bitrate.ts)."""
    q = (SETTINGS.get("quality") or "FLAC").lower()
    return "flac" if q.startswith("flac") else ("320" if "320" in q else "128")


def command():
    cmd = (SETTINGS.get("command") or "deemix").strip() or "deemix"
    if os.sep in cmd and os.path.isfile(cmd):
        return cmd
    found = shutil.which(cmd)
    if not found:
        raise Failure(
            "no se encontró el CLI de deemix (pista: instálalo desde "
            "https://github.com/bambanah/deemix y ajusta el ajuste Command)"
        )
    return found


def run(args, timeout, cwd=None):
    try:
        done = subprocess.run(
            args, capture_output=True, text=True, timeout=timeout, cwd=cwd,
            creationflags=NO_WINDOW,
        )
    except FileNotFoundError:
        raise Failure("no se encontró el CLI de deemix; ajusta el ajuste Command")
    except subprocess.TimeoutExpired:
        raise Failure(f"el CLI de deemix no respondió en {timeout} s")
    return done


def http_json(url):
    """Public Deezer API (`api.deezer.com`): no ARL involved."""
    req = urllib.request.Request(url, headers={"User-Agent": "openfader-deezer/0.1"})
    try:
        with urllib.request.urlopen(req, timeout=SEARCH_TIMEOUT) as r:
            return json.loads(r.read().decode("utf-8", "replace"))
    except OSError as e:
        raise Failure(f"Deezer inalcanzable: {e}")


def rows(payload):
    """Search rows from the public API."""
    data = payload.get("data")
    if not isinstance(data, list):
        msg = (payload.get("error") or {}).get("message") or "sin datos"
        raise Failure(f"Deezer: {msg}")
    return data


def track(row):
    ident = row.get("id")
    if not ident or not row.get("title"):
        return None
    album = row.get("album") or {}
    out = {
        "id": str(ident),
        "title": row["title"],
        "artist": ((row.get("artist") or {}).get("name") or "").strip(),
        "url": "https://www.deezer.com/track/%s" % ident,
    }
    if album.get("title"):
        out["album"] = album["title"]
    if row.get("duration"):
        out["length"] = row["duration"]
    return out


def search(query):
    limit = max(1, min(100, int(SETTINGS.get("results") or 25)))
    url = "https://api.deezer.com/search?q=%s&limit=%d" % (
        urllib.parse.quote(query), limit
    )
    return {"tracks": [t for t in (track(r) for r in rows(http_json(url))) if t]}


# --- the ARL ---------------------------------------------------------------


def config_dir(dir_of_plugin):
    """`--portable` puts the config in `<cwd>/config`: the plugin's data folder."""
    return os.path.join(dir_of_plugin, "config")


def arl_file(dir_of_plugin):
    """Where the ARL lives: `<data>/plugins/deezer/config/.arl`, mode 0600, never shown again."""
    return os.path.join(config_dir(dir_of_plugin), ".arl")


def download_dir(dir_of_plugin):
    """The action's destination: the `folder` setting here (~ and env vars allowed)
    or `<data>/plugins/deezer/downloads`."""
    custom = (SETTINGS.get("folder") or "").strip().strip('"')
    where = (os.path.expandvars(os.path.expanduser(custom)) if custom
             else os.path.join(dir_of_plugin, "downloads"))
    where = os.path.abspath(where)
    os.makedirs(where, exist_ok=True)
    return where


def allow_bitrate_fallback(dir_of_plugin):
    """Deezer does not always serve the requested quality (a transient "no
    alternative found" even for tracks it delivered minutes earlier): allow
    deemix to fall back to the next lower bitrate instead of giving up. The
    config.json of `<config>` is the plugin's own folder, so pre-writing one
    key is safe and the rest of the user's deemix settings stay untouched."""
    at = os.path.join(config_dir(dir_of_plugin), "config.json")
    try:
        os.makedirs(os.path.dirname(at), exist_ok=True)
        with open(at, encoding="utf-8") as f:
            document = json.load(f)
        if document.get("fallbackBitrate") is True:
            return
        document["fallbackBitrate"] = True
        with open(at, "w", encoding="utf-8", newline="") as f:
            json.dump(document, f)
    except (OSError, ValueError):
        pass  # no config or unreadable: deemix keeps its defaults


def arl_written(dir_of_plugin):
    """Writes (or rewrites) `.arl` when the setting holds one; returns whether one is stored."""
    arl = (SETTINGS.get("arl") or "").strip()
    at = arl_file(dir_of_plugin)
    if arl:
        os.makedirs(os.path.dirname(at), exist_ok=True)
        with open(at, "w", encoding="utf-8", newline="") as f:
            f.write(arl)
        os.chmod(at, 0o600)
    return bool(arl) or os.path.isfile(at)


# --- downloads -------------------------------------------------------------


def audio_files(folder):
    """The audio files deemix left in `folder` (not the covers, logs or lyrics)."""
    out = []
    for root, _dirs, files in os.walk(folder):
        for f in files:
            if f.lower().endswith(AUDIO) and os.path.getsize(os.path.join(root, f)) > 0:
                out.append(os.path.join(root, f))
    return sorted(out, key=lambda p: os.path.getsize(p), reverse=True)


def logged_out(done):
    low = ((done.stderr or "") + (done.stdout or "")).lower()
    return "logged in" in low or ".arl" in low


def deemix_url(kind, ident):
    return "https://www.deezer.com/%s/%s" % (kind, ident)


def download(kind, ident, dest, name):
    """The download of one track into `dest` as `<name>.<ext>`; the file appears there only when it is complete."""
    # `dest` is the per-source folder (`<data>/plugins/deezer/deezer`): the plugin's own folder is next to it.
    parent = os.path.dirname(dest)
    if not arl_written(parent):
        raise Failure(
            "sin ARL: ajústalo en las preferencias del plugin (la búsqueda funciona sin él, las descargas no)"
        )
    allow_bitrate_fallback(parent)
    work = tempfile.mkdtemp(prefix=".deemix-", dir=dest)
    try:
        done = run(
            [command(), "--portable", "-b", bitrate(), "-p", work,
             deemix_url(kind, ident)],
            timeout=FETCH_TIMEOUT,
            cwd=os.path.dirname(dest),
        )
        made = audio_files(work)
        if not made:
            if bad_arl_text(done):
                raise Failure(
                    "el ARL no fue aceptado por Deezer: cópialo de nuevo con la sesión "
                    "iniciada en deezer.com (F12 > Application > Cookies > arl). "
                    "Si se copió sin iniciar sesión es una cookie de visitante; si hace "
                    "meses lo copiaste, expiró (deemix: %s)" % last_line(done)
                )
            if unavailable_text(done):
                raise Failure(
                    "Deezer lista la pista pero no sirve su audio ahora mismo (error "
                    "intermitente del servidor): reinténtala; con el fallback de calidad "
                    "que este plugin activa, un intento nuevo baja MP3 320 cuando FLAC "
                    "no está disponible (deemix: %s)" % last_line(done)
                )
            if done.returncode != 0 and logged_out(done):
                raise Failure(
                    "la descarga falló: el ARL falta, expiró o es de otra "
                    "cuenta (deemix: %s)" % last_line(done)
                )
            raise Failure("deemix no escribió ningún archivo de audio%s" % suffix(done))
        # One track in, one file out: keep the largest, the covers are not audio.
        ext = os.path.splitext(made[0])[1]
        final = os.path.join(dest, name + ext)
        os.replace(made[0], final)
        return {"file": os.path.basename(final)}
    finally:
        shutil.rmtree(work, ignore_errors=True)


def last_line(done):
    text = ((done.stdout or "") + (done.stderr or "")).strip()
    return text.splitlines()[-1] if text else "no message"


def bad_arl_text(done):
    """The wording deemix prints when Deezer rejected the ARL (guest or expired)."""
    return "no valid arl" in ((done.stdout or "") + (done.stderr or "")).lower()


def unavailable_text(done):
    """Deezer lists the track but reports no audio for this account right now
    (it is transient: the same request downloads minutes earlier or later)."""
    text = ((done.stdout or "") + (done.stderr or "")).lower()
    return "not available on deezer" in text and "no alternative" in text


def suffix(done):
    msg = last_line(done)
    return f": {msg}" if msg else ""


# --- the ARL dialog --------------------------------------------------------


PS_ARL_DIALOG = r"""
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$path = '%%PATH%%'
$form = New-Object System.Windows.Forms.Form
$form.Text = 'OpenFadeR . Deezer'
$form.ClientSize = New-Object System.Drawing.Size(620, 250)
$form.StartPosition = 'CenterScreen'
$form.TopMost = $true
$form.FormBorderStyle = 'FixedDialog'
$form.MaximizeBox = $false
$form.BackColor = [System.Drawing.Color]::FromArgb(17, 17, 27)
$head = New-Object System.Windows.Forms.Panel
$head.SetBounds(0, 0, 620, 64)
$head.BackColor = [System.Drawing.Color]::FromArgb(30, 30, 46)
$form.Controls.Add($head)
$title = New-Object System.Windows.Forms.Label
$title.Text = 'OpenFadeR . Deezer'
$title.Autosize = $true
$title.ForeColor = [System.Drawing.Color]::FromArgb(0, 180, 216)
$title.Font = New-Object System.Drawing.Font('Segoe UI', 15, [System.Drawing.FontStyle]::Bold)
$title.SetBounds(16, 12, 400, 30)
$head.Controls.Add($title)
$sub = New-Object System.Windows.Forms.Label
$sub.Text = 'Pega la cookie arl de tu cuenta de Deezer'
$sub.AutoSize = $true
$sub.ForeColor = [System.Drawing.Color]::FromArgb(205, 214, 244)
$sub.Font = New-Object System.Drawing.Font('Segoe UI', 10)
$sub.SetBounds(16, 40, 400, 20)
$head.Controls.Add($sub)
$help = New-Object System.Windows.Forms.Label
$help.Text = 'deezer.com > F12 > Application > Cookies > www.deezer.com > arl  (un valor largo que parece aleatorio; la app no volverá a mostrarlo)'
$help.ForeColor = [System.Drawing.Color]::FromArgb(147, 153, 178)
$help.Font = New-Object System.Drawing.Font('Segoe UI', 8.5)
$help.SetBounds(16, 66, 590, 34)
$form.Controls.Add($help)
$entry = New-Object System.Windows.Forms.TextBox
$entry.PasswordChar = [char]42
$entry.Font = New-Object System.Drawing.Font('Consolas', 10)
$entry.SetBounds(16, 112, 588, 28)
$entry.BackColor = [System.Drawing.Color]::FromArgb(30, 30, 46)
$entry.ForeColor = [System.Drawing.Color]::FromArgb(205, 214, 244)
$entry.BorderStyle = 'FixedSingle'
$form.Controls.Add($entry)
$state = New-Object System.Windows.Forms.Label
$state.ForeColor = [System.Drawing.Color]::FromArgb(243, 139, 168)
$state.Font = New-Object System.Drawing.Font('Segoe UI', 9)
$state.SetBounds(16, 150, 588, 20)
$form.Controls.Add($state)
$show = New-Object System.Windows.Forms.Button
$show.Text = 'Mostrar'
$show.SetBounds(16, 200, 90, 30)
$show.Add_Click({ if ($entry.PasswordChar) { $entry.PasswordChar = [char]0 } else { $entry.PasswordChar = [char]42 } })
$form.Controls.Add($show)
$save = New-Object System.Windows.Forms.Button
$save.Text = 'Guardar'
$save.SetBounds(434, 200, 84, 30)
$save.BackColor = [System.Drawing.Color]::FromArgb(0, 180, 216)
$save.ForeColor = [System.Drawing.Color]::FromArgb(17, 17, 27)
$save.FlatStyle = 'Flat'
$save.DialogResult = [System.Windows.Forms.DialogResult]::OK
$form.Controls.Add($save)
$cancel = New-Object System.Windows.Forms.Button
$cancel.Text = 'Cancelar'
$cancel.SetBounds(524, 200, 80, 30)
$cancel.FlatStyle = 'Flat'
$cancel.BackColor = [System.Drawing.Color]::FromArgb(49, 50, 68)
$cancel.ForeColor = [System.Drawing.Color]::FromArgb(205, 214, 244)
$cancel.DialogResult = [System.Windows.Forms.DialogResult]::Cancel
$form.Controls.Add($cancel)
$form.AcceptButton = $save
$form.CancelButton = $cancel
$entry.Add_KeyDown({ if ($_.Control -and $_.KeyCode -eq 'V') { $entry.Paste() } })
if ($form.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {
    $arl = $entry.Text.Trim()
    if ($arl.Length -gt 64) {
        $enc = New-Object System.Text.UTF8Encoding($false)
        [IO.File]::WriteAllText($path, $arl, $enc)
        exit 0
    }
    $state.Text = 'eso no parece un ARL (demasiado corto): pega el valor de la cookie'
    $form.ShowDialog() | Out-Null
}
exit 1
"""


def ps_arl_dialog(target):
    """The Windows dialog (PowerShell is always there): returns whether an ARL was stored."""
    script = PS_ARL_DIALOG.replace("%%PATH%%", target.replace("'", "''"))
    encoded = base64.b64encode(script.encode("utf-16-le")).decode("ascii")
    try:
        done = subprocess.run(
            ["powershell", "-NoProfile", "-STA", "-EncodedCommand", encoded],
            capture_output=True, timeout=86400, creationflags=PS_FLAGS,
        )
    except subprocess.TimeoutExpired:
        raise Failure("la ventana del ARL estuvo abierta más de un día")
    # console output may use the machine's ANSI code page: never decode strictly
    _ = done.stdout.decode("utf-8", errors="replace")
    _ = done.stderr.decode("utf-8", errors="replace")
    return done.returncode == 0


def tk_arl_dialog(target):
    """The same dialog without a native toolkit (macOS/Linux stock python: tkinter)."""
    try:
        import tkinter as tk
    except Exception as e:
        raise Failure(f"aquí no hay toolkit gráfico ({e}); pega el ARL en las preferencias del plugin")
    root = tk.Tk(); root.title("OpenFadeR . Deezer"); root.attributes("-topmost", True)
    tk.Label(root, text="Pega la cookie arl de tu cuenta de Deezer", padx=14).pack(anchor="w")
    var = tk.StringVar()
    entry = tk.Entry(root, textvariable=var, show="*", width=72); entry.pack(padx=14, pady=8)
    status = tk.Label(root, text="", fg="#f38ba8"); status.pack(anchor="w", padx=14)
    saved = {"ok": False}
    def save():
        arl = var.get().strip()
        if len(arl) <= 64:
            status.config(text="eso no parece un ARL (demasiado corto)")
            return
        os.makedirs(os.path.dirname(target), exist_ok=True)
        with open(target, "w", encoding="utf-8", newline="") as f:
            f.write(arl)
        os.chmod(target, 0o600)
        saved["ok"] = True
        root.destroy()
    buttons = tk.Frame(root); buttons.pack(pady=(0, 12))
    tk.Button(buttons, text="Guardar", command=save).pack(side="left", padx=6)
    tk.Button(buttons, text="Cancelar", command=root.destroy).pack(side="left")
    entry.focus_set(); entry.bind("<Return>", lambda _e: save())
    root.mainloop()
    return saved["ok"]


def action_arl(folder):
    """Paste your ARL: opens the dialog, saves it where deemix reads it, tells the user."""
    if not folder:
        raise Failure("la app no dio carpeta a este plugin")
    target = arl_file(folder)
    ok = ps_arl_dialog(target) if platform.system() == "Windows" else tk_arl_dialog(target)
    if not ok:
        raise Failure("no se guardó ningún ARL (la búsqueda funciona sin él)")
    return {"commands": [{"cmd": "toast", "text": "deezer: ARL guardado (descargas activadas)"}]}


# --- actions ---------------------------------------------------------------


def action_download(address, folder):
    """Downloads the address into the plugin's folder and has the app import it."""
    if not folder:
        raise Failure("la app no dio carpeta para descargar")
    where = download_dir(folder)
    if not arl_written(folder):
        raise Failure("sin ARL: ajústalo en las preferencias del plugin")
    allow_bitrate_fallback(folder)
    work = tempfile.mkdtemp(prefix=".deemix-", dir=where)
    try:
        done = run(
            [command(), "--portable", "-b", bitrate(), "-p", work, address],
            timeout=ACTION_TIMEOUT,
            cwd=folder,
        )
        made = audio_files(work)
        if not made:
            if bad_arl_text(done):
                raise Failure(
                    "el ARL no fue aceptado por Deezer: cópialo de nuevo con la sesión "
                    "iniciada en deezer.com (F12 > Application > Cookies > arl). "
                    "Si se copió sin iniciar sesión es una cookie de visitante; si hace "
                    "meses lo copiaste, expiró (deemix: %s)" % last_line(done)
                )
            if unavailable_text(done):
                raise Failure(
                    "Deezer lista la pista pero no sirve su audio ahora mismo (error "
                    "intermitente del servidor): reinténtala; con el fallback de calidad "
                    "que este plugin activa, un intento nuevo baja MP3 320 cuando FLAC "
                    "no está disponible (deemix: %s)" % last_line(done)
                )
            raise Failure(f"deemix no escribió ningún archivo de audio{suffix(done)}")
        files = []
        for f in made:
            final = os.path.join(where, os.path.basename(f))
            os.replace(f, final)
            files.append(final)
        return {"commands": [
            {"cmd": "import", "files": files},
            {"cmd": "toast",
             "text": f"deezer: {len(files)} archivo{'' if len(files) == 1 else 's'} añadido{'' if len(files) == 1 else 's'} a la colección"},
        ]}
    finally:
        shutil.rmtree(work, ignore_errors=True)


def action_check(folder):
    """Is the CLI there? Is an ARL stored? One quick answer."""
    try:
        cmd = command()
        done = run([cmd, "--version"], timeout=SEARCH_TIMEOUT)
        version = (done.stdout or "").strip().splitlines()[-1] if done.stdout else "?"
    except Failure as e:
        raise Failure(str(e))
    arl_state = "sin ARL (descargas desactivadas)" if not (
        (SETTINGS.get("arl") or "").strip()
        or (folder and os.path.isfile(arl_file(folder)))
    ) else "ARL almacenado"
    return {"commands": [{"cmd": "toast",
                          "text": f"deezer: deemix {version} ({cmd}); {arl_state}"}]}


# --- the protocol ----------------------------------------------------------


def handle(req):
    op = req.get("op")
    SETTINGS.update(req.get("settings") or {})
    if op == "action":
        folder = (req.get("context") or {}).get("dir")
        if not folder:
            raise Failure("la app no dio carpeta a este plugin")
        if req.get("action") == "arl":
            return action_arl(folder)
        if req.get("action") == "check":
            return action_check(folder)
        if req.get("action") != "download":
            raise Failure(f"acción desconocida {req.get('action')!r}")
        if not (req.get("input") or "").strip():
            raise Failure("no hay dirección")
        return action_download(req["input"].strip(), folder)
    if req.get("source") != "deezer":
        raise Failure(f"fuente desconocida {req.get('source')!r}")
    if op == "playlists":
        return {"playlists": []}  # search only: an ARL-API listing needs the login engine
    if op == "tracks":
        raise Failure("esta fuente solo lista resultados de búsqueda")
    if op == "search":
        if not (req.get("query") or "").strip():
            return {"tracks": []}
        return search(req["query"])
    if op == "fetch":
        dir_of = req["dir"]
        ident = str(req["id"])
        return download("track", ident, dir_of, req["name"])
    raise Failure(f"operación desconocida {op!r}")


if __name__ == "__main__":
    try:
        reply = handle(json.load(sys.stdin))
    except (Failure, KeyError, TypeError) as e:
        reply = {"error": f"missing {e}" if isinstance(e, KeyError) else str(e)}
    json.dump(reply, sys.stdout)
