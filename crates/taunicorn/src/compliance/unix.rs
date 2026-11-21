#[cfg(any(target_os = "linux", target_os = "openbsd"))]
use interprocess::os::unix::local_socket::ListenerOptionsExt as _;
use {
  super::*,
  interprocess::local_socket::{
    GenericFilePath, ListenerOptions, ToFsName,
    traits::{StreamCommon, tokio::Stream as _},
  },
  std::{
    ffi::CString,
    fs::{self, File},
    os::{
      fd::{AsRawFd, FromRawFd},
      unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt},
      },
    },
    path::{Component, Path, PathBuf},
  },
};

// =============================================================================
// Local identity
// =============================================================================

fn uid() -> u32 { unsafe { libc::geteuid() } }

pub(super) fn local_user() -> Result<Principal> { Ok(Principal::Uid(uid())) }

// =============================================================================
// ACL verification
// =============================================================================

// Linux and OpenBSD retain the existing mode-bit based policy.
// POSIX ACL masks are reflected in mode bits for the supported model.
#[cfg(any(target_os = "linux", target_os = "openbsd"))]
fn check_acl(_dir: &File) -> Result<()> { Ok(()) }

// FreeBSD retains the existing NFSv4 ACL capability check.
// This is not a complete NFSv4 ACL entry audit.
#[cfg(target_os = "freebsd")]
fn check_acl(dir: &File) -> Result<()> {
  let result = unsafe { libc::fpathconf(dir.as_raw_fd(), libc::_PC_ACL_NFS4) };

  if result == 0 {
    return Ok(());
  }

  Err(AdmissionError::UnsupportedPlatform.into())
}

// macOS supports extended ACLs independently of POSIX mode bits.
//
// Security policy:
// - No extended ACL: accept.
// - Extended ACL present: reject.
// - ACL inspection unavailable: reject.
// - Unexpected native error: reject.
//
// The check is descriptor-bound and does not follow a separate path.
#[cfg(target_os = "macos")]
fn check_acl(dir: &File) -> Result<()> {
  use std::ffi::{c_int, c_void};

  const ACL_TYPE_EXTENDED: c_int = 0x0000_0100;

  unsafe extern "C" {
    fn acl_get_fd_np(fd: c_int, acl_type: c_int) -> *mut c_void;
    fn acl_free(acl: *mut c_void) -> c_int;
  }

  let acl = unsafe { acl_get_fd_np(dir.as_raw_fd(), ACL_TYPE_EXTENDED) };

  if acl.is_null() {
    let error = std::io::Error::last_os_error();

    // Darwin reports ENOENT when no extended ACL exists.
    if error.raw_os_error() == Some(libc::ENOENT) {
      return Ok(());
    }

    // Do not silently accept unsupported filesystems,
    // permission errors or unexpected native errors.
    return Err(AdmissionError::UnsupportedPlatform.into());
  }

  // A non-null result means an extended ACL was retrieved.
  // Conservatively reject it without attempting to interpret
  // individual allow/deny entries.
  let free_result = unsafe { acl_free(acl) };

  if free_result != 0 {
    return Err(AdmissionError::UnsupportedPlatform.into());
  }

  Err(AdmissionError::InsecureSocketPermissions.into())
}

// Any other Unix target needs a verified ACL implementation.
#[cfg(not(any(
  target_os = "linux",
  target_os = "openbsd",
  target_os = "freebsd",
  target_os = "macos",
)))]
fn check_acl(_dir: &File) -> Result<()> { Err(AdmissionError::UnsupportedPlatform.into()) }

// =============================================================================
// Directory validation
// =============================================================================

fn check_dir(file: &File, last: bool) -> Result<()> {
  let m = file.metadata()?;

  if !m.is_dir()
    || (m.uid() != 0 && m.uid() != uid())
    || m.mode() & 0o022 != 0
    || (last && (m.uid() != uid() || m.mode() & 0o777 != 0o700))
  {
    return Err(AdmissionError::InsecureSocketPermissions.into());
  }

  check_acl(file)
}

// =============================================================================
// Secure runtime directory
// =============================================================================

