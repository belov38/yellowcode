use super::*;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct ObjectOptions {
    pub shells: ShellOptions,
    pub max_value_bytes: u64,
    pub max_module_bytes: u64,
    pub max_total_module_bytes: u64,
}
impl Default for ObjectOptions {
    fn default() -> Self {
        Self {
            shells: ShellOptions::default(),
            max_value_bytes: 512 << 20,
            max_module_bytes: 64 << 20,
            max_total_module_bytes: 512 << 20,
        }
    }
}
#[derive(Debug, Default)]
pub struct ObjectReport {
    pub value_bytes: u64,
    pub typed_properties: usize,
    pub raw_properties: usize,
    pub by_codec: BTreeMap<String, (usize, usize)>,
    pub modules: usize,
    pub module_bytes: u64,
    pub unread_files: Vec<PathBuf>,
}
#[derive(Debug)]
pub struct ObjectDump {
    pub configuration: Configuration,
    pub format: XmlFormat,
    pub files: BTreeMap<PathBuf, PathBuf>,
    pub shells: ShellReport,
    pub report: ObjectReport,
}
pub fn load_objects(dir: &Path, options: &ObjectOptions) -> Result<ObjectDump> {
    let first = load_shells(dir, &options.shells)?;
    let assembly = Assembly {
        reader: ValueReader::new(&first.configuration)?,
        report: ObjectReport::default(),
        max_value_bytes: options.max_value_bytes,
    };
    let (mut d, mut report) = shells::load_with_values(dir, &options.shells, Some(assembly))?;
    if first.configuration.index != d.configuration.index || first.format != d.format {
        return Err(walk::err(
            dir,
            "dump identities changed during object assembly",
        ));
    }
    drop(first);
    super::modules::load(&mut d, options, &mut report)?;
    Ok(ObjectDump {
        configuration: d.configuration,
        format: d.format,
        files: d.files,
        shells: d.report,
        report,
    })
}

pub(super) struct Assembly {
    pub reader: ValueReader,
    pub report: ObjectReport,
    pub max_value_bytes: u64,
}
impl Assembly {
    pub fn properties(
        &mut self,
        node: shell_xml::N<'_, '_>,
        spec: &[(String, mdschema::PropSpec)],
        tag: &str,
        scope: &str,
        path: &Path,
    ) -> Result<Vec<crate::Prop>> {
        use crate::{Prop, Value};
        let props = shell_xml::required(node, shell_xml::MD, "Properties", path)?;
        let mut out = Vec::new();
        for p in props.children() {
            let descriptor = schema::descriptor(self.reader.schema, spec, tag, scope, p.local())
                .unwrap_or_default();
            let fragment = self.fragment(p, path)?;
            let mut depth = 0usize;
            let mut parent = p.parent();
            while let Some(n) = parent {
                depth += 1;
                parent = n.parent();
            }
            let value = self
                .reader
                .read_property(scope, &descriptor, &fragment, depth);
            let raw = matches!(value, Value::Xml(_));
            let counts = self
                .report
                .by_codec
                .entry(format!("{scope}.{:?}", descriptor.codec))
                .or_default();
            if raw {
                self.report.raw_properties += 1;
                counts.1 += 1;
            } else {
                self.report.typed_properties += 1;
                counts.0 += 1;
            }
            out.push(Prop {
                name: p.local().into(),
                codec: descriptor.codec,
                value,
            });
        }
        Ok(out)
    }
    pub(super) fn fragment(&mut self, n: shell_xml::N<'_, '_>, path: &Path) -> Result<XmlNode> {
        use crate::xml::XmlAttr;
        let mut bytes = 0u64;
        let mut stack = vec![n];
        while let Some(n) = stack.pop() {
            bytes = bytes.saturating_add(std::mem::size_of::<XmlNode>() as u64);
            let mut add = |len: usize| {
                bytes = bytes.saturating_add(len as u64);
            };
            add(n.local().len());
            for s in n.ns().into_iter().chain(n.prefix()).chain(n.text()) {
                add(s.len());
            }
            for a in n.attrs() {
                add(std::mem::size_of::<XmlAttr>());
                add(a.local().len());
                add(a.value().len());
                for s in a.ns().into_iter().chain(a.prefix()) {
                    add(s.len());
                }
            }
            for (p, u) in n.ns_decls() {
                add(std::mem::size_of::<(Option<String>, String)>());
                add(u.len());
                if let Some(p) = p {
                    add(p.len());
                }
            }
            stack.extend(n.children());
        }
        self.report.value_bytes = self.report.value_bytes.saturating_add(bytes);
        if self.report.value_bytes > self.max_value_bytes {
            return Err(walk::err(path, "property value byte limit"));
        }
        fn copy(n: shell_xml::N<'_, '_>) -> XmlNode {
            XmlNode {
                ns: n.ns().map(String::from),
                local: n.local().into(),
                prefix: n.prefix().map(String::from),
                attrs: n
                    .attrs()
                    .map(|a| XmlAttr {
                        ns: a.ns().map(String::from),
                        local: a.local().into(),
                        prefix: a.prefix().map(String::from),
                        value: a.value().into(),
                    })
                    .collect(),
                ns_decls: n
                    .ns_decls()
                    .map(|(p, u)| (p.map(String::from), u.into()))
                    .collect(),
                children: n.children().map(copy).collect(),
                text: n.text().map(String::from),
            }
        }
        Ok(copy(n))
    }
}
pub(super) fn headers(
    props: &[crate::Prop],
    synonym: &mut crate::LocalString,
    comment: &mut String,
) {
    for p in props {
        match (&*p.name, &p.value) {
            ("Synonym", crate::Value::LStr(value)) => *synonym = value.clone(),
            ("Comment", crate::Value::Text(value)) => *comment = value.clone(),
            _ => {}
        }
    }
}
