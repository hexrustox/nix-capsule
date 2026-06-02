use bytes::{Buf, BufMut, BytesMut};
use serde::{Deserialize, Serialize};
use tokio_util::codec::{Decoder, Encoder};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    Request = 0x01,
    Stdin = 0x02,
    Stdout = 0x03,
    Stderr = 0x04,
    Exit = 0x05,
    Error = 0x06,
    ServerStopping = 0x07,
}

impl FrameType {
    pub fn from_u8(b: u8) -> Option<Self> {
        match b {
            0x01 => Some(Self::Request),
            0x02 => Some(Self::Stdin),
            0x03 => Some(Self::Stdout),
            0x04 => Some(Self::Stderr),
            0x05 => Some(Self::Exit),
            0x06 => Some(Self::Error),
            0x07 => Some(Self::ServerStopping),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct Frame {
    pub frame_type: FrameType,
    pub payload: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: Vec<String>,
}

impl Request {
    pub fn command_line(&self) -> String {
        std::iter::once(self.command.as_str())
            .chain(self.args.iter().map(|s| s.as_str()))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Exit {
    pub exit_code: i32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorMessage {
    pub error: String,
    pub cause: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ServerStopping {
    pub reason: Option<String>,
}

const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

pub struct FrameCodec;

impl Decoder for FrameCodec {
    type Item = Frame;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < 5 {
            return Ok(None);
        }

        let frame_type_byte = src[0];
        let length = u32::from_be_bytes([src[1], src[2], src[3], src[4]]) as usize;

        if length > MAX_FRAME_SIZE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("frame too large: {length} > {MAX_FRAME_SIZE}"),
            ));
        }

        if src.len() < 5 + length {
            return Ok(None);
        }

        let frame_type = FrameType::from_u8(frame_type_byte).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown frame type: `{frame_type_byte:#x}`"),
            )
        })?;

        src.advance(5);
        let payload = src.split_to(length).to_vec();

        Ok(Some(Frame {
            frame_type,
            payload,
        }))
    }
}

impl Encoder<Frame> for FrameCodec {
    type Error = std::io::Error;

    fn encode(&mut self, item: Frame, dst: &mut BytesMut) -> Result<(), Self::Error> {
        dst.reserve(5 + item.payload.len());
        dst.put_u8(item.frame_type as u8);
        dst.put_u32(item.payload.len() as u32);
        dst.put_slice(&item.payload);
        Ok(())
    }
}
