use super::{
    parts::{Context, boolean, text},
    shell_xml::{N, attr},
    walk::err,
};
use crate::{Codec, MdKind, MdObject, Result, Value, parts::*};
use md_model::xml_origin::XmlPart;
use std::path::Path;

struct TypeScope<'a> {
    inherited: &'a dyn super::NamespaceScope,
    cfg_binding: Option<&'a str>,
}
impl super::NamespaceScope for TypeScope<'_> {
    fn namespace(&self, prefix: Option<&str>) -> Option<&str> {
        if prefix == Some("cfg") {
            self.cfg_binding
                .or_else(|| self.inherited.namespace(prefix))
        } else {
            self.inherited.namespace(prefix)
        }
    }
}

fn refs(cx: &Context<'_>, root: N<'_, '_>, tag: &str, path: &Path) -> Result<Vec<crate::MdRef>> {
    root.children()
        .filter(|n| n.local() == tag)
        .flat_map(|n| n.children())
        .map(|n| cx.reference(n.text().unwrap_or_default(), path))
        .collect()
}
fn flags(
    cx: &Context<'_>,
    root: N<'_, '_>,
    tag: &str,
    path: &Path,
) -> Result<Vec<(crate::MdRef, bool)>> {
    root.children()
        .filter(|n| n.local() == tag)
        .flat_map(|n| n.children())
        .map(|n| {
            let name = attr(n, None, "ref")
                .ok_or_else(|| err(path, "missing accounting flag reference"))?;
            let value = match n.text() {
                Some("true") => true,
                Some("false") => false,
                _ => return Err(err(path, "invalid accounting flag")),
            };
            Ok((cx.reference(name, path)?, value))
        })
        .collect()
}
fn item(
    cx: &mut Context<'_>,
    owner: &MdObject,
    n: N<'_, '_>,
    path: &Path,
) -> Result<PredefinedItem> {
    if n.local() != "Item" || n.ns() != Some("http://v8.1c.ru/8.3/xcf/predef") {
        return Err(err(path, "wrong predefined item record"));
    }
    let folder_owner = matches!(
        owner.kind,
        MdKind::Catalog | MdKind::ChartOfCharacteristicTypes
    );
    let account_owner = owner.kind == MdKind::ChartOfAccounts;
    let calc_owner = owner.kind == MdKind::ChartOfCalculationTypes;
    for child in n.children() {
        let supported = match child.local() {
            "IsFolder" => folder_owner,
            "AccountType" | "OffBalance" | "Order" | "AccountingFlags" | "ExtDimensionTypes" => {
                account_owner
            }
            "ActionPeriodIsBase" | "Base" | "Leading" | "Displaced" => calc_owner,
            _ => true,
        };
        if !supported {
            return Err(err(
                path,
                format!(
                    "predefined field {} is not supported for {}",
                    child.local(),
                    owner.kind.as_str()
                ),
            ));
        }
    }
    let id = attr(n, None, "id")
        .and_then(brace::Guid::parse)
        .ok_or_else(|| err(path, "invalid predefined ID"))?;
    let name = text(n, "Name").to_string();
    if cx.predef.insert(id, name.clone()).is_some() {
        return Err(err(path, "duplicate predefined ID"));
    }
    let code_node = n.children().find(|n| n.local() == "Code");
    let code = if let Some((node, ty)) = code_node.and_then(|n| {
        attr(n, Some("http://www.w3.org/2001/XMLSchema-instance"), "type").map(|ty| (n, ty))
    }) {
        let (prefix, local) = ty.split_once(':').map_or((None, ty), |(p, l)| (Some(p), l));
        let mut current = Some(node);
        let mut binding = None;
        while let Some(n) = current {
            binding = n.ns_decls().find(|(p, _)| *p == prefix).map(|(_, uri)| uri);
            if binding.is_some() {
                break;
            }
            current = n.parent();
        }
        if binding != Some("http://www.w3.org/2001/XMLSchema") {
            return Err(err(path, "wrong predefined code QName namespace"));
        }
        match local {
            "decimal" => PredefinedCode::Number(text(n, "Code").into()),
            "string" => PredefinedCode::Str(text(n, "Code").into()),
            _ => return Err(err(path, "unsupported predefined code type")),
        }
    } else {
        PredefinedCode::Str(text(n, "Code").into())
    };
    let mut out = PredefinedItem {
        id,
        name,
        code,
        description: text(n, "Description").into(),
        ..Default::default()
    };
    if folder_owner {
        out.is_folder = boolean(n, "IsFolder", path)?;
    }
    if let Some(t) = n.children().find(|n| n.local() == "Type") {
        let mut xml = cx.assembly.fragment(t, path)?;
        let mut cfg_binding = None;
        for (source, node) in t.children().zip(xml.children.iter_mut()) {
            if matches!(source.local(), "Type" | "TypeSet") {
                let qname = source.text().unwrap_or_default();
                if let Some((prefix, local)) = qname.split_once(':') {
                    let mut current = Some(source);
                    let mut binding = None;
                    while let Some(n) = current {
                        binding = n
                            .ns_decls()
                            .find(|(p, _)| *p == Some(prefix))
                            .map(|(_, u)| u);
                        if binding.is_some() {
                            break;
                        }
                        current = n.parent();
                    }
                    let expected = match prefix {
                        "xs" => Some("http://www.w3.org/2001/XMLSchema"),
                        "v8" => Some("http://v8.1c.ru/8.1/data/core"),
                        "cfg" => Some("http://v8.1c.ru/8.1/data/enterprise/current-config"),
                        _ => None,
                    };
                    if binding.is_none() || expected.is_some_and(|ns| binding != Some(ns)) {
                        return Err(err(path, "wrong predefined QName namespace"));
                    }
                    if expected.is_none()
                        && binding != Some("http://v8.1c.ru/8.1/data/enterprise/current-config")
                    {
                        return Err(err(path, "unsupported predefined QName namespace"));
                    }
                    if binding == Some("http://v8.1c.ru/8.1/data/enterprise/current-config") {
                        cfg_binding = binding;
                        node.text = Some(format!("cfg:{local}"));
                        node.ns_decls.retain(|(p, _)| p.as_deref() != Some(prefix));
                    }
                }
            }
        }
        let scope = TypeScope {
            inherited: &t,
            cfg_binding,
        };
        out.type_desc =
            match cx
                .assembly
                .reader
                .read_scoped("PredefinedItem", Codec::Type, &xml, &scope)
            {
                Value::Type(v) => Some(v),
                _ => return Err(err(path, "unsupported predefined type")),
            };
    }
    if account_owner {
        let account_type = match text(n, "AccountType") {
            "Active" => AccountType::Active,
            "Passive" => AccountType::Passive,
            "ActivePassive" => AccountType::ActivePassive,
            _ => return Err(err(path, "invalid account type")),
        };
        let ext_dimension_types = if let Some(types) =
            n.children().find(|n| n.local() == "ExtDimensionTypes")
        {
            Some(
                types
                    .children()
                    .map(|d| {
                        Ok(ExtDimensionType {
                            kind: cx.reference(attr(d, None, "name").unwrap_or_default(), path)?,
                            turnover: boolean(d, "Turnover", path)?,
                            flags: flags(cx, d, "AccountingFlags", path)?,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
            )
        } else {
            None
        };
        out.account = Some(AccountData {
            account_type,
            off_balance: boolean(n, "OffBalance", path)?,
            order: text(n, "Order").into(),
            flags: flags(cx, n, "AccountingFlags", path)?,
            ext_dimension_types,
        });
    }
    if calc_owner {
        out.calc = Some(CalcTypeData {
            action_period_is_base: boolean(n, "ActionPeriodIsBase", path)?,
            base: refs(cx, n, "Base", path)?,
            leading: refs(cx, n, "Leading", path)?,
            displaced: refs(cx, n, "Displaced", path)?,
        });
    }
    for child in n
        .children()
        .filter(|n| n.local() == "ChildItems")
        .flat_map(|n| n.children())
    {
        out.children.push(item(cx, owner, child, path)?);
    }
    Ok(out)
}
pub(super) fn read(
    cx: &mut Context<'_>,
    owner: &MdObject,
    root: N<'_, '_>,
    path: &Path,
) -> Result<Predefined> {
    if root.local() != "PredefinedData" || root.ns() != Some("http://v8.1c.ru/8.3/xcf/predef") {
        return Err(err(path, "wrong predefined root"));
    }
    Ok(Predefined::from_items(
        root.children()
            .map(|n| item(cx, owner, n, path))
            .collect::<Result<_>>()?,
    ))
}

pub(super) fn index(objects: &[MdObject], parent: &Path, cx: &mut Context<'_>) -> Result<()> {
    fn names(cx: &mut Context<'_>, owner: &MdObject, n: N<'_, '_>, path: &Path) -> Result<()> {
        for it in n.children() {
            if it.local() != "Item" || it.ns() != Some("http://v8.1c.ru/8.3/xcf/predef") {
                return Err(err(path, "wrong predefined index record"));
            }
            let id = attr(it, None, "id")
                .and_then(brace::Guid::parse)
                .ok_or_else(|| err(path, "invalid predefined ID"))?;
            let full = format!(
                "{}.{}.{}",
                owner.kind.as_str(),
                owner.name,
                text(it, "Name")
            );
            if cx.names.insert(full, id).is_some() {
                return Err(err(path, "duplicate predefined name"));
            }
            for ch in it.children().filter(|n| n.local() == "ChildItems") {
                names(cx, owner, ch, path)?;
            }
        }
        Ok(())
    }
    for owner in objects {
        let folder = owner
            .kind
            .folder()
            .ok_or_else(|| err(parent, "missing predefined owner folder"))?;
        let dir = parent.join(folder).join(&owner.name);
        let path = dir.join("Ext").join("Predefined.xml");
        if let Some(file) = cx
            .origin
            .get(&path)
            .filter(|file| file.part() == XmlPart::Predefined)
        {
            let arena = cx.arena(&path, file.bytes())?;
            let root = arena
                .root()
                .ok_or_else(|| err(&path, "missing predefined root"))?;
            names(cx, owner, root, &path)?;
        }
        index(&owner.subsystems, &dir, cx)?;
    }
    Ok(())
}
