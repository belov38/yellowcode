use crate::codec::overlay;
use crate::codec::values::write_prop;
use crate::paths::{object_dir, recalc_path, sub_dir};
use crate::{DumpSink, Part, PartStatus, Result, XmlWriter, document};
use brace::Guid;
use md_model::{
    Child, ChildTag, Configuration, GeneratedType, MdObject, Prop, Recalculation, SubKind,
    SubObject,
};
use mdschema::{ChildSpec, Schema};

pub fn child_tag_name(tag: ChildTag) -> &'static str {
    match tag {
        ChildTag::Attribute => "Attribute",
        ChildTag::TabularSection => "TabularSection",
        ChildTag::Dimension => "Dimension",
        ChildTag::Resource => "Resource",
        ChildTag::EnumValue => "EnumValue",
        ChildTag::Command => "Command",
        ChildTag::AddressingAttribute => "AddressingAttribute",
        ChildTag::AccountingFlag => "AccountingFlag",
        ChildTag::ExtDimensionAccountingFlag => "ExtDimensionAccountingFlag",
        ChildTag::Column => "Column",
        ChildTag::URLTemplate => "URLTemplate",
        ChildTag::Method => "Method",
        ChildTag::Operation => "Operation",
        ChildTag::Parameter => "Parameter",
        ChildTag::IntegrationServiceChannel => "IntegrationServiceChannel",
        ChildTag::Function => "Function",
    }
}

pub fn object_xml(cfg: &Configuration, obj: &MdObject) -> String {
    try_object_xml(cfg, obj).unwrap_or_default()
}

pub fn try_object_xml(cfg: &Configuration, obj: &MdObject) -> Result<String> {
    let has_envelope = cfg.xml_namespace_envelopes.contains_key(&obj.id);
    crate::namespace_scope::validate_properties(&obj.props, has_envelope)?;
    fn children(items: &[Child], has_envelope: bool) -> Result<()> {
        for child in items {
            crate::namespace_scope::validate_properties(&child.props, has_envelope)?;
            children(&child.children, has_envelope)?;
        }
        Ok(())
    }
    children(&obj.children, has_envelope)?;
    let text = object_xml_unscoped(cfg, obj);
    if text.is_empty() {
        return Ok(text);
    }
    crate::namespace_scope::restore(&text, cfg.xml_namespace_envelopes.get(&obj.id))
}

fn object_xml_unscoped(cfg: &Configuration, obj: &MdObject) -> String {
    let _ = cfg;
    let Ok(s) = mdschema::schema() else {
        return String::new();
    };
    let kind = obj.kind.as_str();
    let Some(spec) = overlay::kind(s, kind).filter(|k| k.header.is_some()) else {
        return String::new();
    };
    let mut w = XmlWriter::new(1);
    w.open(kind, &[("uuid", &obj.id.to_string())]);
    write_internal_info(
        &mut w,
        s,
        kind,
        &obj.name,
        obj.this_node,
        &obj.generated_types,
    );
    write_props(&mut w, &obj.props);
    let co = format!("{kind}ChildObjects");
    if s.has_xdto_type(&co) {
        let listed = Listed {
            subobjects: &spec.subobjects,
            forms: &obj.forms,
            templates: &obj.templates,
            nested: &obj.subsystems,
            recalculations: &obj.recalculations,
        };
        let at = At {
            s,
            kind,
            objname: &obj.name,
        };
        write_children(&mut w, &at, &spec.children, &co, &obj.children, &listed);
    }
    w.close(kind);
    document(&w)
}

pub fn subobject_xml(cfg: &Configuration, sub: &SubObject) -> String {
    try_subobject_xml(cfg, sub).unwrap_or_default()
}

pub fn try_subobject_xml(cfg: &Configuration, sub: &SubObject) -> Result<String> {
    crate::namespace_scope::validate_properties(
        &sub.props,
        cfg.xml_namespace_envelopes.contains_key(&sub.id),
    )?;
    let text = subobject_xml_unscoped(cfg, sub);
    if text.is_empty() {
        return Ok(text);
    }
    crate::namespace_scope::restore(&text, cfg.xml_namespace_envelopes.get(&sub.id))
}

fn subobject_xml_unscoped(cfg: &Configuration, sub: &SubObject) -> String {
    let _ = cfg;
    let sk = match sub.kind {
        SubKind::Form => "Form",
        SubKind::Template => "Template",
    };
    match mdschema::schema() {
        Ok(s) if overlay::kind(s, sk).is_some() => {}
        _ => return String::new(),
    }
    let mut w = XmlWriter::new(1);
    w.open(sk, &[("uuid", &sub.id.to_string())]);
    write_props(&mut w, &sub.props);
    w.close(sk);
    document(&w)
}

