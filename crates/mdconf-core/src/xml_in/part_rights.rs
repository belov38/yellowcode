use super::{
    parts::{Context, boolean, text},
    shell_xml::N,
    walk::err,
};
use crate::{Result, parts::*};
use brace::{Guid, Node};
use std::{collections::HashSet, path::Path};

fn target(cx: &Context<'_>, name: &str, path: &Path) -> Result<RightTarget> {
    if name.starts_with("Configuration.") {
        cx.reference(name, path)?;
        return Ok(RightTarget::Configuration);
    }
    if let Some((owner, name)) = name.rsplit_once(".StandardAttribute.") {
        let id = cx
            .reference(owner, path)?
            .id
            .ok_or_else(|| err(path, "missing owner UUID"))?;
        let parts: Vec<_> = owner.split('.').collect();
        let kind = if parts.len() >= 4 {
            parts.get(parts.len() - 2).copied()
        } else {
            parts.first().copied()
        }
        .unwrap_or_default();
        let schema = mdschema::schema()?;
        let sid = if kind == "TabularSection" && name == "LineNumber" {
            let sid = match parts.first().copied().unwrap_or_default() {
                "DataProcessor" | "Report" => -3,
                "ChartOfAccounts" => -16,
                "ChartOfCalculationTypes" => -100,
                "Catalog"
                | "Document"
                | "ExchangePlan"
                | "BusinessProcess"
                | "Task"
                | "ChartOfCharacteristicTypes" => -10,
                _ => return Err(err(path, "unsupported tabular section owner")),
            };
            if schema.std_attr(kind, &sid.to_string()) != Some(name) {
                return Err(err(path, "unknown tabular standard attribute"));
            }
            sid
        } else {
            let mut ids = schema.standard_attributes(kind).filter(|(_, n)| *n == name);
            let sid = ids
                .next()
                .and_then(|(s, _)| s.parse().ok())
                .ok_or_else(|| err(path, "unknown standard attribute"))?;
            if ids.next().is_some() {
                return Err(err(path, "ambiguous standard attribute"));
            }
            sid
        };
        return Ok(RightTarget::StandardAttribute { owner: id, id: sid });
    }
    let id = cx.reference(name, path)?.id.unwrap_or(Guid::NIL);
    if name.split('.').count() == 2 || name.starts_with("Subsystem.") {
        return Ok(RightTarget::Object(id));
    }
    let tag = name.rsplit('.').nth(1).unwrap_or_default();
    Ok(RightTarget::Child {
        id,
        field: matches!(
            tag,
            "Attribute"
                | "Dimension"
                | "Resource"
                | "TabularSection"
                | "AddressingAttribute"
                | "AccountingFlag"
                | "ExtDimensionAccountingFlag"
        ),
    })
}
fn field(cx: &Context<'_>, owner: &str, name: &str, path: &Path) -> Result<String> {
    for tag in ["Attribute", "Dimension", "Resource", "AddressingAttribute"] {
        let full = format!("{owner}.{tag}.{name}");
        if cx.names.contains_key(&full) {
            return Ok(full);
        }
    }
    let full = format!("{owner}.StandardAttribute.{name}");
    target(cx, &full, path)?;
    Ok(full)
}
pub(super) fn read(cx: &Context<'_>, root: N<'_, '_>, path: &Path) -> Result<Rights> {
    if root.local() != "Rights" || root.ns() != Some("http://v8.1c.ru/8.2/roles") {
        return Err(err(path, "wrong rights root"));
    }
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.ns() != root.ns() {
            return Err(err(path, "wrong rights element namespace"));
        }
        let children: &[&str] = match n.local() {
            "Rights" => &[
                "setForNewObjects",
                "setForAttributesByDefault",
                "independentRightsOfChildObjects",
                "object",
                "restrictionTemplate",
            ],
            "object" => &["name", "right"],
            "right" => &["name", "value", "restrictionByCondition"],
            "restrictionByCondition" => &["condition", "field"],
            "restrictionTemplate" => &["name", "condition"],
            "name"
            | "condition"
            | "field"
            | "value"
            | "setForNewObjects"
            | "setForAttributesByDefault"
            | "independentRightsOfChildObjects" => &[],
            _ => return Err(err(path, "unknown rights element")),
        };
        let mut single = HashSet::new();
        for child in n.children() {
            if !children.contains(&child.local()) {
                return Err(err(path, "wrong rights element placement"));
            }
            if !matches!(
                child.local(),
                "object" | "right" | "restrictionByCondition" | "restrictionTemplate" | "field"
            ) && !single.insert(child.local())
            {
                return Err(err(path, "duplicate rights field"));
            }
            stack.push(child);
        }
    }
    let mut rights = Rights {
        set_for_new_objects: boolean(root, "setForNewObjects", path)?,
        set_for_attributes_by_default: boolean(root, "setForAttributesByDefault", path)?,
        independent_rights_of_child_objects: boolean(
            root,
            "independentRightsOfChildObjects",
            path,
        )?,
        objects: Vec::new(),
        templates: Vec::new(),
        source: Node::list(Vec::new()),
    };
    let mut seen = HashSet::new();
    for object in root.children().filter(|n| n.local() == "object") {
        let name = text(object, "name");
        if !seen.insert(name) {
            return Err(err(path, "duplicate rights target"));
        }
        let mut got = ObjectRights {
            target: target(cx, name, path)?,
            full_name: name.into(),
            rights: Vec::new(),
        };
        let mut names = HashSet::new();
        for n in object.children().filter(|n| n.local() == "right") {
            let name_right = text(n, "name");
            let right = Right::from_name(name_right).ok_or_else(|| err(path, "unknown right"))?;
            if !names.insert(right) {
                return Err(err(path, "duplicate right"));
            }
            let mut restrictions = Vec::new();
            for r in n
                .children()
                .filter(|c| c.local() == "restrictionByCondition")
            {
                let fields = r
                    .children()
                    .filter(|c| c.local() == "field")
                    .map(|c| field(cx, name, c.text().unwrap_or_default(), path))
                    .collect::<Result<Vec<_>>>()?;
                restrictions.push(Restriction {
                    condition: text(r, "condition").replace("\r\n", "\n"),
                    fields,
                });
            }
            got.rights.push(RightValue {
                right,
                value: boolean(n, "value", path)?,
                restrictions,
            });
        }
        rights.objects.push(got);
    }
    for template in root
        .children()
        .filter(|n| n.local() == "restrictionTemplate")
    {
        rights.templates.push((
            text(template, "name").into(),
            text(template, "condition").replace("\r\n", "\n"),
        ));
    }
    Ok(rights)
}
