use super::tables::{JsonRef, PropRef, TableView};
use brace::Node;

use super::FormResolution;
use super::FormXml;
use super::codecs_a::{self, flstr, fmt_title, path_segs, str_of, value_at};
use super::ctx::{FormInfo, item_key};
use super::decode::{DecodedForm, Item, Rec, decode_form, in_kind, item_tag, nested_at};
use super::fe::{E, from_json, ser};
use super::py::{Py, Raised, py_str};
use super::shared::Shared;
use mdschema::forms::FormCompat;

pub(crate) const ABSENT: &str = "\u{0}";

const DCSSCH: &str = "xmlns:dcssch=\"http://v8.1c.ru/8.1/data-composition-system/schema\" ";

const HEAD: &str = "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n\
<Form xmlns=\"http://v8.1c.ru/8.3/xcf/logform\" \
xmlns:app=\"http://v8.1c.ru/8.2/managed-application/core\" \
xmlns:cfg=\"http://v8.1c.ru/8.1/data/enterprise/current-config\" \
xmlns:dcscor=\"http://v8.1c.ru/8.1/data-composition-system/core\" \
xmlns:dcssch=\"http://v8.1c.ru/8.1/data-composition-system/schema\" \
xmlns:dcsset=\"http://v8.1c.ru/8.1/data-composition-system/settings\" \
xmlns:ent=\"http://v8.1c.ru/8.1/data/enterprise\" \
xmlns:lf=\"http://v8.1c.ru/8.2/managed-application/logform\" \
xmlns:pal=\"http://v8.1c.ru/8.1/data/ui/colors/palette\" xmlns:style=\"http://v8.1c.ru/8.1/data/ui/style\" \
xmlns:sys=\"http://v8.1c.ru/8.1/data/ui/fonts/system\" xmlns:v8=\"http://v8.1c.ru/8.1/data/core\" \
xmlns:v8ui=\"http://v8.1c.ru/8.1/data/ui\" xmlns:web=\"http://v8.1c.ru/8.1/data/ui/colors/web\" \
xmlns:win=\"http://v8.1c.ru/8.1/data/ui/colors/windows\" xmlns:xr=\"http://v8.1c.ru/8.3/xcf/readable\" \
xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
version=\"2.21\">";

const NESTED_ITEM_TAGS: [&str; 6] = [
    "ContextMenu",
    "AutoCommandBar",
    "ExtendedTooltip",
    "SearchStringAddition",
    "ViewStatusAddition",
    "SearchControlAddition",
];

pub(crate) fn di_at(class: &str) -> Option<usize> {
    Some(match class {
        "Button" => 48,
        "Table" => 96,
        "Field" => 55,
        "Group" => 29,
        "Decoration" => 34,
        "Addition" => 23,
        _ => return None,
    })
}

pub(crate) fn di_at_nested(version: &str) -> Option<usize> {
    match version {
        "22" => Some(28),
        "6" => Some(23),
        _ => None,
    }
}

