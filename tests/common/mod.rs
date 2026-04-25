#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::thread::sleep;
use std::time::Duration;

use bytes::BytesMut;
use nix_capsule::protocol::{Frame, FrameCodec, FrameType};
use tokio_util::codec::Encoder;

pub const NCAP: &str = env!("CARGO_BIN_EXE_ncap");
pub const NCAP_SERVER: &str = env!("CARGO_BIN_EXE_ncap-server");
pub const NCAP_INIT: &str = env!("CARGO_BIN_EXE_ncap-init");

pub struct TestServer {
    pub socket: PathBuf,
    _dir: tempfile::TempDir,
    child: Child,
}

impl TestServer {
    pub fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("test.sock");

        let child = Command::new(NCAP_SERVER)
            .arg("--socket")
            .arg(&socket)
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        sleep(Duration::from_millis(100));

        Self {
            socket,
            _dir: dir,
            child,
        }
    }

    pub fn ncap_cmd(&self) -> Command {
        let mut cmd = Command::new(NCAP);
        cmd.arg("--socket").arg(&self.socket);
        cmd
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.ncap_cmd().args(args).output().unwrap()
    }

    pub fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        std::mem::forget(self);
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn assert_line(reader: &mut BufReader<impl Read>, line: &mut String, expected: &str) {
    line.clear();
    reader.read_line(line).unwrap();
    assert_eq!(line.trim(), expected);
}

pub fn write_line(stdin: &mut impl Write, input: &str) {
    writeln!(stdin, "{input}").unwrap();
    stdin.flush().unwrap();
}

pub fn send_request_shutdown(socket: &std::path::Path) {
    let frame = Frame {
        frame_type: FrameType::RequestShutdown,
        payload: vec![],
    };
    let mut buf = BytesMut::new();
    FrameCodec.encode(frame, &mut buf).unwrap();
    let mut stream = UnixStream::connect(socket).unwrap();
    stream.write_all(&buf).unwrap();
}
