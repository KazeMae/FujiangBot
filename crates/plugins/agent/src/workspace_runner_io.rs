use std::io::{self, ErrorKind, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread::JoinHandle;

type DrainRx = Receiver<io::Result<(Vec<u8>, bool)>>;

use anyhow::{bail, Context, Result};

use super::{OUT_CAP, REAP_BOUND};

const POLL_SLICE_MS: i32 = 50;

pub(super) struct OutputReader {
    cancel: Arc<AtomicBool>,
    rx: Option<DrainRx>,
    handle: Option<JoinHandle<()>>,
}

pub(super) fn spawn_drain(read: impl Read + AsRawFd + Send + 'static) -> io::Result<OutputReader> {
    set_nonblock(read.as_raw_fd())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let _ = tx.send(drain(read, &flag));
    });
    Ok(OutputReader {
        cancel,
        rx: Some(rx),
        handle: Some(handle),
    })
}

pub(super) fn recv_bound(reader: &mut OutputReader) -> Result<(Vec<u8>, bool)> {
    let rx = reader.rx.take().context("reader")?;
    let received = rx.recv_timeout(REAP_BOUND);
    reader.finish();
    drop(rx);
    match received {
        Ok(value) => value.context("reader"),
        Err(mpsc::RecvTimeoutError::Timeout) => bail!("reader exceeded bound"),
        Err(mpsc::RecvTimeoutError::Disconnected) => bail!("reader disconnected"),
    }
}

impl OutputReader {
    fn finish(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for OutputReader {
    fn drop(&mut self) {
        self.finish();
    }
}

fn drain(mut read: impl Read + AsRawFd, cancel: &AtomicBool) -> io::Result<(Vec<u8>, bool)> {
    let fd = read.as_raw_fd();
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    let mut truncated = false;
    loop {
        if cancel.load(Ordering::Acquire) {
            return Ok((out, truncated));
        }
        let n = match read.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                if !wait_readable(fd, cancel)? {
                    return Ok((out, truncated));
                }
                continue;
            }
            Err(err) => return Err(err),
        };
        if out.len() < OUT_CAP {
            let room = OUT_CAP - out.len();
            let take = n.min(room);
            out.extend_from_slice(&buf[..take]);
            if take < n {
                truncated = true;
            }
        } else {
            truncated = true;
        }
    }
    Ok((out, truncated))
}

fn wait_readable(fd: RawFd, cancel: &AtomicBool) -> io::Result<bool> {
    loop {
        if cancel.load(Ordering::Acquire) {
            return Ok(false);
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, POLL_SLICE_MS) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if rc == 0 {
            continue;
        }
        if pfd.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            return Err(io::Error::other("output poll failed"));
        }
        return Ok(true);
    }
}

fn set_nonblock(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::time::{Duration, Instant};

    fn pipe() -> (File, File) {
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
        let read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        (File::from(read), File::from(write))
    }

    #[test]
    fn eof_returns_full_output() {
        let (read, mut write) = pipe();
        write.write_all(b"hello-pipe").unwrap();
        drop(write);
        let mut reader = spawn_drain(read).unwrap();
        let (buf, truncated) = recv_bound(&mut reader).unwrap();
        assert_eq!(buf, b"hello-pipe");
        assert!(!truncated);
    }

    #[test]
    fn truncated_output_still_drains() {
        let (read, write) = pipe();
        let (done_tx, done_rx) = mpsc::channel();
        let payload = vec![0xab; OUT_CAP + 256 * 1024];
        std::thread::spawn(move || {
            let mut write = write;
            write.write_all(&payload).unwrap();
            drop(write);
            let _ = done_tx.send(());
        });
        let mut reader = spawn_drain(read).unwrap();
        let (buf, truncated) = recv_bound(&mut reader).unwrap();
        assert_eq!(buf.len(), OUT_CAP);
        assert!(truncated);
        assert!(buf.iter().all(|byte| *byte == 0xab));
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn drop_reclaims_reader_while_writer_open() {
        let (read, write) = pipe();
        let reader = spawn_drain(read).unwrap();
        let started = Instant::now();
        drop(reader);
        assert!(started.elapsed() < Duration::from_millis(500));
        drop(write);
    }
}