pub(crate) fn display_importance(code: &str) -> Option<&'static str> {
    Some(match code {
        "1" => "VeryHigh",
        "2" => "High",
        "3" => "Usual",
        "4" => "Low",
        "5" => "VeryLow",
        _ => return None,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Emit {
    El(E),
    Absent,
    Cannot,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Src<'s> {
    Item { item: &'s Item, tag: &'s str },
    Rec { rec: &'s Rec, tag: &'s str },
    Form(&'s DecodedForm),
}

fn last<'s, K: AsRef<str>>(pairs: &'s [(K, Node)], key: &str) -> Option<&'s Node> {
    pairs
        .iter()
        .rev()
        .find(|(k, _)| k.as_ref() == key)
        .map(|(_, v)| v)
}

impl<'s> Src<'s> {
    pub(crate) fn body(&self) -> Py<&'s [Node]> {
        match self {
            Src::Item { item, .. } => Ok(&item.body),
            Src::Rec { rec, .. } => Ok(&rec.body),
            Src::Form(_) => Err(Raised),
        }
    }

    pub(crate) fn bag(&self, key: &str) -> Option<&'s Node> {
        match self {
            Src::Item { item, .. } => last(&item.bag, key),
            Src::Rec { rec, .. } => last(&rec.bag, key),
            Src::Form(f) => f.ext_get(key),
        }
    }

    pub(crate) fn has_opt(&self) -> bool {
        !matches!(self, Src::Form(_))
    }

    pub(crate) fn opt(&self, key: &str) -> Option<&'s Node> {
        match self {
            Src::Item { item, .. } => last(&item.opt, key),
            Src::Rec { rec, .. } => last(&rec.opt, key),
            Src::Form(_) => None,
        }
    }

    pub(crate) fn head(&self) -> Py<&'s [Node]> {
        match self {
            Src::Form(f) => Ok(f.head()),
            _ => Err(Raised),
        }
    }

    pub(crate) fn tail(&self) -> Py<&'s [Node]> {
        match self {
            Src::Form(f) => Ok(f.tail()),
            _ => Err(Raised),
        }
    }

    pub(crate) fn cls(&self) -> Py<&'s str> {
        match self {
            Src::Item { item, .. } => Ok(item.class.unwrap_or("nested")),
            Src::Rec { rec, .. } => Ok(rec.class),
            Src::Form(_) => Err(Raised),
        }
    }

    pub(crate) fn item(&self) -> Option<&'s Item> {
        match self {
            Src::Item { item, .. } => Some(item),
            _ => None,
        }
    }

    pub(crate) fn rec(&self) -> Option<&'s Rec> {
        match self {
            Src::Rec { rec, .. } => Some(rec),
            _ => None,
        }
    }

    pub(crate) fn tag(&self) -> Option<&'s str> {
        match self {
            Src::Item { tag, .. } | Src::Rec { tag, .. } => Some(tag),
            Src::Form(_) => None,
        }
    }

    pub(crate) fn form(&self) -> Option<&'s DecodedForm> {
        match self {
            Src::Form(f) => Some(f),
            _ => None,
        }
    }
}

pub(crate) struct FormCtx<'a> {
    pub cfg: &'a dyn FormResolution,
    pub shared: &'a Shared,
    pub info: FormInfo<'a>,
    pub files: Vec<(String, Vec<u8>)>,
    pub deferred: bool,
    pub form_cat: String,
    pub source_serialization: super::Serialization,
    pub prop_depth: usize,
    raised: bool,
}

impl<'a> FormCtx<'a> {
    pub(crate) fn new(cfg: &'a dyn FormResolution, shared: &'a Shared, info: FormInfo<'a>) -> Self {
        let mut fc = FormCtx {
            cfg,
            shared,
            source_serialization: info.form.layout.serialization,
            info,
            files: Vec::new(),
            deferred: false,
            form_cat: String::new(),
            prop_depth: 0,
            raised: false,
        };
        codecs_a::prepare(&mut fc);
        fc
    }

    pub(crate) fn tables(&self) -> TableView<'a> {
        self.shared.tables()
    }

    pub(crate) fn raise(&mut self) {
        self.raised = true;
    }

    pub(crate) fn check<T>(&mut self, r: Py<T>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(Raised) => {
                self.raise();
                None
            }
        }
    }

    pub(crate) fn raised(&self) -> bool {
        self.raised || self.info.raised()
    }
}

