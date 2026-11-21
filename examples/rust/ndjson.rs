//! Minimal Taunicorn NDJSON example: Server und Client in einem Binary.
//!
//! Start:
//!   cargo run --example ndjson -- server
//!   cargo run --example ndjson -- client
//!
//! Protokoll: eine JSON-Zeile pro Request/Response, UTF-8, `\n`-terminiert.

use {
  anyhow::{Context, Result, bail},
  serde_json::{Value, json},
  std::time::{SystemTime, UNIX_EPOCH},
  taunicorn::{Connection, ReceiveResult, Server},
};

const ENDPOINT: &str = "taunicorn-rust-ndjson-bridge";
const MAX_LINE: usize = 8192;

#[tokio::main]
async fn main() -> Result<()> {
  let role = std::env::args().nth(1).unwrap_or_else(|| "client".into());
  match role.as_str() {
    "server" => run_server().await,
    "client" => run_client().await,
    other => bail!("unbekannte Rolle: {other} (erwartet: server | client)"),
  }
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

async fn run_server() -> Result<()> {
  let server = Server::start(ENDPOINT).await.context("server start")?;
  println!("[rust-server] listening on {ENDPOINT}");

  // Sequenziell: ein Client nach dem anderen (wie im Python-Beispiel).
  loop {
    let conn = match server.accept().await {
      Ok(c) => c,
      Err(e) => {
        eprintln!("[rust-server] accept failed: {e}");
        break;
      }
    };
    if let Err(e) = handle_connection(conn).await {
      eprintln!("[rust-server] connection error: {e:#}");
    }
  }

  let _ = server.stop().await;
  Ok(())
}

async fn handle_connection(conn: Connection) -> Result<()> {
  println!("[rust-server] client connected (id={})", conn.id());

  let mut buf: Vec<u8> = Vec::with_capacity(4096);

  loop {
    let mut chunk = [0u8; 4096];
    match conn.receive(&mut chunk).await {
      Ok(ReceiveResult::Data(0)) => {
        // Leerer Buffer ist kein EOF – weiter warten.
        continue;
      }
      Ok(ReceiveResult::Data(n)) => {
        buf.extend_from_slice(&chunk[..n]);
      }
      Ok(ReceiveResult::EndOfStream) => {
        println!("[rust-server] client sent EOF");
        break;
      }
      Err(e) => {
        eprintln!("[rust-server] receive failed: {e:#}");
        break;
      }
    }

    // Zeilenweise verarbeiten.
    while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
      if pos > MAX_LINE {
        bail!("record too long ({pos} bytes)");
      }
      let line: Vec<u8> = buf.drain(..=pos).collect();
      let line = &line[..line.len() - 1]; // \n abschneiden
      if line.iter().all(|b| b.is_ascii_whitespace()) {
        continue;
      }

      let req: Value = match serde_json::from_slice(line) {
        Ok(v) => v,
        Err(e) => {
          eprintln!("[rust-server] invalid request: {e}");
          continue;
        }
      };

      let resp = handle_request(&req);
      let mut out = serde_json::to_vec(&resp)?;
      out.push(b'\n');
      if let Err(e) = conn.send(&out).await {
        eprintln!("[rust-server] send failed: {e:#}");
        return Ok(());
      }

      if req.get("method").and_then(Value::as_str) == Some("quit") {
        println!("[rust-server] quit received");
        let _ = conn.close().await;
        return Ok(());
      }
    }

    if buf.len() > MAX_LINE {
      bail!("buffer too long ({} bytes)", buf.len());
    }
  }

  let _ = conn.close().await;
  Ok(())
}

fn handle_request(req: &Value) -> Value {
  let id = req.get("id").cloned().unwrap_or(Value::Null);
  let method = req.get("method").and_then(Value::as_str).unwrap_or("");
  let params = req.get("params").cloned().unwrap_or_else(|| json!({}));

  let result = match method {
    "add" => {
      let a = params.get("a").and_then(Value::as_f64).unwrap_or(0.0);
      let b = params.get("b").and_then(Value::as_f64).unwrap_or(0.0);
      json!(a + b)
    }
    "greet" => {
      let name = params.get("name").and_then(Value::as_str).unwrap_or("?");
      json!(format!("Hallo {name}, grüße aus Rust! 🦀"))
    }
    "time" => {
      let now =
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
      json!(now)
    }
    "quit" => json!("bye"),
    _ => json!(null),
  };

  json!({ "id": id, "result": result })
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

async fn run_client() -> Result<()> {
  println!("[rust-client] connecting to {ENDPOINT} ...");
  let conn = Connection::connect(ENDPOINT).await.context("connect")?;
  println!("[rust-client] connected");

  // Ein einfacher Request/Response-Loop. Wir lesen nach jedem `send` genau
  // eine Zeile – das genügt für dieses line-basierte Demo-Protokoll.
  let requests = [
    json!({"id": 1, "method": "add", "params": {"a": 20, "b": 22}}),
    json!({"id": 2, "method": "greet", "params": {"name": "example"}}),
    json!({"id": 3, "method": "time"}),
    json!({"id": 4, "method": "quit"}),
  ];

  for req in requests {
    let mut line = serde_json::to_vec(&req)?;
    line.push(b'\n');
    println!("[rust-client] -> {}", String::from_utf8_lossy(&line).trim_end());
    conn.send(&line).await.context("send")?;

    let resp = read_line(&conn).await.context("read response")?;
    println!("[rust-client] <- {resp}");

    if req.get("method").and_then(Value::as_str) == Some("quit") {
      break;
    }
  }

  conn.close().await?;
  println!("[rust-client] done ✅");
  Ok(())
}

/// Liest genau eine `\n`-terminierte Zeile (ohne das `\n`).
async fn read_line(conn: &Connection) -> Result<String> {
  let mut buf: Vec<u8> = Vec::with_capacity(256);
  loop {
    let mut chunk = [0u8; 1024];
    match conn.receive(&mut chunk).await? {
      ReceiveResult::Data(0) => continue,
      ReceiveResult::Data(n) => {
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
          buf.truncate(pos);
          return Ok(String::from_utf8(buf)?);
        }
        if buf.len() > MAX_LINE {
          bail!("line too long");
        }
      }
      ReceiveResult::EndOfStream => {
        if buf.is_empty() {
          bail!("connection closed before response");
        }
        return Ok(String::from_utf8(buf)?);
      }
    }
  }
}
