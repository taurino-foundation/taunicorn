use {
  anyhow::Result,
  std::time::Duration,
  taunicorn::{
    Connection, Server,
    profile::{Audit, Limits, Role, Session, Status},
    security::{SecureConnection, identity_public_key},
  },
};

async fn streams(tag: &str) -> Result<(Connection, Connection)> {
  let name = format!("profile-{tag}-{}", std::process::id());
  let server = Server::listen_private(&name).await?;
  tokio::try_join!(Connection::connect_private(&name), server.accept())
}

const CLIENT: [u8; 32] = [7; 32];
const SERVER: [u8; 32] = [9; 32];

#[cfg(not(feature = "compliance-profile"))]
#[tokio::test]
async fn legacy_endpoint_never_enters_profile() -> Result<()> {
  let name = format!("profile-legacy-{}", std::process::id());
  let server = Server::listen(name.as_str()).await?;
  let (conn, _peer) = tokio::try_join!(Connection::connect(name.as_str()), server.accept())?;
  let (audit, _exporter) = Audit::bounded(8)?;
  let err = Session::open(
    conn,
    Role::Client,
    CLIENT,
    identity_public_key(&SERVER),
    Limits::default(),
    audit,
  )
  .await
  .err()
  .unwrap();
  assert_eq!(err.to_string(), "private admitted endpoint required");
  Ok(())
}

#[tokio::test]
async fn note_limits_and_unsupported_data() -> Result<()> {
  let (audit, _exporter) = Audit::bounded(32)?;
  let (session, peer) = pair("schema-limits", Limits::default(), &audit).await?;
  for size in [255, 256] {
    let text = "x".repeat(size);
    session.send(Status { ready: true, private_note: Some(&text) }).await?;
    assert_eq!(peer.receive().await?, [0x93, 1, 0xc3, 0xc0]);
    peer.send_msgpack(&(1u8, false, text)).await?;
    assert!(!session.receive().await?);
  }
  let text = "x".repeat(257);
  assert!(session.send(Status { ready: true, private_note: Some(&text) }).await.is_err());
  peer.send_msgpack(&(1u8, true, text)).await?;
  assert_eq!(session.receive().await.unwrap_err().to_string(), "unsupported profile message");
  Ok(())
}

#[tokio::test]
async fn receive_checks_remaining_budget_before_reading_body() -> Result<()> {
  let (audit, _exporter) = Audit::bounded(8)?;
  let (session, peer) =
    pair("read-cap", Limits { bytes_per_direction: 4, ..Limits::default() }, &audit).await?;
  // Only the length prefix: a body read would time out, not return the expected I/O error.
  peer.connection().send(&30u32.to_be_bytes()).await?;
  let err = tokio::time::timeout(Duration::from_secs(2), session.receive()).await?.unwrap_err();
  assert_eq!(err.to_string(), "profile I/O failed");
  Ok(())
}

#[tokio::test]
async fn two_profile_roles_make_independent_progress_under_load() -> Result<()> {
  let (audit, _exporter) = Audit::bounded(8)?;
  let (c, s) = streams("duplex").await?;
  let (client, server) = tokio::try_join!(
    Session::open(
      c,
      Role::Client,
      CLIENT,
      identity_public_key(&SERVER),
      Limits::default(),
      audit.clone()
    ),
    Session::open(
      s,
      Role::Server,
      SERVER,
      identity_public_key(&CLIENT),
      Limits::default(),
      audit.clone()
    )
  )?;
  async fn write_many(to: &Session, ready: bool) -> Result<()> {
    for _ in 0..128 {
      to.send(Status { ready, private_note: None }).await?;
    }
    Ok(())
  }
  async fn read_many(from: &Session, ready: bool) -> Result<()> {
    for _ in 0..128 {
      assert_eq!(from.receive().await?, ready);
    }
    Ok(())
  }
  let start = std::time::Instant::now();
  tokio::time::timeout(Duration::from_secs(10), async {
    tokio::try_join!(
      write_many(&client, true),
      write_many(&server, false),
      read_many(&client, false),
      read_many(&server, true)
    )
  })
  .await??;
  println!(
    "256 status messages, full duplex: {:?} (debug smoke, not a baseline)",
    start.elapsed()
  );
  Ok(())
}

const CANARY: &str = "synthetic-person@example.invalid";
const READY: Status<'static> = Status { ready: true, private_note: None };

async fn pair(tag: &str, limits: Limits, audit: &Audit) -> Result<(Session, SecureConnection)> {
  let (conn, raw) = streams(tag).await?;
  tokio::try_join!(
    Session::open(
      conn,
      Role::Client,
      CLIENT,
      identity_public_key(&SERVER),
      limits,
      audit.clone()
    ),
    SecureConnection::server(raw, identity_public_key(&CLIENT), SERVER)
  )
}

