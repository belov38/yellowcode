use crate::{Configuration, MdObject, Result};
use md_model::{
    forms::*,
    xml_origin::{XmlOrigin, XmlPart},
};
use mdconf_core::{
    Error,
    xml::view::NodeRef,
    xml_in::{PartsOptions, parse_part},
};
use memory_budget::{Limits, MemoryBudget};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};
const FORM_NS: &str = "http://v8.1c.ru/8.3/xcf/logform";
const CORE_NS: &str = "http://v8.1c.ru/8.1/data/core";
type N<'a, 's> = NodeRef<'a, 's>;

struct TypeNames(HashMap<(String, String), (FormAttributeType, bool)>);

impl TypeNames {
    fn insert_expanded(&mut self, uri: &str, local: &str, identity: FormAttributeType, set: bool) {
        self.0
            .entry((uri.to_string(), local.to_string()))
            .and_modify(|old| {
                if *old != (identity, set) {
                    old.0 = FormAttributeType::Invalid;
                }
            })
            .or_insert((identity, set));
    }

    fn insert_qname(
        &mut self,
        qname: &str,
        namespace: Option<&str>,
        identity: FormAttributeType,
        set: bool,
    ) {
        if let Some((prefix, local)) = qname.split_once(':')
            && let Some(uri) = namespace.or_else(|| {
                crate::forms::NAMESPACE_PREFIXES
                    .iter()
                    .find(|(_, p)| *p == prefix)
                    .map(|(uri, _)| *uri)
            })
        {
            self.insert_expanded(uri, local, identity, set);
        }
    }

    fn new(index: &md_model::index::Index) -> Result<Self> {
        let mut out = Self(HashMap::new());
        for qname in [
            "xs:boolean",
            "xs:string",
            "xs:decimal",
            "xs:dateTime",
            "xs:base64Binary",
            "v8:Null",
        ] {
            out.insert_qname(qname, None, FormAttributeType::Other, false);
        }
        for (id, ty) in mdschema::schema()?.platform_types() {
            if let Some(id) = brace::Guid::parse(id) {
                out.insert_expanded(
                    &ty.namespace_uri,
                    &ty.local,
                    FormAttributeType::Id(id),
                    ty.set,
                );
            }
        }
        for (id, value) in mdschema::static_form_tables(false)
            .types()
            .as_object()
            .unwrap_or_default()
        {
            if let Some(fields) = value.as_array()
                && let Some(qname) = fields.get(1).and_then(|v| v.as_str())
                && let Some(id) = brace::Guid::parse(id)
            {
                out.insert_qname(
                    qname,
                    fields.get(2).and_then(|v| v.as_str()),
                    FormAttributeType::Id(id),
                    fields.first().and_then(|v| v.as_str()) == Some("TypeSet"),
                );
            }
        }
        for (id, name) in &index.types {
            let set = crate::forms::SET_CATEGORIES.contains(&name.split('.').next().unwrap_or(""));
            out.insert_qname(
                &format!("cfg:{name}"),
                None,
                FormAttributeType::Id(*id),
                set,
            );
        }
        Ok(out)
    }

    fn resolve(&self, n: N<'_, '_>) -> Option<(FormAttributeType, bool)> {
        let qname = n.text()?;
        let (prefix, local) = qname
            .split_once(':')
            .map_or((None, qname), |(p, l)| (Some(p), l));
        let mut current = Some(n);
        while let Some(node) = current {
            if let Some((_, uri)) = node.ns_decls().find(|(p, _)| *p == prefix) {
                return self.0.get(&(uri.to_string(), local.to_string())).copied();
            }
            current = node.parent();
        }
        None
    }
}