pub(crate) type Custom =
    fn(&mut FormCtx<'_>, &str, &str, &Src<'_>, Option<PropRef<'_>>) -> Option<Emit>;

pub(crate) type Section = fn(&mut FormCtx<'_>, &mut Vec<String>);

#[rustfmt::skip]
pub(crate) const CUSTOM: &[(&str, &str, &str, Custom)] = {
    use super::codecs_a as a;
    use super::codecs_b as b;
    use super::section_codecs as c;
    use super::sections as s;
    &[
        ("*", "Title", "c_fmt_title", a::c_fmt_title),
        ("*", "WarningOnEditRepresentation", "c_warning_on_edit", a::c_warning_on_edit),
        ("*", "Events", "c_events", a::c_events),
        ("*", "DataPath", "c_datapath", a::c_datapath),
        ("InputField", "FooterDataPath", "c_dp_at", a::c_dp_footer),
        ("InputField", "MinValue", "c_numeric_bound", b::c_numeric_bound),
        ("InputField", "MaxValue", "c_numeric_bound", b::c_numeric_bound),
        ("LabelField", "FooterDataPath", "c_dp_at", a::c_dp_footer),
        ("Page", "TitleDataPath", "c_dp_at", a::c_dp_page_title),
        ("UsualGroup", "TitleDataPath", "c_dp_at", a::c_dp_group_title),
        ("Table", "RowPictureDataPath", "c_rowpicture", a::c_rowpicture),
        ("Table", "RowFilter", "c_row_filter", a::c_row_filter),
        ("*", "AdditionSource", "c_addition_source", a::c_addition_source),
        ("SearchControlAddition", "AutoMaxWidth", "c_search_control_width", a::c_search_control_width),
        ("Table", "UserSettingsGroup", "c_user_settings_group", a::c_user_settings_group),
        ("ButtonGroup", "CommandSource", "c_cmd_source", a::c_cmd_source),
        ("CommandBar", "CommandSource", "c_cmd_source", a::c_cmd_source),
        ("Popup", "CommandSource", "c_cmd_source", a::c_cmd_source),
        ("Button", "CommandName", "c_command_name", a::c_command_name),
        ("*", "CommandSet", "c_cmdset", a::c_cmdset),
        ("*", "Picture", "c_picture", b::c_picture),
        ("*", "HeaderPicture", "c_picture", b::c_picture),
        ("*", "RowsPicture", "c_picture", b::c_picture),
        ("*", "ValuesPicture", "c_picture", b::c_picture),
        ("*", "FooterPicture", "c_picture", b::c_picture),
        ("*", "ChoiceButtonPicture", "c_picture", b::c_picture),
        ("*", "TextColor", "c_color", b::c_color),
        ("*", "BackColor", "c_color", b::c_color),
        ("*", "BorderColor", "c_color", b::c_color),
        ("*", "TitleTextColor", "c_color", b::c_color),
        ("*", "TitleBackColor", "c_color", b::c_color),
        ("*", "HiddenStateTitleBackColor", "c_color", b::c_color),
        ("*", "PictureColor", "c_color", b::c_color),
        ("*", "FooterTextColor", "c_color", b::c_color),
        ("*", "FooterBackColor", "c_color", b::c_color),
        ("*", "HeaderTextColor", "c_color", b::c_color),
        ("*", "HeaderBackColor", "c_color", b::c_color),
        ("*", "Font", "c_font", b::c_font),
        ("*", "TitleFont", "c_font", b::c_font),
        ("*", "FooterFont", "c_font", b::c_font),
        ("*", "HeaderFont", "c_font", b::c_font),
        ("Form", "MobileDeviceCommandBarContent", "c_mobile_cmdbar", a::c_mobile_cmdbar),
        ("Form", "ReportResult", "c_ext_path", a::c_ext_path),
        ("Form", "DetailsData", "c_ext_path", a::c_ext_path),
        ("Form", "VariantAppearance", "c_ext_path", a::c_ext_path),
        ("Form", "CustomSettingsFolder", "c_custom_settings_folder", a::c_custom_settings_folder),
        ("InputField", "ChoiceList", "c_choice_list", b::c_choice_list),
        ("RadioButtonField", "ChoiceList", "c_choice_list", b::c_choice_list),
        ("InputField", "ChoiceParameters", "c_choice_params", b::c_choice_params),
        ("InputField", "ChoiceParameterLinks", "c_choice_links", b::c_choice_links),
        ("InputField", "TypeLink", "c_type_link", b::c_type_link),
        ("Attribute", "Type", "c_type", c::c_type),
        ("Column", "Type", "c_type", c::c_type),
        ("Parameter", "Type", "c_type", c::c_type),
        ("Attribute", "FunctionalOptions", "c_fo", c::c_fo),
        ("Column", "FunctionalOptions", "c_fo", c::c_fo),
        ("Command", "FunctionalOptions", "c_fo", c::c_fo),
        ("Attribute", "View", "c_roles", c::c_roles),
        ("Attribute", "Edit", "c_roles", c::c_roles),
        ("Column", "View", "c_roles", c::c_roles),
        ("Column", "Edit", "c_roles", c::c_roles),
        ("Command", "Use", "c_roles", c::c_roles),
        ("*", "Shortcut", "c_shortcut", c::c_shortcut),
        ("Command", "Picture", "c_cmd_picture", c::c_cmd_picture),
        ("Command", "AssociatedTableElementId", "c_assoc_table", c::c_assoc_table),
        ("Attribute", "Save", "c_fields", c::c_fields),
        ("Attribute", "UseAlways", "c_fields", c::c_fields),
        ("Attribute", "Settings", "c_settings", s::c_settings),
        ("Form", "GroupList", "c_group_list", c::c_group_list),
        ("Table", "ShowCommandBar", "c_table_show_cmdbar", c::c_table_show_cmdbar),
        ("InputField", "ShowCheckBoxesInDropList", "c_show_checkboxes", c::c_show_checkboxes),
        ("InputField", "MultipleValueDataPath", "c_multi_value_dp", c::c_multi_value_dp),
        ("*", "UserVisible", "c_user_visible", c::c_user_visible),
        ("*", "ChoiceForm", "c_md_ref", c::c_md_ref),
        ("*", "AvailableTypes", "c_type_prop", c::c_type_prop),
        ("Button", "Parameter", "c_type_prop", c::c_type_prop),
    ]
};

pub(crate) fn custom(ctx: &str, prop: &str) -> Option<Custom> {
    CUSTOM
        .iter()
        .find(|(c, p, _, _)| *c == ctx && *p == prop)
        .map(|(_, _, _, f)| *f)
}

pub(crate) fn section(name: &str) -> Option<Section> {
    use super::sections as s;
    Some(match name {
        "Attributes" => s::s_attributes,
        "Commands" => s::s_commands,
        "Parameters" => s::s_parameters,
        "CommandInterface" => s::s_cmdinterface,
        _ => return None,
    })
}

pub(crate) fn emit_generic(
    fc: &mut FormCtx<'_>,
    prop: &str,
    ent: PropRef<'_>,
    v: Option<&Node>,
) -> Emit {
    let c = ent.codec;
    let bv = v.map_or_else(|| "None".to_string(), brace::to_compact);
    if c == "absent" {
        return Emit::Absent;
    }
    if ent.default.iter().any(|d| d.as_str() == Some(bv.as_str())) {
        return Emit::Absent;
    }
    match c {
        "int" => Emit::El(E::leaf(prop, str_of(v))),
        "str" => {
            let s = str_of(v);
            if s.is_empty() {
                Emit::Absent
            } else {
                Emit::El(E::leaf(prop, s))
            }
        }
        "flstr" => flstr(prop, v),
        "table" => {
            if fc_external_owner(fc.info.owner)
                && let Some(value) = ent
                    .raw
                    .get("external_values")
                    .and_then(|values| values.get(bv.as_str()))
            {
                return match from_json(value) {
                    Some(element) => Emit::El(element),
                    None => {
                        fc.raise();
                        Emit::Cannot
                    }
                };
            }
            if !ent.raw.get("table").is_some_and(|t| t.is_object()) {
                fc.raise();
                return Emit::Cannot;
            }
            match ent.table.get(bv.as_str()).filter(|x| !x.raw.is_null()) {
                None => {
                    let lstr = v
                        .and_then(Node::as_list)
                        .filter(|l| l.len() >= 2 && l.first().is_some_and(|x| py_str(x) == "1"));
                    match lstr {
                        Some(l) if l.len() == 3 && l.get(1).is_some_and(Node::is_list) => {
                            fmt_title(prop, v)
                        }
                        Some(_) => flstr(prop, v),
                        None => Emit::Cannot,
                    }
                }
                Some(x) if x.raw.as_str() == Some(ABSENT) => Emit::Absent,
                Some(x) => match x.parsed.and_then(from_json) {
                    Some(e) => Emit::El(e),
                    None => {
                        fc.raise();
                        Emit::Cannot
                    }
                },
            }
        }
        _ => Emit::Cannot,
    }
}

pub(crate) fn external_parameter_names(element: &mut E) -> Py<()> {
    fn walk(
        element: &mut E,
        appearance: bool,
        appearance_parent: bool,
        parameter_parent: bool,
        names: &mdschema::FactMap<String, String>,
        aliases: &[(&str, mdschema::StaticValue)],
    ) -> Py<()> {
        let appearance = appearance || element.tag == "dcsset:appearance";
        if appearance
            && element.tag == "dcscor:parameter"
            && element.kids.is_empty()
            && let Some(text) = &element.text
            && let Some((native, _)) = names.iter().find(|(_, value)| *value == text)
        {
            element.text = Some(native.clone());
        }
        if parameter_parent
            && element.tag == "dcscor:parameter"
            && element.kids.is_empty()
            && element.attrs.is_empty()
            && let Some(text) = &element.text
            && let Some((_, alias)) = aliases.iter().find(|(native, _)| *native == text.as_str())
        {
            element.text = Some(alias.as_str().ok_or(Raised)?.to_owned());
        }
        let parameter_parent = appearance_parent
            && element.tag == "dcscor:item"
            && element
                .attrs
                .iter()
                .any(|(key, value)| key == "xsi:type" && value == "dcsset:SettingsParameterValue");
        let appearance_parent = element.tag == "dcsset:appearance";
        for child in &mut element.kids {
            walk(
                child,
                appearance,
                appearance_parent,
                parameter_parent,
                names,
                aliases,
            )?;
        }
        Ok(())
    }
    let tables = mdschema::forms_upgrade::upgrade_tables().map_err(|_| Raised)?;
    let aliases = mdschema::static_schema()
        .table("forms/misc.json")
        .and_then(|table| table.get("external_dcs_names"))
        .and_then(|table| table.as_object())
        .ok_or(Raised)?;
    walk(
        element,
        false,
        false,
        false,
        &tables.writer_set.dcs_names,
        aliases,
    )
}

pub(crate) fn fc_external_owner(owner: Option<&str>) -> bool {
    owner.is_some_and(|o| {
        o.starts_with("ExternalDataProcessor.") || o.starts_with("ExternalReport.")
    })
}

pub(crate) fn external_shape_matches(shape: JsonRef<'_>, item: &Item) -> bool {
    if let Some(choices) = shape.as_array() {
        return choices
            .iter()
            .filter(|shape| shape.is_object())
            .any(|shape| external_shape_matches(shape, item));
    }
    let number = |key| shape.get(key).and_then(JsonRef::as_i64);
    let index = |key| number(key).and_then(|n| usize::try_from(n).ok());
    let Some(kind) = index("kind_slot").and_then(|n| item.body.get(n)) else {
        return false;
    };
    let Some(sublist) = index("sublist_slot")
        .and_then(|n| item.body.get(n))
        .and_then(Node::as_list)
    else {
        return false;
    };
    let class_matches = |class: JsonRef<'_>| {
        if class.is_null() {
            item.class.is_none()
        } else {
            class.as_str().is_some_and(|c| item.class == Some(c))
        }
    };
    let matched = shape.get("class").is_some_and(|class| {
        class.as_array().map_or_else(
            || class_matches(class),
            |classes| classes.iter().any(class_matches),
        )
    }) && shape.get("version").and_then(JsonRef::as_str)
        == Some(item.version.as_str())
        && index("len") == Some(item.body.len())
        && number("kind").is_some_and(|n| kind.as_i64() == Some(n))
        && index("sublist_len") == Some(sublist.len())
        && shape
            .get("sublist_versions")
            .and_then(JsonRef::as_array)
            .is_some_and(|versions| {
                versions
                    .iter()
                    .filter_map(JsonRef::as_i64)
                    .any(|n| sublist.first().and_then(Node::as_i64) == Some(n))
            });
    if !matched {
        return false;
    }
    match (shape.get("value_path"), shape.get("values")) {
        (None, None) => true,
        (Some(path), Some(values)) => {
            let Some(path) = path.as_array() else {
                return false;
            };
            let mut indexes = path
                .iter()
                .map(|index| index.as_i64().and_then(|i| usize::try_from(i).ok()));
            let Some(mut value) = indexes
                .next()
                .flatten()
                .and_then(|index| item.body.get(index))
            else {
                return false;
            };
            for index in indexes {
                let Some(next) = index.and_then(|index| value.at(index)) else {
                    return false;
                };
                value = next;
            }
            let Some(value) = value.as_i64() else {
                return false;
            };
            values.as_array().is_some_and(|values| {
                values
                    .iter()
                    .filter_map(JsonRef::as_i64)
                    .any(|allowed| allowed == value)
            })
        }
        _ => false,
    }
}

pub(crate) fn emit_prop(fc: &mut FormCtx<'_>, ctx: &str, prop: &str, src: &Src<'_>) -> Emit {
    let shared = fc.shared;
    let external = fc_external_owner(fc.info.owner);
    let moved = external
        .then(|| {
            TableView::embedded(true).learned.get(ctx).and_then(|c| {
                c.props
                    .iter()
                    .find(|(name, entry)| {
                        *name == prop
                            && entry.raw.get("moved").and_then(|v| v.as_bool()) == Some(true)
                    })
                    .map(|(_, entry)| entry)
            })
        })
        .flatten();
    let ent = moved.or_else(|| shared.prop(ctx, prop));
    let external_only =
        ent.is_some_and(|e| e.raw.get("external_only").and_then(JsonRef::as_bool) == Some(true));
    let external_strict = external
        && ent
            .is_some_and(|e| e.raw.get("external_strict").and_then(JsonRef::as_bool) == Some(true));
    if external_only && !external {
        return Emit::Absent;
    }
    if (external_only || external_strict)
        && !ent
            .and_then(|e| e.raw.get("external_shape"))
            .zip(src.item())
            .is_some_and(|(shape, item)| external_shape_matches(shape, item))
    {
        fc.raise();
        return Emit::Cannot;
    }
    if let Some(f) = custom(ctx, prop).or_else(|| custom("*", prop))
        && let Some(mut r) = f(fc, ctx, prop, src, ent)
    {
        if external
            && let Emit::El(element) = &mut r
            && external_parameter_names(element).is_err()
        {
            fc.raise();
            return Emit::Cannot;
        }
        if external
            && prop == "Events"
            && let Emit::El(e) = &mut r
        {
            e.kids
                .retain(|event| event.text.as_deref().is_some_and(|s| !s.is_empty()));
            if e.kids.is_empty() {
                r = Emit::Absent;
            }
        }
        return r;
    }
    let Some(ent) = ent else {
        return Emit::Cannot;
    };
    if ent.raw.get("path").is_none() {
        return Emit::Cannot;
    }
    let Some(path) = path_segs(ent) else {
        fc.raise();
        return Emit::Cannot;
    };
    let Some(v) = fc.check(value_at(src, &path)) else {
        return Emit::Cannot;
    };
    let result = emit_generic(fc, prop, ent, v);
    if (external_only || external_strict) && matches!(result, Emit::Cannot) {
        fc.raise();
    }
    result
}

pub(crate) fn esc_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn child_tag(ch: &Item) -> Option<&'static str> {
    if ch.class.is_some_and(in_kind) && ch.body.len() <= 5 {
        return None;
    }
    Some(item_tag(ch, None, None))
}

