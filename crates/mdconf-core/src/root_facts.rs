use crate::entries::{Entries, EntrySource, configuration_version};
use crate::error::Result;
use crate::index::{brace_entry, class_lists, doc_tree, name_at, tree};
use crate::model::{Configuration, MdObject};
use crate::objects::overlay;
use brace::{Guid, Node};
use md_model::{LoadProfile, Prop, Value};

use mdschema::{Codec, MdKind};

pub fn configuration_root(src: &dyn EntrySource) -> Result<Configuration> {
    let entries = Entries::new(src)?;
    let (format, compat) = configuration_version(&entries)?;
    let resource = mdschema::for_format(format)?;
    resource.require_embedded()?;
    let schema = resource.schema()?;
    let root_info = entries.root_info()?;
    let id = root_info.id();
    let entry = id.to_string();
    let data = brace_entry(&entries, &entry)?;
    let cfg = tree(&data, &entry)?;
    let name = cfg
        .get(&[3, 1, 1, 1, 1, 2])
        .and_then(Node::text)
        .unwrap_or_default()
        .to_string();
    let mut objects: Vec<MdObject> = Vec::new();
    for (kind, ids) in class_lists(cfg, schema) {
        if objects.iter().any(|o| o.kind == kind) {
            continue;
        }
        let Some(header) = overlay::kind(schema, kind.as_str()).and_then(|s| s.header.as_ref())
        else {
            continue;
        };
        if let Some((id, name)) = ids.into_iter().find_map(|id| {
            let entry = id.to_string();
            if !entries.contains(&entry) {
                return None;
            }
            let data = entries.doc(&entry).ok().flatten()?;
            let raw = doc_tree(&data)?;
            Some((id, name_at(&overlay::normalize(kind.as_str(), raw), header)))
        }) {
            objects.push(object(kind, id, name));
        }
    }
    Ok(Configuration {
        id,
        root_info: Some(root_info),
        compat,
        props: vec![Prop {
            name: "Name".into(),
            codec: Codec::Str,
            value: Value::Text(name),
        }],
        objects,
        profile: LoadProfile::Query,
        ..Default::default()
    })
}

fn object(kind: MdKind, id: Guid, name: String) -> MdObject {
    MdObject {
        kind,
        id,
        name,
        synonym: Default::default(),
        comment: String::new(),
        generated_types: Vec::new(),
        this_node: None,
        props: Vec::new(),
        children: Vec::new(),
        forms: Vec::new(),
        templates: Vec::new(),
        recalculations: Vec::new(),
        external: Vec::new(),
        subsystems: Vec::new(),
        modules: Vec::new(),
        parts: Default::default(),
        form: None,
        template: None,
        flowchart: None,
    }
}
