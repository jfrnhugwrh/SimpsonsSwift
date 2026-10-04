//! A tiny HTTP server (std only) that shows the emulator's framebuffer live and
//! lets the user drop their own `.ipa` in through the browser.
//!
//! `/` serves a page that polls `/frame.bmp`, `/log` returns the guest's log,
//! `/stats` returns a one-line summary, `/games` lists the game library and
//! `POST /import` takes an uploaded archive.  The server binds `0.0.0.0` so the
//! sandbox's preview proxy can reach it.
//!
//! Uploading goes through the same [`ipa::import_bytes`] the CLI uses, so the
//! validation is identical: a corrupt archive, an arm64-only build, a
//! FairPlay-encrypted App Store binary or some other iOS app all come back as a
//! readable error instead of a half-extracted bundle.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub type Frames = Arc<Mutex<Vec<u8>>>;
pub type Logs = Arc<Mutex<Vec<String>>>;

/// How much of a request header we are willing to buffer before giving up.
const MAX_HEADER: usize = 64 * 1024;

const PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Simpsons Arcade — emulator preview</title>
<style>
  :root { color-scheme: dark; }
  body { margin: 0; background: #10121a; color: #e6e8f0; font: 14px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace; }
  header { padding: 12px 16px; border-bottom: 1px solid #262a36; display: flex; gap: 16px; align-items: baseline; }
  h1 { font-size: 15px; margin: 0; font-weight: 600; }
  h2 { font-size: 13px; margin: 0 0 8px; font-weight: 600; color: #b9bfd4; }
  main { display: grid; grid-template-columns: minmax(320px, 480px) 1fr; gap: 16px; padding: 16px; }
  img { width: 100%; image-rendering: pixelated; background: #000; border: 1px solid #262a36; }
  pre { margin: 0; padding: 12px; background: #161923; border: 1px solid #262a36; overflow: auto; white-space: pre-wrap; word-break: break-word; }
  pre#log { height: 52vh; }
  .muted { color: #8b90a3; }
  section { background: #141722; border: 1px solid #262a36; padding: 12px; margin-bottom: 16px; }
  input[type=file] { font: inherit; color: #e6e8f0; max-width: 100%; }
  button { font: inherit; background: #2b3245; color: #e6e8f0; border: 1px solid #3b4360; padding: 4px 12px; cursor: pointer; }
  button:disabled { opacity: .45; cursor: default; }
  .error { color: #ff9c9c; }
  .ok { color: #9ce8a8; }
</style>
</head>
<body>
<header>
  <h1>Simpsons Arcade — live framebuffer</h1>
  <span class="muted" id="stats">connecting…</span>
</header>
<main>
  <div>
    <img id="frame" alt="framebuffer">
    <section>
      <h2>Import an .ipa</h2>
      <p class="muted">The emulator ships no game. Upload a decrypted copy of
      <em>The Simpsons Arcade</em> v1.1.43 that you obtained yourself; it is
      validated and extracted into the game library, never uploaded anywhere.</p>
      <input type="file" id="ipa" accept=".ipa,application/octet-stream">
      <button id="import" disabled>Import</button>
      <pre id="import-status">no file selected</pre>
    </section>
    <section>
      <h2>Game library</h2>
      <pre id="games">…</pre>
    </section>
  </div>
  <pre id="log">waiting for the guest…</pre>
</main>
<script>
let seq = 0;
async function tick() {
  try {
    const r = await fetch('/frame.bmp?t=' + Date.now(), {cache: 'no-store'});
    if (r.ok) {
      const blob = await r.blob();
      if (blob.size > 54) {
        const url = URL.createObjectURL(blob);
        document.getElementById('frame').src = url;
        setTimeout(() => URL.revokeObjectURL(url), 1000);
      }
    }
    const s = await fetch('/stats', {cache: 'no-store'});
    document.getElementById('stats').textContent = await s.text();
    const l = await fetch('/log', {cache: 'no-store'});
    document.getElementById('log').textContent = await l.text();
  } catch (e) {
    document.getElementById('stats').textContent = 'waiting for the emulator…';
  }
}

const picker = document.getElementById('ipa');
const status = document.getElementById('import-status');
const button = document.getElementById('import');
picker.addEventListener('change', () => {
  button.disabled = picker.files.length === 0;
  if (picker.files.length) {
    const file = picker.files[0];
    status.className = '';
    status.textContent = file.name + ' — ' + file.size + ' bytes, ready to import';
  }
});
button.addEventListener('click', async () => {
  const file = picker.files[0];
  if (!file) return;
  button.disabled = true;
  status.className = '';
  status.textContent = 'uploading ' + file.name + '…';
  try {
    const response = await fetch('/import', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/octet-stream',
        'X-Ipa-Name': encodeURIComponent(file.name)
      },
      body: file
    });
    const text = await response.text();
    let shown = text;
    try { shown = JSON.stringify(JSON.parse(text), null, 2); } catch (e) {}
    status.className = response.ok ? 'ok' : 'error';
    status.textContent = shown;
    loadGames();
  } catch (e) {
    status.className = 'error';
    status.textContent = 'upload failed: ' + e;
  }
  button.disabled = false;
});

async function loadGames() {
  try {
    const response = await fetch('/games', {cache: 'no-store'});
    document.getElementById('games').textContent = await response.text();
  } catch (e) {
    document.getElementById('games').textContent = 'game library unavailable';
  }
}

setInterval(tick, 500);
tick();
loadGames();
</script>
</body>
</html>
"#;

/// Start the preview server on its own thread.
pub fn start(port: u16, frames: Frames, logs: Logs, games: PathBuf) -> Result<(), String> {
    let listener = TcpListener::bind(("0.0.0.0", port)).map_err(|e| format!("bind 0.0.0.0:{port}: {e}"))?;
    std::thread::spawn(move || run(listener, frames, logs, games));
    Ok(())
}

pub fn run(listener: TcpListener, frames: Frames, logs: Logs, games: PathBuf) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                // One thread per connection so a multi-megabyte upload does not
                // stall the framebuffer polling.
                let frames = Arc::clone(&frames);
                let logs = Arc::clone(&logs);
                let games = games.clone();
                std::thread::spawn(move || {
                    let mut stream = stream;
                    let _ = handle(&mut stream, &frames, &logs, games.as_path());
                });
            }
            Err(_) => continue,
        }
    }
}

fn handle(stream: &mut TcpStream, frames: &Frames, logs: &Logs, games: &Path) -> std::io::Result<()> {
    let mut head: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 8192];
    let header_end = loop {
        if let Some(at) = head.windows(4).position(|window| window == b"\r\n\r\n") {
            break at + 4;
        }
        if head.len() > MAX_HEADER {
            break head.len();
        }
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break head.len();
        }
        head.extend_from_slice(&buffer[..read]);
    };
    let request = String::from_utf8_lossy(&head).into_owned();
    let mut lines = request.lines();
    let mut request_line = lines.next().unwrap_or("").split_whitespace();
    let method = request_line.next().unwrap_or("GET").to_string();
    let path = request_line
        .next()
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();
    let header = |name: &str| -> Option<String> {
        request.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name).then(|| value.trim().to_string())
        })
    };
    let content_length: usize = header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);

    let mut body = head[header_end..].to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&buffer[..read]);
    }
    body.truncate(content_length);

    match (method.as_str(), path.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => respond(stream, "text/html; charset=utf-8", PAGE.as_bytes()),
        ("GET", "/frame.bmp") => {
            let frame = frames.lock().map(|f| f.clone()).unwrap_or_default();
            respond(stream, "image/bmp", &frame)
        }
        ("GET", "/log") => {
            let log = logs
                .lock()
                .map(|l| l.iter().rev().take(400).cloned().collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"))
                .unwrap_or_default();
            respond(stream, "text/plain; charset=utf-8", log.as_bytes())
        }
        ("GET", "/stats") => {
            let size = frames.lock().map(|f| f.len()).unwrap_or(0);
            let text = if size > 54 {
                format!("frame {size} bytes  (polling 2 Hz)")
            } else {
                "no frame yet".to_string()
            };
            respond(stream, "text/plain; charset=utf-8", text.as_bytes())
        }
        ("GET", "/games") => {
            let listing = game_listing(games);
            respond(stream, "text/plain; charset=utf-8", listing.as_bytes())
        }
        ("POST", "/import") => {
            let name = header("x-ipa-name").map(|v| percent_decode(&v)).unwrap_or_else(|| "upload.ipa".to_string());
            let (code, status, payload) = import(&body, &name, games);
            if let Ok(mut log) = logs.lock() {
                log.push(format!("[import] {name}: {status}"));
            }
            respond_status(stream, code, &status, "application/json; charset=utf-8", payload.as_bytes())
        }
        _ => {
            let body = b"not found";
            let header = format!(
                "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(header.as_bytes())?;
            stream.write_all(body)?;
            Ok(())
        }
    }
}

/// Validate and extract an uploaded archive, returning an HTTP status and a JSON
/// body describing what happened.
fn import(data: &[u8], name: &str, games: &Path) -> (u16, String, String) {
    use ipa::json::Json;
    if data.is_empty() {
        return (400, "400 Bad Request".to_string(), error_json("the upload was empty", "empty"));
    }
    if data.len() as u64 > ipa::MAX_IPA_BYTES {
        return (
            413,
            "413 Payload Too Large".to_string(),
            error_json(
                &format!("{} bytes is larger than the {}-byte limit", data.len(), ipa::MAX_IPA_BYTES),
                "too-large",
            ),
        );
    }
    // Reusing an existing import is the friendly behaviour for a button: the
    // panel reports `reused: true` rather than failing.  The CLI keeps the
    // stricter "refuse unless --force" rule, where overwriting is a surprise.
    let options = ipa::ImportOptions { root: Some(games.to_path_buf()), reuse: true, ..Default::default() };
    match ipa::import_bytes(data, name, &options) {
        Ok(report) => {
            let mut payload = String::new();
            Json::Object(vec![
                ("ok".to_string(), Json::Bool(true)),
                ("title".to_string(), Json::str(report.game.label())),
                (
                    "bundle_id".to_string(),
                    report.bundle.info.bundle_id.clone().map(Json::Str).unwrap_or(Json::Null),
                ),
                ("version".to_string(), report.bundle.version().map(Json::Str).unwrap_or(Json::Null)),
                (
                    "architectures".to_string(),
                    Json::Array(report.bundle.architectures.iter().map(|a| Json::str(a.name.clone())).collect()),
                ),
                ("summary".to_string(), Json::str(report.bundle.summary())),
                ("files".to_string(), Json::number(report.report.files as f64)),
                ("bytes".to_string(), Json::number(report.report.bytes as f64)),
                ("reused".to_string(), Json::Bool(report.reused)),
                ("library".to_string(), Json::str(report.game.dir.display().to_string())),
                ("executable".to_string(), Json::str(report.game.executable().display().to_string())),
                (
                    "next".to_string(),
                    Json::str(format!(
                        "simpsons-emu run {} --bundle {} --serve 8080",
                        report.game.executable().display(),
                        report.game.bundle_dir().display()
                    )),
                ),
            ])
            .write(&mut payload);
            (200, "200 OK".to_string(), payload)
        }
        Err(error) => {
            let code = match error_kind(&error) {
                "already-imported" => 409,
                "too-large" => 413,
                _ => 400,
            };
            let status = match code {
                409 => "409 Conflict",
                413 => "413 Payload Too Large",
                _ => "400 Bad Request",
            };
            (code, status.to_string(), error_json(&error.to_string(), error_kind(&error)))
        }
    }
}

fn error_json(message: &str, kind: &str) -> String {
    let mut out = String::new();
    ipa::json::Json::Object(vec![
        ("ok".to_string(), ipa::json::Json::Bool(false)),
        ("error".to_string(), ipa::json::Json::str(message)),
        ("kind".to_string(), ipa::json::Json::str(kind)),
    ])
    .write(&mut out);
    out
}

/// A short machine-readable tag for the import panel.
fn error_kind(error: &ipa::IpaError) -> &'static str {
    match error {
        ipa::IpaError::NotAZip(_) => "not-an-ipa",
        ipa::IpaError::Corrupt { .. } => "corrupt",
        ipa::IpaError::NoAppBundle { .. } => "no-app-bundle",
        ipa::IpaError::NoArmSlice { .. } => "no-armv7",
        ipa::IpaError::Encrypted { .. } => "encrypted",
        ipa::IpaError::UnsupportedApp { .. } => "wrong-app",
        ipa::IpaError::AlreadyImported { .. } => "already-imported",
        ipa::IpaError::TooLarge { .. } => "too-large",
        ipa::IpaError::UnsafePath(_) => "unsafe-path",
        _ => "invalid",
    }
}

