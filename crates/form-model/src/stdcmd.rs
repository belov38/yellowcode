use crate::desc::{FormDesc, Item, ItemKind};
use crate::types::{TypeKind, TypeResolver};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ListOf {
    Catalog,
    Document,
    InformationRegister,
    AccumulationRegister,
    Enum,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FormRole {
    Object {
        document: bool,
    },
    Group,
    Record,
    List {
        of: ListOf,
        hierarchical: bool,
        choice: bool,
    },
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum TableKind {
    TabularSection,
    ValueTable,
    ValueTree,
    DynamicList,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StdCommand {
    pub id: &'static str,
    pub item: String,
    pub on_bar: bool,
    pub picture_only: bool,
    pub in_menu: bool,
    pub default: bool,
    pub new_group: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum NameLang {
    #[default]
    Ru,
    En,
}

const NAMES: &[(&str, &str, &str)] = &[
    ("WriteAndClose", "ЗаписатьИЗакрыть", "WriteAndClose"),
    ("Write", "Записать", "Write"),
    ("PostAndClose", "ПровестиИЗакрыть", "PostAndClose"),
    ("Post", "Провести", "Post"),
    ("UndoPosting", "ОтменитьПроведение", "UndoPosting"),
    ("Reread", "Перечитать", "Reread"),
    ("Copy", "Скопировать", "Copy"),
    ("CopyItem", "Скопировать", "Copy"),
    (
        "SetDeletionMark",
        "УстановитьПометкуУдаления",
        "SetDeletionMark",
    ),
    ("Delete", "Удалить", "Delete"),
    ("ShowInList", "ПоказатьВСписке", "ShowInList"),
    ("CustomizeForm", "ИзменитьФорму", "CustomizeForm"),
    ("Help", "Справка", "Help"),
    ("Create", "Создать", "Create"),
    ("CreateFolder", "СоздатьГруппу", "CreateFolder"),
    ("Change", "Изменить", "Change"),
    ("Refresh", "Обновить", "Refresh"),
    ("Choose", "Выбрать", "Choose"),
    ("Add", "Добавить", "Add"),
    ("MoveUp", "ПереместитьВверх", "MoveUp"),
    ("MoveDown", "ПереместитьВниз", "MoveDown"),
    ("GetURL", "ПолучитьНавигационнуюСсылку", "GetURL"),
    ("ListSettings", "НастроитьСписок", "ListSettings"),
    ("OutputList", "ВывестиСписок", "OutputList"),
    ("CommandBar", "КоманднаяПанель", "CommandBar"),
    ("SearchString", "СтрокаПоиска", "SearchString"),
    ("SearchControl", "УправлениеПоиском", "SearchControl"),
    ("ViewStatus", "СостояниеПросмотра", "ViewStatus"),
    ("Form", "Форма", "Form"),
    ("List", "Список", "List"),
    ("Code", "Код", "Code"),
    ("Description", "Наименование", "Description"),
    ("Parent", "Родитель", "Parent"),
    ("Number", "Номер", "Number"),
    ("Date", "Дата", "Date"),
    ("Period", "Период", "Period"),
    (
        "FindByCurrentValue",
        "ПоискПоТекущемуЗначению",
        "FindByCurrentValue",
    ),
    ("Find", "Найти", "Find"),
    ("CancelSearch", "ОтменитьПоиск", "CancelSearch"),
    ("SearchHistory", "ИсторияПоиска", "SearchHistory"),
    ("SetDateInterval", "УстановитьИнтервал", "SetDateInterval"),
    ("ViewMode", "РежимПросмотра", "ViewMode"),
    (
        "HierarchicalList",
        "ИерархическийСписок",
        "HierarchicalList",
    ),
    ("Tree", "Дерево", "Tree"),
    ("AdvancedSearch", "РасширенныйПоиск", "AdvancedSearch"),
    ("LoadSettings", "ЗагрузитьНастройки", "LoadSettings"),
    ("SaveSettings", "СохранитьНастройки", "SaveSettings"),
    (
        "StandardSettings",
        "СтандартныеНастройки",
        "StandardSettings",
    ),
    (
        "ViewModeSubmenu",
        "ПодменюРежимПросмотра",
        "ViewModeSubmenu",
    ),
    ("MoveItem", "ПереместитьЭлемент", "MoveItem"),
    ("LevelDown", "УровеньНиже", "LevelDown"),
    ("LevelUp", "УровеньВыше", "LevelUp"),
    ("Object", "Объект", "Object"),
    ("Record", "Запись", "Record"),
    ("Main", "Основное", "Main"),
    ("LineNumber", "НомерСтроки", "LineNumber"),
    ("Recorder", "Регистратор", "Recorder"),
    ("Ref", "Ссылка", "Ref"),
];

pub fn std_name(id: &str, lang: NameLang) -> &str {
    match NAMES.iter().find(|(i, _, _)| *i == id) {
        Some((_, ru, en)) => match lang {
            NameLang::Ru => ru,
            NameLang::En => en,
        },
        None => id,
    }
}

#[derive(Clone, Copy)]
enum Pl {
    Text,
    Pic,
    Menu,
}
use Pl::{Menu, Pic, Text};

const OBJECT: &[(&str, Pl)] = &[
    ("WriteAndClose", Text),
    ("Write", Text),
    ("Reread", Menu),
    ("Copy", Menu),
    ("SetDeletionMark", Menu),
    ("Delete", Menu),
    ("ShowInList", Menu),
    ("CustomizeForm", Menu),
    ("Help", Menu),
];
const DOCUMENT: &[(&str, Pl)] = &[
    ("PostAndClose", Text),
    ("Write", Text),
    ("Post", Text),
    ("Reread", Menu),
    ("Copy", Menu),
    ("SetDeletionMark", Menu),
    ("Delete", Menu),
    ("UndoPosting", Menu),
    ("ShowInList", Menu),
    ("CustomizeForm", Menu),
    ("Help", Menu),
];
const RECORD: &[(&str, Pl)] = &[
    ("WriteAndClose", Text),
    ("Write", Text),
    ("Copy", Menu),
    ("Reread", Menu),
    ("Delete", Menu),
    ("CustomizeForm", Menu),
    ("Help", Menu),
];
const OTHER: &[(&str, Pl)] = &[("CustomizeForm", Menu), ("Help", Menu)];

const LIST_TAIL: &[&str] = &[
    "FindByCurrentValue",
    "AdvancedSearch",
    "CancelSearch",
    "GetURL",
    "ListSettings",
    "LoadSettings",
    "SaveSettings",
    "StandardSettings",
    "OutputList",
    "CustomizeForm",
    "Help",
];

fn make(id: &'static str, item: String, pl: Pl, default: bool, in_menu: bool) -> StdCommand {
    StdCommand {
        id,
        item,
        on_bar: !matches!(pl, Menu),
        picture_only: matches!(pl, Pic),
        in_menu,
        default,
        new_group: id == "Post",
    }
}

fn is_default(id: &str) -> bool {
    matches!(id, "WriteAndClose" | "PostAndClose" | "Choose")
}

pub fn form_commands(role: FormRole, bar: &str, lang: NameLang) -> Vec<StdCommand> {
    let form = std_name("Form", lang);
    let name = |id: &str| format!("{form}{}", std_name(id, lang));
    let rows: Vec<(&'static str, Pl)> = match role {
        FormRole::Object { document: false } | FormRole::Group => OBJECT.to_vec(),
        FormRole::Object { document: true } => DOCUMENT.to_vec(),
        FormRole::Record => RECORD.to_vec(),
        FormRole::Other => OTHER.to_vec(),
        FormRole::List {
            of,
            hierarchical,
            choice,
        } => list_rows(of, hierarchical, choice),
    };
    let (bar_rows, menu_rows): (Vec<_>, Vec<_>) =
        rows.into_iter().partition(|(_, p)| !matches!(p, Menu));
    bar_rows
        .into_iter()
        .chain(menu_rows)
        .map(|(id, pl)| {
            let item = if id == "SearchControl" {
                format!(
                    "{bar}{}{}",
                    std_name("List", lang),
                    std_name("SearchControl", lang)
                )
            } else {
                name(id)
            };
            make(id, item, pl, is_default(id), true)
        })
        .collect()
}

fn list_rows(of: ListOf, hierarchical: bool, choice: bool) -> Vec<(&'static str, Pl)> {
    let mut v: Vec<(&'static str, Pl)> = vec![("SearchControl", Pic)];
    if choice {
        v.push(("Choose", Text));
    }
    let creates = !matches!(of, ListOf::AccumulationRegister | ListOf::Enum);
    if creates {
        v.push(("Create", Text));
        if hierarchical && of == ListOf::Catalog {
            v.push(("CreateFolder", Text));
        }
        v.push(("CopyItem", Pic));
    }
    v.push(("Change", Menu));
    if matches!(of, ListOf::Catalog | ListOf::Document | ListOf::Other) {
        v.push(("SetDeletionMark", Menu));
    }
    if creates {
        v.push(("Delete", Menu));
    }
    v.push(("Refresh", Menu));
    match of {
        ListOf::Catalog | ListOf::Other if hierarchical => {
            v.extend([
                ("ViewModeSubmenu", Menu),
                ("MoveItem", Menu),
                ("LevelDown", Menu),
                ("LevelUp", Menu),
            ]);
        }
        ListOf::Document => v.extend([
            ("SetDateInterval", Menu),
            ("Post", Menu),
            ("UndoPosting", Menu),
        ]),
        ListOf::InformationRegister | ListOf::AccumulationRegister => {
            v.push(("SetDateInterval", Menu));
        }
        _ => {}
    }
    v.extend(LIST_TAIL.iter().map(|id| (*id, Menu)));
    v
}

fn std_command_id(cmd: &str) -> Option<&str> {
    let (head, id) = cmd.rsplit_once('.')?;
    (head.ends_with(".StandardCommand") || head == "StandardCommand").then_some(id)
}

pub fn bar_commands(form: &FormDesc, bar: &Item) -> Vec<StdCommand> {
    bar_commands_with(form, bar, &crate::NoConfig)
}

pub fn bar_commands_with(form: &FormDesc, bar: &Item, r: &dyn TypeResolver) -> Vec<StdCommand> {
    if bar.props.autofill == Some(false) {
        return Vec::new();
    }
    let mut explicit = Vec::new();
    collect_std_ids(&bar.children, &mut explicit);
    form_commands(form_role_with(form, r), &bar.name, form.name_lang)
        .into_iter()
        .filter(|c| !explicit.contains(&c.id))
        .collect()
}

fn collect_std_ids<'a>(items: &'a [Item], out: &mut Vec<&'a str>) {
    for it in items {
        if let Some(id) = it.props.command_name.as_deref().and_then(std_command_id) {
            out.push(id);
        }
        collect_std_ids(&it.children, out);
    }
}

pub fn table_kind(form: &FormDesc, table: &Item) -> TableKind {
    let Some(path) = table.props.data_path.as_deref() else {
        return TableKind::ValueTable;
    };
    if path.contains('.') {
        return TableKind::TabularSection;
    }
    match form.attribute(path).map(|a| a.ty.kind()) {
        Some(TypeKind::ValueTable) | None => TableKind::ValueTable,
        Some(TypeKind::ValueTree) => TableKind::ValueTree,
        Some(TypeKind::DynamicList) => TableKind::DynamicList,
        Some(_) => TableKind::Other,
    }
}

pub fn table_commands(kind: TableKind, table: &Item, lang: NameLang) -> Vec<StdCommand> {
    let own_bar = table
        .parts
        .iter()
        .find(|p| p.kind == ItemKind::AutoCommandBar);
    if own_bar.is_some_and(|b| b.props.autofill == Some(false)) {
        return Vec::new();
    }
    let name = |id: &str| format!("{}{}", table.name, std_name(id, lang));
    let rows: &[(&'static str, Pl)] = match kind {
        TableKind::TabularSection | TableKind::ValueTable | TableKind::ValueTree => {
            let set = table.props.change_row_set != Some(false);
            let order = table.props.change_row_order != Some(false);
            let mut v = Vec::new();
            if set {
                v.push(("Add", Text));
            }
            if order {
                v.push(("MoveUp", Pic));
                v.push(("MoveDown", Pic));
            }
            return v
                .into_iter()
                .map(|(id, pl)| make(id, name(id), pl, false, false))
                .collect();
        }
        TableKind::DynamicList => &[("Create", Text), ("CopyItem", Menu)],
        TableKind::Other => &[],
    };
    rows.iter()
        .map(|(id, pl)| make(id, name(id), *pl, false, true))
        .collect()
}

pub fn table_bar_commands(form: &FormDesc, table: &Item) -> Vec<StdCommand> {
    let mut explicit = Vec::new();
    if let Some(bar) = table
        .parts
        .iter()
        .find(|p| p.kind == ItemKind::AutoCommandBar)
    {
        collect_std_ids(&bar.children, &mut explicit);
    }
    table_commands(table_kind(form, table), table, form.name_lang)
        .into_iter()
        .filter(|c| !explicit.contains(&c.id))
        .collect()
}

pub fn main_table(form: &FormDesc) -> Option<&Item> {
    let main = form.attributes.iter().find(|a| a.main)?;
    fn find<'a>(items: &'a [Item], name: &str) -> Option<&'a Item> {
        items.iter().find_map(|it| {
            if it.kind == ItemKind::Table && it.props.data_path.as_deref() == Some(name) {
                return Some(it);
            }
            find(&it.children, name)
        })
    }
    find(&form.items, &main.name)
}

fn list_of(main_table: &str) -> ListOf {
    match main_table.split('.').next().unwrap_or_default() {
        "Catalog" => ListOf::Catalog,
        "Document" => ListOf::Document,
        "InformationRegister" => ListOf::InformationRegister,
        "AccumulationRegister" => ListOf::AccumulationRegister,
        "Enum" => ListOf::Enum,
        _ => ListOf::Other,
    }
}

pub fn floating(role: FormRole) -> bool {
    match role {
        FormRole::Object { document } => !document,
        FormRole::Group | FormRole::Record => true,
        FormRole::List { of, choice, .. } => choice && of == ListOf::Catalog,
        FormRole::Other => false,
    }
}

pub fn form_role(form: &FormDesc) -> FormRole {
    form_role_with(form, &crate::NoConfig)
}

pub fn form_role_with(form: &FormDesc, r: &dyn TypeResolver) -> FormRole {
    if let Some(role) = form.role {
        return role;
    }
    let Some(main) = form.attributes.iter().find(|a| a.main) else {
        return FormRole::Other;
    };
    let ty = main
        .ty
        .types
        .first()
        .map(String::as_str)
        .unwrap_or_default();
    let ty = ty.strip_prefix("cfg:").unwrap_or(ty);
    if ty.starts_with("CatalogObject.") {
        return FormRole::Object { document: false };
    }
    if ty.starts_with("DocumentObject.") {
        return FormRole::Object { document: true };
    }
    if ty.starts_with("InformationRegisterRecordManager.") {
        return FormRole::Record;
    }
    if ty == "DynamicList" {
        let Some(table) = main.main_table.as_deref() else {
            return FormRole::Other;
        };
        let of = list_of(table);
        let hierarchical = of == ListOf::Catalog
            && ["Parent", "Родитель"]
                .iter()
                .any(|p| r.resolve(table, &[p]).is_some());
        let choice = main_table(form).is_some_and(|t| t.props.choice_mode == Some(true));
        return FormRole::List {
            of,
            hierarchical,
            choice,
        };
    }
    FormRole::Other
}
