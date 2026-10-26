use std::time::{Duration, Instant};
use std::{io::Write, net::TcpStream};

pub const DRAIN: Duration = Duration::from_millis(500);

pub struct DeadlineReader<'a> {
    pub stream: &'a TcpStream,
    pub deadline: Instant,
}

impl std::io::Read for DeadlineReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(left))?;
        let mut s = self.stream;
        s.read(buf)
    }
}

pub fn close_gracefully(stream: &TcpStream) {
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let deadline = Instant::now() + DRAIN;
    let mut sink = [0u8; 4096];
    let mut r = stream;
    let mut left = 64 * 1024usize;
    while left > 0 {
        let t = deadline.saturating_duration_since(Instant::now());
        if t.is_zero() || stream.set_read_timeout(Some(t)).is_err() {
            break;
        }
        match std::io::Read::read(&mut r, &mut sink) {
            Ok(0) | Err(_) => break,
            Ok(n) => left = left.saturating_sub(n),
        }
    }
}

pub(crate) struct DeadlineWriter<'a> {
    pub(crate) stream: &'a TcpStream,
    pub(crate) deadline: Instant,
    pub(crate) wire_bytes: u64,
}
impl Write for DeadlineWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        loop {
            let left = self.deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            match self.stream.write(buf) {
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(left.min(Duration::from_millis(1)));
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                result => {
                    if let Ok(bytes) = result {
                        self.wire_bytes = self.wire_bytes.saturating_add(bytes as u64);
                    }
                    return result;
                }
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}
