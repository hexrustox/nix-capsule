//! v2 wire protocol: framing, frame types, and typed payloads.
//!
//! Every frame is one tag byte, a 4-byte big-endian payload length, then the
//! payload. Struct frames carry JSON; stream frames carry raw bytes.

use bytes::{Buf, BufMut, BytesMut};
use serde::{Deserialize, Serialize};
use tokio_util::codec::{Decoder, Encoder};

/// Largest payload a single frame may carry (16 MiB).
pub const MAX_PAYLOAD: usize = 16 * 1024 * 1024;

/// Current protocol version, embedded from `CARGO_PKG_VERSION` at build time.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Wire frame type tag. Each byte value is part of the protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    Request = 0x01,
    Stdin = 0x02,
    Stdout = 0x03,
    Stderr = 0x04,
    Exit = 0x05,
    Error = 0x06,
    ServerStopping = 0x07,
    Version = 0x08,
    Signal = 0x09,
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
            0x08 => Some(Self::Version),
            0x09 => Some(Self::Signal),
            _ => None,
        }
    }

    pub fn to_byte(self) -> u8 {
        self as u8
    }
}

/// A wire frame: tag, big-endian length, raw payload.
#[derive(Debug)]
pub struct Frame {
    /// Frame type tag.
    pub frame_type: FrameType,
    /// Raw payload bytes (JSON for struct frames, raw bytes for stream frames).
    pub payload: Vec<u8>,
}

/// Client → server: the command to run inside the container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// Executable to run.
    pub command: String,
    /// Arguments passed to the executable.
    pub args: Vec<String>,
    /// Working directory for the command.
    pub cwd: String,
    /// `KEY=VALUE` entries applied by the server over its environment.
    pub env: Vec<String>,
    /// Sender's protocol version, when it opts into the handshake.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Server → client: terminal status of the executed command.
/// Exactly one field is set in practice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exit {
    /// Process exit code on normal termination.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<u8>,
    /// POSIX signal number on death by signal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<u8>,
}

/// Server → client: the connection failed without an exit status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorMsg {
    /// What went wrong.
    pub message: String,
}

/// Either side → the other: the sender's protocol version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionMsg {
    /// Version string; compared by exact equality, advisory only.
    pub version: String,
}

/// Client → server: a host-shell signal forwarded toward the child.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalMsg {
    /// POSIX signal number (e.g. 2, 9, 15).
    pub signal: u8,
}

/// A typed message carried by a [`Frame`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Client → server: the request to run a command.
    Request(Request),
    /// Client → server: raw stdin bytes for the child.
    Stdin(Vec<u8>),
    /// Server → client: raw stdout bytes from the child.
    Stdout(Vec<u8>),
    /// Server → client: raw stderr bytes from the child.
    Stderr(Vec<u8>),
    /// Server → client: the child's terminal status.
    Exit(Exit),
    /// Server → client: failure without an exit status.
    Error(ErrorMsg),
    /// Server → client: the server is shutting down. Empty payload.
    ServerStopping,
    /// Either side → the other: version handshake.
    Version(VersionMsg),
    /// Client → server: a forwarded host signal.
    Signal(SignalMsg),
}

impl Message {
    pub fn frame_type(&self) -> FrameType {
        match self {
            Self::Request(_) => FrameType::Request,
            Self::Stdin(_) => FrameType::Stdin,
            Self::Stdout(_) => FrameType::Stdout,
            Self::Stderr(_) => FrameType::Stderr,
            Self::Exit(_) => FrameType::Exit,
            Self::Error(_) => FrameType::Error,
            Self::ServerStopping => FrameType::ServerStopping,
            Self::Version(_) => FrameType::Version,
            Self::Signal(_) => FrameType::Signal,
        }
    }

