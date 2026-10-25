use super::{Picture, R, fetch, find_mut, miss};
use crate::entries::EntryData;
use crate::load::LoadCtx;
use crate::{Configuration, MdKind, Result};
use brace::Node;

const PIC_MAGIC: &[(&[u8], &str)] = &[
    (b"\x89PNG", "png"),
    (b"\xff\xd8", "jpg"),
    (b"GIF8", "gif"),
    (b"BM", "bmp"),
    (b"<svg", "svg"),
    (b"<?xml", "svg"),
    (b"\x00\x00\x01\x00", "ico"),
    (b"PK\x03\x04", "zip"),
];

const CFG_PICTURES: &[(&str, &str)] =
    &[("1", "Logo"), ("2", "Splash"), ("c", "MainSectionPicture")];

pub(crate) fn ext_of(data: &[u8]) -> &'static str {
    PIC_MAGIC
        .iter()
        .find(|(m, _)| data.starts_with(m))
        .map_or("bin", |(_, e)| e)
}

fn b64(n: &Node) -> Option<Vec<u8>> {
    let t = n.text()?;
    let body = t.strip_prefix("#base64:").unwrap_or(t);
    let clean: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    if !clean.len().is_multiple_of(4) {
        return None;
    }
    Node::atom(format!("#base64:{clean}")).base64()
}

fn decode(entry: &Node) -> R<Picture> {
    let payload = entry.at(2).and_then(|n| n.at(0)).ok_or("shape")?;
    let payload = payload.at(0).unwrap_or(payload);
    let data = b64(payload).ok_or("base64")?;
    let flags = entry.at(1).and_then(Node::as_list).ok_or("flags")?;
    let text = |i: usize| flags.get(i).and_then(Node::text).ok_or("flags");
    let load_transparent = text(0)? == "1";
    let transparent_pixel = if text(2)? != "-1" {
        let num = |i: usize| text(i)?.parse::<i64>().map_err(|_| "flags");
        Some((num(2)?, num(3)?))
    } else {
        None
    };
    Ok(Picture {
        ext: ext_of(&data).to_string(),
        data,
        load_transparent,
        transparent_pixel,
    })
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn decode_picture(entry: &Node) -> Option<Picture> {
    decode(entry).ok()
}

fn fetch_picture(
    cx: &LoadCtx,
    cfg: &mut Configuration,
    name: &str,
    which: &str,
) -> Result<Option<(Node, bool)>> {
    Ok(match fetch(cx, name)?.as_deref() {
        Some(EntryData::Brace(doc)) => Some((doc.root.clone(), doc.bom && doc.lead.is_empty())),
        Some(_) => {
            miss(cfg, &["pictures", which, "not_brace"]);
            None
        }
        None => None,
    })
}

fn load_objects(cx: &LoadCtx, cfg: &mut Configuration) -> Result<()> {
    let ids: Vec<_> = cfg.of_kind(MdKind::CommonPicture).map(|o| o.id).collect();
    for id in ids {
        let which = id.to_string();
        let Some((node, bom)) = fetch_picture(cx, cfg, &format!("{id}.0"), &which)? else {
            continue;
        };
        if !bom {
            continue;
        }
        match decode(&node) {
            Ok(p) => {
                if let Some(o) = find_mut(&mut cfg.objects, id) {
                    o.parts.picture = Some(p);
                }
            }
            Err(why) => miss(cfg, &["pictures", &which, why]),
        }
    }
    Ok(())
}

pub(crate) fn load(cx: &LoadCtx, cfg: &mut Configuration) -> Result<()> {
    load_objects(cx, cfg)?;
    let h = cfg.header_id;
    for (suffix, name) in CFG_PICTURES {
        let Some((node, _)) = fetch_picture(cx, cfg, &format!("{h}.{suffix}"), name)? else {
            continue;
        };
        match decode(&node) {
            Ok(p) => cfg.parts.pictures.push((name.to_string(), p)),
            Err(why) => miss(cfg, &["pictures", name, why]),
        }
    }
    Ok(())
}
