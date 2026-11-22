use {
  anyhow::Result,
  futures_util::poll,
  std::time::Duration,
  taunicorn::{
    Client, Connection, Server,
    security::{SecureConnection, generate_identity_private_key, identity_public_key},
  },
  tokio::time::timeout,
};

async fn streams(label: &str) -> Result<(Connection, Connection)> {
  let endpoint = format!("taunicorn-secure-{label}-{}", std::process::id());
  let server = Server::listen(endpoint.as_str()).await?;
  tokio::try_join!(Client::connect(endpoint.as_str()), server.accept())
}

async fn sessions(label: &str) -> Result<(SecureConnection, SecureConnection)> {
  let (client, peer) = streams(label).await?;
  let c = generate_identity_private_key()?;
  let s = generate_identity_private_key()?;
  tokio::try_join!(
    SecureConnection::client(client, c, identity_public_key(&s)),
    SecureConnection::server(peer, identity_public_key(&c), s),
  )
}

#[tokio::test]
async fn small_plaintext_limits_preserve_local_rejection_but_close_on_wire_rejection()
-> Result<()> {
  timeout(Duration::from_secs(5), async {
    let (client, peer) = sessions("length").await?;
    assert!(client.send_limited(b"four", 3).await.is_err());
    client.send_limited(b"abc", 3).await?;
    assert_eq!(peer.receive_limited(3).await?, b"abc");
    client.send(b"abcd").await?;
    assert!(peer.receive_limited(3).await.is_err());
    assert!(peer.is_closed());
    Ok(())
  })
  .await?
}

#[tokio::test]
async fn idle_handshake_ends_at_core_deadline() -> Result<()> {
  timeout(Duration::from_secs(15), async {
    let (_peer, server) = streams("deadline").await?;
    let outcome =
      SecureConnection::server(server, [0; 32], generate_identity_private_key()?).await;
    assert!(outcome.err().is_some_and(|e| e.to_string().contains("timed out")));
    Ok(())
  })
  .await?
}

#[tokio::test]
async fn cancelling_active_receive_closes_both_directions_and_wakes_waiter() -> Result<()> {
  timeout(Duration::from_secs(5), async {
    let (client, _peer) = sessions("cancel").await?;
    let mut active = Box::pin(client.receive());
    let mut queued = Box::pin(client.receive());
    assert!(poll!(&mut active).is_pending());
    assert!(poll!(&mut queued).is_pending());
    drop(active);
    assert!(client.connection().is_closed());
    assert!(queued.await.is_err());
    assert!(client.send(b"closed").await.is_err());
    Ok(())
  })
  .await?
}

#[tokio::test]
async fn cancelling_only_a_queued_receive_preserves_duplex() -> Result<()> {
  timeout(Duration::from_secs(5), async {
    let (client, peer) = sessions("duplex").await?;
    let mut active = Box::pin(client.receive());
    assert!(poll!(&mut active).is_pending());
    let mut queued = Box::pin(client.receive());
    assert!(poll!(&mut queued).is_pending());
    drop(queued);
    client.send(b"still open").await?;
    assert_eq!(peer.receive().await?, b"still open");
    peer.send(b"").await?;
    assert!(active.await?.is_empty());
    Ok(())
  })
  .await?
}
