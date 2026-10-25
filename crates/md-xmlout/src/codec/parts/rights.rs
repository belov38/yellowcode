use super::{XML_HEAD, bool_text};
use crate::paths::object_dir;
use crate::{DUMP_VERSION, DumpSink, Part, PartStatus, Result, XmlWriter};
use md_model::parts::*;
use md_model::*;
use std::collections::HashSet;

pub fn ordinal_utf16(left: &str, right: &str) -> std::cmp::Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

fn restriction_field_names(restriction: &Restriction) -> Vec<&str> {
    let mut names: Vec<_> = restriction
        .fields
        .iter()
        .filter_map(|path| path.rsplit('.').next())
        .filter(|name| !name.is_empty())
        .collect();
    names.sort_unstable_by(|left, right| ordinal_utf16(left, right));
    names.dedup();
    names
}

pub fn rights_xml(cfg: &Configuration, r: &Rights) -> Option<String> {
    let mut w = XmlWriter::new(1);
    w.leaf(
        "setForNewObjects",
        Some(bool_text(r.set_for_new_objects)),
        &[],
    );
    w.leaf(
        "setForAttributesByDefault",
        Some(bool_text(r.set_for_attributes_by_default)),
        &[],
    );
    w.leaf(
        "independentRightsOfChildObjects",
        Some(bool_text(r.independent_rights_of_child_objects)),
        &[],
    );
    let mut objects: Vec<&ObjectRights> = r.objects.iter().collect();
    objects.sort_by_key(|o| o.target.sort_key(cfg.id));
    for o in objects {
        let default = r.default_for(&o.target);
        let dropped: HashSet<Right> = o
            .rights
            .iter()
            .filter(|rv| rv.value == default && rv.restrictions.is_empty())
            .map(|rv| rv.right)
            .collect();
        let mut written = o
            .rights
            .iter()
            .filter(|rv| !dropped.contains(&rv.right))
            .peekable();
        if written.peek().is_none() {
            continue;
        }
        w.open("object", &[]);
        w.leaf("name", Some(&o.full_name), &[]);
        for rv in written {
            let name = rv.right.name();
            if name.is_empty() {
                return None;
            }
            w.open("right", &[]);
            w.leaf("name", Some(name), &[]);
            w.leaf("value", Some(bool_text(rv.value)), &[]);
            for rs in &rv.restrictions {
                if rs.condition.is_empty() && rs.fields.is_empty() {
                    continue;
                }
                w.open("restrictionByCondition", &[]);
                for name in restriction_field_names(rs) {
                    w.leaf("field", Some(name), &[]);
                }
                w.leaf("condition", Some(&rs.condition), &[]);
                w.close("restrictionByCondition");
            }
            w.close("right");
        }
        w.close("object");
    }
    for (name, text) in &r.templates {
        w.open("restrictionTemplate", &[]);
        w.leaf("name", Some(name), &[]);
        w.leaf("condition", Some(text), &[]);
        w.close("restrictionTemplate");
    }
    Some(format!(
        "{XML_HEAD}\r\n<Rights xmlns=\"http://v8.1c.ru/8.2/roles\" xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" \
         xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:type=\"Rights\" version=\"{DUMP_VERSION}\">\r\n{}\r\n</Rights>",
        w.text()
    ))
}

pub fn dump(cfg: &Configuration, sink: &mut DumpSink) -> Result<PartStatus> {
    for role in cfg.of_kind(MdKind::Role) {
        let Some(xml) = role.rights().and_then(|r| rights_xml(cfg, r)) else {
            continue;
        };
        let rel = format!("{}/Ext/Rights.xml", object_dir(cfg, role));
        sink.put(Part::Rights, &rel, xml.as_bytes())?;
    }
    Ok(PartStatus::Written)
}
