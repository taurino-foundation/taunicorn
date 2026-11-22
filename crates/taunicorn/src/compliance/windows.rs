use {
  super::*,
  interprocess::{
    local_socket::{ListenerOptions, traits::StreamCommon},
    os::windows::{local_socket::ListenerOptionsExt, security_descriptor::SecurityDescriptor},
  },
  std::{
    ffi::c_void,
    fs::OpenOptions,
    io,
    os::windows::{
      fs::OpenOptionsExt,
      io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle},
    },
    ptr::{addr_of_mut, null_mut},
  },
  tokio::io::{AsyncReadExt, AsyncWriteExt},
  widestring::{U16CStr, U16CString},
  windows_sys::Win32::{
    Foundation::{ERROR_NO_TOKEN, HANDLE, LocalFree},
    Security::{
      Authorization::{ConvertSidToStringSidW, GetSecurityInfo, SE_KERNEL_OBJECT},
      *,
    },
    Storage::FileSystem::*,
    System::{Pipes::ImpersonateNamedPipeClient, Threading::*},
  },
};

struct LocalMem(*mut c_void);
impl Drop for LocalMem {
  fn drop(&mut self) {
    unsafe {
      LocalFree(self.0);
    }
  }
}
fn ok(rc: i32) -> io::Result<()> {
  if rc == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}
// SAFETY: sid must point inside a live Windows-returned token/descriptor buffer.
unsafe fn sid_string(sid: PSID) -> Result<String> {
  if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
    return Err(AdmissionError::MissingPeerIdentity.into());
  }
  let mut text = null_mut();
  ok(unsafe { ConvertSidToStringSidW(sid, &mut text) })?;
  let _mem = LocalMem(text.cast());
  Ok(unsafe { U16CStr::from_ptr_str(text) }.to_string()?)
}
fn token_user(token: &OwnedHandle) -> Result<Principal> {
  // TOKEN_USER and the maximum-sized SID fit in this aligned stack buffer; no external lengths.
  let mut buf = [0usize; 128];
  let mut written = 0;
  ok(unsafe {
    GetTokenInformation(
      token.as_raw_handle(),
      TokenUser,
      buf.as_mut_ptr().cast(),
      size_of_val(&buf) as u32,
      &mut written,
    )
  })?;
  if (written as usize) < size_of::<TOKEN_USER>() {
    return Err(AdmissionError::MissingPeerIdentity.into());
  }
  let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
  Ok(Principal::Sid(unsafe { sid_string(sid)? }))
}
pub(super) fn local_user() -> Result<Principal> {
  let mut h = null_mut();
  ok(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut h) })?;
  token_user(&unsafe { OwnedHandle::from_raw_handle(h) })
}