pub fn recalculation_xml(cfg: &Configuration, register: &MdObject, r: &Recalculation) -> String {
    try_recalculation_xml(cfg, register, r).unwrap_or_default()
}

pub fn try_recalculation_xml(
    cfg: &Configuration,
    register: &MdObject,
    r: &Recalculation,
) -> Result<String> {
    crate::namespace_scope::validate_properties(
        &r.props,
        cfg.xml_namespace_envelopes.contains_key(&r.id),
    )?;
    for dimension in &r.dimensions {
        crate::namespace_scope::validate_properties(
            &dimension.props,
            cfg.xml_namespace_envelopes.contains_key(&r.id),
        )?;
    }
    let text = recalculation_xml_unscoped(cfg, register, r);
    if text.is_empty() {
        return Ok(text);
    }
    crate::namespace_scope::restore(&text, cfg.xml_namespace_envelopes.get(&r.id))
}

fn recalculation_xml_unscoped(
    cfg: &Configuration,
    register: &MdObject,
    r: &Recalculation,
) -> String {
    let _ = cfg;
    let Ok(s) = mdschema::schema() else {
        return String::new();
    };
    let mut w = XmlWriter::new(1);
    w.open(RECALC_TAG, &[("uuid", &r.id.to_string())]);
    let full = format!("{}.{}", register.name, r.name);
    write_internal_info(&mut w, s, RECALC_TAG, &full, None, &r.generated_types);
    write_props(&mut w, &r.props);
    if r.dimensions.is_empty() {
        w.leaf("ChildObjects", None, &[]);
    } else {
        w.open("ChildObjects", &[]);
        for d in &r.dimensions {
            w.open("Dimension", &[("uuid", &d.id.to_string())]);
            write_props(&mut w, &d.props);
            w.close("Dimension");
        }
        w.close("ChildObjects");
    }
    w.close(RECALC_TAG);
    document(&w)
}

pub fn object_files(cfg: &Configuration) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if each_file(cfg, &mut |rel, text| {
        out.push((rel, text));
        Ok(())
    })
    .is_err()
    {
        out.clear();
    }
    out
}

pub fn dump(cfg: &Configuration, sink: &mut DumpSink) -> Result<PartStatus> {
    preflight(cfg)?;
    each_file(cfg, &mut |rel, text| {
        sink.put(Part::Objects, &rel, text.as_bytes())
    })?;
    Ok(PartStatus::Written)
}

pub fn preflight(cfg: &Configuration) -> Result<()> {
    each_file(cfg, &mut |_, _| Ok(()))
}

fn each_file(cfg: &Configuration, f: &mut dyn FnMut(String, String) -> Result<()>) -> Result<()> {
    for o in &cfg.objects {
        object_with_subs(cfg, o, f)?;
    }
    for o in &cfg.objects {
        let mut stack: Vec<&MdObject> = o.subsystems.iter().rev().collect();
        while let Some(n) = stack.pop() {
            object_with_subs(cfg, n, f)?;
            stack.extend(n.subsystems.iter().rev());
        }
    }
    Ok(())
}

fn object_with_subs(
    cfg: &Configuration,
    o: &MdObject,
    f: &mut dyn FnMut(String, String) -> Result<()>,
) -> Result<()> {
    let text = try_object_xml(cfg, o)?;
    if text.is_empty() {
        return Ok(());
    }
    f(format!("{}.xml", object_dir(cfg, o)), text)?;
    for sub in o.forms.iter().chain(&o.templates) {
        let t = try_subobject_xml(cfg, sub)?;
        if !t.is_empty() {
            f(format!("{}.xml", sub_dir(cfg, o, sub)), t)?;
        }
    }
    for r in &o.recalculations {
        let t = try_recalculation_xml(cfg, o, r)?;
        if !t.is_empty() {
            f(recalc_path(cfg, o, r), t)?;
        }
    }
    Ok(())
}

const RECALC_TAG: &str = "Recalculation";

struct At<'m> {
    s: &'m Schema,
    kind: &'m str,
    objname: &'m str,
}

struct Listed<'m> {
    subobjects: &'m [(String, String)],
    forms: &'m [SubObject],
    templates: &'m [SubObject],
    nested: &'m [MdObject],
    recalculations: &'m [Recalculation],
}

