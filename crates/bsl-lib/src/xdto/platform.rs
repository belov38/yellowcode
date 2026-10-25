use super::lexical::{XS_NS, builtin_types};
use super::model::Model;
use super::xdto_file::parse_xdto_model;
use flate2::read::DeflateDecoder;
use std::io::Read;
use std::sync::OnceLock;

static DATA: &[u8] = include_bytes!("../../data/xdto_models.bin");
static SERVER_DATA: &[u8] = include_bytes!("../../data/xdto_server_model.bin");

const MAX_INFLATED: u64 = 64 << 20;

pub fn split(data: &[u8]) -> Result<Vec<(String, String)>, String> {
    let head = data.get(..4).ok_or("xdto_models.bin: truncated")?;
    let n = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
    let index = data
        .get(4..n.checked_add(4).ok_or("xdto_models.bin: truncated index")?)
        .ok_or("xdto_models.bin: truncated index")?;
    let index = std::str::from_utf8(index).map_err(|e| format!("xdto_models.bin: {e}"))?;
    let mut raw = Vec::new();
    DeflateDecoder::new(&data[4 + n..])
        .take(MAX_INFLATED)
        .read_to_end(&mut raw)
        .map_err(|e| format!("xdto_models.bin: inflate: {e}"))?;
    let mut out = Vec::new();
    let mut at = 0usize;
    for line in index.lines() {
        let (name, len) = line
            .split_once('\t')
            .ok_or("xdto_models.bin: bad index line")?;
        let len: usize = len.parse().map_err(|_| "xdto_models.bin: bad length")?;
        let end = at
            .checked_add(len)
            .ok_or("xdto_models.bin: text out of range")?;
        let text = raw
            .get(at..end)
            .ok_or("xdto_models.bin: text out of range")?;
        at = end;
        out.push((
            name.to_string(),
            String::from_utf8(text.to_vec()).map_err(|e| format!("xdto_models.bin: {e}"))?,
        ));
    }
    Ok(out)
}

pub fn load_resource_model() -> Result<Model, String> {
    let mut files = split(DATA)?;
    let primitive = files
        .iter()
        .position(|(name, _)| name == "xdto_root/XSDModel.xdto")
        .ok_or("xdto_models.bin: missing XSDModel.xdto bootstrap")?;
    let (name, text) = files.remove(primitive);
    let mut packages = parse_xdto_model(&text).map_err(|e| format!("{name}: {e}"))?;
    let schema = packages
        .iter_mut()
        .find(|p| p.ns == XS_NS)
        .ok_or("XSDModel.xdto: missing XMLSchema package")?;
    schema.types.extend(builtin_types());
    for (name, text) in files {
        let mut pk = parse_xdto_model(&text).map_err(|e| format!("{name}: {e}"))?;
        packages.append(&mut pk);
    }
    Ok(Model::new(packages))
}

pub fn load_platform_model() -> Result<Model, String> {
    let mut packages = Vec::new();
    for (name, text) in split(SERVER_DATA)? {
        packages.extend(parse_xdto_model(&text).map_err(|e| format!("{name}: {e}"))?);
    }
    let schema = packages
        .iter_mut()
        .find(|p| p.ns == XS_NS)
        .ok_or("server XDTO model: missing XMLSchema bootstrap")?;
    let at = schema
        .types
        .iter()
        .position(|t| t.name().name == "anyType")
        .ok_or("server XDTO model: missing anyType bootstrap")?;
    let any = schema.types.remove(at);
    schema.types.insert(0, any);
    for index in &mut schema.anonymous_types {
        if *index < at {
            *index += 1;
        }
    }
    Ok(Model::new(packages))
}

#[allow(
    clippy::panic,
    reason = "embedded data: xdto_models.bin is generated and checked by lib-gen (tools/lib-gen/tests/fresh.rs)"
)]
pub fn platform_model() -> &'static Model {
    static M: OnceLock<Model> = OnceLock::new();
    M.get_or_init(|| match load_platform_model() {
        Ok(m) => m,
        Err(e) => panic!("embedded XDTO models are broken: {e}"),
    })
}
