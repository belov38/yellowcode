use std::{
    fs,
    path::{Path, PathBuf},
};

pub const BUILD: &str = "8.5.1.15290";
pub const ENV: &str = "YELLOWCODE_PLATFORM_RESOURCES";
pub const CATALOG: &[u8] = include_bytes!("../../data/catalog.bin");
pub const MESSAGES: &str = include_str!("../../data/messages.tsv");
pub const CATEGORIES: &str = include_str!("../../data/categories.tsv");

pub fn write(root: &Path, build: &str) -> PathBuf {
    let dir = root.join(build);
    fs::create_dir_all(&dir).unwrap();
    let old_len = usize::from(u16::from_le_bytes(CATALOG[8..10].try_into().unwrap()));
    let mut bytes = CATALOG[..8].to_vec();
    bytes.extend_from_slice(&u16::try_from(build.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(build.as_bytes());
    bytes.extend_from_slice(&CATALOG[10 + old_len..]);
    fs::write(dir.join("catalog.bin"), bytes).unwrap();
    for (name, text) in [("messages.tsv", MESSAGES), ("categories.tsv", CATEGORIES)] {
        let (_, body) = text.split_once('\n').unwrap();
        fs::write(dir.join(name), format!("# platform {build}\n{body}")).unwrap();
    }
    dir
}
