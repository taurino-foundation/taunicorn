use {
  super::*,
  crate::{Connection, Server},
};

#[test]
fn missing_or_wrong_required_identity_is_not_a_match() {
  let local = Principal::Uid(5);
  let peer = Identity { user: local.clone(), gid: Some(9), pid: None };
  let mut policy = IpcCompliancePolicyEngine::default();
  assert!(policy.check(&local, &peer).is_ok());
  assert_eq!(policy.check(&Principal::Uid(6), &peer), Err(AdmissionError::UnauthorizedIpcPeer));
  policy.allowed_pids = Some(vec![8]);
  assert_eq!(policy.check(&local, &peer), Err(AdmissionError::MissingPeerIdentity));
  policy.allowed_pids = None;
  policy.allowed_gids = Some(vec![]);
  assert_eq!(policy.check(&local, &peer), Err(AdmissionError::UnauthorizedIpcPeer));
  policy.allowed_gids = Some(vec![9]);
  policy.allowed_sids = Some(vec!["S-1-5-18".into()]);
  assert_eq!(policy.check(&local, &peer), Err(AdmissionError::MissingPeerIdentity));
}

#[test]
fn quota_counts_users_not_processes_and_releases_on_drop() {
  let counts = Arc::new(Mutex::new(Counts::default()));
  let take = |uid| Lease::reserve(&counts, Principal::Uid(uid), 2, 1);
  let first = take(1).unwrap();
  assert!(matches!(take(1), Err(AdmissionError::StreamQuotaExceeded)));
  let other = take(2).unwrap();
  assert!(matches!(take(3), Err(AdmissionError::StreamQuotaExceeded)));
  drop(first);
  drop(other);
  assert!(take(1).is_ok());
  assert!(counts.lock().unwrap().users.is_empty());
}

#[tokio::test]
async fn normal_accept_admits_bytes_and_rejects_disallowed_peer() -> anyhow::Result<()> {
  let name = format!("admission-{}-{}", std::process::id(), crate::next_socket_id());
  let server = Server::listen_private(&name).await?;
  assert!(Server::listen_private(&name).await.is_err());
  let (client, peer) = tokio::try_join!(Connection::connect_private(&name), server.accept())?;
  client.send(b"a").await?;
  assert_eq!(peer.read_bytes(1).await?, b"a");
  peer.send(b"b").await?;
  assert_eq!(client.read_bytes(1).await?, b"b");
  // Denied clients never receive a public stream, including when the server is otherwise valid.
  let deny = IpcCompliancePolicyEngine { max_connections: 0, ..Default::default() };
  let err = Connection::connect_with_policy(&name, deny).await.err().unwrap();
  assert_eq!(err.downcast_ref(), Some(&AdmissionError::StreamQuotaExceeded));
  let weak = Arc::downgrade(&peer.core.stream()?);
  assert!(weak.upgrade().unwrap()._lease.is_some());
  let (read, write) = peer.into_split();
  drop(read);
  assert!(weak.upgrade().is_some());
  drop(write);
  assert!(weak.upgrade().is_none());
  let weak = Arc::downgrade(&client.core.stream()?);
  client.close().await?;
  assert!(weak.upgrade().is_none());
  let policy = IpcCompliancePolicyEngine { allowed_pids: Some(vec![]), ..Default::default() };
  let name = format!("{name}-deny");
  let server = Server::listen_with_policy(&name, policy).await?;
  let (_client, peer) = tokio::join!(Connection::connect_private(&name), server.accept());
  let err = peer.err().expect("ordinary accept must apply policy");
  assert!(matches!(
    err.downcast_ref::<AdmissionError>(),
    Some(AdmissionError::UnauthorizedIpcPeer | AdmissionError::MissingPeerIdentity)
  ));
  Ok(())
}

#[cfg(windows)]
#[test]
fn locked_pipe_backend_defaults_to_local_clients_only() {
  use interprocess::os::windows::named_pipe::PipeListenerOptions;
  assert!(!PipeListenerOptions::new().accept_remote);
}

#[cfg(feature = "compliance-profile")]
#[tokio::test]
async fn profile_routes_legacy_names_and_rejects_overrides() {
  let e = Server::listen("../invalid").await.err().unwrap();
  assert_eq!(e.downcast_ref(), Some(&AdmissionError::InvalidEndpoint));
  let e = Connection::connect("../invalid").await.err().unwrap();
  assert_eq!(e.downcast_ref(), Some(&AdmissionError::InvalidEndpoint));
  let e = Server::bind("override", Some(Default::default())).await.err().unwrap();
  assert_eq!(e.downcast_ref(), Some(&AdmissionError::InsecureSocketPermissions));
}
