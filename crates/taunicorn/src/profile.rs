//! Opt-in typed messages and audit. No raw connection is returned by this API.
mod audit;
pub use audit::{Audit, AuditEvent, EventKind, Exporter};
use {
  crate::{Connection, ConnectionCore, security::SecureConnection},
  anyhow::{Result, anyhow, bail, ensure},
  std::{
    sync::{
      Arc,
      atomic::{AtomicBool, Ordering},
    },
    time::Duration,
  },
  tokio::{
    sync::{Mutex, watch},
    time::{Instant, sleep_until, timeout},
  },
};

#[derive(Clone, Copy)]
pub enum Role {
  Client,
  Server,
}

#[derive(Clone, Copy)]
pub struct Limits {
  pub bytes_per_direction: usize,
  pub idle: Duration,
}

impl Default for Limits {
  fn default() -> Self {
    Self { bytes_per_direction: 1024 * 1024, idle: Duration::from_secs(30) }
  }
}

/// Schema 1. Only `ready` may cross the boundary. Deliberately not Debug.
pub struct Status<'a> {
  pub ready: bool,
  pub private_note: Option<&'a str>,
}

const NOTE_MAX: usize = 256;
const FRAME_MAX: usize = 512;

fn ready_field(frame: &[u8]) -> Option<bool> {
  let [0x93, 1, flag, note @ ..] = frame else { return None };
  let ready = match flag {
    0xc2 => false,
    0xc3 => true,
    _ => return None,
  };
  if note != [0xc0] {
    let (text, tail) = rmp::decode::read_str_from_slice(note).ok()?;
    if text.len() > NOTE_MAX || !tail.is_empty() {
      return None;
    }
  }
  Some(ready)
}

#[cfg(test)]
#[test]
fn reject_unallowlisted_shapes_and_trailing_bytes() {
  for frame in [
    &[0x80][..],
    &[0x93, 2, 0xc3, 0xc0],
    &[0x93, 1, 1, 0xc0],
    &[0x93, 1, 0xc3, 0xc4, 0],
    &[0x93, 1, 0xc3, 0x90],
    &[0x93, 1, 0xc3, 0xa1, 0xff],
    &[0x93, 1, 0xc3, 0xc0, 0xc0],
    &[0x93, 1, 0xc3, 0xa1],
  ] {
    assert!(ready_field(frame).is_none());
  }
}

struct Lifecycle {
  core: Arc<ConnectionCore>,
  audit: Audit,
  finished: AtomicBool,
}

impl Lifecycle {
  fn record(&self, kind: EventKind) -> bool { self.audit.record(self.core.id, kind) }
  fn finish(&self, reason: EventKind) {
    self.core.fail_connection();
    if !self.finished.swap(true, Ordering::AcqRel) {
      self.record(reason);
    }
  }
}

// Arm only after acquiring a direction. A dropped queued operation made no progress.
struct Active<'a>(Option<&'a Lifecycle>);
impl Drop for Active<'_> {
  fn drop(&mut self) {
    if let Some(live) = self.0 {
      live.finish(EventKind::Cancelled);
    }
  }
}

pub struct Session {
  secure: SecureConnection,
  life: Arc<Lifecycle>,
  read: Mutex<usize>,
  write: Mutex<usize>,
  activity: watch::Sender<Instant>,
  idle: Duration,
}

