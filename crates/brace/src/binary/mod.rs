mod atom;
mod read;
mod write;

pub use atom::{Atom, AtomValue};
pub use read::{Error, ErrorKind, Event, ReadLimits, Reader, decode_all};
pub use write::{Writer, encode_all};

pub const F_CLOSE: u8 = 0x20;
pub const F_OPEN: u8 = 0x40;
pub const F_COMMA: u8 = 0x80;
pub const BASE_MASK: u8 = 0x1F;

const T_DIGIT0: u8 = 0x01;
const T_DIGIT9: u8 = 0x0A;
const T_U8: u8 = 0x0B;
const T_NEG_U8: u8 = 0x0C;
const T_U16: u8 = 0x0D;
const T_NEG_U16: u8 = 0x0E;
const T_U32: u8 = 0x0F;
const T_NEG_U32: u8 = 0x10;
const T_U64: u8 = 0x11;
const T_NEG_U64: u8 = 0x12;
const T_F32: u8 = 0x13;
const T_F64: u8 = 0x14;
const T_GUID: u8 = 0x15;
const T_NUMERIC: u8 = 0x16;
const T_WSTR8: u8 = 0x17;
const T_WSTR64: u8 = 0x19;
const T_BSTR8: u8 = 0x1A;
const T_BSTR64: u8 = 0x1C;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinNode {
    List(Vec<BinNode>),
    Atom(Atom),
}
