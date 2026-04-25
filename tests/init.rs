mod common;

use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use common::{NCAP, NCAP_INIT, NCAP_SERVER};

#[test]
fn init_sends_shutdown_and_exits() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("init-test.sock");

    let mut child = Command::new(NCAP_INIT)
        .env("NCAP_SOCKET", &socket)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    sleep(Duration::from_millis(100));

    Command::new("kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .output()
        .unwrap();

    let status = child.wait().unwrap();
    assert!(status.success());
}

#[test]
fn init_shutdown_notifies_all_clients() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("test.sock");
    let mut init = Command::new(NCAP_INIT)
        .env("NCAP_SOCKET", &socket)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    sleep(Duration::from_millis(100));
    let mut server = Command::new(NCAP_SERVER)
        .arg("--socket")
        .arg(&socket)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    sleep(Duration::from_millis(200));
    let clients: Vec<Child> = (0..3)
        .map(|_| {
            Command::new(NCAP)
                .arg("--socket")
                .arg(&socket)
                .arg("--")
                .arg("sleep")
                .arg("30")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    sleep(Duration::from_millis(100));
    Command::new("kill")
        .arg("-TERM")
        .arg(init.id().to_string())
        .output()
        .unwrap();
    let status = init.wait().unwrap();
    assert!(status.success());
    for client in clients {
        let output = client.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(78));
    }
    let _ = server.kill();
    let _ = server.wait();
}
