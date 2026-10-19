use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tempfile::TempDir;
use test_case::test_case;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::time::timeout;
use tokio_util::codec::Framed;

use nix_capsule::protocol::{Exit, FrameCodec, Message, Request};

#[tokio::test]
async fn drops_a_client_that_leaves_before_its_first_frame_silently() -> Result<(), Box<dyn Error>>
{
    let server = Server::start().await?;
    let mut conn = UnixStream::connect(server.socket()).await?;
    // A write-half close is what a vanished Client leaves on the Server's read
    // side; the read half stays open to observe how the Connection is dropped.
    conn.shutdown().await?;
    let mut buf = [0u8; 64];
    let read = timeout(Duration::from_secs(5), conn.read(&mut buf)).await;
    let n = read.map_err(|_| "the Server held the Connection open instead of dropping it")??;
    assert_eq!(
        n, 0,
        "expected the Server to send nothing before dropping the Connection, read {n} bytes"
    );
    Ok(())
}

#[tokio::test]
async fn treats_an_empty_stdin_frame_as_stdin_eof_not_a_disconnect() -> Result<(), Box<dyn Error>> {
    let server = Server::start().await?;
    let mut conn = server.connect().await?;
    conn.send(sh_request(server.dir(), "cat; exit 5").into_frame()?)
        .await?;
    conn.send(Message::Stdin(Vec::new()).into_frame()?).await?;
    let msg = recv(&mut conn).await?;
    assert_eq!(
        msg,
        Message::Exit(Exit {
            code: Some(5),
            signal: None
        })
    );
    Ok(())
}

#[tokio::test]
async fn treats_a_client_write_half_close_as_stdin_eof_not_a_disconnect()
-> Result<(), Box<dyn Error>> {
    let server = Server::start().await?;
    let mut conn = server.connect().await?;
    conn.send(sh_request(server.dir(), "cat; exit 5").into_frame()?)
        .await?;
    conn.get_mut().shutdown().await?;
    let msg = recv(&mut conn).await?;
    assert_eq!(
        msg,
        Message::Exit(Exit {
            code: Some(5),
            signal: None
        })
    );
    Ok(())
}

#[tokio::test]
async fn runs_a_silent_child_of_a_vanished_client_to_completion() -> Result<(), Box<dyn Error>> {
    let server = Server::start().await?;
    let mut conn = server.connect().await?;
    let dir = server.dir();
    let marker = dir.join("m");
    let script = format!(
        "trap 'printf termed > {m}; exit 0' TERM
     sleep 1
     printf done > {m}",
        m = marker.display()
    );
    conn.send(sh_request(dir, &script).into_frame()?).await?;
    conn.send(Message::Stdin(Vec::new()).into_frame()?).await?;
    drop(conn);
    wait_for("the silent Child to run to completion", || marker.exists()).await?;
    assert_eq!(fs::read_to_string(&marker)?, "done");
    Ok(())
}

#[tokio::test]
async fn terms_the_child_process_group_when_the_client_vanishes_mid_bridge()
-> Result<(), Box<dyn Error>> {
    let server = Server::start().await?;
    let mut conn = server.connect().await?;
    let dir = server.dir();
    let marker = dir.join("m");
    let grand_marker = dir.join("gm");
    let ready = dir.join("ready");
    // The background `sh` shares the Child's process group; its marker proves
    // the TERM targeted the group rather than just the Child.
    let script = format!(
        "sh -c 'trap \"printf g > {}; exit 0\" TERM; printf x > {}; sleep 30' &
     trap 'printf termed > {}; exit 0' TERM
     while :; do echo x; done",
        grand_marker.display(),
        ready.display(),
        marker.display(),
    );
    conn.send(sh_request(dir, &script).into_frame()?).await?;
    wait_for("the grandchild's trap to be armed", || ready.exists()).await?;
    drop(conn);
    wait_for("the Child's TERM marker", || marker.exists()).await?;
    wait_for("the grandchild's TERM marker", || grand_marker.exists()).await?;
    assert_eq!(fs::read_to_string(&marker)?, "termed");
    assert_eq!(fs::read_to_string(&grand_marker)?, "g");
    Ok(())
}

