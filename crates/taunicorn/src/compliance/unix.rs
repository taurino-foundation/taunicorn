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

fn uid() -> u32 { unsafe { libc::geteuid() } }
pub(super) fn local_user() -> Result<Principal> { Ok(Principal::Uid(uid())) }
// POSIX ACL masks are reflected in mode bits. Where another ACL model may grant more,
// reject it rather than trusting mode bits. Other Unix targets need a verified ACL adapter.
fn check_acl(dir: &File) -> Result<()> {
  let _ = dir;
  if cfg!(any(target_os = "linux", target_os = "openbsd")) {
    return Ok(());
  }
  #[cfg(target_os = "freebsd")]
  if unsafe { libc::fpathconf(dir.as_raw_fd(), libc::_PC_ACL_NFS4) } == 0 {
    return Ok(());
  }
  #[cfg(target_os = "macos")]
  {
    unsafe extern "C" {
      fn acl_get_fd(fd: libc::c_int) -> *mut libc::c_void;
      fn acl_free(acl: *mut libc::c_void) -> libc::c_int;
    }
    let acl = unsafe { acl_get_fd(dir.as_raw_fd()) };
    if acl.is_null() && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
      return Ok(());
    }
    if !acl.is_null() {
      unsafe { acl_free(acl) };
    }
  }
  Err(AdmissionError::UnsupportedPlatform.into())
}
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
fn socket_path(name: &str) -> Result<(File, PathBuf)> {
  let path = PathBuf::from(
    std::env::var_os("XDG_RUNTIME_DIR").ok_or(AdmissionError::InsecureSocketPermissions)?,
  );
  let dir = runtime(&path)?;
  // Linux binds relative to the pinned directory; other targets require every ancestor
  // to remain controlled by the owner or root. Never change permissions or unlink by path.
  #[cfg(target_os = "linux")]
  let path = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()));
  Ok((dir, path.join(format!("taunicorn.{name}.sock"))))
}
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
pub(super) fn listen(name: &str) -> Result<RawListener> {
  let (_dir, path) = socket_path(name)?;
  let opts = ListenerOptions::new()
    .name(path.clone().to_fs_name::<GenericFilePath>()?)
    .reclaim_name(false)
    .try_overwrite(false);
  // Other kernels may not support fchmod-before-bind. Require the process umask to
  // produce an owner-only socket inside the private directory; never change process umask.
  #[cfg(any(target_os = "linux", target_os = "openbsd"))]
  let opts = opts.mode(0o600);
  let l = opts.create_tokio()?;
  check_socket(&path)?;
  Ok(l)
}
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
pub(super) async fn peer(stream: &RawStream) -> Result<Identity> {
  let c = stream.peer_creds().map_err(|_| AdmissionError::MissingPeerIdentity)?;
  let uid = c.euid().filter(|u| *u != u32::MAX).ok_or(AdmissionError::MissingPeerIdentity)?;
  Ok(Identity {
    user: Principal::Uid(uid),
    gid: c.egid().filter(|g| *g != u32::MAX),
    pid: c.pid().filter(|p| *p > 0).and_then(|p| p.try_into().ok()),
  })
}
#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn shared_or_relative_runtime_is_refused() {
    for path in ["/tmp", ".", "/", "/../"] {
      assert!(runtime(Path::new(path)).is_err());
    }
  }
}
