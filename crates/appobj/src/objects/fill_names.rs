use super::ObjectInstance;
use crate::model::{ObjKind, StdAttr};
use crate::read::std_attr_named;
use crate::read_budget;
use crate::recordset::{applies, text};
use bsl_prim::Str;
use bsl_vm::{CallResult, Execution};
use md_model::MdKind;

const ATTRIBUTES: &str = "<Имя реквизита>";
const COMMON_ATTRIBUTES: &str = "<Имя общего реквизита>";
const ORDER: [StdAttr; 6] = [
    StdAttr::Code,
    StdAttr::Description,
    StdAttr::Parent,
    StdAttr::Owner,
    StdAttr::DeletionMark,
    StdAttr::PredefinedDataName,
];

pub(super) fn names(
    inst: &ObjectInstance,
    ex: &mut Execution,
    retained: u64,
) -> CallResult<Option<Vec<Str>>> {
    let def = inst.app.model().obj(inst.obj);
    if def.kind != ObjKind::Catalog {
        return Ok(None);
    }
    let Some(t) = ex.runtime().types().catalog_type(inst.ty) else {
        return Ok(None);
    };
    let Some(template) = platform_catalog::catalog().types().get(usize::from(t.0)) else {
        return Ok(None);
    };
    if has_common_attribute(inst, &def.name) {
        return Ok(None);
    }
    let properties = &template.properties;
    let module = inst.module();
    let vars = module.as_ref().map_or(0, |m| m.module().vars.len());
    ex.charge(
        (properties.len() as u64)
            .saturating_add(def.fields.len() as u64)
            .saturating_add(vars as u64),
    )?;
    for p in properties.iter().filter(|p| p.writable) {
        let known = match p.name_ru.as_str() {
            ATTRIBUTES | COMMON_ATTRIBUTES => true,
            name => std_attr_named(name).is_some_and(|a| ORDER.contains(&a)),
        };
        if !known {
            return Err(inst
                .not_implemented(&format!("{}: свойство шаблона", p.name_ru))
                .into());
        }
    }
    let writable = |attr: StdAttr| {
        properties
            .iter()
            .any(|p| p.writable && std_attr_named(&p.name_ru) == Some(attr))
    };
    let mut names: Vec<&str> = Vec::new();
    for attr in ORDER.iter().copied().filter(|&a| writable(a)) {
        let always = matches!(attr, StdAttr::Parent | StdAttr::Owner);
        if always || def.fields.iter().any(|f| f.std == Some(attr)) {
            names.push(attr.names().0);
        }
    }
    if properties
        .iter()
        .any(|p| p.writable && p.name_ru == ATTRIBUTES)
    {
        names.extend(
            def.fields
                .iter()
                .filter(|f| f.std.is_none())
                .map(|f| f.name.as_str()),
        );
    }
    if let Some(module) = &module {
        names.extend(
            module
                .module()
                .vars
                .iter()
                .filter(|v| v.export)
                .map(|v| v.name.name.as_ref()),
        );
    }
    let bytes = names
        .iter()
        .fold(read_budget::slots::<Str>(names.len()), |n, name| {
            n.saturating_add((name.len() as u64).saturating_mul(2).saturating_add(64))
        });
    read_budget::reserve(ex, retained.saturating_add(bytes))?;
    Ok(Some(names.into_iter().map(Str::from).collect()))
}

fn has_common_attribute(inst: &ObjectInstance, name: &str) -> bool {
    let full = format!("{}.{name}", MdKind::Catalog.as_str());
    inst.app
        .model()
        .config
        .objects
        .iter()
        .filter(|o| o.kind == MdKind::CommonAttribute)
        .any(|ca| applies(ca, &full) && text(ca, "DataSeparation") != Some("Separate"))
}
