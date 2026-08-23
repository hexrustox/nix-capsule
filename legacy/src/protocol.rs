use bytes::{Buf, BufMut, BytesMut};
use serde::{Deserialize, Serialize};
use tokio_util::codec::{Decoder, Encoder};

/// Current protocol version, embedded from `CARGO_PKG_VERSION` at build time.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Exit code the client surfaces when the server shuts down its connection
/// (shell-visible code for SIGTERM: `128 + 15`).
pub const SIGTERM_EXIT: u8 = 143;

/// Wire frame type tag. Each byte value is part of the on-disk protocol.
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
    /// Frame type tag.
    pub frame_type: FrameType,
    /// Raw payload bytes (JSON for struct variants, raw bytes for stdio frames).
    pub payload: Vec<u8>,
}

/// Client → server: the command to run inside the container.
#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    /// Executable to run.
    pub command: String,
    /// Arguments passed to the executable.
    pub args: Vec<String>,
    /// Working directory for the command.
    pub cwd: String,
    /// `KEY=VALUE` environment overrides in insertion order.
    pub env: Vec<String>,
    /// Optional protocol version, sent for version negotiation.
    #[serde(default)]
    pub version: Option<String>,
}

impl Request {
    /// One-line human-readable rendering of the command and its arguments.
    pub fn command_line(&self) -> String {
        std::iter::once(self.command.as_str())
            .chain(self.args.iter().map(|s| s.as_str()))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Server → client: the executed command's final exit status.
#[derive(Debug, Serialize, Deserialize)]
pub struct Exit {
    /// Process exit code.
    pub exit_code: i32,
}

/// Server → client: the command could not be run, or failed in a way that has
/// no exit code.
#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorMessage {
    /// The error message.
    pub error: String,
    /// Optional underlying cause.
    pub cause: Option<String>,
}

/// Server → client: the server is shutting down and is terminating the child.
#[derive(Debug, Serialize, Deserialize)]
pub struct ServerStopping {
    /// Optional reason for the shutdown.
    pub reason: Option<String>,
}

/// Either side → the other: the sender's protocol version.
#[derive(Debug, Serialize, Deserialize)]
pub struct VersionMsg {
    /// Protocol version string.
    pub version: String,
}

/// Which side of the connection is comparing versions, used to label
/// warnings from the correct perspective.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The `ncap` client.
    Client,
    /// The `ncap-server` daemon.
    Server,
}

/// Result of comparing our protocol version against the peer's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionCheck {
    Match,
    Mismatch { client: String, server: String },
    ClientMissing { server: String },
    ServerMissing { client: String },
}

impl VersionCheck {
    /// Compare an observed peer version against ours from the given side's
    /// perspective.
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

    /// Render a human-readable warning for a non-matching check, or `None`
    /// when both sides agree.
    pub fn warning_message(&self) -> Option<String> {
        match self {
            Self::Match => None,
            Self::Mismatch { client, server } => Some(format!(
                "client/server version mismatch (client={client}, server={server})"
            )),
            Self::ServerMissing { client } => {
                Some(format!("server did not send version (client={client})"))
            }
            Self::ClientMissing { server } => {
                Some(format!("client did not send version (server={server})"))
            }
        }
    }
}

/// Convert an [`std::process::ExitStatus`] to the code the shell would observe:
/// the exit code if set, otherwise `128 + signal` when killed by a signal.
pub fn exit_code_from(status: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code), _) => code,
        (None, Some(signal)) => 128 + signal,
        (None, None) => 1,
    }
}

/// A typed message carried by a [`Frame`].
#[derive(Debug)]
pub enum Message {
    /// Client → server: the request to run a command.
    Request(Request),
    /// Client → server: raw stdin bytes for the child.
    Stdin(Vec<u8>),
    /// Server → client: raw stdout bytes from the child.
    Stdout(Vec<u8>),
    /// Server → client: raw stderr bytes from the child.
    Stderr(Vec<u8>),
    /// Server → client: the child exited with a final code.
    Exit(Exit),
    /// Server → client: the command failed without an exit code.
    Error(ErrorMessage),
    /// Server → client: the server is shutting down.
    ServerStopping(ServerStopping),
    /// Either side → the other: version handshake.
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

/// Error decoding a frame payload.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// The frame type byte is not a recognized variant.
    #[error("unknown frame type: `{0:#x}`")]
    UnknownFrameType(u8),
    /// A JSON-encoded variant failed to parse/encode.
    #[error("frame payload parse error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Five-byte framed encoding: one tag byte, a big-endian length, then the
/// payload. Shared by the codecs on both ends of the socket.
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

/// Encoder half of [`FrameCodec`]: write tag, length, then payload bytes.
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
