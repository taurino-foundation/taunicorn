use tokio::io::{AsyncReadExt, AsyncWriteExt};
use std::io;
use serde::{Serialize, de::DeserializeOwned};



pub const FRAME_HEADER_LEN: usize = 4;
pub const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024; // 16 MB Schutz

/// Sendet exakt einen Frame (length-prefixed)
pub async fn write_frame<W>(writer: &mut W, payload: &[u8]) -> io::Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    let len = payload.len();

    if len > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }

    writer.write_all(&(len as u32).to_be_bytes()).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;

    Ok(())
}



pub async fn read_frame<R>(reader: &mut R) -> io::Result<Option<Vec<u8>>>
where
    R: AsyncReadExt + Unpin,
{
    let mut header = [0u8; FRAME_HEADER_LEN];

    // EOF → None
    if let Err(e) = reader.read_exact(&mut header).await {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            return Ok(None);
        }
        return Err(e);
    }

    let len = u32::from_be_bytes(header) as usize;

    if len > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }

    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;

    Ok(Some(buf))
}





pub async fn write_json<W, T>(writer: &mut W, value: &T) -> io::Result<()>
where
    W: AsyncWriteExt + Unpin,
    T: Serialize,
{
    let data = serde_json::to_vec(value)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write_frame(writer, &data).await
}

pub async fn read_json<R, T>(reader: &mut R) -> io::Result<Option<T>>
where
    R: AsyncReadExt + Unpin,
    T: DeserializeOwned,
{
    match read_frame(reader).await? {
        Some(data) => {
            let v = serde_json::from_slice(&data)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            Ok(Some(v))
        }
        None => Ok(None),
    }
}

/* 

use crate::utils::framing::write_frame;

while let Some(payload) = guard.recv().await {
    if write_frame(&mut writer, &payload).await.is_err() {
        break;
    }
}



use crate::utils::framing::read_frame;

loop {
    match read_frame(&mut reader).await {
        Ok(Some(frame)) => {
            let _ = tx_read.send(frame).await;
        }
        Ok(None) => break, // EOF
        Err(_) => break,
    }
}


*/


/* 

use tokio_util::codec::{Decoder, Encoder};
use bytes::{BytesMut, Buf, BufMut};
use std::io;

pub struct FrameCodec;

impl Decoder for FrameCodec {
    type Item = Vec<u8>;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < 4 {
            return Ok(None);
        }

        let len = u32::from_be_bytes(src[..4].try_into().unwrap()) as usize;

        if src.len() < 4 + len {
            return Ok(None);
        }

        src.advance(4);
        let data = src.split_to(len).to_vec();

        Ok(Some(data))
    }
}

impl Encoder<Vec<u8>> for FrameCodec {
    type Error = io::Error;

    fn encode(&mut self, item: Vec<u8>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        dst.put_u32(item.len() as u32);
        dst.extend_from_slice(&item);
        Ok(())
    }
}



*/