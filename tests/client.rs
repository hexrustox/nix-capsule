mod common;

use common::TestServer;

#[test]
fn exit_code_forwarded_custom() {
    let server = TestServer::start();
    let output = server.run(&["--", "sh", "-c", "exit 42"]);
    assert_eq!(output.status.code(), Some(42));
}
