//! A tiny HTTP server (std only) that shows the emulator's framebuffer live.
//!
//! `/` serves a page that polls `/frame.bmp`, `/log` returns the guest's log,
//! `/stats` returns a one-line summary.  The server binds `0.0.0.0` so the
//! sandbox's preview proxy can reach it.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

pub type Frames = Arc<Mutex<Vec<u8>>>;
pub type Logs = Arc<Mutex<Vec<String>>>;

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
  main { display: grid; grid-template-columns: minmax(320px, 480px) 1fr; gap: 16px; padding: 16px; }
  img { width: 100%; image-rendering: pixelated; background: #000; border: 1px solid #262a36; }
  pre { margin: 0; padding: 12px; background: #161923; border: 1px solid #262a36; height: 70vh; overflow: auto; white-space: pre-wrap; }
  .muted { color: #8b90a3; }
</style>
</head>
<body>
<header>
  <h1>Simpsons Arcade — live framebuffer</h1>
  <span class="muted" id="stats">connecting…</span>
</header>
<main>
  <div><img id="frame" alt="framebuffer"></div>
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
setInterval(tick, 500);
tick();
</script>
</body>
</html>
"#;

/// Start the preview server on its own thread.
pub fn start(port: u16, frames: Frames, logs: Logs) -> Result<(), String> {
    let listener = TcpListener::bind(("0.0.0.0", port)).map_err(|e| format!("bind 0.0.0.0:{port}: {e}"))?;
    std::thread::spawn(move || run(listener, frames, logs));
    Ok(())
}

pub fn run(listener: TcpListener, frames: Frames, logs: Logs) {
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                if let Err(e) = handle(&mut stream, &frames, &logs) {
                    let _ = e;
                }
            }
            Err(_) => continue,
        }
    }
}

fn handle(stream: &mut TcpStream, frames: &Frames, logs: &Logs) -> std::io::Result<()> {
    let mut buffer = [0u8; 2048];
    let read = stream.read(&mut buffer)?;
    let request = String::from_utf8_lossy(&buffer[..read]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let path = path.split('?').next().unwrap_or("/");

    match path {
        "/" | "/index.html" => respond(stream, "text/html; charset=utf-8", PAGE.as_bytes()),
        "/frame.bmp" => {
            let frame = frames.lock().map(|f| f.clone()).unwrap_or_default();
            respond(stream, "image/bmp", &frame)
        }
        "/log" => {
            let log = logs
                .lock()
                .map(|l| l.iter().rev().take(400).cloned().collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"))
                .unwrap_or_default();
            respond(stream, "text/plain; charset=utf-8", log.as_bytes())
        }
        "/stats" => {
            let size = frames.lock().map(|f| f.len()).unwrap_or(0);
            let text = if size > 54 {
                format!("frame {size} bytes  (polling 2 Hz)")
            } else {
                "no frame yet".to_string()
            };
            respond(stream, "text/plain; charset=utf-8", text.as_bytes())
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

fn respond(stream: &mut TcpStream, content_type: &str, body: &[u8]) -> std::io::Result<()> {
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

use std::io::Read;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_the_page_and_an_empty_frame() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let frames: Frames = Arc::new(Mutex::new(Vec::new()));
        let logs: Logs = Arc::new(Mutex::new(vec!["hello".to_string()]));
        std::thread::spawn(move || run(listener, frames, logs));

        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.contains("live framebuffer"), "{response}");

        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(b"GET /log HTTP/1.1\r\n\r\n").unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.contains("hello"), "{response}");
    }
}