fn runtime(path: &Path) -> Result<File> {
  if !path.is_absolute() || path == Path::new("/") {
    return Err(AdmissionError::InsecureSocketPermissions.into());
  }

  let mut dir = File::open("/")?;
  let mut parts = path.components().skip(1).peekable();

  check_dir(&dir, false)?;

  while let Some(c) = parts.next() {
    let Component::Normal(part) = c else {
      return Err(AdmissionError::InsecureSocketPermissions.into());
    };

    let component = CString::new(part.as_bytes())?;

    let fd = unsafe {
      libc::openat(
        dir.as_raw_fd(),
        component.as_ptr(),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
      )
    };

    if fd < 0 {
      return Err(AdmissionError::InsecureSocketPermissions.into());
    }

    dir = unsafe { File::from_raw_fd(fd) };

    check_dir(&dir, parts.peek().is_none())?;
  }

  Ok(dir)
}

// =============================================================================
// Socket path
// =============================================================================

fn socket_path(name: &str) -> Result<(File, PathBuf)> {
  let path = PathBuf::from(
    std::env::var_os("XDG_RUNTIME_DIR").ok_or(AdmissionError::InsecureSocketPermissions)?,
  );

  let dir = runtime(&path)?;

  // Linux binds relative to the pinned directory.
  // Other targets require every ancestor to remain controlled
  // by the owner or root.
  //
  // Never change permissions or unlink by path.
  #[cfg(target_os = "linux")]
  let path = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()));

  Ok((dir, path.join(format!("taunicorn.{name}.sock"))))
}

// =============================================================================
// Socket permission validation
// =============================================================================

fn check_socket(path: &Path) -> Result<()> {
  let m = fs::symlink_metadata(path)?;

  if !m.file_type().is_socket()
    || m.uid() != uid()
    || m.mode() & 0o077 != 0
    || m.mode() & 0o200 == 0
  {
    return Err(AdmissionError::InsecureSocketPermissions.into());
  }

  Ok(())
}

// =============================================================================
// Listener
// =============================================================================

pub(super) fn listen(name: &str) -> Result<RawListener> {
  let (_dir, path) = socket_path(name)?;

  let opts = ListenerOptions::new()
    .name(path.clone().to_fs_name::<GenericFilePath>()?)
    .reclaim_name(false)
    .try_overwrite(false);

  // Linux and OpenBSD support explicit listener modes.
  //
  // Other kernels may not support fchmod-before-bind.
  // Require an owner-only socket through the process umask.
  //
  // Never change the process-wide umask here.
  #[cfg(any(target_os = "linux", target_os = "openbsd"))]
  let opts = opts.mode(0o600);

  let listener = opts.create_tokio()?;

  check_socket(&path)?;

  Ok(listener)
}

// =============================================================================
// Connection
// =============================================================================

pub(super) async fn connect(
  name: &str,
  policy: &IpcCompliancePolicyEngine,
) -> Result<(RawStream, Lease)> {
  let (_dir, path) = socket_path(name)?;

  check_socket(&path)?;

  let stream = RawStream::connect(path.to_fs_name::<GenericFilePath>()?).await?;

  let lease = policy.admit(peer(&stream).await?)?;

  Ok((stream, lease))
}

// =============================================================================
// Peer identity
// =============================================================================

pub(super) async fn peer(stream: &RawStream) -> Result<Identity> {
  let c = stream.peer_creds().map_err(|_| AdmissionError::MissingPeerIdentity)?;

  let uid = c.euid().filter(|u| *u != u32::MAX).ok_or(AdmissionError::MissingPeerIdentity)?;

  Ok(Identity {
    user: Principal::Uid(uid),
    gid: c.egid().filter(|g| *g != u32::MAX),
    pid: c.pid().filter(|p| *p > 0).and_then(|p| p.try_into().ok()),
  })
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn shared_or_relative_runtime_is_refused() {
    for path in ["/tmp", ".", "/", "/../"] {
      assert!(
        runtime(Path::new(path)).is_err(),
        "insecure runtime directory was accepted: {path}"
      );
    }
  }

  #[cfg(target_os = "macos")]
  #[test]
  fn macos_root_acl_check_is_supported() {
    // The filesystem root must be inspectable.
    // Unexpected ACL inspection errors must not be ignored.
    let root = File::open("/").expect("failed to open filesystem root");

    let result = check_acl(&root);

    // A root directory with no extended ACL is accepted.
    // An existing ACL or an inspection failure is rejected.
    if let Err(error) = result {
      assert!(
        error.downcast_ref::<AdmissionError>().is_some(),
        "unexpected macOS ACL error: {error}"
      );
    }
  }
}
