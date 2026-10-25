use sha2::Digest;
use sha2::{Sha256, Sha512};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashFn {
    Md5,
    Sha1,
    Sha256,
    Sha512,
    Crc32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HashSum {
    Bytes(Vec<u8>),
    Crc32(u32),
}

#[derive(Clone)]
pub struct Hasher {
    state: HasherState,
}

#[derive(Clone)]
enum HasherState {
    Md5(md5::Md5),
    Sha1(sha1::Sha1),
    Sha256(Sha256),
    Sha512(Sha512),
    Crc32(crc32fast::Hasher),
}

impl Hasher {
    pub fn new(f: HashFn) -> Hasher {
        let state = match f {
            HashFn::Md5 => HasherState::Md5(md5::Md5::new()),
            HashFn::Sha1 => HasherState::Sha1(sha1::Sha1::new()),
            HashFn::Sha256 => HasherState::Sha256(Sha256::new()),
            HashFn::Sha512 => HasherState::Sha512(Sha512::new()),
            HashFn::Crc32 => HasherState::Crc32(crc32fast::Hasher::new()),
        };
        Hasher { state }
    }

    pub fn update(&mut self, data: &[u8]) {
        match &mut self.state {
            HasherState::Md5(h) => {
                h.update(data);
            }
            HasherState::Sha1(h) => {
                h.update(data);
            }
            HasherState::Sha256(h) => {
                h.update(data);
            }
            HasherState::Sha512(h) => {
                h.update(data);
            }
            HasherState::Crc32(h) => {
                h.update(data);
            }
        }
    }

    pub fn update_str(&mut self, s: &[u16]) {
        self.update(&crate::codec::encode(s, crate::codec::Encoding::Utf8));
    }

    pub fn sum(&self) -> HashSum {
        match &self.state {
            HasherState::Md5(h) => {
                let h_copy = h.clone();
                HashSum::Bytes(h_copy.finalize().to_vec())
            }
            HasherState::Sha1(h) => {
                let h_copy = h.clone();
                HashSum::Bytes(h_copy.finalize().to_vec())
            }
            HasherState::Sha256(h) => {
                let h_copy = h.clone();
                HashSum::Bytes(h_copy.finalize().to_vec())
            }
            HasherState::Sha512(h) => {
                let h_copy = h.clone();
                HashSum::Bytes(h_copy.finalize().to_vec())
            }
            HasherState::Crc32(h) => {
                let h_copy = h.clone();
                HashSum::Crc32(h_copy.finalize())
            }
        }
    }
}
