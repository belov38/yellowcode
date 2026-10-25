use crate::{Configuration, MdKind, MdObject, Result, Value};
use md_model::{
    graph::Flowchart,
    templates::{Template, TemplateType},
    xml_origin::{XmlOrigin, XmlPart},
};
use std::path::Path;

pub fn load(cfg: &mut Configuration) -> Result<()> {
    let Some(origin) = &cfg.xml_origin else {
        return Ok(());
    };
    for object in &mut cfg.objects {
        load_object(object, Path::new(""), origin)?;
    }
    Ok(())
}
fn template(props: &[md_model::Prop], ext: &Path, origin: &XmlOrigin) -> Result<Option<Template>> {
    let kind = props
        .iter()
        .find(|p| p.name == "TemplateType")
        .and_then(|p| match &p.value {
            Value::Text(s) => TemplateType::from_name(s),
            _ => None,
        });
    if kind.is_none() {
        return Err(crate::Error::Xml {
            path: ext.into(),
            what: "unsupported or missing TemplateType".into(),
        });
    }
    let files = origin
        .files()
        .filter_map(|(p, f)| {
            let rel = p.strip_prefix(ext).ok()?;
            let name = rel
                .iter()
                .filter_map(|s| s.to_str())
                .collect::<Vec<_>>()
                .join("/");
            (name.starts_with("Template.") || name.starts_with("Template/"))
                .then(|| (name, f.bytes().to_vec()))
        })
        .collect::<Vec<_>>();
    Ok(Some(Template { kind, files }))
}
fn load_object(object: &mut MdObject, parent: &Path, origin: &XmlOrigin) -> Result<()> {
    let Some(folder) = object.kind.folder() else {
        return Ok(());
    };
    let dir = parent.join(folder).join(&object.name);
    let ext = dir.join("Ext");
    if object.kind == MdKind::CommonTemplate {
        object.template = template(&object.props, &ext, origin)?;
    }
    if let Some(f) = origin
        .get(&ext.join("Flowchart.xml"))
        .filter(|file| file.part() == XmlPart::Flowcharts)
    {
        object.flowchart = Some(Flowchart {
            xml: f.bytes().to_vec(),
        });
    }
    for sub in &mut object.templates {
        sub.template = template(
            &sub.props,
            &dir.join("Templates").join(&sub.name).join("Ext"),
            origin,
        )?;
    }
    for sub in &mut object.subsystems {
        load_object(sub, &dir, origin)?;
    }
    Ok(())
}