    pub fn into_frame(self) -> Result<Frame, EncodeError> {
        let frame_type = self.frame_type();
        let payload = match self {
            Self::Stdin(b) | Self::Stdout(b) | Self::Stderr(b) => b,
            Self::Request(m) => serde_json::to_vec(&m)?,
            Self::Exit(m) => serde_json::to_vec(&m)?,
            Self::Error(m) => serde_json::to_vec(&m)?,
            Self::ServerStopping => Vec::new(),
            Self::Version(m) => serde_json::to_vec(&m)?,
            Self::Signal(m) => serde_json::to_vec(&m)?,
        };
        Ok(Frame {
            frame_type,
            payload,
        })
    }

    pub fn from_frame(frame: Frame) -> Result<Self, DecodeError> {
        let Frame {
            frame_type,
            payload,
        } = frame;
        Ok(match frame_type {
            FrameType::Request => Self::Request(serde_json::from_slice(&payload)?),
            FrameType::Stdin => Self::Stdin(payload),
            FrameType::Stdout => Self::Stdout(payload),
            FrameType::Stderr => Self::Stderr(payload),
            FrameType::Exit => Self::Exit(serde_json::from_slice(&payload)?),
            FrameType::Error => Self::Error(serde_json::from_slice(&payload)?),
            FrameType::ServerStopping => Self::ServerStopping,
            FrameType::Version => Self::Version(serde_json::from_slice(&payload)?),
            FrameType::Signal => Self::Signal(serde_json::from_slice(&payload)?),
        })
    }
}

/// Failure while decoding a frame off the wire.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// The tag byte does not name a known frame.
    #[error("unknown frame tag: `{0:#x}`")]
    UnknownFrameType(u8),
    /// The declared payload length exceeds [`MAX_PAYLOAD`].
    #[error("frame declares a {0}-byte payload above the 16 MiB cap")]
    PayloadTooLarge(usize),
    /// A JSON struct payload failed to parse.
    #[error("frame payload parse error: {0}")]
    Json(#[from] serde_json::Error),
    /// Underlying I/O failure.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// Failure while encoding a frame onto the wire.
#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    /// The payload exceeds [`MAX_PAYLOAD`].
    #[error("frame payload above the 16 MiB cap: {0} bytes")]
    PayloadTooLarge(usize),
    /// A JSON struct payload failed to serialize.
    #[error("frame payload serialization error: {0}")]
    Json(#[from] serde_json::Error),
    /// Underlying I/O failure.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// Five-byte framed codec: one tag byte, a big-endian length, then the
/// payload. Shared by both ends of the socket.
pub struct FrameCodec;

impl FrameCodec {
    /// Parse the 5-byte header. Returns `Ok(None)` while the header is
    /// incomplete, `Err` on a transport violation (oversized or unknown tag)
    /// so the connection fails before any payload bytes are buffered.
    fn header(src: &BytesMut) -> Result<Option<(FrameType, usize)>, DecodeError> {
        if src.len() < 5 {
            return Ok(None);
        }
        let length = u32::from_be_bytes([src[1], src[2], src[3], src[4]]) as usize;
        if length > MAX_PAYLOAD {
            return Err(DecodeError::PayloadTooLarge(length));
        }
        let frame_type = FrameType::from_u8(src[0]).ok_or(DecodeError::UnknownFrameType(src[0]))?;
        Ok(Some((frame_type, length)))
    }
}

impl Decoder for FrameCodec {
    type Item = Frame;
    type Error = DecodeError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let Some((frame_type, length)) = Self::header(src)? else {
            return Ok(None);
        };
        if src.len() < 5 + length {
            return Ok(None);
        }
        src.advance(5);
        let payload = src.split_to(length).to_vec();
        Ok(Some(Frame {
            frame_type,
            payload,
        }))
    }
}

impl Encoder<Frame> for FrameCodec {
    type Error = EncodeError;

    fn encode(&mut self, item: Frame, dst: &mut BytesMut) -> Result<(), Self::Error> {
        if item.payload.len() > MAX_PAYLOAD {
            return Err(EncodeError::PayloadTooLarge(item.payload.len()));
        }
        dst.reserve(5 + item.payload.len());
        dst.put_u8(item.frame_type.to_byte());
        dst.put_u32(item.payload.len() as u32);
        dst.put_slice(&item.payload);
        Ok(())
    }
}
