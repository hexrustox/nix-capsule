use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

const NCAP_INIT: &str = env!("CARGO_BIN_EXE_ncap-init");
const NCAP: &str = env!("CARGO_BIN_EXE_ncap");
const NCAP_SERVER: &str = env!("CARGO_BIN_EXE_ncap-server");

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
    // 1. Spawn init
    let mut init = Command::new(NCAP_INIT)
        .env("NCAP_SOCKET", &socket)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    sleep(Duration::from_millis(100));
    // 2. Spawn server
    let mut server = Command::new(NCAP_SERVER)
        .arg("--socket")
        .arg(&socket)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    sleep(Duration::from_millis(200));
    // 3. Spawn 3 long-running clients
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
    // 4. SIGTERM init → triggers full shutdown chain
    Command::new("kill")
        .arg("-TERM")
        .arg(init.id().to_string())
        .output()
        .unwrap();
    // 5. Init exits clean
    let status = init.wait().unwrap();
    assert!(status.success());
    // 6. All clients received ServerStopping
    for client in clients {
        let output = client.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(78));
    }
    // 7. Cleanup server
    let _ = server.kill();
    let _ = server.wait();
}
