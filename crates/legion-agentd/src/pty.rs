//! Terminals (docs/protocol.md §3 `/v1/pty`): one login shell per WebSocket in a fresh PTY.

use axum::extract::ws::{Message, WebSocket};
use serde::Deserialize;
use serde_json::json;
use std::{
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::process::{CommandExt, ExitStatusExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};

#[derive(Deserialize)]
struct Control {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    cols: u16,
    #[serde(default)]
    rows: u16,
}

pub async fn serve(mut socket: WebSocket, workspace: PathBuf, cols: u16, rows: u16) {
    let (master, mut child) = match open_shell(&workspace, cols, rows) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("legion-agentd: cannot open terminal: {e}");
            let _ = socket.send(exit_frame(-1)).await;
            return;
        }
    };
    let (Ok(reader), Ok(writer)) = (master.try_clone(), master.try_clone()) else {
        let _ = child.kill();
        let _ = child.wait();
        return;
    };
    let pid = child.id() as libc::pid_t;
    let exited = Arc::new(AtomicBool::new(false));

    let (output_tx, mut output_rx) = mpsc::channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        let mut reader = File::from(reader);
        let mut buf = [0u8; 16 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if output_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break, // EIO once every slave descriptor is closed
            }
        }
    });

    // Unbounded so a shell that isn't reading input can never stall output delivery.
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut writer = File::from(writer);
        while let Some(data) = input_rx.blocking_recv() {
            if writer.write_all(&data).is_err() {
                break;
            }
        }
    });

    let (exit_tx, mut exit_rx) = oneshot::channel::<i32>();
    std::thread::spawn({
        let exited = exited.clone();
        move || {
            let code = match child.wait() {
                Ok(status) => status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0)),
                Err(_) => -1,
            };
            exited.store(true, Ordering::SeqCst);
            let _ = exit_tx.send(code);
        }
    });

    let mut output_open = true;
    let mut exit_code: Option<i32> = None;
    let client_gone = loop {
        if exit_code.is_some() && !output_open {
            break false;
        }
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Binary(data))) => {
                    let _ = input_tx.send(data.to_vec());
                }
                Some(Ok(Message::Text(text))) => {
                    if let Ok(control) = serde_json::from_str::<Control>(&text) {
                        if control.kind == "resize" && control.cols > 0 && control.rows > 0 {
                            resize(&master, control.cols.min(1000), control.rows.min(1000));
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break true,
                Some(Ok(_)) => {}
            },
            chunk = output_rx.recv(), if output_open => match chunk {
                Some(data) => {
                    if socket.send(Message::Binary(data.into())).await.is_err() {
                        break true;
                    }
                }
                None => output_open = false,
            },
            code = &mut exit_rx, if exit_code.is_none() => exit_code = Some(code.unwrap_or(-1)),
            // Background jobs can hold the terminal open after the shell exits; don't wait on them.
            _ = tokio::time::sleep(Duration::from_millis(300)), if exit_code.is_some() && output_open => {
                output_open = false;
            }
        }
    };

    if client_gone {
        if !exited.load(Ordering::SeqCst) {
            // The shell leads its own session (setsid), so this takes its whole job tree down.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    } else {
        let _ = socket.send(exit_frame(exit_code.unwrap_or(-1))).await;
        let _ = socket.send(Message::Close(None)).await;
    }
}

fn exit_frame(code: i32) -> Message {
    Message::Text(json!({"type": "exit", "code": code}).to_string().into())
}

fn winsize(cols: u16, rows: u16) -> libc::winsize {
    libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 }
}

fn resize(master: &OwnedFd, cols: u16, rows: u16) {
    let size = winsize(cols, rows);
    unsafe {
        libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ as _, &size as *const libc::winsize);
    }
}

fn open_shell(workspace: &Path, cols: u16, rows: u16) -> io::Result<(OwnedFd, Child)> {
    let (mut master, mut slave): (libc::c_int, libc::c_int) = (-1, -1);
    let size = winsize(cols, rows);
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null::<libc::termios>() as _,
            &size as *const libc::winsize as _,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    for fd in [&master, &slave] {
        unsafe {
            libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }

    let shell = ["/bin/bash", "/bin/sh"].into_iter().find(|p| Path::new(p).exists()).unwrap_or("/bin/sh");
    let cwd = if workspace.is_dir() { workspace } else { Path::new("/") };
    let mut command = Command::new(shell);
    command
        .arg("-l")
        .current_dir(cwd)
        .env("TERM", "xterm-256color")
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    // Drop our copies of the slave side so reads report EOF/EIO once the shell's tree exits.
    drop(command);
    Ok((master, child))
}
