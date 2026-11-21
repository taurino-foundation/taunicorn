use {
  std::time::Duration,
  taunicorn::{Client, ReceiveResult, Server},
};

#[tokio::test]
async fn listener_and_client_exchange_bytes_in_both_directions() -> anyhow::Result<()> {
  tokio::time::timeout(Duration::from_secs(10), async {
    let endpoint = format!("taunicorn-shared-api-{}", std::process::id());
    let server = Server::listen(endpoint.as_str()).await?;
    let (client, peer) = tokio::try_join!(Client::connect(endpoint.as_str()), server.accept())?;

    tokio::try_join!(client.send(b"\xff"), peer.send(b"\0"))?;

    let mut from_client = [0];
    let mut from_server = [0];
    let (server_read, client_read) =
      tokio::try_join!(peer.receive(&mut from_client), client.receive(&mut from_server))?;
    assert_eq!(server_read, ReceiveResult::Data(1));
    assert_eq!(client_read, ReceiveResult::Data(1));
    assert_eq!(from_client, [255]);
    assert_eq!(from_server, [0]);

    tokio::try_join!(client.close(), peer.close(), server.stop())?;
    Ok(())
  })
  .await?
}
