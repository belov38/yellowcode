use mdconf::{ChildTag, Configuration, MdKind, MdObject};
mod view;
pub use view::{GeneratedChild, GeneratedObject, GeneratedValue};
pub mod facts;

use crate::stdcmd::{FormRole, ListOf, NameLang, std_name};
use crate::{Attribute, FormDesc, GroupKind, Item, ItemKind, LStr, ParseStats, Props, TypeDesc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum GeneratedKind {
    Object,
    Group,
    List,
    Choice,
    Record,
}

impl GeneratedKind {
    pub fn from_ru(s: &str) -> Option<Self> {
        Some(match s {
            "ФормаОбъекта" => GeneratedKind::Object,
            "ФормаГруппы" => GeneratedKind::Group,
            "ФормаСписка" => GeneratedKind::List,
            "ФормаВыбора" => GeneratedKind::Choice,
            "ФормаЗаписи" => GeneratedKind::Record,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            GeneratedKind::Object => "ФормаОбъекта",
            GeneratedKind::Group => "ФормаГруппы",
            GeneratedKind::List => "ФормаСписка",
            GeneratedKind::Choice => "ФормаВыбора",
            GeneratedKind::Record => "ФормаЗаписи",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AutoformError {
    NoSuchRole(&'static str),
    Unsupported(String),
}

impl std::fmt::Display for AutoformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AutoformError::NoSuchRole(r) => write!(f, "the object has no form of the role {r}"),
            AutoformError::Unsupported(k) => write!(f, "generated forms of {k} are not supported"),
        }
    }
}

impl std::error::Error for AutoformError {}

fn text_prop<'a>(o: &'a impl GeneratedObject, name: &str) -> Option<&'a str> {
    match o.prop(name)? {
        GeneratedValue::Text(t) => Some(t),
        _ => None,
    }
}

fn bool_prop(o: &impl GeneratedObject, name: &str) -> bool {
    matches!(o.prop(name), Some(GeneratedValue::Bool(true)))
}

fn len_prop(o: &impl GeneratedObject, name: &str, default: u32) -> u32 {
    text_prop(o, name)
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or(default)
}

fn ru(text: &str) -> Option<LStr> {
    Some(LStr(vec![("ru".into(), text.into())]))
}

struct Field {
    data: String,
    name: String,
    boolean: bool,
    title: Option<LStr>,
}

impl Field {
    fn child(c: impl GeneratedChild) -> Field {
        let boolean = c.is_boolean();
        Field {
            data: c.name().to_owned(),
            name: c.name().to_owned(),
            boolean,
            title: None,
        }
    }

    fn standard(id: &str, title: &str, lang: NameLang) -> Field {
        let n = std_name(id, lang).to_string();
        Field {
            data: n.clone(),
            name: n,
            boolean: false,
            title: ru(title),
        }
    }
}

struct Builder {
    next_id: i32,
    lang: NameLang,
    used: std::collections::HashSet<String>,
}

impl Builder {
    fn item(&mut self, name: String, kind: ItemKind, props: Props) -> Item {
        let mut unique = name.clone();
        let mut n = 1u32;
        while !self.used.insert(unique.clone()) {
            unique = format!("{name}{n}");
            n = n.saturating_add(1);
        }
        let name = unique;
        self.next_id = self.next_id.saturating_add(1);
        Item {
            id: self.next_id,
            name,
            kind,
            props,
            children: Vec::new(),
            parts: Vec::new(),
        }
    }

    fn std<'a>(&self, id: &'a str) -> &'a str {
        std_name(id, self.lang)
    }

    fn field(&mut self, f: &Field, prefix: &str, path: &str, kind: Option<ItemKind>) -> Item {
        let kind = if f.boolean {
            ItemKind::CheckBoxField
        } else {
            kind.unwrap_or(ItemKind::InputField)
        };
        let props = Props {
            data_path: Some(format!("{path}.{}", f.data)),
            title: f.title.clone(),
            ..Props::default()
        };
        self.item(format!("{prefix}{}", f.name), kind, props)
    }

    fn group(&mut self, children: Vec<Item>) -> Item {
        let props = Props {
            group: Some(GroupKind::Vertical),
            show_title: Some(false),
            representation: Some("None".into()),
            ..Props::default()
        };
        let mut g = self.item(self.std("Main").to_string(), ItemKind::UsualGroup, props);
        g.children = children;
        g
    }

    fn tabular(&mut self, ts: impl GeneratedChild, main: &str) -> Item {
        let path = format!("{main}.{}", ts.name());
        let props = Props {
            data_path: Some(path.clone()),
            ..Props::default()
        };
        let mut t = self.item(ts.name().to_owned(), ItemKind::Table, props);
        let n = Field::standard("LineNumber", "N", self.lang);
        t.children
            .push(self.field(&n, ts.name(), &path, Some(ItemKind::LabelField)));
        for c in ts.children().filter(|c| c.tag() == ChildTag::Attribute) {
            let f = Field::child(c);
            t.children.push(self.field(&f, ts.name(), &path, None));
        }
        t
    }

    fn command_bar(&mut self) -> Item {
        let name = format!("{}{}", self.std("Form"), self.std("CommandBar"));
        self.item(name, ItemKind::AutoCommandBar, Props::default())
    }

    fn form(&mut self, role: FormRole, attr: Attribute, items: Vec<Item>) -> FormDesc {
        FormDesc {
            title: None,
            width: None,
            auto_command_bar: Some(self.command_bar()),
            items,
            attributes: vec![attr],
            commands: Vec::new(),
            events: Vec::new(),
            stats: ParseStats::default(),
            role: Some(role),
            name_lang: self.lang,
        }
    }
}

