use crate::spec::*;
use crate::{FactJson, FactMap};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, thiserror::Error)]
#[error("embedded schema: {0}")]
pub struct SchemaError(pub String);

type Pairs<T> = Vec<(String, T)>;

#[derive(Debug)]
pub struct Schema {
    pub(crate) resource: &'static crate::SchemaResource,
    pub(crate) kinds: FactMap<String, KindSpec>,
    pub(crate) tag_props: FactMap<String, Pairs<PropSpec>>,
    pub(crate) hp_templates: FactMap<String, FactMap<String, PropSpec>>,
    pub(crate) fd_template: FactMap<String, PropSpec>,
    pub(crate) classes_top: Pairs<String>,
    pub(crate) class_index: Vec<usize>,
    pub(crate) types: FactMap<String, PlatformType>,
    pub(crate) stdattrs: FactMap<String, FactMap<String, String>>,
    pub(crate) bag: FactMap<String, BagProp>,
    pub(crate) ui: Ui,
    pub(crate) modules: FactMap<String, Pairs<String>>,
    pub(crate) order: FactMap<String, Vec<String>>,
    pub(crate) counts: FactMap<String, u32>,
    pub(crate) xdto: FactMap<String, Vec<XdtoProp>>,
    pub(crate) folders: Pairs<String>,
    pub(crate) folder_index: Vec<usize>,
    pub(crate) kind_order: Vec<String>,
    pub(crate) gentypes: FactMap<String, Vec<String>>,
    pub(crate) gt_name: FactMap<String, String>,
    pub(crate) cfg_classes: Vec<String>,
    pub(crate) ui_tables: UiTables,
    pub(crate) std_pictures: FactMap<String, String>,
    pub(crate) config_props: Vec<ConfigPropSpec>,
    pub(crate) config_index: Vec<usize>,
}

pub fn schema() -> Result<&'static Schema, SchemaError> {
    crate::embedded_resource()?.schema()
}

fn parse<'a, T: Deserialize<'a>>(file: &str, text: &'a str) -> Result<T, SchemaError> {
    serde_json::from_str(text).map_err(|e| SchemaError(format!("{file}: {e}")))
}

#[derive(Deserialize)]
struct ClassesFile {
    top: Ordered<String>,
}

#[derive(Deserialize)]
struct ModulesFile {
    modules: FactMap<String, Ordered<String>>,
}

#[derive(Deserialize)]
struct OrderFile {
    order: FactMap<String, Vec<String>>,
    #[serde(default)]
    counts: FactMap<String, u32>,
}

#[derive(Deserialize)]
struct BagFile {
    props: FactMap<String, BagProp>,
}

#[derive(Deserialize)]
struct GenFile {
    folders: Vec<(String, String)>,
    gentypes: FactMap<String, Vec<String>>,
    gt_name: FactMap<String, String>,
    cfg_classes: Vec<String>,
}

#[derive(Deserialize)]
struct PicturesFile {
    std_pictures: FactMap<String, String>,
}

#[derive(Deserialize)]
struct ConfigFile {
    props: Vec<(String, Value, String, Value)>,
}

pub(crate) fn sort_ordinals<T>(indices: &mut Vec<usize>, rows: &[T], key: impl Fn(&T) -> &str) {
    indices.sort_unstable_by(|a, b| {
        rows.get(*a)
            .map(&key)
            .cmp(&rows.get(*b).map(&key))
            .then(a.cmp(b))
    });
    indices.dedup_by(|a, b| rows.get(*a).map(&key) == rows.get(*b).map(&key));
}
fn index_of<T>(rows: &[T], key: impl Fn(&T) -> &str) -> Vec<usize> {
    let mut out = (0..rows.len()).collect();
    sort_ordinals(&mut out, rows, key);
    out
}
fn find_ordinal<T>(
    indices: &[usize],
    rows: &[T],
    wanted: &str,
    key: impl Fn(&T) -> &str,
) -> Option<usize> {
    let at = indices
        .binary_search_by(|i| rows.get(*i).map(&key).cmp(&Some(wanted)))
        .ok()?;
    indices.get(at).copied()
}

