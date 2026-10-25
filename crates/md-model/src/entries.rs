#[derive(Debug, Clone, PartialEq)]
pub enum EntryData {
    Bytes(Vec<u8>),
    Brace(brace::Doc),
    Json(Vec<u8>),
    Nested(Vec<(String, Vec<u8>)>),
}
