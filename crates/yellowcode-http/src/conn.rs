use std::net::TcpStream;
use std::time::{Duration, Instant};

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
