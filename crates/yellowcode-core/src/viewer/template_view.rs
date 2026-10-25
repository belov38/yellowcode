use mdconf::templates::{Template, TemplateType};
use mdconf::xml::{XmlNode, parse_xml};
use mdconf::{Configuration, MdKind, SubKind};
use serde::Serialize;

use super::path::{self, Target};

pub const MAX_ROWS: u32 = 2000;
pub const DEFAULT_ROWS: u32 = 500;
pub const MAX_CELLS: usize = 100_000;
pub const MAX_COLUMNS: usize = 4096;
pub const MAX_MERGES: usize = 50_000;
pub const MAX_TEXT: usize = 1 << 20;
pub const MAX_XML: usize = 64 << 20;
const DEFAULT_WIDTH: u32 = 72;
const MAX_SET_DEPTH: usize = 16;

#[derive(Serialize, Debug, PartialEq)]
pub struct Cell {
    pub col: u32,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameter: Option<String>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct DcsField {
    pub path: String,
    pub field: String,
    pub title: String,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct DcsSet {
    pub name: String,
    pub kind: String,
    pub data_source: String,
    pub query: Option<String>,
    pub object_name: Option<String>,
    pub auto_fill: Option<bool>,
    pub use_query_group: bool,
    pub fields: Vec<DcsField>,
    pub items: Vec<DcsSet>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct DcsLink {
    pub source: String,
    pub destination: String,
    pub source_expression: String,
    pub destination_expression: String,
    pub parameter: String,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct DcsResource {
    pub path: String,
    pub expression: String,
    pub groups: Vec<String>,
}

#[derive(Serialize, Debug, Default, PartialEq)]
pub struct DcsSettings {
    pub selection: Vec<DcsSelectedItem>,
    pub filter: Vec<DcsFilterItem>,
    pub order: Vec<DcsOrderItem>,
    pub conditional_appearance: Vec<DcsAppearanceItem>,
    pub data_parameters: Vec<DcsSettingParameter>,
    pub output_parameters: Vec<DcsSettingParameter>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct DcsSelectedItem {
    pub kind: String,
    pub r#use: Option<bool>,
    pub field: String,
    pub title: String,
    pub items: Vec<DcsSelectedItem>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct DcsFilterItem {
    pub kind: String,
    pub r#use: Option<bool>,
    pub left: Option<DcsValue>,
    pub comparison_type: String,
    pub right: Option<DcsValue>,
    pub group_type: String,
    pub items: Vec<DcsFilterItem>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct DcsOrderItem {
    pub kind: String,
    pub r#use: Option<bool>,
    pub field: String,
    pub order_type: String,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct DcsValue {
    pub r#type: String,
    pub text: String,
    pub items: Vec<DcsValue>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct DcsSettingParameter {
    pub parameter: String,
    pub r#use: Option<bool>,
    pub value: Option<DcsValue>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct DcsAppearanceItem {
    pub r#use: Option<bool>,
    pub selection: Vec<DcsSelectedItem>,
    pub filter: Vec<DcsFilterItem>,
    pub appearance: Vec<DcsSettingParameter>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct DcsGroupItem {
    pub kind: String,
    pub field: String,
    pub group_type: String,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct DcsCalculatedField {
    pub path: String,
    pub expression: String,
    pub title: String,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct DcsStructure {
    pub settings: DcsSettings,
    pub group_items: Vec<DcsGroupItem>,
    pub kind: String,
    pub fields: Vec<String>,
    pub items: Vec<DcsStructure>,
    pub rows: Vec<DcsStructure>,
    pub columns: Vec<DcsStructure>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct DcsVariant {
    pub settings: DcsSettings,
    pub name: String,
    pub presentation: String,
    pub structure: Vec<DcsStructure>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct DcsParam {
    pub value_types: Vec<String>,
    pub input_parameters: Vec<String>,
    pub available_as_field: Option<bool>,
    pub use_restriction: Option<bool>,
    pub value_list_allowed: Option<bool>,
    pub deny_incomplete: bool,
    pub usage: String,
    pub functional_option: String,
    pub name: String,
    pub title: String,
    pub value: Option<String>,
    pub typed_value: Option<DcsValue>,
    pub expression: String,
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(tag = "view")]
pub enum TemplateView {
    Sheet {
        columns: Vec<u32>,
        rows: Vec<Vec<Cell>>,
        merges: Vec<[u32; 4]>,
        first_row: u32,
        total_rows: u32,
        truncated: bool,
    },
    Dcs {
        sets: Vec<DcsSet>,
        links: Vec<DcsLink>,
        resources: Vec<DcsResource>,
        parameters: Vec<DcsParam>,
        variants: Vec<DcsVariant>,
        default_settings: Option<Box<DcsSettings>>,
        calculated_fields: Vec<DcsCalculatedField>,
    },
    Other {
        kind: String,
        files: Vec<(String, u64)>,
        text: Option<String>,
        too_large: bool,
    },
}

pub fn template_view(cfg: &Configuration, path: &str) -> Option<TemplateView> {
    template_view_range(cfg, path, 0, DEFAULT_ROWS)
}

pub fn template_view_range(
    cfg: &Configuration,
    path: &str,
    first: u32,
    rows: u32,
) -> Option<TemplateView> {
    let t = match path::resolve(cfg, path).ok()? {
        Target::Object(o) if o.kind == MdKind::CommonTemplate => o.template.as_ref()?,
        Target::Sub { sub, .. } if sub.kind == SubKind::Template => sub.template.as_ref()?,
        _ => return None,
    };
    Some(view_of_range(t, first, rows))
}

pub fn view_of(t: &Template) -> TemplateView {
    view_of_range(t, 0, DEFAULT_ROWS)
}

pub fn view_of_range(t: &Template, first: u32, rows: u32) -> TemplateView {
    let rows = rows.min(MAX_ROWS);
    let main = t.files.iter().find(|(n, _)| n == "Template.xml");
    let too_large = main.is_some_and(|(_, b)| b.len() > MAX_XML)
        && matches!(
            t.kind,
            Some(TemplateType::SpreadsheetDocument | TemplateType::DataCompositionSchema)
        );
    let main = main
        .filter(|_| !too_large)
        .and_then(|(_, b)| parse_xml(b).ok());
    let parsed = match (t.kind, main) {
        (Some(TemplateType::SpreadsheetDocument), Some(x)) if x.local == "document" => {
            Some(sheet(&x, first, rows))
        }
        (Some(TemplateType::DataCompositionSchema), Some(x))
            if x.local == "DataCompositionSchema" =>
        {
            Some(dcs(&x))
        }
        _ => None,
    };
    parsed.unwrap_or_else(|| other(t, too_large))
}

fn other(t: &Template, too_large: bool) -> TemplateView {
    let files = t
        .files
        .iter()
        .map(|(n, b)| (n.clone(), b.len() as u64))
        .collect();
    let pick = |ok: fn(&str) -> bool| t.files.iter().find(|(n, _)| ok(n));
    let text_file = match t.kind {
        Some(TemplateType::TextDocument) => pick(|n| n.ends_with(".txt")),
        Some(TemplateType::HTMLDocument) => pick(|n| n.ends_with(".html")),
        _ => None,
    };
    let text = text_file.map(|(_, b)| {
        let b = b.strip_prefix(b"\xef\xbb\xbf").unwrap_or(b);
        let cut = b.get(..MAX_TEXT).unwrap_or(b);
        String::from_utf8_lossy(cut).into_owned()
    });
    TemplateView::Other {
        kind: t.kind.map_or("Unknown", TemplateType::name).to_string(),
        files,
        text,
        too_large,
    }
}

fn child<'a>(n: &'a XmlNode, name: &str) -> Option<&'a XmlNode> {
    n.elements().find(|c| c.local == name)
}

fn text_of(n: &XmlNode, name: &str) -> String {
    child(n, name)
        .and_then(|c| c.text.clone())
        .unwrap_or_default()
}

fn number(n: &XmlNode, name: &str) -> Option<u32> {
    child(n, name)?.text.as_deref()?.trim().parse().ok()
}

fn localized(n: &XmlNode, lang: Option<&str>) -> String {
    let items: Vec<&XmlNode> = n.elements().filter(|i| i.local == "item").collect();
    let pick = lang
        .and_then(|l| items.iter().find(|i| text_of(i, "lang") == l))
        .or_else(|| items.first());
    pick.map(|i| text_of(i, "content")).unwrap_or_default()
}

fn sheet(doc: &XmlNode, first: u32, limit: u32) -> TemplateView {
    let lang = child(doc, "languageSettings")
        .and_then(|l| child(l, "currentLanguage"))
        .and_then(|l| l.text.clone());
    let lang = lang.as_deref();
    let widths: Vec<Option<u32>> = doc
        .elements()
        .filter(|f| f.local == "format")
        .map(|f| number(f, "width"))
        .collect();
    let width_of = |idx: Option<u32>| -> Option<u32> {
        let i = usize::try_from(idx?).ok()?.checked_sub(1)?;
        widths.get(i).copied().flatten()
    };
    let default = width_of(number(doc, "defaultFormatIndex")).unwrap_or(DEFAULT_WIDTH);
    let mut columns = Vec::new();
    if let Some(cols) = child(doc, "columns") {
        let size = (number(cols, "size").unwrap_or(0) as usize).min(MAX_COLUMNS);
        columns = vec![default; size];
        for item in cols.elements().filter(|i| i.local == "columnsItem") {
            let slot = number(item, "index").and_then(|i| columns.get_mut(i as usize));
            let w = child(item, "column").and_then(|c| width_of(number(c, "formatIndex")));
            if let (Some(slot), Some(w)) = (slot, w) {
                *slot = w;
            }
        }
    }
    let merges: Vec<[u32; 4]> = doc
        .elements()
        .filter(|m| m.local == "merge")
        .take(MAX_MERGES)
        .map(|m| {
            let (r, c) = (number(m, "r").unwrap_or(0), number(m, "c").unwrap_or(0));
            let (h, w) = (number(m, "h").unwrap_or(0), number(m, "w").unwrap_or(0));
            [r, c, r.saturating_add(h), c.saturating_add(w)]
        })
        .collect();
    let end = first.saturating_add(limit);
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    let mut cells = 0usize;
    let mut truncated = false;
    let mut max_row = 0u32;
    for item in doc.elements().filter(|i| i.local == "rowsItem") {
        let Some(index) = number(item, "index") else {
            continue;
        };
        max_row = max_row.max(index.saturating_add(1));
        if index < first || index >= end {
            continue;
        }
        if cells >= MAX_CELLS {
            truncated = true;
            continue;
        }
        let Some(row) = child(item, "row") else {
            continue;
        };
        let mut out = Vec::new();
        let mut col = 0u32;
        for c in row.elements().filter(|c| c.local == "c") {
            if let Some(i) = number(c, "i") {
                col = i;
            }
            if let Some(body) = child(c, "c") {
                let text = child(body, "tl")
                    .map(|t| localized(t, lang))
                    .unwrap_or_default();
                let parameter = child(body, "parameter").and_then(|p| p.text.clone());
                if !text.is_empty() || parameter.is_some() {
                    cells += 1;
                    out.push(Cell {
                        col,
                        text,
                        parameter,
                    });
                }
            }
            col = col.saturating_add(1);
        }
        let at = (index - first) as usize;
        if rows.len() <= at {
            rows.resize_with(at + 1, Vec::new);
        }
        if let Some(slot) = rows.get_mut(at) {
            *slot = out;
        }
    }
    let total_rows = number(doc, "height").unwrap_or(0).max(max_row);
    truncated |= total_rows > end;
    TemplateView::Sheet {
        columns,
        rows,
        merges,
        first_row: first,
        total_rows,
        truncated,
    }
}

fn dcs(root: &XmlNode) -> TemplateView {
    let mut budget = DcsBudget(MAX_DCS_NODES);
    let sets = root
        .elements()
        .filter(|s| s.local == "dataSet")
        .map(|s| data_set(s, 0))
        .collect();
    let links = root
        .elements()
        .filter(|l| l.local == "dataSetLink")
        .map(|l| DcsLink {
            source: text_of(l, "sourceDataSet"),
            destination: text_of(l, "destinationDataSet"),
            source_expression: text_of(l, "sourceExpression"),
            destination_expression: text_of(l, "destinationExpression"),
            parameter: text_of(l, "parameter"),
        })
        .collect();
    let resources = root
        .elements()
        .filter(|r| r.local == "totalField")
        .map(|r| DcsResource {
            path: text_of(r, "dataPath"),
            expression: text_of(r, "expression"),
            groups: r
                .elements()
                .filter(|g| g.local == "group")
                .filter_map(|g| g.text.clone())
                .collect(),
        })
        .collect();
    let parameters = root
        .elements()
        .filter(|p| p.local == "parameter")
        .map(|p| DcsParam {
            value_types: child(p, "valueType")
                .map(|v| {
                    v.elements()
                        .filter(|t| t.local == "Type")
                        .filter_map(|t| t.text.as_deref())
                        .map(|t| t.rsplit(':').next().unwrap_or(t).to_string())
                        .collect()
                })
                .unwrap_or_default(),
            input_parameters: child(p, "inputParameters")
                .map(|v| {
                    v.elements()
                        .filter(|i| i.local == "item")
                        .map(|i| format!("{} = {}", text_of(i, "parameter"), text_of(i, "value")))
                        .collect()
                })
                .unwrap_or_default(),
            available_as_field: bool_of(p, "availableAsField"),
            use_restriction: bool_of(p, "useRestriction"),
            value_list_allowed: bool_of(p, "valueListAllowed"),
            deny_incomplete: bool_of(p, "denyIncompleteValues").unwrap_or(false),
            usage: child(p, "use")
                .and_then(|v| v.text.clone())
                .unwrap_or_else(|| "Auto".into()),
            functional_option: text_of(p, "functionalOptionsParameter"),
            name: text_of(p, "name"),
            title: child(p, "title")
                .map(|t| localized(t, None))
                .unwrap_or_default(),
            value: child(p, "value").map(|v| v.text.clone().unwrap_or_default()),
            typed_value: child(p, "value").and_then(|v| dcs_value(v, 0, &mut budget)),
            expression: text_of(p, "expression"),
        })
        .collect();
    TemplateView::Dcs {
        sets,
        links,
        resources,
        parameters,
        calculated_fields: root
            .elements()
            .filter(|n| n.local == "calculatedField")
            .take_while(|_| budget.enter(0))
            .map(|n| DcsCalculatedField {
                path: text_of(n, "dataPath"),
                expression: text_of(n, "expression"),
                title: child(n, "title")
                    .map(|n| localized(n, None))
                    .unwrap_or_default(),
            })
            .collect(),
        default_settings: child(root, "defaultSettings")
            .or_else(|| child(root, "settings"))
            .map(|n| Box::new(dcs_settings(n, 0, &mut budget))),
        variants: root
            .elements()
            .filter(|v| v.local == "settingsVariant")
            .map(|v| DcsVariant {
                settings: child(v, "settings")
                    .map(|n| dcs_settings(n, 0, &mut budget))
                    .unwrap_or_default(),
                name: text_of(v, "name"),
                presentation: child(v, "presentation")
                    .map(|p| localized(p, None))
                    .unwrap_or_default(),
                structure: child(v, "settings")
                    .map(|settings| structure_children(settings, "item", 0, &mut budget))
                    .unwrap_or_default(),
            })
            .collect(),
    }
}

const XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
const MAX_DCS_NODES: usize = 100_000;

struct DcsBudget(usize);
impl DcsBudget {
    fn enter(&mut self, depth: usize) -> bool {
        if depth >= MAX_SET_DEPTH || self.0 == 0 {
            return false;
        }
        self.0 -= 1;
        true
    }
}

fn dcs_kind(n: &XmlNode) -> String {
    n.attr(Some(XSI), "type")
        .unwrap_or_default()
        .rsplit(':')
        .next()
        .unwrap_or_default()
        .to_string()
}

fn direct_items<'a>(n: &'a XmlNode, name: &str) -> impl Iterator<Item = &'a XmlNode> {
    child(n, name)
        .into_iter()
        .flat_map(|n| n.elements())
        .filter(|n| n.local == "item")
}

fn dcs_value(n: &XmlNode, depth: usize, budget: &mut DcsBudget) -> Option<DcsValue> {
    if !budget.enter(depth) || matches!(n.attr(Some(XSI), "nil"), Some("true" | "1")) {
        return None;
    }
    let kind = dcs_kind(n);
    let mut text = n.text.clone().unwrap_or_default();
    let mut items = Vec::new();
    match kind.as_str() {
        "LocalStringType" => text = localized(n, None),
        "StandardPeriod" => {
            text = ["variant", "startDate", "endDate"]
                .iter()
                .filter_map(|name| child(n, name))
                .filter_map(|n| n.text.clone())
                .collect::<Vec<_>>()
                .join("; ");
        }
        "Array" | "ValueListType" => {
            for c in n.elements() {
                let value = if c.local == "value" {
                    Some(c)
                } else if kind == "ValueListType" && c.local == "item" {
                    child(c, "value")
                } else {
                    None
                };
                let value = value.and_then(|v| {
                    if matches!(v.attr(Some(XSI), "nil"), Some("true" | "1")) {
                        budget.enter(depth + 1).then(|| DcsValue {
                            r#type: "Null".into(),
                            text: String::new(),
                            items: Vec::new(),
                        })
                    } else {
                        dcs_value(v, depth + 1, budget)
                    }
                });
                if let Some(value) = value {
                    items.push(value);
                }
            }
        }
        _ => {}
    }
    Some(DcsValue {
        r#type: kind,
        text,
        items,
    })
}

fn selected_item(n: &XmlNode, depth: usize, budget: &mut DcsBudget) -> Option<DcsSelectedItem> {
    if !budget.enter(depth) {
        return None;
    }
    Some(DcsSelectedItem {
        kind: dcs_kind(n),
        r#use: bool_of(n, "use"),
        field: text_of(n, "field"),
        title: child(n, "lwsTitle")
            .map(|n| localized(n, None))
            .unwrap_or_default(),
        items: n
            .elements()
            .filter(|n| n.local == "item")
            .filter_map(|n| selected_item(n, depth + 1, budget))
            .collect(),
    })
}

fn filter_item(n: &XmlNode, depth: usize, budget: &mut DcsBudget) -> Option<DcsFilterItem> {
    if !budget.enter(depth) {
        return None;
    }
    Some(DcsFilterItem {
        kind: dcs_kind(n),
        r#use: bool_of(n, "use"),
        left: child(n, "left").and_then(|n| dcs_value(n, depth + 1, budget)),
        comparison_type: text_of(n, "comparisonType"),
        right: child(n, "right").and_then(|n| dcs_value(n, depth + 1, budget)),
        group_type: text_of(n, "groupType"),
        items: n
            .elements()
            .filter(|n| n.local == "item")
            .filter_map(|n| filter_item(n, depth + 1, budget))
            .collect(),
    })
}

fn setting_parameter(
    n: &XmlNode,
    depth: usize,
    budget: &mut DcsBudget,
) -> Option<DcsSettingParameter> {
    if !budget.enter(depth) {
        return None;
    }
    Some(DcsSettingParameter {
        parameter: text_of(n, "parameter"),
        r#use: bool_of(n, "use"),
        value: child(n, "value").and_then(|n| dcs_value(n, depth + 1, budget)),
    })
}

fn dcs_settings(n: &XmlNode, depth: usize, budget: &mut DcsBudget) -> DcsSettings {
    DcsSettings {
        selection: direct_items(n, "selection")
            .filter_map(|n| selected_item(n, depth, budget))
            .collect(),
        filter: direct_items(n, "filter")
            .filter_map(|n| filter_item(n, depth, budget))
            .collect(),
        order: direct_items(n, "order")
            .filter_map(|n| {
                if !budget.enter(depth) {
                    return None;
                }
                Some(DcsOrderItem {
                    kind: dcs_kind(n),
                    r#use: bool_of(n, "use"),
                    field: text_of(n, "field"),
                    order_type: text_of(n, "orderType"),
                })
            })
            .collect(),
        conditional_appearance: direct_items(n, "conditionalAppearance")
            .filter_map(|n| {
                if !budget.enter(depth) {
                    return None;
                }
                Some(DcsAppearanceItem {
                    r#use: bool_of(n, "use"),
                    selection: direct_items(n, "selection")
                        .filter_map(|n| selected_item(n, depth + 1, budget))
                        .collect(),
                    filter: direct_items(n, "filter")
                        .filter_map(|n| filter_item(n, depth + 1, budget))
                        .collect(),
                    appearance: direct_items(n, "appearance")
                        .filter_map(|n| setting_parameter(n, depth + 1, budget))
                        .collect(),
                })
            })
            .collect(),
        data_parameters: direct_items(n, "dataParameters")
            .filter_map(|n| setting_parameter(n, depth, budget))
            .collect(),
        output_parameters: direct_items(n, "outputParameters")
            .filter_map(|n| setting_parameter(n, depth, budget))
            .collect(),
    }
}

fn structure_children(
    node: &XmlNode,
    tag: &str,
    depth: usize,
    budget: &mut DcsBudget,
) -> Vec<DcsStructure> {
    node.elements()
        .filter(|n| n.local == tag)
        .filter_map(|n| {
            if !budget.enter(depth) {
                return None;
            }
            let group_items: Vec<_> = direct_items(n, "groupItems")
                .filter_map(|n| {
                    if !budget.enter(depth + 1) {
                        return None;
                    }
                    Some(DcsGroupItem {
                        kind: dcs_kind(n),
                        field: text_of(n, "field"),
                        group_type: text_of(n, "groupType"),
                    })
                })
                .collect();
            Some(DcsStructure {
                kind: dcs_kind(n),
                fields: group_items
                    .iter()
                    .map(|n| n.field.clone())
                    .filter(|n| !n.is_empty())
                    .collect(),
                group_items,
                settings: dcs_settings(n, depth + 1, budget),
                items: structure_children(n, "item", depth + 1, budget),
                rows: structure_children(n, "row", depth + 1, budget),
                columns: structure_children(n, "column", depth + 1, budget),
            })
        })
        .collect()
}

fn bool_of(node: &XmlNode, name: &str) -> Option<bool> {
    match text_of(node, name).as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

fn data_set(s: &XmlNode, depth: usize) -> DcsSet {
    let kind = s
        .attr(Some("http://www.w3.org/2001/XMLSchema-instance"), "type")
        .map(|t| t.rsplit(':').next().unwrap_or(t).to_string())
        .unwrap_or_default();
    let fields = s
        .elements()
        .filter(|f| f.local == "field")
        .map(|f| DcsField {
            path: text_of(f, "dataPath"),
            field: text_of(f, "field"),
            title: child(f, "title")
                .map(|t| localized(t, None))
                .unwrap_or_default(),
        })
        .collect();
    let items = if depth < MAX_SET_DEPTH {
        s.elements()
            .filter(|i| i.local == "item")
            .map(|i| data_set(i, depth + 1))
            .collect()
    } else {
        Vec::new()
    };
    DcsSet {
        name: text_of(s, "name"),
        kind,
        data_source: text_of(s, "dataSource"),
        auto_fill: bool_of(s, "autoFillFields"),
        use_query_group: bool_of(s, "useQueryGroupIfPossible").unwrap_or(true),
        object_name: child(s, "objectName").map(|o| o.text.clone().unwrap_or_default()),
        query: child(s, "query").map(|q| q.text.clone().unwrap_or_default()),
        fields,
        items,
    }
}