impl Session {
  /// Consume a fresh, OS-admitted private connection. Pins do not establish OS admission.
  pub async fn open(
    connection: Connection,
    role: Role,
    local_key: [u8; 32],
    peer_pin: [u8; 32],
    limits: Limits,
    audit: Audit,
  ) -> Result<Self> {
    ensure!(
      limits.bytes_per_direction >= 4
        && !limits.idle.is_zero()
        && limits.idle <= Duration::from_secs(86400),
      "invalid profile limits"
    );
    let life = Arc::new(Lifecycle {
      core: connection.core.clone(),
      audit,
      finished: AtomicBool::new(false),
    });
    if !connection.core.stream().is_ok_and(|s| s._lease.is_some()) {
      life.finish(EventKind::PrivateEndpointRequired);
      bail!("private admitted endpoint required");
    }
    ensure!(life.record(EventKind::AdmissionStarted), "audit unavailable");
    let mut active = Active(Some(&life));
    let secure = match role {
      Role::Client => SecureConnection::client(connection, local_key, peer_pin).await,
      Role::Server => SecureConnection::server(connection, peer_pin, local_key).await,
    };
    active.0.take();
    drop(active);
    let secure = secure.map_err(|_| {
      life.finish(EventKind::HandshakeFailed);
      anyhow!("profile handshake failed")
    })?;
    ensure!(life.record(EventKind::Admitted), "audit unavailable");
    let (activity, rx) = watch::channel(Instant::now());
    let idle = limits.idle;
    let watcher = Arc::clone(&life);
    tokio::spawn(async move {
      loop {
        let deadline = *rx.borrow() + idle;
        tokio::select! {
            _ = watcher.core.close_token.cancelled() => break,
            _ = sleep_until(deadline) => {
                let last = rx.borrow();
                if Instant::now() >= *last + idle { watcher.finish(EventKind::Idle); break; }
            }
        }
      }
    });
    Ok(Self {
      secure,
      life,
      read: Mutex::new(limits.bytes_per_direction),
      write: Mutex::new(limits.bytes_per_direction),
      activity,
      idle,
    })
  }

  fn budget(&self, remaining: usize) -> Result<()> {
    ensure!(!self.secure.connection().is_closed(), "profile closed");
    if remaining < 4 {
      self.life.record(EventKind::BudgetExceeded);
      bail!("profile budget exceeded");
    }
    Ok(())
  }

  async fn exchange<T>(&self, io: impl Future<Output = Result<T>>) -> Result<T> {
    let mut active = Active(Some(&self.life));
    let result = timeout(self.idle, io).await;
    active.0.take();
    match result {
      Ok(Ok(value)) => Ok(value),
      Err(_) => {
        self.life.finish(EventKind::Idle);
        bail!("profile operation timed out")
      }
      Ok(Err(_)) => {
        self.life.finish(EventKind::IoFailed);
        bail!("profile I/O failed")
      }
    }
  }

  fn complete(&self, kind: EventKind) -> Result<()> {
    self.activity.send_replace(Instant::now());
    ensure!(!self.secure.connection().is_closed(), "profile closed");
    self.life.record(kind);
    Ok(())
  }

  pub async fn send(&self, status: Status<'_>) -> Result<()> {
    if status.private_note.is_some_and(|s| s.len() > NOTE_MAX) {
      self.life.record(EventKind::SchemaRejected);
      bail!("unsupported profile message");
    }
    let mut remaining = self.write.lock().await;
    self.budget(*remaining)?;
    let frame = [0x93, 1, if status.ready { 0xc3 } else { 0xc2 }, 0xc0];
    self.exchange(self.secure.send(&frame)).await?;
    *remaining -= frame.len();
    self.complete(EventKind::Sent)
  }

  /// Receive the allowlisted field, never the optional private text.
  pub async fn receive(&self) -> Result<bool> {
    let mut remaining = self.read.lock().await;
    self.budget(*remaining)?;
    let frame = self.exchange(self.secure.receive_limited(FRAME_MAX.min(*remaining))).await?;
    let ready = ready_field(&frame).ok_or_else(|| {
      self.life.finish(EventKind::SchemaRejected);
      anyhow!("unsupported profile message")
    })?;
    *remaining -= frame.len();
    self.complete(EventKind::Received)?;
    Ok(ready)
  }

  pub fn close(&self) { self.life.finish(EventKind::Closed); }
}

impl Drop for Session {
  fn drop(&mut self) { self.close(); }
}
