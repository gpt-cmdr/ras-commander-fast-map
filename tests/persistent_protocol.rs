//! Persistent process protocol termination and framing without external models.
use std::io::Write;
use std::process::{Command, Stdio};

fn command(input: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ras2fim-batch"))
        .arg("sloping-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(input);
    child.wait_with_output().unwrap()
}

#[test]
fn close_and_eof_release_the_owned_session() {
    for input in [b"{\"close\":true}\n".as_slice(), b"".as_slice()] {
        let output = command(input);
        assert!(output.status.success(), "{:?}", output);
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn malformed_or_oversized_commands_fail_without_success_frames() {
    for input in [b"invalid\n".to_vec(), vec![b' '; 65537], b"{}\n".to_vec()] {
        let output = command(&input);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
}
