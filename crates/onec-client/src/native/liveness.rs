use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

pub const MARK: [u8; 4] = [0x23, 0x49, 0x56, 0xB6];
pub const PACKET_LEN: usize = 32;
pub const PERIOD_MS: u16 = 3000;
pub const TIMEOUT_MS: u32 = 15_000;
const MAX_READS: usize = 8;

pub fn packet(id: &[u8; 16], counter: u16, timeout_ms: u32, period_ms: u16) -> [u8; PACKET_LEN] {
    let mut p = [0u8; PACKET_LEN];
    p[..4].copy_from_slice(&MARK);
    p[6..8].copy_from_slice(&(PACKET_LEN as u16).to_be_bytes());
    p[8..12].copy_from_slice(&timeout_ms.to_be_bytes());
    p[12..14].copy_from_slice(&period_ms.to_be_bytes());
    p[14..30].copy_from_slice(id);
    p[30..32].copy_from_slice(&counter.to_be_bytes());
    p
}

pub fn is_packet(d: &[u8]) -> bool {
    d.len() == PACKET_LEN && d[..4] == MARK && d[6..8] == (PACKET_LEN as u16).to_be_bytes()
}

pub struct Liveness {
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
    sent: Arc<AtomicU64>,
    answered: Arc<AtomicU64>,
}

impl Liveness {
    pub fn start(peer: SocketAddr, id: [u8; 16]) -> io::Result<Self> {
        Self::start_with(peer, id, Duration::from_millis(u64::from(PERIOD_MS)))
    }

    pub fn start_with(peer: SocketAddr, id: [u8; 16], period: Duration) -> io::Result<Self> {
        let local: SocketAddr = if peer.is_ipv4() {
            (Ipv4Addr::UNSPECIFIED, 0).into()
        } else {
            (Ipv6Addr::UNSPECIFIED, 0).into()
        };
        let socket = UdpSocket::bind(local)?;
        socket.connect(peer)?;
        socket.set_nonblocking(true)?;
        let (stop, rx) = mpsc::channel::<()>();
        let sent = Arc::new(AtomicU64::new(0));
        let answered = Arc::new(AtomicU64::new(0));
        let (s, a) = (sent.clone(), answered.clone());
        let thread = std::thread::Builder::new()
            .name("onec-s-liveness".into())
            .spawn(move || {
                let mut counter: u16 = 0;
                let mut buf = [0u8; 64];
                while let Err(RecvTimeoutError::Timeout) = rx.recv_timeout(period) {
                    counter = counter.wrapping_add(1);
                    if socket
                        .send(&packet(&id, counter, TIMEOUT_MS, PERIOD_MS))
                        .is_ok()
                    {
                        s.fetch_add(1, Ordering::Relaxed);
                    }
                    for _ in 0..MAX_READS {
                        match socket.recv(&mut buf) {
                            Ok(n) if is_packet(&buf[..n]) => {
                                a.fetch_add(1, Ordering::Relaxed);
                            }
                            Ok(_) => {}
                            Err(_) => break,
                        }
                    }
                }
            })?;
        Ok(Self {
            stop: Some(stop),
            thread: Some(thread),
            sent,
            answered,
        })
    }

    pub fn sent(&self) -> u64 {
        self.sent.load(Ordering::Relaxed)
    }

    pub fn answered(&self) -> u64 {
        self.answered.load(Ordering::Relaxed)
    }
}

impl Drop for Liveness {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
