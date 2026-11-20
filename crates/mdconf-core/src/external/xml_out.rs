use super::ExternalDocument;
use crate::startup::index::GuidText;
use crate::{Error, Result};
use md_model::{Child, ChildTag, Prop, SubKind, SubObject, Value};
use md_xmlout::ManagedXmlWriter;
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, Charged, MemoryBudget};
use std::sync::atomic::{AtomicBool, Ordering};

pub fn error(entry: &str, what: impl Into<String>) -> Error {
    Error::Format {
        entry: entry.into(),
        what: what.into(),
    }
}
pub fn memory(error: BudgetError) -> Error {
    match error {
        BudgetError::Limit(limit) => Error::Memory(limit),
        other => self::error("external XML", other.to_string()),
    }
}
pub fn check(cancel: Option<&AtomicBool>) -> Result<()> {
    if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        return Err(error("external XML", "cancelled"));
    }
    Ok(())
}
pub fn text(budget: &MemoryBudget, parts: &[&str]) -> Result<Charged<String>> {
    let mut out = BudgetedText::new(budget, Category::Metadata, u64::MAX).map_err(memory)?;
    for part in parts {
        out.try_push_str(part).map_err(memory)?;
    }
    out.into_string().map_err(memory)
}
fn namespace(prefix: &str) -> Option<&'static str> {
    md_xmlout::NS_DECL
        .split_ascii_whitespace()
        .find_map(|declaration| {
            let (name, uri) = declaration.split_once("=\"")?;
            (name.strip_prefix("xmlns:") == Some(prefix))
                .then(|| uri.strip_suffix('"'))
                .flatten()
        })
}
fn qname(name: &str) -> bool {
    match name.split_once(':') {
        Some((prefix, local)) => namespace(prefix).is_some() && mdschema::is_ncname(local),
        None => mdschema::is_ncname(name),
    }
}
fn validate_raw(
    raw: &md_model::XmlElem,
    budget: &MemoryBudget,
    cancel: Option<&AtomicBool>,
) -> Result<()> {
    let mut pending = BudgetedVec::new(budget, Category::Metadata).map_err(memory)?;
    pending.try_push(raw).map_err(memory)?;
    while let Some(value) = pending.pop() {
        check(cancel)?;
        if !qname(&value.name) {
            return Err(error(
                &value.name,
                "invalid or unsupported XML element namespace",
            ));
        }
        for (name, content) in &value.attrs {
            if let Some(prefix) = name.strip_prefix("xmlns:") {
                if namespace(prefix) != Some(content.as_str()) {
                    return Err(error(name, "unsupported raw XML namespace binding"));
                }
            } else if name == "xmlns" || !qname(name) || (name == "xsi:type" && !qname(content)) {
                return Err(error(
                    name,
                    "invalid or unsupported XML attribute namespace",
                ));
            }
        }
        for child in &value.children {
            pending.try_push(child).map_err(memory)?;
        }
    }
    Ok(())
}
fn validate_props(
    props: &[Prop],
    budget: &MemoryBudget,
    cancel: Option<&AtomicBool>,
) -> Result<()> {
    for prop in props {
        check(cancel)?;
        if !mdschema::is_ncname(&prop.name) || prop.codec == mdschema::Codec::Unknown {
            return Err(error(&prop.name, "invalid or unknown XML property"));
        }
        match &prop.value {
            Value::Type(value) => {
                for member in &value.types {
                    if mdschema::ExpandedTypeName::new(
                        member.name.namespace_uri(),
                        &member.name.local,
                    )
                    .is_none()
                    {
                        return Err(error(&prop.name, "invalid XML type identity"));
                    }
                }
            }
            Value::Xml(raw) => {
                if raw.name != prop.name {
                    return Err(error(&prop.name, "raw property name mismatch"));
                }
                validate_raw(raw, budget, cancel)?;
            }
            Value::StandardAttributes(values) => {
                for value in values {
                    validate_props(&value.props, budget, cancel)?;
                }
            }
            Value::Ref(value) if value.id.is_some() && value.name.is_empty() => {
                return Err(error(&prop.name, "unresolved metadata reference"));
            }
            Value::RefList(values)
                if values.iter().any(|v| v.id.is_some() && v.name.is_empty()) =>
            {
                return Err(error(&prop.name, "unresolved metadata reference list"));
            }
            _ => {}
        }
    }
    Ok(())
}
fn validate_children(
    children: &[Child],
    nested: bool,
    budget: &MemoryBudget,
    cancel: Option<&AtomicBool>,
) -> Result<()> {
    for child in children {
        check(cancel)?;
        if !matches!(child.tag, ChildTag::Attribute | ChildTag::TabularSection)
            || (nested && child.tag != ChildTag::Attribute)
            || (child.tag == ChildTag::Attribute && !child.children.is_empty())
            || child.module.is_some()
        {
            return Err(error(&child.name, "unsupported external child structure"));
        }
        validate_props(&child.props, budget, cancel)?;
        if !child.children.is_empty() {
            validate_children(&child.children, true, budget, cancel)?;
        }
    }
    Ok(())
}
fn root_properties(doc: &ExternalDocument) -> Result<&'static [mdschema::StaticValue]> {
    let key = match doc.kind {
        super::ExternalKind::DataProcessor => "ExternalDataProcessorProperties",
        super::ExternalKind::Report => "ExternalReportProperties",
    };
    mdschema::static_schema()
        .table("xdto_md.json")
        .and_then(|table| table.get(key))
        .and_then(mdschema::StaticValue::as_array)
        .ok_or_else(|| error(key, "missing external XML property schema"))
}
fn property_name(value: &mdschema::StaticValue) -> Option<&str> {
    value.at(0).and_then(mdschema::StaticValue::as_str)
}
pub fn validate(
    doc: &ExternalDocument,
    budget: &MemoryBudget,
    cancel: Option<&AtomicBool>,
) -> Result<()> {
    check(cancel)?;
    let object = &doc.object;
    if object.kind != doc.kind.base_kind() {
        return Err(error(
            &object.name,
            "external kind does not match object kind",
        ));
    }
    if !doc.diagnostics.misses.is_empty() || !doc.diagnostics.unsupported.is_empty() {
        return Err(error(
            &object.name,
            "external document has unsupported or missing content",
        ));
    }
    if object.generated_types.len() != 1
        || object
            .generated_types
            .first()
            .is_none_or(|gt| gt.category != "Object")
    {
        return Err(error(
            &object.name,
            "external Object generated type is required",
        ));
    }
    if !object.external.is_empty()
        || !object.subsystems.is_empty()
        || !object.recalculations.is_empty()
        || object.form.is_some()
        || object.template.is_some()
        || object.flowchart.is_some()
        || object.this_node.is_some()
    {
        return Err(error(&object.name, "unsupported external object content"));
    }
    validate_props(&object.props, budget, cancel)?;
    let properties = root_properties(doc)?;
    for (position, prop) in object.props.iter().enumerate() {
        if !properties
            .iter()
            .any(|value| property_name(value) == Some(prop.name.as_str()))
            || [
                "ObjectBelonging",
                "ExtendedConfigurationObject",
                "ObjectModule",
                "Help",
            ]
            .contains(&prop.name.as_str())
            || object
                .props
                .iter()
                .take(position)
                .any(|prior| prior.name == prop.name)
        {
            return Err(error(
                &prop.name,
                "unsupported or duplicate external root property",
            ));
        }
    }
    validate_children(&object.children, false, budget, cancel)?;
    for sub in object.forms.iter().chain(&object.templates) {
        validate_props(&sub.props, budget, cancel)?;
    }
    Ok(())
}
pub fn document_xml(
    doc: &ExternalDocument,
    budget: &MemoryBudget,
    limit: u64,
    cancel: Option<&AtomicBool>,
) -> Result<Charged<String>> {
    validate(doc, budget, cancel)?;
    let object = &doc.object;
    let kind = doc.kind.xml_name();
    let mut writer = ManagedXmlWriter::document(budget, limit).map_err(memory)?;
    writer
        .open(
            kind,
            &[(
                "uuid",
                GuidText::new(doc.wrapper_id).as_str().map_err(memory)?,
            )],
        )
        .map_err(memory)?;
    writer.open("InternalInfo", &[]).map_err(memory)?;
    writer.open("xr:ContainedObject", &[]).map_err(memory)?;
    writer
        .leaf(
            "xr:ClassId",
            Some(
                GuidText::new(doc.kind.class_id())
                    .as_str()
                    .map_err(memory)?,
            ),
            &[],
        )
        .map_err(memory)?;
    writer
        .leaf(
            "xr:ObjectId",
            Some(GuidText::new(object.id).as_str().map_err(memory)?),
            &[],
        )
        .map_err(memory)?;
    writer.close("xr:ContainedObject").map_err(memory)?;
    for generated in &object.generated_types {
        check(cancel)?;
        let name = text(budget, &[kind, "Object.", &object.name])?;
        writer
            .open(
                "xr:GeneratedType",
                &[("name", &name), ("category", &generated.category)],
            )
            .map_err(memory)?;
        writer
            .leaf(
                "xr:TypeId",
                Some(GuidText::new(generated.type_id).as_str().map_err(memory)?),
                &[],
            )
            .map_err(memory)?;
        writer
            .leaf(
                "xr:ValueId",
                Some(GuidText::new(generated.value_id).as_str().map_err(memory)?),
                &[],
            )
            .map_err(memory)?;
        writer.close("xr:GeneratedType").map_err(memory)?;
    }
    writer.close("InternalInfo").map_err(memory)?;
    writer.open("Properties", &[]).map_err(memory)?;
    for field in root_properties(doc)? {
        check(cancel)?;
        if let Some(prop) = object
            .props
            .iter()
            .find(|prop| Some(prop.name.as_str()) == property_name(field))
        {
            writer
                .property(&prop.name, prop.codec, &prop.value)
                .map_err(memory)?;
        }
    }
    writer.close("Properties").map_err(memory)?;
    if object.children.is_empty() && object.forms.is_empty() && object.templates.is_empty() {
        writer.leaf("ChildObjects", None, &[]).map_err(memory)?;
    } else {
        writer.open("ChildObjects", &[]).map_err(memory)?;
        for tag in [ChildTag::Attribute, ChildTag::TabularSection] {
            for child in object.children.iter().filter(|value| value.tag == tag) {
                check(cancel)?;
                md_xmlout::managed_object_child(
                    &mut writer,
                    budget,
                    object.kind.as_str(),
                    &object.name,
                    child,
                )
                .map_err(memory)?;
            }
        }
        for (tag, values) in [("Form", &object.forms), ("Template", &object.templates)] {
            for value in values {
                check(cancel)?;
                writer.leaf(tag, Some(&value.name), &[]).map_err(memory)?;
            }
        }
        writer.close("ChildObjects").map_err(memory)?;
    }
    writer.close(kind).map_err(memory)?;
    check(cancel)?;
    writer.finish().map_err(memory)
}