fn write_children(fc: &mut FormCtx<'_>, children: &[Item], depth: usize, lines: &mut Vec<String>) {
    if children.is_empty() {
        return;
    }
    let pad = "\t".repeat(depth);
    lines.push(format!("{pad}<ChildItems>"));
    for ch in children {
        let Some(t) = child_tag(ch) else {
            fc.raise();
            return;
        };
        write_item(fc, ch, t, t, depth + 1, lines);
        if fc.raised() {
            return;
        }
    }
    lines.push(format!("{pad}</ChildItems>"));
}

fn write_item(
    fc: &mut FormCtx<'_>,
    it: &Item,
    tag: &str,
    ctx: &str,
    depth: usize,
    lines: &mut Vec<String>,
) {
    let shared = fc.shared;
    let src = Src::Item { item: it, tag };
    let pcls = match it.class {
        Some(c) => c,
        None if tag.ends_with("Addition") => "Addition",
        None => "nested",
    };
    let mut nested: Vec<(&str, &Item)> = Vec::new();
    for (pos, n) in &it.nested {
        if let Some(nt) = nested_at(pcls, *pos) {
            match nested.iter_mut().find(|(t, _)| *t == nt) {
                Some(slot) => slot.1 = n,
                None => nested.push((nt, n)),
            }
        }
    }
    let mut body: Vec<String> = Vec::new();
    for el in shared
        .order_for(ctx, fc_external_owner(fc.info.owner))
        .iter()
    {
        if fc.raised() {
            return;
        }
        let sub = NESTED_ITEM_TAGS
            .contains(&el)
            .then(|| nested.iter().find(|(t, _)| *t == el))
            .flatten();
        if let Some((_, n)) = sub {
            let sub_ctx = if matches!(el, "ContextMenu" | "ExtendedTooltip" | "AutoCommandBar") {
                format!("{tag}/{el}")
            } else {
                el.to_string()
            };
            write_item(fc, n, el, &sub_ctx, depth + 1, &mut body);
        } else if el == "ChildItems" {
            write_children(fc, &it.children, depth + 1, &mut body);
        } else if let Emit::El(e) = emit_prop(fc, ctx, el, &src) {
            ser(&e, depth + 1, &mut body);
        }
    }
    if fc.raised() {
        return;
    }
    let Some(name) = it.name() else {
        fc.raise();
        return;
    };
    let mut attrs = format!(" name=\"{}\" id=\"{}\"", esc_attr(name), item_key(it));
    let dpos = match it.class {
        Some(c) => di_at(c),
        None => di_at_nested(&it.version),
    };
    if let Some(di) = dpos
        .and_then(|p| it.body.get(p))
        .and_then(Node::text)
        .and_then(display_importance)
    {
        attrs.push_str(&format!(" DisplayImportance=\"{di}\""));
    }
    let pad = "\t".repeat(depth);
    if body.is_empty() {
        lines.push(format!("{pad}<{tag}{attrs}/>"));
    } else {
        lines.push(format!("{pad}<{tag}{attrs}>"));
        lines.append(&mut body);
        lines.push(format!("{pad}</{tag}>"));
    }
}

