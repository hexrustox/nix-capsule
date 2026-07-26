use bytes::{Buf, BufMut, BytesMut};
use serde::{Deserialize, Serialize};
use std::fmt;
use tokio_util::codec::{Decoder, Encoder};

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub const SIGTERM_EXIT: u8 = 143;

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
            _ => None,
        }
    }

    pub fn to_byte(self) -> u8 {
        self as u8
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
    #[serde(default)]
    pub version: Option<String>,
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

#[derive(Debug, Serialize, Deserialize)]
pub struct VersionMsg {
    pub version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Client,
    Server,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionCheck {
    Match,
    Mismatch { client: String, server: String },
    ClientMissing { server: String },
    ServerMissing { client: String },
}

impl VersionCheck {
    pub fn from(observed: Option<&VersionMsg>, our_version: &str, role: Role) -> Self {
        match (observed, role) {
            (Some(msg), Role::Client) => {
                if msg.version == our_version {
                    Self::Match
                } else {
                    Self::Mismatch {
                        client: our_version.to_string(),
                        server: msg.version.clone(),
                    }
                }
            }
            (Some(msg), Role::Server) => {
                if msg.version == our_version {
                    Self::Match
                } else {
                    Self::Mismatch {
                        client: msg.version.clone(),
                        server: our_version.to_string(),
                    }
                }
            }
            (None, Role::Client) => Self::ServerMissing {
                client: our_version.to_string(),
            },
            (None, Role::Server) => Self::ClientMissing {
                server: our_version.to_string(),
            },
        }
    }
}

pub fn exit_code_from(status: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code), _) => code,
        (None, Some(signal)) => 128 + signal,
        (None, None) => 1,
    }
}

#[derive(Debug)]
pub enum Message {
    Request(Request),
    Stdin(Vec<u8>),
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Exit(Exit),
    Error(ErrorMessage),
    ServerStopping(ServerStopping),
    Version(VersionMsg),
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
            Self::ServerStopping(_) => FrameType::ServerStopping,
            Self::Version(_) => FrameType::Version,
        }
    }

    pub fn into_frame(self) -> Result<Frame, DecodeError> {
        let frame_type = self.frame_type();
        let payload = match self {
            Self::Stdin(b) | Self::Stdout(b) | Self::Stderr(b) => b,
            Self::Request(m) => serde_json::to_vec(&m)?,
            Self::Exit(m) => serde_json::to_vec(&m)?,
            Self::Error(m) => serde_json::to_vec(&m)?,
            Self::ServerStopping(m) => serde_json::to_vec(&m)?,
            Self::Version(m) => serde_json::to_vec(&m)?,
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
            FrameType::ServerStopping => Self::ServerStopping(serde_json::from_slice(&payload)?),
            FrameType::Version => Self::Version(serde_json::from_slice(&payload)?),
        })
    }
}

#[derive(Debug)]
pub enum DecodeError {
    UnknownFrameType(u8),
    Json(serde_json::Error),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownFrameType(b) => write!(f, "unknown frame type: `{b:#x}`"),
            Self::Json(e) => write!(f, "frame payload parse error: {e}"),
        }
    }
}

impl std::error::Error for DecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for DecodeError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

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
        dst.put_u8(item.frame_type.to_byte());
        dst.put_u32(item.payload.len() as u32);
        dst.put_slice(&item.payload);
        Ok(())
    }
}