fn attribute_types(n: N<'_, '_>, path: &Path, names: &TypeNames) -> Result<Vec<FormAttributeType>> {
    let mut fields = n.children().filter(|n| n.local() == "Type");
    let Some(field) = fields.next() else {
        return Ok(Vec::new());
    };
    if fields.next().is_some() || field.ns() != Some(FORM_NS) {
        return Err(error(path, "wrong or duplicate attribute type description"));
    }
    let mut out = Vec::new();
    for ty in field
        .children()
        .filter(|n| matches!(n.local(), "Type" | "TypeSet"))
    {
        if ty.ns() != Some(CORE_NS) || ty.children().next().is_some() || ty.attrs().next().is_some()
        {
            return Err(error(path, "wrong attribute type identity"));
        }
        let identity = match names.resolve(ty) {
            Some((identity, set)) if set == (ty.local() == "TypeSet") => identity,
            Some(_) => return Err(error(path, "attribute Type/TypeSet mismatch")),
            None => FormAttributeType::Invalid,
        };
        out.push(identity);
    }
    Ok(out)
}
fn error(path: &Path, what: &str) -> Error {
    Error::Xml {
        path: path.into(),
        what: what.into(),
    }
}
fn attr<'a>(n: N<'a, '_>, name: &str) -> &'a str {
    n.attrs()
        .find(|a| a.ns().is_none() && a.local() == name)
        .map(|a| a.value())
        .unwrap_or_default()
}
fn scalar<'a>(n: N<'a, '_>, path: &Path) -> Result<&'a str> {
    if n.ns() != Some(FORM_NS) || n.children().next().is_some() {
        return Err(error(path, "wrong form scalar namespace or shape"));
    }
    Ok(n.text().unwrap_or_default())
}
fn text<'a>(n: N<'a, '_>, name: &str, path: &Path) -> Result<&'a str> {
    let mut fields = n.children().filter(|n| n.local() == name);
    let Some(field) = fields.next() else {
        return Ok("");
    };
    if fields.next().is_some() {
        return Err(error(path, "duplicate form field"));
    }
    scalar(field, path)
}
fn flag(n: N<'_, '_>, name: &str, path: &Path) -> Result<bool> {
    match text(n, name, path)? {
        "true" => Ok(true),
        "false" | "" => Ok(false),
        _ => Err(error(path, "invalid form flag")),
    }
}
fn item(n: N<'_, '_>, path: &Path) -> Result<FormItem> {
    if n.ns() != Some(FORM_NS) {
        return Err(error(path, "wrong form item namespace"));
    }
    let id = attr(n, "id")
        .parse()
        .map_err(|_| error(path, "invalid form item id"))?;
    let mut children = Vec::new();
    let mut fields = HashSet::new();
    for child in n.children() {
        if (child.local() == "ChildItems" || child.attrs().any(|a| a.local() == "id"))
            && (child.ns() != Some(FORM_NS) || !fields.insert(child.local()))
        {
            return Err(error(path, "wrong or duplicate form item field"));
        }
        if child.local() == "ChildItems" {
            for n in child.children() {
                children.push(item(n, path)?);
            }
        } else if child.attrs().any(|a| a.local() == "id") {
            children.push(item(child, path)?);
        }
    }
    Ok(FormItem {
        id,
        name: attr(n, "name").into(),
        tag: n.local().into(),
        children,
    })
}
fn form(
    origin: &XmlOrigin,
    path: &Path,
    opts: &PartsOptions,
    budget: &MemoryBudget,
    work: &mut u64,
    names: &TypeNames,
) -> Result<Option<Form>> {
    let Some(file) = origin
        .get(path)
        .filter(|file| file.part() == XmlPart::Forms)
    else {
        let companions = path.with_extension("");
        if origin
            .files()
            .any(|(p, f)| f.part() == XmlPart::Forms && p.starts_with(&companions))
        {
            return Err(error(path, "orphan form companions without descriptor"));
        }
        return Ok(None);
    };
    let arena = parse_part(file.bytes(), path, &opts.objects.shells, budget, work)?;
    let root = arena.root().ok_or_else(|| error(path, "missing Form"))?;
    if root.local() != "Form" || root.ns() != Some("http://v8.1c.ru/8.3/xcf/logform") {
        return Err(error(path, "wrong form root"));
    }
    let mut out = Form::default();
    let mut sections = HashSet::new();
    for n in root.children() {
        if !sections.insert(n.local()) {
            return Err(error(path, "duplicate form section"));
        }
        if n.ns() != root.ns() {
            return Err(error(path, "wrong form section namespace"));
        }
        let record = match n.local() {
            "Attributes" => Some("Attribute"),
            "Commands" => Some("Command"),
            "Parameters" => Some("Parameter"),
            "Events" => Some("Event"),
            _ => None,
        };
        for c in n.children() {
            if record.is_some_and(|name| c.local() != name) {
                return Err(error(path, "wrong form record element"));
            }
            if matches!(
                n.local(),
                "Attributes" | "Commands" | "Parameters" | "Events" | "ChildItems"
            ) && c.ns() != root.ns()
            {
                return Err(error(path, "wrong form record namespace"));
            }
        }
        match n.local() {
            "AutoCommandBar" => out.items.push(item(n, path)?),
            "ChildItems" => {
                for child in n.children() {
                    out.items.push(item(child, path)?);
                }
            }
            "Attributes" => {
                for a in n.children() {
                    out.attributes.push(FormAttribute {
                        id: attr(a, "id")
                            .parse()
                            .map_err(|_| error(path, "invalid attribute id"))?,
                        name: attr(a, "name").into(),
                        main: flag(a, "MainAttribute", path)?,
                        types: attribute_types(a, path, names)?,
                    });
                }
            }
            "Commands" => {
                for a in n.children() {
                    out.commands.push(FormCommand {
                        id: attr(a, "id")
                            .parse()
                            .map_err(|_| error(path, "invalid command id"))?,
                        name: attr(a, "name").into(),
                        action: text(a, "Action", path)?.into(),
                    });
                }
            }
            "Parameters" => {
                for a in n.children() {
                    out.parameters.push(FormParameter {
                        name: attr(a, "name").into(),
                        key: flag(a, "KeyParameter", path)?,
                    });
                }
            }
            "Events" => {
                for a in n.children() {
                    out.events
                        .push((attr(a, "name").into(), scalar(a, path)?.into()));
                }
            }
            _ => {}
        }
    }
    Ok(Some(out))
}
pub fn load(cfg: &mut Configuration, opts: &PartsOptions) -> Result<()> {
    fn object(
        o: &mut MdObject,
        parent: &Path,
        origin: &XmlOrigin,
        opts: &PartsOptions,
        budget: &MemoryBudget,
        work: &mut u64,
        names: &TypeNames,
    ) -> Result<()> {
        let folder = o
            .kind
            .folder()
            .ok_or_else(|| error(parent, "missing form owner folder"))?;
        let dir = parent.join(folder).join(&o.name);
        o.form = form(
            origin,
            &dir.join("Ext").join("Form.xml"),
            opts,
            budget,
            work,
            names,
        )?;
        for f in &mut o.forms {
            f.form = form(
                origin,
                &dir.join("Forms").join(&f.name).join("Ext").join("Form.xml"),
                opts,
                budget,
                work,
                names,
            )?;
        }
        for s in &mut o.subsystems {
            object(s, &dir, origin, opts, budget, work, names)?;
        }
        Ok(())
    }
    let Some(origin) = &cfg.xml_origin else {
        return Ok(());
    };
    let budget = MemoryBudget::new(Limits::new(opts.objects.shells.max_xml_memory));
    let mut work = 0;
    let names = TypeNames::new(&cfg.index)?;
    for o in &mut cfg.objects {
        object(o, Path::new(""), origin, opts, &budget, &mut work, &names)?;
    }
    Ok(())
}
