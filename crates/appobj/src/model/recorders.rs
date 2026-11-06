use super::{ObjId, ObjKind, Uuid16};
use md_model::{MdObject, MdRef, Value};
use std::collections::HashMap;

pub(super) fn index(
    src: &[(ObjKind, &MdObject)],
    by_uuid: &HashMap<Uuid16, ObjId>,
    resolve: &dyn Fn(&MdRef) -> Option<Uuid16>,
    diagnostics: &mut Vec<String>,
) -> Vec<Vec<ObjId>> {
    let mut out = vec![Vec::new(); src.len()];
    let mut invalid = false;
    for (i, (_, doc)) in src
        .iter()
        .enumerate()
        .filter(|(_, (k, _))| *k == ObjKind::Document)
    {
        let links = match doc.prop("RegisterRecords") {
            None => continue,
            Some(Value::RefList(links)) => links,
            Some(_) => {
                diagnostics.push(format!(
                    "Document.{}.RegisterRecords is not a reference list; recorder types unavailable for all registers",
                    doc.name
                ));
                invalid = true;
                continue;
            }
        };
        for link in links {
            let target = resolve(link)
                .and_then(|uuid| by_uuid.get(&uuid))
                .filter(|id| {
                    src.get(id.0 as usize)
                        .is_some_and(|(kind, register)| match kind {
                            ObjKind::InformationRegister => matches!(register.prop("WriteMode"),
                                Some(Value::Text(mode)) if mode == "RecorderSubordinate"),
                            ObjKind::AccumulationRegister
                            | ObjKind::AccountingRegister
                            | ObjKind::CalculationRegister => true,
                            _ => false,
                        })
                })
                .and_then(|id| out.get_mut(id.0 as usize));
            match target {
                Some(docs) => docs.push(ObjId(i as u32)),
                None => {
                    diagnostics.push(format!(
                        "Document.{}.RegisterRecords has an unresolved or ineligible register target ({:?}, {}); recorder types unavailable for all registers",
                        doc.name, link.id, link.name
                    ));
                    invalid = true;
                }
            }
        }
    }
    for docs in &mut out {
        if invalid {
            docs.clear();
        } else {
            docs.sort_unstable();
            docs.dedup();
        }
    }
    out
}