fn game_listing(root: &Path) -> String {
    let games = ipa::list(root);
    if games.is_empty() {
        return format!(
            "no games imported yet in {}\n\nupload an .ipa above, or run:\n  simpsons-emu import \"The Simpsons Arcade v1.1.43.ipa\"",
            root.display()
        );
    }
    let mut out = String::new();
    for game in &games {
        out.push_str(&format!(
            "{}  [{}]  {} files\n  {}\n",
            game.label(),
            game.manifest.bundle_id.clone().unwrap_or_else(|| "?".to_string()),
            game.manifest.files,
            game.executable().display()
        ));
    }
    out
}

/// Percent-decode a header value (the browser encodes the file name so that
/// non-ASCII names survive the trip).
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut at = 0usize;
    while at < bytes.len() {
        match bytes[at] {
            b'%' if at + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[at + 1..at + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        at += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        at += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                at += 1;
            }
            other => {
                out.push(other);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn respond(stream: &mut TcpStream, content_type: &str, body: &[u8]) -> std::io::Result<()> {
    respond_status(stream, 200, "200 OK", content_type, body)
}

fn respond_status(
    stream: &mut TcpStream,
    _code: u16,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("simpsons-serve-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn serves_the_page_and_an_empty_frame() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let frames: Frames = Arc::new(Mutex::new(Vec::new()));
        let logs: Logs = Arc::new(Mutex::new(vec!["hello".to_string()]));
        let games = scratch("page");
        std::thread::spawn(move || run(listener, frames, logs, games));

        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.contains("live framebuffer"), "{response}");
        assert!(response.contains("Import an .ipa"), "the import panel is part of the page");

        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(b"GET /log HTTP/1.1\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.contains("hello"), "{response}");
    }

    #[test]
    fn imports_an_uploaded_ipa_and_lists_it() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let frames: Frames = Arc::new(Mutex::new(Vec::new()));
        let logs: Logs = Arc::new(Mutex::new(Vec::new()));
        let games = scratch("upload");
        let library = games.clone();
        std::thread::spawn(move || run(listener, frames, logs, games));

        let archive = ipa::test_support::simpsons_ipa();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            stream,
            "POST /import HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/octet-stream\r\n\
             X-Ipa-Name: The%20Simpsons%20Arcade%20v1.1.43.ipa\r\nContent-Length: {}\r\n\r\n",
            archive.len()
        )
        .unwrap();
        stream.write_all(&archive).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        assert!(response.contains("\"ok\":true"), "{response}");
        assert!(response.contains("com.ea.simpsonsarcade.bv"), "{response}");

        // The bundle is now in the library, and `/games` says so.
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(b"GET /games HTTP/1.1\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.contains("The Simpsons Arcade 1.1.43"), "{response}");
        assert!(library.join("com.ea.simpsonsarcade.bv/TheSimpsons.app/TheSimpsons").is_file());

        // A second upload of the same archive is accepted (it is reused), and a
        // bogus one is refused with a readable error.
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(stream, "POST /import HTTP/1.1\r\nContent-Length: {}\r\n\r\n", archive.len()).unwrap();
        stream.write_all(&archive).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.contains("\"reused\":true"), "{response}");

        let junk = b"PK\x03\x04this is not an ipa".to_vec();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(stream, "POST /import HTTP/1.1\r\nContent-Length: {}\r\n\r\n", junk.len()).unwrap();
        stream.write_all(&junk).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
        assert!(response.contains("\"ok\":false"), "{response}");

        let _ = std::fs::remove_dir_all(&library);
    }

    #[test]
    fn percent_decodes_header_values() {
        assert_eq!(percent_decode("The%20Simpsons%20Arcade%20v1.1.43.ipa"), "The Simpsons Arcade v1.1.43.ipa");
        assert_eq!(percent_decode("caf%C3%A9.ipa"), "café.ipa");
        assert_eq!(percent_decode("plain.ipa"), "plain.ipa");
    }
}