fn main_attribute(id: i32, name: &str, ty: String) -> Attribute {
    Attribute {
        id,
        name: name.to_string(),
        title: None,
        ty: TypeDesc {
            types: vec![ty],
            ..TypeDesc::default()
        },
        main: true,
        columns: Vec::new(),
        main_table: None,
    }
}

fn used_for(c: &impl GeneratedChild, group: bool) -> bool {
    match c.prop("Use") {
        Some(GeneratedValue::Text(t)) => match t {
            "ForFolder" => group,
            "ForFolderAndItem" => true,
            _ => !group,
        },
        _ => !group,
    }
}

fn is_hierarchical(o: &impl GeneratedObject) -> bool {
    o.kind() == MdKind::Catalog && bool_prop(o, "Hierarchical")
}

fn has_folders(o: &impl GeneratedObject) -> bool {
    is_hierarchical(o) && text_prop(o, "HierarchyType") != Some("HierarchyOfItems")
}

fn is_periodic(o: &impl GeneratedObject) -> bool {
    text_prop(o, "InformationRegisterPeriodicity").is_some_and(|p| p != "Nonperiodical")
}

fn list_of(k: MdKind) -> ListOf {
    match k {
        MdKind::Catalog => ListOf::Catalog,
        MdKind::Document => ListOf::Document,
        MdKind::InformationRegister => ListOf::InformationRegister,
        MdKind::AccumulationRegister => ListOf::AccumulationRegister,
        MdKind::Enum => ListOf::Enum,
        _ => ListOf::Other,
    }
}

pub fn generate(
    cfg: &Configuration,
    obj: &MdObject,
    kind: GeneratedKind,
) -> Result<FormDesc, AutoformError> {
    generate_view(cfg.settings.script_variant, obj, kind)
}

pub fn generate_view(
    script_variant: mdconf::ScriptVariant,
    obj: &impl GeneratedObject,
    kind: GeneratedKind,
) -> Result<FormDesc, AutoformError> {
    let lang = match script_variant {
        mdconf::ScriptVariant::Russian => NameLang::Ru,
        mdconf::ScriptVariant::English => NameLang::En,
    };
    let mut b = Builder {
        next_id: 0,
        lang,
        used: Default::default(),
    };
    let no_role = AutoformError::NoSuchRole(kind.name());
    match kind {
        GeneratedKind::Object | GeneratedKind::Group => {
            let group = kind == GeneratedKind::Group;
            match obj.kind() {
                MdKind::Catalog => {
                    if group && !has_folders(obj) {
                        return Err(no_role);
                    }
                    Ok(catalog_form(&mut b, obj, group))
                }
                MdKind::Document if !group => Ok(document_form(&mut b, obj)),
                MdKind::Document => Err(no_role),
                MdKind::Enum | MdKind::InformationRegister | MdKind::AccumulationRegister => {
                    Err(no_role)
                }
                k => Err(AutoformError::Unsupported(k.as_str().to_string())),
            }
        }
        GeneratedKind::Record => match obj.kind() {
            MdKind::InformationRegister => {
                if text_prop(obj, "WriteMode") == Some("RecorderSubordinate") {
                    return Err(no_role);
                }
                Ok(record_form(&mut b, obj))
            }
            MdKind::Catalog | MdKind::Document | MdKind::Enum | MdKind::AccumulationRegister => {
                Err(no_role)
            }
            k => Err(AutoformError::Unsupported(k.as_str().to_string())),
        },
        GeneratedKind::List | GeneratedKind::Choice => {
            let choice = kind == GeneratedKind::Choice;
            match obj.kind() {
                MdKind::Catalog | MdKind::Document | MdKind::Enum => {
                    Ok(list_form(&mut b, obj, choice))
                }
                MdKind::InformationRegister | MdKind::AccumulationRegister if !choice => {
                    Ok(list_form(&mut b, obj, false))
                }
                MdKind::InformationRegister | MdKind::AccumulationRegister => Err(no_role),
                k => Err(AutoformError::Unsupported(k.as_str().to_string())),
            }
        }
    }
}