fn path_of(v: &Value) -> Result<Vec<usize>, SchemaError> {
    let bad = || SchemaError(format!("configuration.json: bad path {v}"));
    v.as_array()
        .ok_or_else(bad)?
        .iter()
        .map(|x| {
            x.as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(bad)
        })
        .collect()
}

fn config_path(v: &Value) -> Result<Option<ConfigPath>, SchemaError> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) if s == "part2" => Ok(Some(ConfigPath::Part2)),
        Value::Array(_) => Ok(Some(ConfigPath::Body(path_of(v)?))),
        other => Err(SchemaError(format!("configuration.json: bad path {other}"))),
    }
}

impl Schema {
    pub(crate) fn load(resource: &'static crate::SchemaResource) -> Result<Schema, SchemaError> {
        let kinds: FactMap<String, KindSpec> = parse("kinds.json", resource.json("kinds.json")?)?;
        let tag_props: FactMap<String, Ordered<PropSpec>> =
            parse("tag_props.json", resource.json("tag_props.json")?)?;
        let tag_props = tag_props.into_iter().map(|(k, v)| (k, v.0)).collect();
        let hp_templates = parse("hp_templates.json", resource.json("hp_templates.json")?)?;
        let fd_template = parse("fd_template.json", resource.json("fd_template.json")?)?;
        let classes: ClassesFile = parse("classes.json", resource.json("classes.json")?)?;
        let classes_top = classes.top.0;
        let class_index = index_of(&classes_top, |(k, _)| k.as_str());

        let types: FactMap<String, PlatformType> =
            crate::type_names::parse_platform_types(resource.json("types.json")?)?
                .into_iter()
                .collect();

        let raw_std: serde_json::Map<String, Value> =
            parse("stdattrs.json", resource.json("stdattrs.json")?)?;
        let mut stdattrs = FactMap::new();
        for (kind, v) in raw_std {
            if v.is_object() {
                let m: FactMap<String, String> = serde_json::from_value(v)
                    .map_err(|e| SchemaError(format!("stdattrs.json: {kind}: {e}")))?;
                stdattrs.insert(kind, m);
            }
        }

        let bag: BagFile = parse("stdattr_bag.json", resource.json("stdattr_bag.json")?)?;
        let ui = parse("ui.json", resource.json("ui.json")?)?;
        let modules: ModulesFile = parse("modules.json", resource.json("modules.json")?)?;
        let order: OrderFile = parse("order.json", resource.json("order.json")?)?;

        let raw_xdto: FactMap<String, Vec<RawXdtoProp>> =
            parse("xdto_md.json", resource.json("xdto_md.json")?)?;
        let xdto = raw_xdto
            .into_iter()
            .map(|(k, v)| (k, v.into_iter().map(XdtoProp::from).collect()))
            .collect();

        let gen_file: GenFile = parse("gen.json", resource.json("gen.json")?)?;
        let folders = gen_file.folders;
        let folder_index = index_of(&folders, |(k, _)| k.as_str());
        let kind_order = folders.iter().map(|(k, _)| k.clone()).collect();

        let ui_tables = parse("ui_tables.json", resource.json("ui_tables.json")?)?;
        let pics: PicturesFile = parse("std_pictures.json", resource.json("std_pictures.json")?)?;
        let cfg: ConfigFile = parse("configuration.json", resource.json("configuration.json")?)?;
        let mut config_props = Vec::with_capacity(cfg.props.len());
        for (name, path, codec, extra) in cfg.props {
            let path = config_path(&path)?;
            config_props.push(ConfigPropSpec {
                name,
                path,
                codec,
                extra: FactJson::Legacy(extra),
            });
        }
        let config_index = index_of(&config_props, |p| p.name.as_str());

        Ok(Schema {
            resource,
            kinds,
            tag_props,
            hp_templates,
            fd_template,
            classes_top,
            class_index,
            types,
            stdattrs,
            bag: bag.props,
            ui,
            modules: modules.modules.into_iter().map(|(k, v)| (k, v.0)).collect(),
            order: order.order,
            counts: order.counts,
            xdto,
            folders,
            folder_index,
            kind_order,
            gentypes: gen_file.gentypes,
            gt_name: gen_file.gt_name,
            cfg_classes: gen_file.cfg_classes,
            ui_tables,
            std_pictures: pics.std_pictures,
            config_props,
            config_index,
        })
    }

