use super::{Builder, CompletionItem, same};
use crate::viewer::{Lang, names};
use mdconf::{Child, ChildTag, Configuration, MdKind, MdObject};
use platform_catalog::{Context, catalog};

fn kind(name: &str) -> Option<MdKind> {
    MdKind::all().iter().copied().find(|k| {
        k.folder().is_some()
            && [Lang::Ru, Lang::En]
                .iter()
                .any(|l| same(&names::section_name(*k, *l), name))
    })
}
fn named(name: &str, out: &mut Builder) {
    out.add(CompletionItem::named(name, name, "property", out.lang));
}
fn template(stem: &str, out: &mut Builder) {
    if let Some(t) = catalog().types().iter().find(|t| {
        t.metadata_template
            && t.name_en
                .split_once(".<")
                .is_some_and(|(s, _)| same(s, stem))
    }) {
        out.members(t);
    }
}
fn group(name: &str) -> Option<ChildTag> {
    names::CHILD_TAGS.iter().copied().find(|tag| {
        [Lang::Ru, Lang::En]
            .iter()
            .any(|l| same(&names::group_name(names::child_tag_en(*tag), *l), name))
    })
}
fn children(all: &[Child], owner: &str, rest: &[String], out: &mut Builder) {
    let Some((first, tail)) = rest.split_first() else {
        return;
    };
    let cat = catalog();
    let property = cat
        .find_type(&format!("MetadataObject: {owner}"))
        .and_then(|idx| cat.find_property(idx, first));
    if !property.is_some_and(|p| p.avail.has(Context::Server)) {
        return;
    }
    let Some(tag) = group(first) else {
        return;
    };
    if let Some((name, rest)) = tail.split_first() {
        if let Some(c) = all.iter().find(|c| c.tag == tag && same(&c.name, name)) {
            if rest.is_empty() {
                out.type_members(&format!("MetadataObject: {}", names::child_tag_en(c.tag)));
            } else {
                children(&c.children, names::child_tag_en(c.tag), rest, out);
            }
        }
    } else {
        out.type_members("КоллекцияОбъектовМетаданных");
        for c in all.iter().filter(|c| c.tag == tag) {
            named(&c.name, out);
        }
    }
}
fn object(o: &MdObject, rest: &[String], out: &mut Builder) {
    if rest.is_empty() {
        out.type_members(&format!("MetadataObject: {}", o.kind.as_str()));
    } else {
        children(&o.children, o.kind.as_str(), rest, out);
    }
}

pub(super) fn members(cfg: &Configuration, path: &[String], out: &mut Builder) {
    let Some((root, rest)) = path.split_first() else {
        return;
    };
    if same(root, "Метаданные") || same(root, "Metadata") {
        let Some((section, tail)) = rest.split_first() else {
            out.type_members("ConfigurationMetadataObject");
            return;
        };
        let Some(k) = kind(section) else {
            return;
        };
        if let Some((name, object_rest)) = tail.split_first() {
            if let Some(o) = cfg
                .objects
                .iter()
                .find(|o| o.kind == k && same(&o.name, name))
            {
                object(o, object_rest, out);
            }
        } else {
            out.type_members("КоллекцияОбъектовМетаданных");
            for o in cfg.objects.iter().filter(|o| o.kind == k) {
                named(&o.name, out);
            }
        }
        return;
    }
    if let Some(k @ (MdKind::Catalog | MdKind::Document)) = kind(root) {
        if rest.is_empty() {
            out.type_members(&format!("{}Менеджер", names::section_name(k, Lang::Ru)));
            for o in cfg.objects.iter().filter(|o| o.kind == k) {
                named(&o.name, out);
            }
        } else if let [name] = rest
            && cfg
                .objects
                .iter()
                .any(|o| o.kind == k && same(&o.name, name))
        {
            template(&format!("{}Manager", k.as_str()), out);
        }
        return;
    }
    if rest.is_empty()
        && let Some(idx) = catalog().find_type(root)
    {
        let t = catalog().type_info(idx);
        if t.enum_global && t.avail.has(Context::Server) {
            for v in &t.values {
                out.add(CompletionItem::named(
                    &v.name_ru, &v.name_en, "constant", out.lang,
                ));
            }
        }
    }
}