fn catalog_form(b: &mut Builder, o: &impl GeneratedObject, group: bool) -> FormDesc {
    let main = b.std("Object");
    let mut fields = Vec::new();
    if len_prop(o, "CodeLength", 9) > 0 {
        fields.push(Field::standard("Code", "Код", b.lang));
    }
    if len_prop(o, "DescriptionLength", 25) > 0 {
        fields.push(Field::standard("Description", "Наименование", b.lang));
    }
    if is_hierarchical(o) {
        fields.push(Field::standard("Parent", "Родитель", b.lang));
    }
    fields.extend(
        o.attributes()
            .filter(|c| used_for(c, group))
            .map(Field::child),
    );
    let inputs: Vec<Item> = fields.iter().map(|f| b.field(f, "", main, None)).collect();
    let mut items = vec![b.group(inputs)];
    for ts in o.tabular_sections().filter(|c| used_for(c, group)) {
        items.push(b.tabular(ts, main));
    }
    let role = if group {
        FormRole::Group
    } else {
        FormRole::Object { document: false }
    };
    let attr = main_attribute(
        b.next_id.saturating_add(1),
        main,
        format!("cfg:CatalogObject.{}", o.name()),
    );
    b.form(role, attr, items)
}

fn document_form(b: &mut Builder, o: &impl GeneratedObject) -> FormDesc {
    let main = b.std("Object");
    let mut fields = Vec::new();
    if len_prop(o, "NumberLength", 9) > 0 {
        fields.push(Field::standard("Number", "Номер", b.lang));
    }
    fields.push(Field::standard("Date", "Дата", b.lang));
    fields.extend(o.attributes().map(Field::child));
    let inputs: Vec<Item> = fields.iter().map(|f| b.field(f, "", main, None)).collect();
    let mut items = vec![b.group(inputs)];
    for ts in o.tabular_sections() {
        items.push(b.tabular(ts, main));
    }
    let attr = main_attribute(
        b.next_id.saturating_add(1),
        main,
        format!("cfg:DocumentObject.{}", o.name()),
    );
    b.form(FormRole::Object { document: true }, attr, items)
}

fn register_fields(o: &impl GeneratedObject) -> Vec<Field> {
    o.dimensions()
        .chain(o.resources())
        .chain(o.attributes())
        .map(Field::child)
        .collect()
}

fn record_form(b: &mut Builder, o: &impl GeneratedObject) -> FormDesc {
    let main = b.std("Record");
    let mut fields = Vec::new();
    if is_periodic(o) {
        fields.push(Field::standard("Period", "Период", b.lang));
    }
    fields.extend(register_fields(o));
    let inputs: Vec<Item> = fields.iter().map(|f| b.field(f, "", main, None)).collect();
    let items = vec![b.group(inputs)];
    let attr = main_attribute(
        b.next_id.saturating_add(1),
        main,
        format!("cfg:InformationRegisterRecordManager.{}", o.name()),
    );
    b.form(FormRole::Record, attr, items)
}

fn list_form(b: &mut Builder, o: &impl GeneratedObject, choice: bool) -> FormDesc {
    let list = b.std("List");
    let mut fields = Vec::new();
    match o.kind() {
        MdKind::Catalog => {
            if len_prop(o, "DescriptionLength", 25) > 0 {
                fields.push(Field::standard("Description", "Наименование", b.lang));
            }
            if len_prop(o, "CodeLength", 9) > 0 {
                fields.push(Field::standard("Code", "Код", b.lang));
            }
            if !choice {
                fields.extend(o.attributes().map(Field::child));
            }
        }
        MdKind::Document => {
            fields.push(Field::standard("Date", "Дата", b.lang));
            if len_prop(o, "NumberLength", 9) > 0 {
                fields.push(Field::standard("Number", "Номер", b.lang));
            }
            if !choice {
                fields.extend(o.attributes().map(Field::child));
            }
        }
        MdKind::InformationRegister => {
            if is_periodic(o) {
                fields.push(Field::standard("Period", "Период", b.lang));
            }
            fields.extend(register_fields(o));
        }
        MdKind::AccumulationRegister => {
            fields.push(Field::standard("Period", "Период", b.lang));
            fields.push(Field::standard("Recorder", "Регистратор", b.lang));
            let mut n = Field::standard("LineNumber", "Номер строки", b.lang);
            n.title = ru("Номер строки");
            fields.push(n);
            fields.extend(register_fields(o));
        }
        MdKind::Enum => {
            let mut f = Field::standard("Ref", "", b.lang);
            f.title = ru("");
            fields.push(f);
        }
        _ => {}
    }
    let props = Props {
        data_path: Some(list.to_string()),
        choice_mode: choice.then_some(true),
        ..Props::default()
    };
    let mut table = b.item(list.to_string(), ItemKind::Table, props);
    for f in &fields {
        table
            .children
            .push(b.field(f, list, list, Some(ItemKind::LabelField)));
    }
    let mut attr = main_attribute(
        b.next_id.saturating_add(1),
        list,
        "cfg:DynamicList".to_string(),
    );
    attr.main_table = Some(format!("{}.{}", o.kind().as_str(), o.name()));
    let role = FormRole::List {
        of: list_of(o.kind()),
        hierarchical: has_folders(o),
        choice,
    };
    b.form(role, attr, vec![table])
}
