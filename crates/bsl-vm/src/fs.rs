use bsl_prim::BslDate;

pub trait FileSystem: Send + Sync {
    fn temp_dir(&self) -> String;
    fn read(&self, path: &str) -> Result<Vec<u8>, FsError>;
    fn write(&self, path: &str, data: &[u8], append: bool) -> Result<(), FsError>;
    fn stat(&self, path: &str) -> Result<Option<FsStat>, FsError>;
    fn path_key(&self, _path: &str) -> Option<String> {
        None
    }
    fn remove(&self, path: &str, mask: Option<&str>) -> Result<(), FsError>;
    fn create_dir(&self, path: &str) -> Result<(), FsError>;
    fn list(&self, dir: &str, mask: &str, recursive: bool) -> Result<Vec<String>, FsError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FsStat {
    pub is_dir: bool,
    pub size: u64,
    pub modified_utc: BslDate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsErrorKind {
    DirNotFound,
    NotFound,
    AccessDenied,
    TooLarge,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsError {
    pub kind: FsErrorKind,
    pub path: String,
}
