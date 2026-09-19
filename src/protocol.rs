//! v2 wire protocol: framing, frame types, and typed payloads.
//!
//! Every frame is one tag byte, a 4-byte big-endian payload length, then the
//! payload. Struct frames carry JSON; stream frames carry raw bytes.

use bytes::{Buf, BufMut, BytesMut};
use serde::{Deserialize, Serialize};
use tokio_util::codec::{Decoder, Encoder};

/// 16 MiB.
pub const MAX_PAYLOAD: usize = 16 * 1024 * 1024;

/// Embedded from `CARGO_PKG_VERSION` at build time.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Each byte value is part of the protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    /// Client → server: run this command.
    Request = 0x01,
    /// Client → server: stdin bytes for the child.
    Stdin = 0x02,
    /// Server → client: stdout bytes from the child.
    Stdout = 0x03,
    /// Server → client: stderr bytes from the child.
    Stderr = 0x04,
    /// Server → client: the child's terminal status.
    Exit = 0x05,
    /// Server → client: failure without an exit status.
    Error = 0x06,
    /// Server → client: orderly shutdown notice.
    ServerStopping = 0x07,
    /// Reserved — sent by no one; a receiver ignores it wherever it arrives.
    Version = 0x08,
    /// Client → server: forwarded host signal.
    Signal = 0x09,
    /// Client → server: opens the Version probe; exactly 0 bytes.
    RequestVersion = 0x0A,
    /// Server → client: the Server's version, as the probe's reply.
    ServerVersion = 0x0B,
}

impl FrameType {
    /// `None` for unknown tags.
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
            0x0A => Some(Self::RequestVersion),
            0x0B => Some(Self::ServerVersion),
            _ => None,
        }
    }

    /// The wire tag byte.
    pub fn to_byte(self) -> u8 {
        self as u8
    }
}

/// A wire frame: tag, big-endian length, raw payload.
#[derive(Debug)]
pub struct Frame {
    /// Decoded tag byte.
    pub frame_type: FrameType,
    /// JSON for struct frames, raw bytes for stream frames.
    pub payload: Vec<u8>,
}

/// Client → server: the command to run inside the container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// Executable, not a shell line.
    pub command: String,
    /// Argv after the executable.
    pub args: Vec<String>,
    /// Working directory for the child.
    pub cwd: String,
    /// `KEY=VALUE` entries applied by the server over its environment.
    pub env: Vec<String>,
}

/// Exactly one field is set in practice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exit {
    /// On normal termination.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<u8>,
    /// On death by signal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<u8>,
}

/// Server → client: the connection failed without an exit status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorMsg {
    /// Human-readable failure description.
    pub message: String,
}

/// The Version probe's reply payload shape, and the reserved `Version`
/// frame's payload (carried only so receivers can ignore it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionMsg {
    /// Sender's protocol version.
    pub version: String,
}

/// A host-shell signal forwarded toward the child.
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
    /// Reserved — sent by no one; carried only so a receiver can ignore it.
    Version(VersionMsg),
    /// Client → server: a forwarded host signal.
    Signal(SignalMsg),
    /// Client → server: opens the Version probe. Empty payload.
    RequestVersion,
    /// Server → client: the Server's version, as the probe's reply.
    ServerVersion(VersionMsg),
}

impl Message {
    /// The wire tag byte for this message.
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
            Self::RequestVersion => FrameType::RequestVersion,
            Self::ServerVersion(_) => FrameType::ServerVersion,
        }
    }

    /// Fails if an `Exit` carries both `code` and `signal`.
    pub fn into_frame(self) -> Result<Frame, EncodeError> {
        // `None`/`None` stays valid: an unknowable status.
        if let Self::Exit(Exit {
            code: Some(_),
            signal: Some(_),
        }) = &self
        {
            return Err(EncodeError::InvalidExit);
        }
        let frame_type = self.frame_type();
        let payload = match self {
            Self::Stdin(b) | Self::Stdout(b) | Self::Stderr(b) => b,
            Self::Request(m) => serde_json::to_vec(&m)?,
            Self::Exit(m) => serde_json::to_vec(&m)?,
            Self::Error(m) => serde_json::to_vec(&m)?,
            Self::ServerStopping => Vec::new(),
            Self::Version(m) => serde_json::to_vec(&m)?,
            Self::Signal(m) => serde_json::to_vec(&m)?,
            Self::RequestVersion => Vec::new(),
            Self::ServerVersion(m) => serde_json::to_vec(&m)?,
        };
        Ok(Frame {
            frame_type,
            payload,
        })
    }

    /// Rejects a non-empty `ServerStopping` payload.
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
            FrameType::Exit => {
                let exit: Exit = serde_json::from_slice(&payload)?;
                if exit.code.is_some() && exit.signal.is_some() {
                    return Err(DecodeError::InvalidExit);
                }
                Self::Exit(exit)
            }
            FrameType::Error => Self::Error(serde_json::from_slice(&payload)?),
            FrameType::ServerStopping => {
                if !payload.is_empty() {
                    return Err(DecodeError::NonEmptyServerStopping(payload.len()));
                }
                Self::ServerStopping
            }
            FrameType::Version => Self::Version(serde_json::from_slice(&payload)?),
            FrameType::Signal => Self::Signal(serde_json::from_slice(&payload)?),
            FrameType::RequestVersion => {
                if !payload.is_empty() {
                    return Err(DecodeError::NonEmptyRequestVersion(payload.len()));
                }
                Self::RequestVersion
            }
            FrameType::ServerVersion => Self::ServerVersion(serde_json::from_slice(&payload)?),
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
    #[error("frame payload parse error: {source}")]
    Json {
        /// The underlying JSON parse failure.
        #[from]
        #[source]
        source: serde_json::Error,
    },
    /// `ServerStopping` arrived with a non-empty payload (spec: empty).
    #[error("non-empty `ServerStopping` payload: {0} bytes")]
    NonEmptyServerStopping(usize),
    /// `Exit` sets both `code` and `signal` (spec: exactly one in practice).
    #[error("both `code` and `signal` set in `Exit` frame")]
    InvalidExit,
    /// `RequestVersion` arrived with a non-empty payload (spec: empty).
    #[error("non-empty `RequestVersion` payload: {0} bytes")]
    NonEmptyRequestVersion(usize),
    /// Reading a frame off the socket failed.
    #[error(transparent)]
    Read {
        /// The underlying socket read failure.
        #[from]
        source: std::io::Error,
    },
}

/// Failure while encoding a frame onto the wire.
#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    /// The payload exceeds [`MAX_PAYLOAD`].
    #[error("frame payload above the 16 MiB cap: {0} bytes")]
    PayloadTooLarge(usize),
    /// A JSON struct payload failed to serialize.
    #[error("frame payload serialization error: {source}")]
    Json {
        /// The underlying JSON serialization failure.
        #[from]
        #[source]
        source: serde_json::Error,
    },
    /// `Exit` sets both `code` and `signal` (spec: exactly one in practice).
    #[error("both `code` and `signal` set in `Exit` frame")]
    InvalidExit,
    /// Writing a frame to the socket failed.
    #[error(transparent)]
    Write {
        /// The underlying socket write failure.
        #[from]
        source: std::io::Error,
    },
}

/// Shared by both ends of the socket.
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