#[tokio::test]
async fn disconnected_exporter_denies_admission_before_handshake() -> Result<()> {
  let (audit, exporter) = Audit::bounded(1)?;
  drop(exporter);
  let (conn, _peer) = streams("disconnect").await?;
  let result = Session::open(
    conn,
    Role::Client,
    CLIENT,
    identity_public_key(&SERVER),
    Limits::default(),
    audit.clone(),
  )
  .await;
  assert_eq!(result.err().unwrap().to_string(), "audit unavailable");
  assert!(audit.is_disconnected());
  assert_eq!(audit.dropped(), 1);
  Ok(())
}

#[tokio::test]
async fn full_export_queue_denies_even_an_authenticated_peer() -> Result<()> {
  let (audit, _exporter) = Audit::bounded(1)?;
  assert_eq!(
    pair("full", Limits::default(), &audit).await.err().unwrap().to_string(),
    "audit unavailable"
  );
  assert_eq!(audit.dropped(), 1);
  Ok(())
}

#[tokio::test]
async fn allowlist_filters_on_both_sides_of_encryption() -> Result<()> {
  use taunicorn::profile::EventKind::*;
  let (audit, exporter) = Audit::bounded(16)?;
  let (session, peer) = pair("redaction", Limits::default(), &audit).await?;
  session.send(Status { ready: true, private_note: Some(CANARY) }).await?;
  assert_eq!(peer.receive().await?, rmp_serde::to_vec(&(1u8, true, None::<&str>))?);
  peer.send_msgpack(&(1u8, false, CANARY)).await?;
  assert!(!session.receive().await?);
  peer.send_msgpack(&(2u8, true, CANARY)).await?;
  let err = session.receive().await.unwrap_err();
  assert_eq!(format!("{err:#}"), "unsupported profile message");
  assert!(session.send(READY).await.is_err());
  drop(session);
  drop(audit);
  let events = tokio::task::spawn_blocking(move || {
    let mut events = vec![];
    exporter
      .run(|e| {
        events.push(*e);
        Ok(())
      })
      .unwrap();
    events
  })
  .await?;
  assert_eq!(events.iter().map(|e| e.kind).collect::<Vec<_>>(), [
    AdmissionStarted,
    Admitted,
    Sent,
    Received,
    SchemaRejected
  ]);
  assert!(events.iter().enumerate().all(|(i, e)| e.sequence == i as u64 + 1));
  assert!(!format!("{events:?}").contains(CANARY));
  Ok(())
}

#[tokio::test]
async fn audit_export_failure_disconnects_and_counts_abandoned_events() -> Result<()> {
  let (audit, exporter) = Audit::bounded(16)?;
  let (session, _peer) = pair("export", Limits::default(), &audit).await?;
  let failure =
    tokio::task::spawn_blocking(move || exporter.run(|_| Err(std::io::Error::other(CANARY))))
      .await?
      .unwrap_err();
  assert_eq!(failure.to_string(), "audit export failed");
  assert!(audit.is_disconnected());
  assert_eq!(audit.dropped(), 2);
  session.close();
  assert_eq!(audit.dropped(), 3);
  Ok(())
}

#[tokio::test]
async fn budget_is_independent_per_direction() -> Result<()> {
  let (audit, _exporter) = Audit::bounded(8)?;
  let (session, peer) =
    pair("budget", Limits { bytes_per_direction: 4, ..Limits::default() }, &audit).await?;
  session.send(READY).await?;
  assert_eq!(session.send(READY).await.unwrap_err().to_string(), "profile budget exceeded");
  let msg = peer.receive().await?;
  peer.send(&msg).await?;
  assert!(session.receive().await?);
  assert_eq!(session.receive().await.unwrap_err().to_string(), "profile budget exceeded");
  Ok(())
}

#[tokio::test]
async fn active_cancel_is_terminal_but_queued_cancel_does_not_block_sends() -> Result<()> {
  let (audit, _exporter) = Audit::bounded(16)?;
  let (session, peer) = pair("cancel", Limits::default(), &audit).await?;
  let mut active = Box::pin(session.receive());
  assert!(futures_util::poll!(&mut active).is_pending());
  let mut queued = Box::pin(session.receive());
  assert!(futures_util::poll!(&mut queued).is_pending());
  drop(queued);
  for _ in 0..64 {
    session.send(READY).await?;
    assert_eq!(peer.receive().await?, [0x93, 1, 0xc3, 0xc0]);
  }
  assert!(audit.dropped() > 0);
  drop(active);
  assert!(session.send(READY).await.is_err());
  Ok(())
}

#[tokio::test]
async fn idle_connection_closes_without_another_application_call() -> Result<()> {
  let (audit, _exporter) = Audit::bounded(8)?;
  let (_session, peer) =
    pair("idle", Limits { idle: Duration::from_millis(100), ..Limits::default() }, &audit)
      .await?;
  assert!(tokio::time::timeout(Duration::from_secs(3), peer.receive()).await?.is_err());
  Ok(())
}
