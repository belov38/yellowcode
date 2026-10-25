use mdconf::{ChildTag, MdKind, ModuleKind, SubKind};

use super::icon_table::ICONS;
use super::names::{child_tag_en, kind_name, module_name, sub_kind_en};
use super::out::Lang;

#[derive(Debug, Clone, Copy)]
pub enum NodeRef<'a> {
    Root,
    Group(&'a str),
    Section(MdKind),
    Object(MdKind),
    Folder(&'a str),
    Child(ChildTag),
    Sub(SubKind),
    Recalc,
    Module(ModuleKind),
}

fn plural(key: &str) -> String {
    if key.ends_with('s') {
        key.to_string()
    } else {
        format!("{key}s")
    }
}

pub fn icon_key(node: &NodeRef) -> String {
    match node {
        NodeRef::Root => "root".to_string(),
        NodeRef::Group(k) => format!("group:{k}"),
        NodeRef::Section(k) => format!("section:{}", kind_name(*k, Lang::En)),
        NodeRef::Object(k) => format!("object:{}", kind_name(*k, Lang::En)),
        NodeRef::Folder(k) => format!("folder:{}", plural(k)),
        NodeRef::Child(t) => format!("child:{}", child_tag_en(*t)),
        NodeRef::Sub(k) => format!("sub:{}", sub_kind_en(*k)),
        NodeRef::Recalc => "recalc".to_string(),
        NodeRef::Module(k) => format!("module:{}", module_name(*k, Lang::En)),
    }
}

pub fn manifest_kind(key: &str) -> Option<&str> {
    let kind = match key {
        "root" => "Configuration",
        "group:Common" => "Common",
        "recalc" => "Recalculation",
        k if k.starts_with("module:") => "CommonModule",
        "folder:Modules" => "CommonModule",
        k => {
            let (head, tail) = k.split_once(':')?;
            match head {
                "section" | "object" | "child" | "sub" => tail,
                "folder" => tail.strip_suffix('s').unwrap_or(tail),
                _ => return None,
            }
        }
    };
    ICONS.iter().any(|(k, _, _)| *k == kind).then_some(kind)
}

pub fn picture(key: &str) -> Option<(&'static [u8], Option<&'static [u8]>)> {
    let kind = manifest_kind(key)?;
    ICONS
        .iter()
        .find(|(k, _, _)| *k == kind)
        .map(|(_, png, png2)| (*png, *png2))
}

pub fn version() -> &'static str {
    static V: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |b: &[u8]| {
            for x in b {
                h = (h ^ u64::from(*x)).wrapping_mul(0x0100_0000_01b3);
            }
        };
        for (kind, png, png2) in ICONS {
            eat(kind.as_bytes());
            eat(png);
            eat(png2.unwrap_or_default());
        }
        format!("{h:016x}")[..10].to_string()
    })
}
