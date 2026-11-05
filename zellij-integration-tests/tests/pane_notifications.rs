#![cfg(unix)]

use zellij_integration_tests::{Size, TestRunner};

fn isolated(name: &str) -> bool {
    const CHILD: &str = "ZELLIJ_PANE_NOTIFICATION_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        return false;
    }
    // Pane attachment exit uses process::exit, so require proof that assertions ran first.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("notification checks passed"));
    true
}

#[test]
fn pane_attachment_receives_only_its_applications_notifications() {
    if isolated("pane_attachment_receives_only_its_applications_notifications") {
        return;
    }
    let size = Size {
        cols: 100,
        rows: 30,
    };
    let mut session = TestRunner::new(size).start();
    let terminal = session.expect_pty_spawn();
    terminal.output(b"Ready");
    session.wait_until("ready", |grid| grid.contains("Ready"));
    session.send_stdin(&zellij_integration_tests::keys::ctrl('p'));
    session.wait_until("pane mode", |grid| grid.contains("<p> PANE"));
    session.send_stdin(b"r");
    let other_terminal = session.expect_pty_spawn();
    other_terminal.output(b"Other");
    let attachment = session.attach_pane(terminal.terminal_id(), size);
    attachment.wait_until("attached", |grid| grid.contains("Ready"));
    let other = session.attach_pane(other_terminal.terminal_id(), size);
    other.wait_until("other attached", |grid| grid.contains("Other"));
    let other_before = other.raw_bytes().len();
    let before = attachment.raw_bytes().len();
    terminal.output(b"\x07");
    attachment.wait_until_raw_output("application bell", |bytes| bytes[before..].contains(&7));
    for notification in [
        b"\x1b]9;Turn complete\x07".as_slice(),
        b"\x1b]777;notify;Codex;Input requested\x1b\\".as_slice(),
    ] {
        let before = attachment.raw_bytes().len();
        terminal.output(notification);
        attachment.wait_until_raw_output("application notification", |bytes| {
            bytes[before..].windows(4).any(|part| part == b"\x1b]9;")
        });
    }
    other_terminal.output(b" barrier");
    other.wait_until("other pane barrier", |grid| grid.contains("barrier"));
    assert!(!other.raw_bytes()[other_before..].contains(&7));
    println!("notification checks passed");
    terminal.exit(Some(0));
    attachment.wait_for_client_to_exit();
    session.quit();
    panic!("pane attachment did not return its exit status");
}

#[test]
fn pane_attachment_forwards_host_focus_to_the_application() {
    if isolated("pane_attachment_forwards_host_focus_to_the_application") {
        return;
    }
    let size = Size {
        cols: 100,
        rows: 30,
    };
    let mut session = TestRunner::new(size)
        .with_layout(zellij_utils::data::LayoutInfo::Stringified(
            "layout { pane borderless=true }".to_owned(),
        ))
        .start();
    let terminal = session.expect_pty_spawn();
    terminal.output(b"\x1b[?1004hReady");
    session.wait_until("focus tracking enabled", |grid| grid.contains("Ready"));
    session.send_stdin(b"\x1b[O");
    terminal.wait_for_stdin("session host unfocused", |bytes| bytes.ends_with(b"\x1b[O"));
    let attachment = session.attach_pane(terminal.terminal_id(), size);
    attachment.wait_until("attached", |grid| grid.contains("Ready"));
    let before = terminal.stdin_bytes().len();
    attachment.send_stdin(b"\x1b[O");
    terminal.wait_for_stdin("attachment host unfocused", |bytes| {
        bytes[before..].ends_with(b"\x1b[O")
    });
    attachment.send_stdin(b"\x1b[I");
    terminal.wait_for_stdin("attachment host focused", |bytes| {
        bytes.ends_with(b"\x1b[I")
    });
    let peer = session.attach_pane(terminal.terminal_id(), size);
    peer.wait_until("second attachment", |grid| grid.contains("Ready"));
    let before = terminal.stdin_bytes().len();
    peer.send_stdin(b"\x1b[O!");
    terminal.wait_for_stdin("peer input barrier", |bytes| bytes.ends_with(b"!"));
    assert!(!terminal.stdin_bytes()[before..]
        .windows(3)
        .any(|part| part == b"\x1b[O"));
    attachment.send_stdin(b"\x1b[O");
    terminal.wait_for_stdin("attachment host unfocused", |bytes| {
        bytes.ends_with(b"\x1b[O")
    });
    println!("notification checks passed");
    terminal.exit(Some(0));
    attachment.wait_for_client_to_exit();
    session.quit();
    panic!("pane attachment did not return its exit status");
}
