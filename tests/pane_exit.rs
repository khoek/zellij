#![cfg(unix)]

use nix::poll::{poll, PollFd, PollFlags};
use nix::pty::{openpty, Winsize};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const RESUME_COMMAND: &str = "kai r 123e4567-e89b-12d3-a456-426614174000";
const EXIT_ALTERNATE_SCREEN: &[u8] = b"\x1b[?1049l";

struct Session {
    directory: tempfile::TempDir,
    client: Option<Child>,
}

impl Session {
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_zellij"));
        command
            .arg("--config")
            .arg(self.directory.path().join("config.kdl"));
        command.env("TERM", "xterm-256color");
        for key in [
            "ZELLIJ",
            "ZELLIJ_SESSION_NAME",
            "ZELLIJ_PANE_ID",
            "ZELLIJ_ORIGIN_PANE_ID",
        ] {
            command.env_remove(key);
        }
        command
    }

    fn name(&self) -> &std::ffi::OsStr {
        self.directory.path().file_name().unwrap()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.command().arg("kill-session").arg(self.name()).output();
        if let Some(client) = &mut self.client {
            if client.try_wait().ok().flatten().is_none() {
                let _ = client.kill();
            }
            let _ = client.wait();
        }
    }
}

fn contains(output: &[u8], text: &[u8]) -> bool {
    output.windows(text.len()).any(|part| part == text)
}

fn exit_output(restores_normal_screen: bool, expected_status: i32) -> Vec<u8> {
    let mut session = Session {
        directory: tempfile::Builder::new()
            .prefix("pane-exit-")
            .tempdir_in("/tmp")
            .unwrap(),
        client: None,
    };
    fs::write(
        session.directory.path().join("config.kdl"),
        concat!(
            "session_serialization false\nshow_startup_tips false\n",
            "show_release_notes false\non_force_close \"detach\"\n",
        ),
    )
    .unwrap();
    let handler = if restores_normal_screen {
        format!(
            concat!(
                "printf '\\033[?1049lToken usage: 1 total\\r\\n",
                "To continue this session, run:\\r\\n  \\033[38;2;12;34;56m{}\\033[0m\\r\\n'\nexit {}",
            ),
            RESUME_COMMAND, expected_status,
        )
    } else {
        format!("exit {expected_status}")
    };
    let script = session.directory.path().join("app.sh");
    fs::write(
        &script,
        format!(
            concat!(
                "finish() {{\n{}\n}}\ntrap finish INT\n",
                "printf '\\033[?1049h\\033[2J\\033[HAPP_READY'\n",
                "while :; do read -r line; done\n",
            ),
            handler,
        ),
    )
    .unwrap();
    let created = session
        .command()
        .args(["attach", "--create-background"])
        .arg(session.name())
        .args(["--start-suspended", "--close-on-exit", "--", "/bin/sh"])
        .arg(&script)
        .output()
        .unwrap();
    assert!(created.status.success(), "{created:?}");
    // Background creation returns before the asynchronous pane layout has been applied.
    let deadline = Instant::now() + Duration::from_secs(15);
    let pane_id = loop {
        assert!(Instant::now() < deadline, "command pane was not created");
        let listed = session
            .command()
            .arg("--session")
            .arg(session.name())
            .args(["action", "list-panes", "--json"])
            .output()
            .unwrap();
        assert!(listed.status.success(), "{listed:?}");
        let panes: Vec<serde_json::Value> = serde_json::from_slice(&listed.stdout).unwrap();
        let mut terminals = panes
            .iter()
            .filter(|pane| pane["is_plugin"].as_bool() == Some(false));
        if let Some(pane) = terminals.next() {
            assert!(
                terminals.next().is_none(),
                "multiple command panes were created"
            );
            break pane["id"].as_u64().unwrap().to_string();
        }
    };
    let size = Winsize {
        ws_row: 25,
        ws_col: 120,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let pty = openpty(Some(&size), None).unwrap();
    session.client = Some(
        session
            .command()
            .arg("attach")
            .arg(session.name())
            .args(["--pane", &pane_id])
            .stdin(Stdio::from(pty.slave.try_clone().unwrap()))
            .stdout(Stdio::from(pty.slave.try_clone().unwrap()))
            .stderr(Stdio::from(pty.slave))
            .spawn()
            .unwrap(),
    );
    let mut terminal = File::from(pty.master);
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut output = Vec::new();
    let mut interrupted = false;
    loop {
        assert!(Instant::now() < deadline, "pane attachment did not exit");
        let mut descriptors = [PollFd::new(terminal.as_fd(), PollFlags::POLLIN)];
        if poll(&mut descriptors, 100u16).unwrap() == 0 {
            continue;
        }
        let mut chunk = [0u8; 16384];
        match terminal.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => output.extend_from_slice(&chunk[..count]),
            Err(error) if error.raw_os_error() == Some(nix::libc::EIO) => break,
            Err(error) => panic!("terminal read failed: {error}"),
        }
        if contains(&output, b"APP_READY") && !interrupted {
            terminal.write_all(b"\x03").unwrap();
            interrupted = true;
        }
    }
    assert!(
        interrupted,
        "application did not start; client status {:?}; terminal output {:?}",
        session.client.as_mut().unwrap().try_wait().unwrap(),
        String::from_utf8_lossy(&output),
    );
    assert_eq!(
        session.client.as_mut().unwrap().wait().unwrap().code(),
        Some(expected_status)
    );
    let restore = output
        .windows(EXIT_ALTERNATE_SCREEN.len())
        .rposition(|part| part == EXIT_ALTERNATE_SCREEN)
        .unwrap();
    output[restore + EXIT_ALTERNATE_SCREEN.len()..].to_vec()
}

#[test]
fn ctrl_c_leaves_the_applications_exit_summary_in_host_scrollback() {
    for _ in 0..5 {
        let output = exit_output(true, 0);
        assert!(contains(&output, b"Token usage: 1 total"));
        assert!(contains(&output, RESUME_COMMAND.as_bytes()));
        assert!(contains(&output, b"\x1b[38;2;12;34;56m"));
        assert!(!contains(&output, b"APP_READY"));
    }
}

#[test]
fn an_application_that_exits_in_alternate_screen_does_not_replay_its_ui() {
    let output = exit_output(false, 7);
    assert!(!contains(&output, b"APP_READY"));
    assert!(!contains(&output, RESUME_COMMAND.as_bytes()));
}
