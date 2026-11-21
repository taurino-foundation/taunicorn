use {
  anyhow::Result,
  futures_util::poll,
  std::time::Duration,
  taunicorn::{Connection, ReceiveResult, Server},
  tokio::time::timeout,
};

async fn listen(case: &str) -> Result<Server> {
  Server::listen(format!("tu-{}-{case}", std::process::id())).await
}

async fn pair(server: &Server) -> Result<(Connection, Connection)> {
  tokio::try_join!(Connection::connect(server.name()), server.accept())
}

async fn read_byte(conn: &Connection) -> Result<u8> {
  let mut byte = [0];
  assert_eq!(conn.receive(&mut byte).await?, ReceiveResult::Data(1));
  Ok(byte[0])
}

#[tokio::test]
async fn queued_reads_make_progress_without_blocking_reverse_io() -> Result<()> {
  timeout(Duration::from_secs(10), async {
    let server = listen("progress").await?;
    let (client, peer) = pair(&server).await?;
    let first = read_byte(&client);
    let second = read_byte(&client);
    tokio::pin!(first, second);
    assert!(poll!(&mut first).is_pending());
    assert!(poll!(&mut second).is_pending());

    // The peer replies only after receiving both whole sends on the busy connection.
    tokio::try_join!(client.send(b"ab"), client.send(b"cd"))?;
    let mut received = [0; 4];
    for byte in &mut received {
      *byte = read_byte(&peer).await?;
    }
    assert!(received == *b"abcd" || received == *b"cdab");
    peer.send(b"12").await?;
    let (a, b) = tokio::try_join!(first, second)?;
    assert!(matches!((a, b), (b'1', b'2') | (b'2', b'1')));
    Ok(())
  })
  .await?
}

#[tokio::test]
async fn write_shutdown_preserves_reply_and_close_ends_stream() -> Result<()> {
  timeout(Duration::from_secs(10), async {
    let server = listen("close").await?;
    let (client, peer) = pair(&server).await?;
    client.send(b"q").await?;
    client.shutdown_write().await?;
    assert!(client.send(b"x").await.is_err());
    assert_eq!(read_byte(&peer).await?, b'q');

    // Some backends signal EOF only on full close. Keep the same receive across both phases.
    let mut byte = [0];
    let eof = peer.receive(&mut byte);
    tokio::pin!(eof);
    let early = timeout(Duration::from_millis(150), &mut eof).await;
    let observed_eof = match early {
      Ok(result) => {
        assert_eq!(result?, ReceiveResult::EndOfStream);
        true
      }
      Err(_) => false,
    };
    eprintln!(
      "{}: peer EOF within 150 ms of write shutdown: {observed_eof}",
      std::env::consts::OS
    );
    peer.send(b"r").await?;
    assert_eq!(read_byte(&client).await?, b'r');
    client.close().await?;
    assert!(client.receive(&mut [0]).await.is_err());
    assert!(client.send(b"x").await.is_err());
    if !observed_eof {
      assert_eq!(eof.await?, ReceiveResult::EndOfStream);
    }
    let (fresh, accepted) = pair(&server).await?;
    fresh.send(b"n").await?;
    assert_eq!(read_byte(&accepted).await?, b'n');
    Ok(())
  })
  .await?
}

#[tokio::test]
async fn read_shutdown_interrupts_waiter_but_still_delivers_outgoing_bytes() -> Result<()> {
  timeout(Duration::from_secs(10), async {
    let server = listen("read-stop").await?;
    let (client, peer) = pair(&server).await?;
    let mut byte = [0];
    let pending = client.receive(&mut byte);
    tokio::pin!(pending);
    assert!(poll!(&mut pending).is_pending());
    client.shutdown_read().await?;
    assert!(pending.await.is_err());
    assert!(client.receive(&mut [0]).await.is_err());
    client.send(b"s").await?;
    assert_eq!(read_byte(&peer).await?, b's');
    Ok(())
  })
  .await?
}
