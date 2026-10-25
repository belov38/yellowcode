use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum PicFormat {
    Png,
    Svg,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct StdPictureInfo {
    pub en: String,
    pub ru: Vec<String>,
    pub id: String,
    pub variant: String,
    pub format: String,
    pub w: u32,
    pub h: u32,
    pub offset: u64,
    pub len: u64,
}

pub struct StdPicture<'a> {
    pub info: &'a StdPictureInfo,
    pub format: PicFormat,
    pub bytes: &'static [u8],
}

static BIN: &[u8] = include_bytes!("../data/pictures-taxi.bin");

struct Table {
    infos: Vec<StdPictureInfo>,
    index: HashMap<String, usize>,
}

static TABLE: OnceLock<Table> = OnceLock::new();

#[allow(
    clippy::expect_used,
    reason = "embedded data/pictures.json; parsed by the crate tests"
)]
fn table() -> &'static Table {
    TABLE.get_or_init(|| {
        let infos: Vec<StdPictureInfo> =
            serde_json::from_str(include_str!("../data/pictures.json"))
                .expect("data/pictures.json is valid");
        let mut index = HashMap::new();
        for (i, p) in infos.iter().enumerate() {
            index.insert(p.en.clone(), i);
            index.insert(p.id.clone(), i);
            for r in &p.ru {
                index.insert(r.clone(), i);
            }
        }
        Table { infos, index }
    })
}

fn make(info: &'static StdPictureInfo) -> Option<StdPicture<'static>> {
    let start = usize::try_from(info.offset).ok()?;
    let end = start.checked_add(usize::try_from(info.len).ok()?)?;
    let format = if info.format == "svg" {
        PicFormat::Svg
    } else {
        PicFormat::Png
    };
    Some(StdPicture {
        info,
        format,
        bytes: BIN.get(start..end)?,
    })
}

pub fn std_picture(name_or_id: &str) -> Option<StdPicture<'static>> {
    let t = table();
    make(t.infos.get(*t.index.get(name_or_id)?)?)
}

pub fn std_pictures() -> impl Iterator<Item = StdPicture<'static>> {
    table().infos.iter().filter_map(make)
}