#[tokio::test]
async fn reaps_the_child_after_a_disconnect_term() -> Result<(), Box<dyn Error>> {
    let server = Server::start().await?;
    let mut conn = server.connect().await?;
    let dir = server.dir();
    let marker = dir.join("m");
    let pidfile = dir.join("child.pid");
    let script = format!(
        "trap 'printf termed > {}; exit 0' TERM
     printf %s $$ > {}
     while :; do echo x; done",
        marker.display(),
        pidfile.display()
    );
    conn.send(sh_request(dir, &script).into_frame()?).await?;
    wait_for("the Child to start", || pidfile.exists()).await?;
    let pid = read_pid(&pidfile)?;
    drop(conn);
    wait_for("the Child's TERM marker", || marker.exists()).await?;
    wait_for(&format!("the Child (pid {pid}) to be reaped"), || {
        !Path::new(&format!("/proc/{pid}")).exists()
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn honors_the_child_term_grace_instead_of_killing_it() -> Result<(), Box<dyn Error>> {
    let server = Server::start().await?;
    let mut conn = server.connect().await?;
    let dir = server.dir();
    let marker = dir.join("m");
    let pidfile = dir.join("child.pid");
    let script = format!(
        "trap 'sleep 2; printf done > {}; exit 0' TERM
     printf %s $$ > {}
     while :; do echo x; done",
        marker.display(),
        pidfile.display()
    );
    conn.send(sh_request(dir, &script).into_frame()?).await?;
    wait_for("the Child to start", || pidfile.exists()).await?;
    drop(conn);
    wait_for("the Child's post-TERM cleanup to finish", || {
        marker.exists()
    })
    .await?;
    assert_eq!(fs::read_to_string(&marker)?, "done");
    Ok(())
}

#[test_case(0x09, b"not json" ; "an_undecodable_mid_bridge_frame")]
#[test_case(0x7f, b"" ; "a_codec_error_mid_bridge_frame")]
#[tokio::test]
async fn sends_a_terminal_error_then_terms_the_child_on_a_mid_bridge_frame_the_protocol_rejects(
    tag: u8,
    payload: &'static [u8],
) -> Result<(), Box<dyn Error>> {
    let server = Server::start().await?;
    let mut conn = server.connect().await?;
    let dir = server.dir();
    let marker = dir.join("m");
    let pidfile = dir.join("child.pid");
    let script = format!(
        "trap 'printf termed > {}; exit 0' TERM
     printf %s $$ > {}
     sleep 30",
        marker.display(),
        pidfile.display()
    );
    conn.send(sh_request(dir, &script).into_frame()?).await?;
    wait_for("the Child to start", || pidfile.exists()).await?;
    // A `Signal` frame is what the bridge decodes mid-bridge; `not json` is
    // the undecodable-payload case (spec/protocol.md: a payload that is not
    // valid JSON for its shape is a decode failure) and `0x7f` the
    // unknown-tag codec-error case.
    conn.get_mut().write_all(&raw_frame(tag, payload)).await?;
    let msg = recv(&mut conn).await?;
    assert!(matches!(&msg, Message::Error(_)), "got {msg:?}");
    let after = timeout(Duration::from_secs(5), conn.next())
        .await
        .map_err(|_| "the Server held the Connection open after the terminal Error")?;
    assert!(
        after.is_none(),
        "expected the Error to be terminal, got {after:?}"
    );
    wait_for("the Child's TERM marker", || marker.exists()).await?;
    assert_eq!(fs::read_to_string(&marker)?, "termed");
    Ok(())
}

#[tokio::test]
async fn keeps_serving_other_connections_during_a_disconnect() -> Result<(), Box<dyn Error>> {
    let server = Server::start().await?;
    let mut vanishing = server.connect().await?;
    let dir = server.dir();
    let pidfile = dir.join("child.pid");
    let script = format!(
        "printf %s $$ > {}
     while :; do echo x; done",
        pidfile.display()
    );
    vanishing
        .send(sh_request(dir, &script).into_frame()?)
        .await?;
    wait_for("the Child to start", || pidfile.exists()).await?;
    drop(vanishing);
    let mut healthy = server.connect().await?;
    healthy
        .send(sh_request(dir, "exit 7").into_frame()?)
        .await?;
    let msg = recv(&mut healthy).await?;
    assert_eq!(
        msg,
        Message::Exit(Exit {
            code: Some(7),
            signal: None
        })
    );
    Ok(())
}

#[tokio::test]
async fn exits_143_when_the_socket_closes_without_a_terminal_frame() -> Result<(), Box<dyn Error>> {
    let mut server = Server::start().await?;
    let dir = server.dir();
    let pidfile = dir.join("child.pid");
    // The Child is silent (pidfile only) so no partial frame is in flight when
    // the Server dies — a clean EOF is exactly "closed without a terminal frame".
    let script = format!(
        "printf %s $$ > {d}/child.pid
     sleep 30",
        d = dir.display()
    );
    let mut client = tokio::process::Command::new(env!("CARGO_BIN_EXE_ncap"))
        .arg("--socket")
        .arg(server.socket())
        .arg("--cwd")
        .arg(dir)
        .arg("--")
        .arg("sh")
        .arg("-c")
        .arg(&script)
        .stdin(std::process::Stdio::null())
        .spawn()?;
    // The pidfile proves the Server received the Request and spawned the
    // Child before the Server is killed mid-command.
    wait_for("the Child to start", || pidfile.exists()).await?;
    server.kill()?;
    let status = client.wait().await?;
    assert_eq!(status.code(), Some(143));
    Ok(())
}

type Conn = Framed<UnixStream, FrameCodec>;

struct Server {
    child: std::process::Child,
    socket: PathBuf,
    dir: TempDir,
}

impl Server {
    async fn start() -> Result<Server, Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        let socket = dir.path().join("ncap.sock");
        let log_dir = dir.path().join("logs");
        let child = std::process::Command::new(env!("CARGO_BIN_EXE_ncap-server"))
            .arg("--socket")
            .arg(&socket)
            .arg("--log-dir")
            .arg(&log_dir)
            .arg("--timeout")
            .arg("2")
            .arg("--log-level")
            .arg("error")
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match UnixStream::connect(&socket).await {
                Ok(_) => break,
                Err(err) if Instant::now() >= deadline => {
                    return Err(format!("the Server socket never came up: {err}").into());
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(25)).await,
            }
        }
        Ok(Server { child, socket, dir })
    }

    async fn connect(&self) -> Result<Conn, Box<dyn Error>> {
        Ok(Framed::new(
            UnixStream::connect(&self.socket).await?,
            FrameCodec,
        ))
    }

    fn dir(&self) -> &Path {
        self.dir.path()
    }

    fn socket(&self) -> &Path {
        &self.socket
    }

    fn kill(&mut self) -> Result<(), Box<dyn Error>> {
        Ok(self.child.kill()?)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let Ok(entries) = fs::read_dir(self.dir.path()) else {
            return;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().ends_with(".pid")
                && let Ok(pid) = read_pid(&entry.path())
            {
                // The Child is its process-group leader, so one kill takes
                // down the whole group — grandchildren included.
                unsafe { libc::kill(-pid, libc::SIGKILL) };
            }
        }
    }
}

fn sh_request(dir: &Path, script: &str) -> Message {
    Message::Request(Request {
        command: "sh".into(),
        args: vec!["-c".into(), script.into()],
        cwd: dir.to_string_lossy().into_owned(),
        env: vec![],
    })
}

async fn recv(conn: &mut Conn) -> Result<Message, Box<dyn Error>> {
    let next = timeout(Duration::from_secs(5), conn.next()).await;
    let frame = next
        .map_err(|_| "timed out waiting for a frame from the Server")?
        .ok_or("the Server closed the Connection without a frame")??;
    Ok(Message::from_frame(frame)?)
}

fn raw_frame(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![tag];
    bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

fn read_pid(path: &Path) -> Result<i32, Box<dyn Error>> {
    Ok(fs::read_to_string(path)?.trim().parse()?)
}

async fn wait_for(what: &str, mut done: impl FnMut() -> bool) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if done() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for {what}").into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
