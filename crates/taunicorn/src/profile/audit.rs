use {
  anyhow::{Result, ensure},
  std::{
    io,
    sync::{
      Arc,
      atomic::{AtomicU64, Ordering},
    },
    time::SystemTime,
  },
  tokio::sync::mpsc,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventKind {
  PrivateEndpointRequired,
  AdmissionStarted,
  Admitted,
  HandshakeFailed,
  Sent,
  Received,
  SchemaRejected,
  BudgetExceeded,
  IoFailed,
  Cancelled,
  Idle,
  Closed,
}

#[derive(Clone, Copy, Debug)]
pub struct AuditEvent {
  pub time: SystemTime,
  /// One-based queue order within this exporter, independent of wall-clock corrections.
  pub sequence: u64,
  /// Process-local connection counter, not a peer identity.
  pub connection: u64,
  pub kind: EventKind,
}

// A queued decision counts as lost unless the sink acknowledges it. This also covers
// a receiver dropped concurrently with try_send and an exporter unwinding mid-append.
struct Pending {
  event: AuditEvent,
  unacknowledged: Option<Arc<AtomicU64>>,
}
impl Drop for Pending {
  fn drop(&mut self) {
    if let Some(n) = &self.unacknowledged {
      n.fetch_add(1, Ordering::Relaxed);
    }
  }
}

#[derive(Clone)]
pub struct Audit {
  tx: mpsc::Sender<Pending>,
  dropped: Arc<AtomicU64>,
}

pub struct Exporter {
  rx: mpsc::Receiver<Pending>,
}

impl Audit {
  pub fn bounded(capacity: usize) -> Result<(Self, Exporter)> {
    ensure!((1..=65536).contains(&capacity), "invalid audit capacity");
    let (tx, rx) = mpsc::channel(capacity);
    let dropped = Arc::new(AtomicU64::new(0));
    Ok((Self { tx, dropped }, Exporter { rx }))
  }

  pub fn dropped(&self) -> u64 { self.dropped.load(Ordering::Relaxed) }
  pub fn is_disconnected(&self) -> bool { self.tx.is_closed() }

  pub(super) fn record(&self, connection: u64, kind: EventKind) -> bool {
    self
      .tx
      .try_send(Pending {
        event: AuditEvent { time: SystemTime::now(), sequence: 0, connection, kind },
        unacknowledged: Some(self.dropped.clone()),
      })
      .is_ok()
  }
}

impl Exporter {
  /// Run off the I/O executor. Return success from the callback only after protected append/anchor.
  pub fn run(mut self, mut append: impl FnMut(&AuditEvent) -> io::Result<()>) -> io::Result<()> {
    let mut seq = 0u64;
    while let Some(mut pending) = self.rx.blocking_recv() {
      seq = seq.checked_add(1).ok_or_else(|| io::Error::other("audit sequence exhausted"))?;
      pending.event.sequence = seq;
      append(&pending.event).map_err(|_| io::Error::other("audit export failed"))?;
      pending.unacknowledged.take();
    }
    Ok(())
  }
}