impl Listed<'_> {
    const NONE: Listed<'static> = Listed {
        subobjects: &[],
        forms: &[],
        templates: &[],
        nested: &[],
        recalculations: &[],
    };
}

fn write_props(w: &mut XmlWriter, props: &[Prop]) {
    w.open("Properties", &[]);
    for p in props {
        write_prop(w, &p.name, p.codec, &p.value);
    }
    w.close("Properties");
}

fn write_internal_info(
    w: &mut XmlWriter,
    s: &Schema,
    kind: &str,
    fullname: &str,
    this_node: Option<Guid>,
    types: &[GeneratedType],
) {
    if types.is_empty() && this_node.is_none() {
        return;
    }
    w.open("InternalInfo", &[]);
    if let Some(g) = this_node {
        w.leaf("xr:ThisNode", Some(&g.to_string()), &[]);
    }
    for gt in types {
        let pref = s
            .gt_name(&gt.category)
            .filter(|n| !n.is_empty())
            .map_or_else(|| format!("{kind}{}", gt.category), str::to_string);
        let name = format!("{pref}.{fullname}");
        w.open(
            "xr:GeneratedType",
            &[("name", &name), ("category", &gt.category)],
        );
        w.leaf("xr:TypeId", Some(&gt.type_id.to_string()), &[]);
        w.leaf("xr:ValueId", Some(&gt.value_id.to_string()), &[]);
        w.close("xr:GeneratedType");
    }
    w.close("InternalInfo");
}

fn write_child(w: &mut XmlWriter, at: &At, cs: &ChildSpec, c: &Child) {
    let tag = child_tag_name(c.tag);
    w.open(tag, &[("uuid", &c.id.to_string())]);
    let full = format!("{}.{}", at.objname, c.name);
    write_internal_info(
        w,
        at.s,
        child_generated_kind(at.kind, &cs.tag),
        &full,
        None,
        &c.generated_types,
    );
    write_props(w, &c.props);
    if !cs.children.is_empty() || tag == "TabularSection" {
        let co = format!("{tag}ChildObjects");
        write_children(w, at, &cs.children, &co, &c.children, &Listed::NONE);
    }
    w.close(tag);
}

pub fn child_generated_kind<'a>(owner: &'a str, tag: &'a str) -> &'a str {
    if tag.starts_with(owner) { tag } else { owner }
}

enum Items<'m> {
    Children(&'m ChildSpec, Vec<&'m Child>),
    Names(Vec<&'m str>),
}

impl Items<'_> {
    fn is_empty(&self) -> bool {
        match self {
            Items::Children(_, v) => v.is_empty(),
            Items::Names(v) => v.is_empty(),
        }
    }
}

fn write_children(
    w: &mut XmlWriter,
    at: &At,
    spec_children: &[(String, ChildSpec)],
    co_type: &str,
    children: &[Child],
    listed: &Listed,
) {
    let mut parts: Vec<(&str, Items)> = Vec::new();
    for p in at.s.xdto_props(co_type) {
        let tag = p.name.as_str();
        if let Some((_, cs)) = spec_children.iter().rev().find(|(_, c)| c.tag == tag) {
            let items = children
                .iter()
                .filter(|c| child_tag_name(c.tag) == tag)
                .collect();
            parts.push((tag, Items::Children(cs, items)));
        } else if tag == RECALC_TAG {
            let names = listed.recalculations.iter().map(|r| r.name.as_str());
            parts.push((tag, Items::Names(names.collect())));
        } else if listed.subobjects.iter().any(|(t, _)| t == tag) {
            let names: Vec<&str> = if tag == at.kind {
                listed.nested.iter().map(|o| o.name.as_str()).collect()
            } else if tag == "Form" {
                listed.forms.iter().map(|f| f.name.as_str()).collect()
            } else if tag == "Template" {
                listed.templates.iter().map(|f| f.name.as_str()).collect()
            } else {
                Vec::new()
            };
            parts.push((tag, Items::Names(names)));
        }
    }
    if parts.iter().all(|(_, items)| items.is_empty()) {
        w.leaf("ChildObjects", None, &[]);
        return;
    }
    w.open("ChildObjects", &[]);
    for (tag, items) in &parts {
        match items {
            Items::Children(cs, v) => {
                for c in v {
                    write_child(w, at, cs, c);
                }
            }
            Items::Names(v) => {
                for n in v {
                    w.leaf(tag, Some(n), &[]);
                }
            }
        }
    }
    w.close("ChildObjects");
}
