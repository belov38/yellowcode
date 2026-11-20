use super::{CopyDataPath, CopyInfo, CopyObject, CopyStandardDataPath, CopyType, fail, uint};
use crate::{Result, entries::Entries};
use brace::Node;

pub(super) fn read(entries: &Entries<'_>) -> Result<CopyInfo> {
    if !entries.contains("copyinfo") {
        return Ok(CopyInfo::default());
    }
    let data = super::document(entries, "copyinfo")?;
    let data = super::tree(&data, "copyinfo")?;
    let Some([version, objects, types, paths, reserved, tail]) = data.as_list() else {
        return Err(fail("copyinfo", "expected version-4 envelope"));
    };
    if version.as_atom() != Some("4") {
        return Err(fail("copyinfo", "unsupported version"));
    }
    if reserved
        .as_list()
        .is_none_or(|v| v.len() != 2 || v.iter().any(|n| n.as_atom() != Some("0")))
        || tail
            .as_list()
            .is_none_or(|v| v.len() != 1 || v.first().and_then(Node::as_atom) != Some("0"))
    {
        return Err(fail("copyinfo", "unsupported reserved fields"));
    }
    let mut out = CopyInfo::default();
    for record in counted(objects)? {
        let Some([id, source, one, class_name]) = record.as_list() else {
            return Err(fail("copyinfo", "object record"));
        };
        let Some([class, name]) = class_name.as_list() else {
            return Err(fail("copyinfo", "object class/name"));
        };
        if one.as_atom() != Some("1") {
            return Err(fail("copyinfo", "object flag"));
        }
        let object = CopyObject {
            id: guid(id)?,
            source_id: guid(source)?,
            class_id: guid(class)?,
            name: name
                .as_str()
                .ok_or_else(|| fail("copyinfo", "quoted object name"))?
                .into(),
        };
        if out.objects.iter().any(|v| v.id == object.id) {
            return Err(fail("copyinfo", "duplicate object identity"));
        }
        out.objects.push(object);
    }
    for record in counted(types)? {
        let Some([id, owner, category]) = record.as_list() else {
            return Err(fail("copyinfo", "type record"));
        };
        let ty = CopyType {
            type_id: guid(id)?,
            owner: guid(owner)?,
            category: uint(category)
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| fail("copyinfo", "type category"))?,
        };
        if out.types.iter().any(|v| v.type_id == ty.type_id) {
            return Err(fail("copyinfo", "duplicate type identity"));
        }
        out.types.push(ty);
    }
    for record in counted(paths)? {
        let Some([child, owner, minus_one]) = record.as_list() else {
            return Err(fail("copyinfo", "data path"));
        };
        if let Some([index]) = child.as_list() {
            let Some([target]) = minus_one.as_list() else {
                return Err(fail("copyinfo", "standard data path target"));
            };
            let signed = |node: &Node| {
                node.as_atom()
                    .and_then(|s| s.parse::<i32>().ok())
                    .ok_or_else(|| fail("copyinfo", "standard data path index"))
            };
            out.standard_data_paths.push(CopyStandardDataPath {
                index: signed(index)?,
                owner_type: guid(owner)?,
                target_index: signed(target)?,
            });
            continue;
        }
        let Some([zero, child]) = child.as_list() else {
            return Err(fail("copyinfo", "data path child"));
        };
        if zero.as_atom() != Some("0")
            || minus_one
                .as_list()
                .is_none_or(|v| v.len() != 1 || v.first().and_then(Node::as_atom) != Some("-1"))
        {
            return Err(fail("copyinfo", "unsupported data path"));
        }
        out.data_paths.push(CopyDataPath {
            child: guid(child)?,
            owner_type: guid(owner)?,
        });
    }
    Ok(out)
}
fn counted(node: &Node) -> Result<&[Node]> {
    let Some([count, records @ ..]) = node.as_list() else {
        return Err(fail("copyinfo", "counted list"));
    };
    if uint(count) != Some(records.len()) {
        return Err(fail("copyinfo", "count mismatch"));
    }
    Ok(records)
}
fn guid(node: &Node) -> Result<brace::Guid> {
    node.as_guid().ok_or_else(|| fail("copyinfo", "GUID atom"))
}