pub fn subobject_xml(
    value: &SubObject,
    budget: &MemoryBudget,
    limit: u64,
    cancel: Option<&AtomicBool>,
) -> Result<Charged<String>> {
    check(cancel)?;
    validate_props(&value.props, budget, cancel)?;
    let (tag, table) = match value.kind {
        SubKind::Form => ("Form", "FormProperties"),
        SubKind::Template => ("Template", "TemplateProperties"),
    };
    let fields = mdschema::static_schema()
        .table("xdto_md.json")
        .and_then(|schema| schema.get(table))
        .and_then(mdschema::StaticValue::as_array)
        .ok_or_else(|| error(&value.name, "missing external subobject property schema"))?;
    for (position, prop) in value.props.iter().enumerate() {
        check(cancel)?;
        if !fields
            .iter()
            .any(|field| property_name(field) == Some(prop.name.as_str()))
            || [
                "ObjectBelonging",
                "ExtendedConfigurationObject",
                "Form",
                "Help",
                "Template",
            ]
            .contains(&prop.name.as_str())
            || value
                .props
                .iter()
                .take(position)
                .any(|prior| prior.name == prop.name)
        {
            return Err(error(
                &value.name,
                "unsupported or duplicate external subobject property",
            ));
        }
    }
    let mut writer = ManagedXmlWriter::document(budget, limit).map_err(memory)?;
    writer
        .open(
            tag,
            &[("uuid", GuidText::new(value.id).as_str().map_err(memory)?)],
        )
        .map_err(memory)?;
    writer.open("Properties", &[]).map_err(memory)?;
    for field in fields {
        check(cancel)?;
        let name = property_name(field).ok_or_else(|| error(table, "missing property name"))?;
        if value.kind == SubKind::Form && name == "UseInInterfaceCompatibilityMode" {
            continue;
        }
        if value.kind == SubKind::Form && name == "ExtendedPresentation" {
            subobject_property(
                &mut writer,
                &value.props,
                "UseInInterfaceCompatibilityMode",
                cancel,
            )?;
        }
        subobject_property(&mut writer, &value.props, name, cancel)?;
    }
    writer.close("Properties").map_err(memory)?;
    writer.close(tag).map_err(memory)?;
    check(cancel)?;
    writer.finish().map_err(memory)
}
fn subobject_property(
    writer: &mut ManagedXmlWriter,
    props: &[Prop],
    name: &str,
    cancel: Option<&AtomicBool>,
) -> Result<()> {
    check(cancel)?;
    if let Some(prop) = props.iter().find(|prop| prop.name == name) {
        writer
            .property(&prop.name, prop.codec, &prop.value)
            .map_err(memory)?;
    }
    Ok(())
}

