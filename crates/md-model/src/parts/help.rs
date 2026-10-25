#[derive(Debug, Clone, Default, PartialEq)]
pub struct Help {
    pub pages: Vec<(String, Vec<u8>)>,
    pub files: Vec<(String, Vec<u8>)>,
}
