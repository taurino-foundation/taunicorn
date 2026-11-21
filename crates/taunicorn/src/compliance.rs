//! Build with `compliance-profile` to gate *all* core bind/listen/accept/connect paths.
//! Permission overrides are refused; no negotiation, environment switch or legacy retry.
//! Otherwise only private/with-policy endpoints are gated. Both ends must select private mode.
//! Unix requires an existing absolute, owner-only `XDG_RUNTIME_DIR`. No fallback, automatic
//! overwrite or unlink (even on stop): choose a fresh name or clean the directory offline.
//! Windows consumes one bounded opening byte before admitting the client token. Account
//! authentication is not authentication of a particular executable or login session.
//!
//! Unix: use a real 0700 directory below root-/effective-user-owned, non-group/world-writable
//! ancestors. Linux needs `/proc/self/fd`; Linux/OpenBSD request mode 0600. macOS/FreeBSD
//! require an owner-only socket at creation (e.g. service umask 0077). macOS rejects extended
//! directory ACLs and cannot supply a PID through this backend. FreeBSD rejects NFSv4 ACL
//! filesystems and missing PID when required. Other Unix ACL models fail closed.
//! Windows: local pipe only, exact owner-only duplex/instance DACL checked on the client
//! handle. Server queries the opening record's impersonation token and reverts synchronously;
//! an already-impersonating accept thread is unsupported. No PID-to-token lookup or cross-user
//! grants. OS refusals before admission remain OS errors, not inferred attack diagnoses.
//! Defaults: 64 pending identity checks, five seconds per check, 256 live ends, 64 per account.
use {
  crate::{RawListener, RawStream},
  anyhow::Result,
  std::{
    collections::HashMap,
    fmt,
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
  },
  tokio::sync::Semaphore,
};
#[cfg(unix)]
#[path = "compliance/unix.rs"]
mod os;
#[cfg(windows)]
#[path = "compliance/windows.rs"]
mod os;
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
#[non_exhaustive]
pub enum AdmissionError {
  InvalidEndpoint,
  UnauthorizedIpcPeer,
  MissingPeerIdentity,
  InsecureSocketPermissions,
  StreamQuotaExceeded,
  AdmissionTimeout,
  UnsupportedPlatform,
}
impl fmt::Display for AdmissionError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{self:?}") }
}
impl std::error::Error for AdmissionError {}
#[derive(Clone, Hash, Eq, PartialEq)]
#[allow(dead_code)] // Native account types differ across targets.
enum Principal {
  Uid(u32),
  Sid(String),
}
struct Identity {
  user: Principal,
  gid: Option<u32>,
  pid: Option<u32>,
}

/// Lists are conjunctive restrictions on the same-user boundary. `None` skips that filter;
/// an empty list admits nobody. SIDs use canonical numeric notation. PIDs are OS snapshots,
/// never used to look up an account. Counts include inbound and outbound connections across
/// private endpoints in this core instance; split halves share a single lease.
#[derive(Clone, Debug)]
pub struct IpcCompliancePolicyEngine {
  pub allowed_uids: Option<Vec<u32>>,
  pub allowed_gids: Option<Vec<u32>>,
  pub allowed_pids: Option<Vec<u32>>,
  pub allowed_sids: Option<Vec<String>>,
  pub max_connections: usize,
  pub max_per_user: usize,
}
impl Default for IpcCompliancePolicyEngine {
  fn default() -> Self {
    Self {
      allowed_uids: None,
      allowed_gids: None,
      allowed_pids: None,
      allowed_sids: None,
      max_connections: 256,
      max_per_user: 64,
    }
  }
}
fn require<T: PartialEq>(list: &Option<Vec<T>>, value: Option<&T>) -> Result<(), AdmissionError> {
  if let Some(list) = list
    && !list.contains(value.ok_or(AdmissionError::MissingPeerIdentity)?)
  {
    return Err(AdmissionError::UnauthorizedIpcPeer);
  }
  Ok(())
}
impl IpcCompliancePolicyEngine {
  fn check(&self, local: &Principal, peer: &Identity) -> Result<(), AdmissionError> {
    if local != &peer.user {
      return Err(AdmissionError::UnauthorizedIpcPeer);
    }
    let (uid, sid) = match &peer.user {
      Principal::Uid(u) => (Some(u), None),
      Principal::Sid(s) => (None, Some(s)),
    };
    require(&self.allowed_uids, uid)?;
    require(&self.allowed_sids, sid)?;
    require(&self.allowed_gids, peer.gid.as_ref())?;
    require(&self.allowed_pids, peer.pid.as_ref())
  }
  fn admit(&self, peer: Identity) -> Result<Lease> {
    self.check(&os::local_user()?, &peer)?;
    Ok(Lease::reserve(&COUNTS, peer.user, self.max_connections, self.max_per_user)?)
  }
}
#[derive(Default)]
struct Counts {
  total: usize,
  users: HashMap<Principal, usize>,
}
static COUNTS: LazyLock<Arc<Mutex<Counts>>> = LazyLock::new(Default::default);
static PENDING: Semaphore = Semaphore::const_new(64);
pub(crate) struct Lease {
  counts: Arc<Mutex<Counts>>,
  user: Principal,
}
impl Lease {
  fn reserve(
    counts: &Arc<Mutex<Counts>>,
    user: Principal,
    total: usize,
    per_user: usize,
  ) -> Result<Self, AdmissionError> {
    let mut c = counts.lock().map_err(|_| AdmissionError::StreamQuotaExceeded)?;
    if c.total >= total || c.users.get(&user).copied().unwrap_or(0) >= per_user {
      return Err(AdmissionError::StreamQuotaExceeded);
    }
    *c.users.entry(user.clone()).or_default() += 1;
    c.total += 1;
    Ok(Self { counts: counts.clone(), user })
  }
}
impl Drop for Lease {
  fn drop(&mut self) {
    let mut c = self.counts.lock().unwrap_or_else(|e| e.into_inner());
    c.total -= 1;
    let n = c.users.get_mut(&self.user).expect("live admission lease");
    *n -= 1;
    if *n == 0 {
      c.users.remove(&self.user);
    }
  }
}
fn validate(name: &str) -> Result<()> {
  if name.is_empty()
    || name.len() > 64
    || !name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    || name.starts_with('.')
  {
    return Err(AdmissionError::InvalidEndpoint.into());
  }
  Ok(())
}
pub(super) fn listen(name: &str) -> Result<RawListener> {
  validate(name)?;
  os::listen(name)
}
pub(super) async fn connect(
  name: &str,
  policy: &IpcCompliancePolicyEngine,
) -> Result<(RawStream, Lease)> {
  validate(name)?;
  let _slot = PENDING.try_acquire().map_err(|_| AdmissionError::StreamQuotaExceeded)?;
  tokio::time::timeout(Duration::from_secs(5), os::connect(name, policy))
    .await
    .map_err(|_| AdmissionError::AdmissionTimeout)?
}
pub(super) async fn accept(
  stream: &RawStream,
  policy: &IpcCompliancePolicyEngine,
) -> Result<Lease> {
  let _slot = PENDING.try_acquire().map_err(|_| AdmissionError::StreamQuotaExceeded)?;
  let peer = tokio::time::timeout(Duration::from_secs(5), os::peer(stream))
    .await
    .map_err(|_| AdmissionError::AdmissionTimeout)??;
  policy.admit(peer)
}
#[cfg(test)]
mod tests;