pub fn check_basename(name: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|ch| ch.is_control() || "/\\:*?\"<>|".contains(ch))
    {
        return Err(error(name, "expected a portable basename"));
    }
    let stem = name.split('.').next().unwrap_or_default();
    if [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "COM¹", "COM²", "COM³", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7",
        "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³",
    ]
    .iter()
    .any(|reserved| stem.eq_ignore_ascii_case(reserved))
    {
        return Err(error(name, "reserved portable basename"));
    }
    Ok(())
}
pub fn check_path(path: &str) -> Result<()> {
    for component in path.split('/') {
        check_basename(component)?;
    }
    Ok(())
}
pub fn path_key(budget: &MemoryBudget, path: &str) -> Result<Charged<String>> {
    use unicode_normalization::UnicodeNormalization;
    let mut scalars = 0u64;
    for ch in path.chars() {
        unicode_normalization::char::decompose_canonical(ch, |_| {
            scalars = scalars.saturating_add(1)
        });
    }
    let scratch_bytes = scalars
        .checked_mul(64)
        .ok_or_else(|| error(path, "normalization scratch size overflow"))?;
    let _scratch = budget
        .try_reserve(Category::Metadata, scratch_bytes)
        .map_err(Error::Memory)?;
    let mut out = BudgetedText::new(budget, Category::Metadata, u64::MAX).map_err(memory)?;
    for ch in path
        .nfc()
        .flat_map(char::to_uppercase)
        .flat_map(char::to_lowercase)
    {
        out.try_push(ch).map_err(memory)?;
    }
    out.into_string().map_err(memory)
}