fn pipe_name(name: &str) -> String { format!("taunicorn.private.{name}") }
pub(super) fn listen(name: &str) -> Result<RawListener> {
  let Principal::Sid(sid) = local_user()? else {
    return Err(AdmissionError::MissingPeerIdentity.into());
  };
  let sd = SecurityDescriptor::deserialize(&U16CString::from_str(format!(
    "O:{sid}D:P(A;;FRFW;;;{sid})"
  ))?)?;
  Ok(
    ListenerOptions::new()
      .name(crate::get_async_socket_name(&pipe_name(name))?)
      .security_descriptor(sd)
      .create_tokio()?,
  )
}
// Verify the *connected object*, not a process opened later by a recyclable PID. Only this
// account may own the pipe and create its server instances. No null, inherited or extra ACEs.
fn server_user(handle: HANDLE) -> Result<Principal> {
  let mut owner = null_mut();
  let mut acl = null_mut();
  let mut descriptor = null_mut();
  let rc = unsafe {
    GetSecurityInfo(
      handle,
      SE_KERNEL_OBJECT,
      OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
      &mut owner,
      null_mut(),
      &mut acl,
      null_mut(),
      &mut descriptor,
    )
  };
  if rc != 0 {
    return Err(io::Error::from_raw_os_error(rc as i32).into());
  }
  let _mem = LocalMem(descriptor);
  let owner = unsafe { sid_string(owner)? };
  if acl.is_null() || unsafe { IsValidAcl(acl) == 0 || (*acl).AceCount != 1 } {
    return Err(AdmissionError::InsecureSocketPermissions.into());
  }
  let mut ace = null_mut();
  ok(unsafe { GetAce(acl, 0, &mut ace) })?;
  // Check the common ACE header before reading type-specific fields.
  let header = unsafe { *ace.cast::<ACE_HEADER>() };
  if header.AceType != 0
    || header.AceFlags != 0
    || usize::from(header.AceSize) < std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart) + 8
  {
    return Err(AdmissionError::InsecureSocketPermissions.into());
  }
  let allow = ace.cast::<ACCESS_ALLOWED_ACE>();
  let sid = unsafe { addr_of_mut!((*allow).SidStart).cast::<u8>() };
  // The SID header is eight bytes; bound its variable tail to this ACE before calling Win32.
  let sid_bytes = 8 + 4 * usize::from(unsafe { *sid.add(1) });
  if std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart) + sid_bytes != usize::from(header.AceSize)
    || unsafe { (*allow).Mask } != FILE_GENERIC_READ | FILE_GENERIC_WRITE
    || unsafe { sid_string(sid.cast())? } != owner
  {
    return Err(AdmissionError::InsecureSocketPermissions.into());
  }
  Ok(Principal::Sid(owner))
}
fn pid(stream: &RawStream) -> Option<u32> { stream.peer_creds().ok()?.pid().filter(|p| *p != 0) }
pub(super) async fn connect(
  name: &str,
  policy: &IpcCompliancePolicyEngine,
) -> Result<(RawStream, Lease)> {
  let path = format!(r"\\.\pipe\{}", pipe_name(name));
  let file = OpenOptions::new()
    .access_mode(FILE_GENERIC_READ | FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES)
    .custom_flags(FILE_FLAG_OVERLAPPED)
    .security_qos_flags(SECURITY_IDENTIFICATION)
    .open(path)?;
  let user = server_user(file.as_raw_handle())?;
  let stream = RawStream::NamedPipe(
    OwnedHandle::from(file).try_into().map_err(|_| AdmissionError::UnsupportedPlatform)?,
  );
  let lease = policy.admit(Identity { user, gid: None, pid: pid(&stream) })?;
  (&stream).write_all(&[1]).await?;
  Ok((stream, lease))
}
fn client_user(handle: HANDLE) -> Result<Principal> {
  let mut h = null_mut();
  // Never overwrite a caller's thread token. There is no allocation, panic path or await
  // between impersonation and reversion. Failing to revert cannot safely return to the runtime.
  if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut h) } != 0 {
    drop(unsafe { OwnedHandle::from_raw_handle(h) });
    return Err(AdmissionError::UnsupportedPlatform.into());
  }
  if io::Error::last_os_error().raw_os_error() != Some(ERROR_NO_TOKEN as i32) {
    return Err(AdmissionError::MissingPeerIdentity.into());
  }
  ok(unsafe { ImpersonateNamedPipeClient(handle) })?;
  let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut h) };
  if unsafe { RevertToSelf() } == 0 {
    std::process::abort();
  }
  if opened == 0 {
    return Err(AdmissionError::MissingPeerIdentity.into());
  }
  token_user(&unsafe { OwnedHandle::from_raw_handle(h) })
}
pub(super) async fn peer(mut stream: &RawStream) -> Result<Identity> {
  if stream.read_u8().await? != 1 {
    return Err(AdmissionError::UnauthorizedIpcPeer.into());
  }
  let RawStream::NamedPipe(pipe) = stream;
  Ok(Identity {
    user: client_user(pipe.as_handle().as_raw_handle())?,
    gid: None,
    pid: pid(stream),
  })
}