    pub fn resource(&self) -> &'static crate::SchemaResource {
        self.resource
    }

    pub fn kind(&self, name: &str) -> Option<&KindSpec> {
        self.kinds.get(name)
    }

    pub fn class_kind(&self, class: &str) -> Option<&str> {
        let i = find_ordinal(&self.class_index, &self.classes_top, class, |(k, _)| k)?;
        self.classes_top.get(i).map(|(_, k)| k.as_str())
    }

    pub fn classes_top(&self) -> impl Iterator<Item = (&str, &str)> {
        self.classes_top
            .iter()
            .map(|(g, k)| (g.as_str(), k.as_str()))
    }

    pub fn platform_type(&self, guid: &str) -> Option<&PlatformType> {
        self.types.get(guid)
    }

    pub fn platform_types(&self) -> impl Iterator<Item = (&str, &PlatformType)> {
        self.types.iter().map(|(id, ty)| (id.as_str(), ty))
    }

    pub fn std_attr(&self, kind: &str, id: &str) -> Option<&str> {
        self.stdattrs.get(kind)?.get(id).map(String::as_str)
    }

    pub fn standard_attributes(&self, kind: &str) -> impl Iterator<Item = (&str, &str)> {
        self.stdattrs
            .get(kind)
            .into_iter()
            .flat_map(|attributes| attributes.iter())
            .map(|(id, name)| (id.as_str(), name.as_str()))
    }

    pub fn bag_prop(&self, guid: &str) -> Option<&BagProp> {
        self.bag.get(guid)
    }

    pub fn bag_properties(&self) -> impl Iterator<Item = (&str, &BagProp)> {
        self.bag.iter().map(|(id, prop)| (id.as_str(), prop))
    }

    pub fn ui(&self) -> &Ui {
        &self.ui
    }

    pub fn modules(&self, kind: &str) -> &[(String, String)] {
        self.modules.get(kind).map_or(&[], Vec::as_slice)
    }

    pub fn observed_order(&self, tag: &str) -> Option<&[String]> {
        self.order.get(tag).map(Vec::as_slice)
    }

    pub fn order_count(&self, context: &str) -> u32 {
        self.counts.get(context).copied().unwrap_or(0)
    }

    pub fn xdto_props(&self, type_name: &str) -> &[XdtoProp] {
        self.xdto.get(type_name).map_or(&[], Vec::as_slice)
    }

    pub fn has_xdto_type(&self, type_name: &str) -> bool {
        self.xdto.contains_key(type_name)
    }

    pub fn tag_props(&self, tag: &str) -> Option<&[(String, PropSpec)]> {
        self.tag_props.get(tag).map(Vec::as_slice)
    }

    pub fn hp_template(&self, tag: &str, prop: &str) -> Option<&PropSpec> {
        self.hp_templates.get(tag)?.get(prop)
    }

    pub fn fd_template(&self, prop: &str) -> Option<&PropSpec> {
        self.fd_template.get(prop)
    }

    pub fn folder(&self, kind: &str) -> Option<&str> {
        let i = find_ordinal(&self.folder_index, &self.folders, kind, |(k, _)| k)?;
        self.folders.get(i).map(|(_, f)| f.as_str())
    }

    pub fn kind_order(&self) -> &[String] {
        &self.kind_order
    }

    pub fn gen_types(&self, kind: &str) -> &[String] {
        self.gentypes.get(kind).map_or(&[], Vec::as_slice)
    }

    pub fn gt_name(&self, category: &str) -> Option<&str> {
        self.gt_name.get(category).map(String::as_str)
    }

    pub fn cfg_classes(&self) -> &[String] {
        &self.cfg_classes
    }

    pub fn ui_tables(&self) -> &UiTables {
        &self.ui_tables
    }

    pub fn std_picture(&self, key: &str) -> Option<&str> {
        self.std_pictures.get(key).map(String::as_str)
    }

    pub fn config_props(&self) -> &[ConfigPropSpec] {
        &self.config_props
    }

    pub fn config_prop(&self, name: &str) -> Option<&ConfigPropSpec> {
        self.config_props.get(find_ordinal(
            &self.config_index,
            &self.config_props,
            name,
            |p| p.name.as_str(),
        )?)
    }
}
