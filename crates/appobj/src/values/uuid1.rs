use ibschema::values::{RefId, Uuid16};
use rand::RngCore;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const GREGORIAN_TO_UNIX: u64 = 0x01B2_1DD2_1381_4000;
const TIME_MASK: u64 = (1 << 60) - 1;

pub trait Clock: Send + Sync {
    fn ticks_100ns_since_1582(&self) -> u64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn ticks_100ns_since_1582(&self) -> u64 {
        let d = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        GREGORIAN_TO_UNIX.wrapping_add((d.as_nanos() / 100) as u64)
    }
}

struct State {
    last: u64,
}

pub struct Uuid1Gen {
    node: [u8; 6],
    clock_seq: u16,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
}

impl Uuid1Gen {
    pub fn new(node: Option<[u8; 6]>, clock: Arc<dyn Clock>) -> Uuid1Gen {
        let mut rng = rand::rng();
        let node = node.unwrap_or_else(|| {
            let mut n = [0u8; 6];
            rng.fill_bytes(&mut n);
            n[0] |= 1;
            n
        });
        let clock_seq = (rng.next_u32() & 0x3fff) as u16;
        Uuid1Gen {
            node,
            clock_seq,
            clock,
            state: Mutex::new(State { last: 0 }),
        }
    }

    pub fn next(&self) -> Uuid16 {
        let t = {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let now = self.clock.ticks_100ns_since_1582() & TIME_MASK;
            let t = if now > st.last {
                now
            } else {
                (st.last + 1) & TIME_MASK
            };
            st.last = t;
            t
        };
        let low = (t & 0xffff_ffff) as u32;
        let mid = ((t >> 32) & 0xffff) as u16;
        let hi = (((t >> 48) & 0x0fff) as u16) | 0x1000;
        let seq = (self.clock_seq & 0x3fff) | 0x8000;
        let mut b = [0u8; 16];
        b[0..4].copy_from_slice(&low.to_be_bytes());
        b[4..6].copy_from_slice(&mid.to_be_bytes());
        b[6..8].copy_from_slice(&hi.to_be_bytes());
        b[8..10].copy_from_slice(&seq.to_be_bytes());
        b[10..16].copy_from_slice(&self.node);
        Uuid16::from_bytes(b)
    }

    pub fn next_ref(&self) -> RefId {
        RefId::from_uuid(self.next())
    }
}