pub(crate) fn form_to_xml(
    cfg: &dyn FormResolution,
    shared: &Shared,
    entry: &Node,
    owner: Option<&str>,
) -> Option<FormXml> {
    form_to_xml_from_source(cfg, shared, entry, owner, super::Serialization::of(entry)?)
}

pub(crate) fn form_to_xml_from_source(
    cfg: &dyn FormResolution,
    shared: &Shared,
    entry: &Node,
    owner: Option<&str>,
    source: super::Serialization,
) -> Option<FormXml> {
    let mut df = decode_form(entry)?;
    if fc_external_owner(owner) {
        let tail = super::upgrade::form::xml_tail(df.tail()).ok()?;
        let begin = df.form.children_at?.checked_add(1)?;
        df.form.body.truncate(begin);
        df.form.body.extend(tail);
    }
    debug_assert!(
        df.layout.serialization.is_4_59(),
        "form_to_xml: a 4/59 entry expected"
    );
    if !df.layout.serialization.is_4_59() {
        return None;
    }

    let info = FormInfo::new(cfg, shared, &df, owner)?;
    let mut fc = FormCtx::new(cfg, shared, info);
    fc.source_serialization = source;
    if !shared.tables().learned.contains_key("Form") {
        return None;
    }
    let src = Src::Form(&df);
    let mut lines = vec![match shared.compat() {
        FormCompat::V8_3_12 => HEAD.replacen(DCSSCH, "", 1),
        FormCompat::Base | FormCompat::V8_5_1 => HEAD.to_string(),
    }];
    for el in shared
        .order_for("Form", fc_external_owner(fc.info.owner))
        .iter()
    {
        if fc.raised() {
            return None;
        }
        match el {
            "AutoCommandBar" => {
                let acb = df.acb()?;
                write_item(
                    &mut fc,
                    acb,
                    "AutoCommandBar",
                    "AutoCommandBar",
                    1,
                    &mut lines,
                );
            }
            "ChildItems" => write_children(&mut fc, &df.form.children, 1, &mut lines),
            el => match section(el) {
                Some(s) => s(&mut fc, &mut lines),
                None => {
                    if let Emit::El(e) = emit_prop(&mut fc, "Form", el, &src) {
                        ser(&e, 1, &mut lines);
                    }
                }
            },
        }
    }
    if fc.raised() {
        return None;
    }
    lines.push("</Form>".to_string());
    Some((lines.join("\r\n"), fc.files, fc.deferred))
}
