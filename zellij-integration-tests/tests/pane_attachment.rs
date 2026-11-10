#![cfg(unix)]

use zellij_integration_tests::{Size, TestRunner};
use zellij_utils::nested_session::NESTED_FRAME_HEADER;

#[test]
fn application_pane_renders_normal_and_alternate_screens_without_session_control_frames() {
    const CHILD: &str = "ZELLIJ_PANE_ATTACHMENT_TEST_CHILD";
    const VERIFIED: &str = "pane application rendering and control-frame checks passed";
    // Pane attachments return the application's status through process::exit. Isolate that
    // existing CLI contract so it cannot bypass assertions or terminate the test runner.
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "application_pane_renders_normal_and_alternate_screens_without_session_control_frames",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains(VERIFIED));
        return;
    }
    let size = Size {
        cols: 100,
        rows: 30,
    };
    let mut zellij = TestRunner::new(size)
        .with_layout(zellij_utils::data::LayoutInfo::Stringified(
            "layout { pane borderless=true }".to_owned(),
        ))
        .start();
    let terminal = zellij.expect_pty_spawn();
    terminal.output(b"Application prompt");
    zellij.wait_until("application prompt", |grid| {
        grid.contains("Application prompt")
    });

    let attachment = zellij.attach_pane(terminal.terminal_id(), size);
    attachment.wait_until("attached prompt", |grid| {
        grid.contains("Application prompt")
    });
    assert!(zellij
        .raw_bytes()
        .windows(NESTED_FRAME_HEADER.len())
        .any(|bytes| bytes == NESTED_FRAME_HEADER));

    terminal.output(
        b"\x1b[?1049h\x1b[2J\x1b[HResume a previous session\r\nSession alpha\r\nSession beta",
    );
    attachment.wait_until("full-screen session picker", |grid| {
        grid.contains("Resume a previous session") && grid.contains("Session beta")
    });
    attachment.send_stdin(b"\x1b[B");
    terminal.wait_for_stdin("picker navigation", |bytes| bytes.ends_with(b"\x1b[B"));
    terminal.output(b"\x1b[2J\x1b[HSelected Session beta");
    attachment.wait_until("picker redraw", |grid| {
        grid.contains("Selected Session beta")
    });
    terminal.output(b"\x1b[?1049l");
    attachment.wait_until("restored prompt", |grid| {
        grid.contains("Application prompt")
    });

    assert!(!attachment
        .raw_bytes()
        .windows(NESTED_FRAME_HEADER.len())
        .any(|bytes| bytes == NESTED_FRAME_HEADER));
    println!("{VERIFIED}");
    terminal.exit(Some(0));
    attachment.wait_for_client_to_exit();
    zellij.quit();
    panic!("pane attachment did not return its exit status");
}
