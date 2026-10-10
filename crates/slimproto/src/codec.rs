//! `tokio_util` codec adaptors: decode player → server frames, encode server → player frames.

use crate::client::ClientMessage;
use crate::server::ServerFrame;
use crate::{Error, MAX_CLIENT_FRAME};
use bytes::{Buf, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

/// Framed codec for a single player connection.
#[derive(Debug, Default, Clone, Copy)]
pub struct SlimprotoCodec;

impl Decoder for SlimprotoCodec {
    type Item = ClientMessage;
    type Error = Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<ClientMessage>, Error> {
        if src.len() < 8 {
            return Ok(None);
        }
        let len = u32::from_be_bytes([src[4], src[5], src[6], src[7]]);
        if len > MAX_CLIENT_FRAME {
            return Err(Error::TooLarge(len));
        }
        let total = 8 + len as usize;
        if src.len() < total {
            src.reserve(total - src.len());
            return Ok(None);
        }
        let mut op = [0u8; 4];
        op.copy_from_slice(&src[0..4]);
        src.advance(8);
        let payload = src.split_to(len as usize).freeze();
        ClientMessage::parse(op, payload).map(Some)
    }
}

impl Encoder<ServerFrame> for SlimprotoCodec {
    type Error = Error;

    fn encode(&mut self, item: ServerFrame, dst: &mut BytesMut) -> Result<(), Error> {
        item.encode(dst);
        Ok(())
    }
}

impl Encoder<&ServerFrame> for SlimprotoCodec {
    type Error = Error;

    fn encode(&mut self, item: &ServerFrame, dst: &mut BytesMut) -> Result<(), Error> {
        item.encode(dst);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BufMut;

    #[test]
    fn decodes_partial_and_multiple_frames() {
        let mut codec = SlimprotoCodec;
        let mut buf = BytesMut::new();
        // ANIC with zero payload followed by partial DSCO
        buf.put_slice(b"ANIC");
        buf.put_u32(0);
        buf.put_slice(b"DSCO");
        buf.put_u32(1);
        assert_eq!(codec.decode(&mut buf).unwrap(), Some(ClientMessage::Anic));
        assert_eq!(codec.decode(&mut buf).unwrap(), None);
        buf.put_u8(0);
        assert!(matches!(codec.decode(&mut buf).unwrap(), Some(ClientMessage::Dsco(_))));
        assert!(buf.is_empty());
    }

    #[test]
    fn rejects_huge_frames() {
        let mut codec = SlimprotoCodec;
        let mut buf = BytesMut::new();
        buf.put_slice(b"STAT");
        buf.put_u32(u32::MAX);
        assert!(matches!(codec.decode(&mut buf), Err(Error::TooLarge(_))));
    }
}
