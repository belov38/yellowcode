use std::collections::BTreeMap;

use crate::dom::XNode;
use crate::{ItemKind, LStr, PictureRef};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum TitleLocation {
    Auto,
    Left,
    Top,
    Right,
    Bottom,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum HAlign {
    Auto,
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum GroupKind {
    Vertical,
    Horizontal,
    HorizontalIfPossible,
    AlwaysHorizontal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Spacing {
    None,
    Half,
    Single,
    OneAndHalf,
    Double,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Location {
    None,
    Auto,
    Top,
    Bottom,
    CommandBar,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Props {
    pub title: Option<LStr>,
    pub title_location: Option<TitleLocation>,
    pub title_height: Option<u32>,
    pub tooltip: Option<LStr>,
    pub visible: Option<bool>,
    pub enabled: Option<bool>,
    pub read_only: Option<bool>,
    pub data_path: Option<String>,
    pub command_name: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub auto_max_width: Option<bool>,
    pub max_width: Option<u32>,
    pub horizontal_stretch: Option<bool>,
    pub vertical_stretch: Option<bool>,
    pub group_horizontal_align: Option<HAlign>,
    pub horizontal_align: Option<HAlign>,
    pub group: Option<GroupKind>,
    pub show_title: Option<bool>,
    pub representation: Option<String>,
    pub united: Option<bool>,
    pub children_align: Option<String>,
    pub child_items_width: Option<String>,
    pub horizontal_spacing: Option<Spacing>,
    pub vertical_spacing: Option<Spacing>,
    pub multi_line: Option<bool>,
    pub password_mode: Option<bool>,
    pub input_hint: Option<LStr>,
    pub mark_negatives: Option<bool>,
    pub choice_button: Option<bool>,
    pub drop_list_button: Option<bool>,
    pub clear_button: Option<bool>,
    pub spin_button: Option<bool>,
    pub open_button: Option<bool>,
    pub list_choice_mode: Option<bool>,
    pub choice_list: Vec<LStr>,
    pub check_box_type: Option<String>,
    pub radio_button_type: Option<String>,
    pub columns_count: Option<u32>,
    pub pages_representation: Option<String>,
    pub height_in_table_rows: Option<u32>,
    pub button_type: Option<String>,
    pub default_button: Option<bool>,
    pub hyperlink: Option<bool>,
    pub picture: Option<PictureRef>,
    pub events: Vec<(String, String)>,
    pub autofill: Option<bool>,
    pub change_row_set: Option<bool>,
    pub change_row_order: Option<bool>,
    pub header: Option<bool>,
    pub command_bar_location: Option<Location>,
    pub search_string_location: Option<Location>,
    pub view_status_location: Option<Location>,
    pub search_control_location: Option<Location>,
    pub choice_mode: Option<bool>,
}

pub(crate) fn lstr(n: &XNode) -> LStr {
    let mut out = Vec::new();
    for it in n.children.iter().filter(|c| c.name == "item") {
        let lang = it.text_of("lang").unwrap_or("").to_string();
        let text = it
            .child("content")
            .map(|c| c.text.clone())
            .unwrap_or_default();
        out.push((lang, text));
    }
    LStr(out)
}

pub(crate) fn picture(n: &XNode) -> Option<PictureRef> {
    if let Some(r) = n.text_of("Ref") {
        if let Some(x) = r.strip_prefix("StdPicture.") {
            return Some(PictureRef::Std(x.to_string()));
        }
        if let Some(x) = r.strip_prefix("CommonPicture.") {
            return Some(PictureRef::Config(x.to_string()));
        }
        return None;
    }
    n.text_of("Abs")
        .map(|a| PictureRef::Embedded(a.to_string()))
}

fn bump(m: &mut BTreeMap<String, u32>, key: String) {
    *m.entry(key).or_insert(0) += 1;
}

fn enum_of<T: Copy>(table: &[(&str, T)], s: &str) -> Option<T> {
    table.iter().find(|(k, _)| *k == s).map(|(_, v)| *v)
}

const TITLE_LOCATION: &[(&str, TitleLocation)] = &[
    ("Auto", TitleLocation::Auto),
    ("Left", TitleLocation::Left),
    ("Top", TitleLocation::Top),
    ("Right", TitleLocation::Right),
    ("Bottom", TitleLocation::Bottom),
    ("None", TitleLocation::None),
];
const HALIGN: &[(&str, HAlign)] = &[
    ("Auto", HAlign::Auto),
    ("Left", HAlign::Left),
    ("Center", HAlign::Center),
    ("Right", HAlign::Right),
];
const GROUP: &[(&str, GroupKind)] = &[
    ("Vertical", GroupKind::Vertical),
    ("Horizontal", GroupKind::Horizontal),
    ("HorizontalIfPossible", GroupKind::HorizontalIfPossible),
    ("AlwaysHorizontal", GroupKind::AlwaysHorizontal),
];
const LOCATION: &[(&str, Location)] = &[
    ("None", Location::None),
    ("Auto", Location::Auto),
    ("Top", Location::Top),
    ("Bottom", Location::Bottom),
    ("CommandBar", Location::CommandBar),
];
const SPACING: &[(&str, Spacing)] = &[
    ("None", Spacing::None),
    ("Half", Spacing::Half),
    ("Single", Spacing::Single),
    ("OneAndHalf", Spacing::OneAndHalf),
    ("Double", Spacing::Double),
];

impl Props {
    pub(crate) fn set(
        &mut self,
        kind: &ItemKind,
        n: &XNode,
        bad: &mut BTreeMap<String, u32>,
    ) -> bool {
        let tag = n.name.as_str();
        let txt = n.text.trim();
        let mut bad_value = false;

        macro_rules! boolean {
            ($f:ident) => {
                match txt {
                    "true" => self.$f = Some(true),
                    "false" => self.$f = Some(false),
                    _ => bad_value = true,
                }
            };
        }
        macro_rules! number {
            ($f:ident) => {
                match txt.parse::<u32>() {
                    Ok(v) => self.$f = Some(v),
                    Err(_) => bad_value = true,
                }
            };
        }
        macro_rules! en {
            ($f:ident, $table:expr) => {
                match enum_of($table, txt) {
                    Some(v) => self.$f = Some(v),
                    None => bad_value = true,
                }
            };
        }
        macro_rules! string {
            ($f:ident) => {
                self.$f = Some(txt.to_string())
            };
        }

        match tag {
            "Title" => self.title = Some(lstr(n)),
            "ToolTip" => self.tooltip = Some(lstr(n)),
            "InputHint" => self.input_hint = Some(lstr(n)),
            "TitleLocation" => en!(title_location, TITLE_LOCATION),
            "TitleHeight" => number!(title_height),
            "Visible" => boolean!(visible),
            "Enabled" => boolean!(enabled),
            "ReadOnly" => boolean!(read_only),
            "DataPath" => string!(data_path),
            "CommandName" => string!(command_name),
            "Width" => number!(width),
            "Height" => number!(height),
            "AutoMaxWidth" => boolean!(auto_max_width),
            "MaxWidth" => number!(max_width),
            "HorizontalStretch" => boolean!(horizontal_stretch),
            "VerticalStretch" => boolean!(vertical_stretch),
            "GroupHorizontalAlign" => en!(group_horizontal_align, HALIGN),
            "HorizontalAlign" => en!(horizontal_align, HALIGN),
            "Group" => en!(group, GROUP),
            "ShowTitle" => boolean!(show_title),
            "Representation" => string!(representation),
            "United" => boolean!(united),
            "ChildrenAlign" => string!(children_align),
            "ChildItemsWidth" => string!(child_items_width),
            "HorizontalSpacing" => en!(horizontal_spacing, SPACING),
            "VerticalSpacing" => en!(vertical_spacing, SPACING),
            "MultiLine" => boolean!(multi_line),
            "PasswordMode" => boolean!(password_mode),
            "MarkNegatives" => boolean!(mark_negatives),
            "ChoiceButton" => boolean!(choice_button),
            "DropListButton" => boolean!(drop_list_button),
            "ClearButton" => boolean!(clear_button),
            "SpinButton" => boolean!(spin_button),
            "OpenButton" => boolean!(open_button),
            "ListChoiceMode" => boolean!(list_choice_mode),
            "ChoiceList" => {
                for it in n.children.iter().filter(|c| c.name == "Item") {
                    let inner = it.child("Value");
                    let pres = inner
                        .and_then(|v| v.child("Presentation"))
                        .map(lstr)
                        .filter(|l| !l.0.is_empty())
                        .or_else(|| {
                            it.child("Presentation")
                                .map(lstr)
                                .filter(|l| !l.0.is_empty())
                        })
                        .or_else(|| {
                            let v = inner?.child("Value")?;
                            if v.attr("type").is_some_and(|t| t.ends_with("DesignTimeRef")) {
                                return None;
                            }
                            let t = Some(v.text.trim()).filter(|t| !t.is_empty())?;
                            Some(LStr(vec![("ru".into(), t.to_string())]))
                        });
                    if let Some(l) = pres.or_else(|| it.child("Presentation").map(lstr)) {
                        self.choice_list.push(l);
                    }
                }
            }
            "CheckBoxType" => string!(check_box_type),
            "RadioButtonType" => string!(radio_button_type),
            "ColumnsCount" => number!(columns_count),
            "PagesRepresentation" => string!(pages_representation),
            "HeightInTableRows" => number!(height_in_table_rows),
            "Type"
                if matches!(
                    kind,
                    ItemKind::Button | ItemKind::Popup | ItemKind::ButtonGroup
                ) =>
            {
                string!(button_type)
            }
            "Autofill" => boolean!(autofill),
            "ChangeRowSet" => boolean!(change_row_set),
            "ChangeRowOrder" => boolean!(change_row_order),
            "Header" => boolean!(header),
            "CommandBarLocation" => en!(command_bar_location, LOCATION),
            "SearchStringLocation" => en!(search_string_location, LOCATION),
            "ViewStatusLocation" => en!(view_status_location, LOCATION),
            "SearchControlLocation" => en!(search_control_location, LOCATION),
            "ChoiceMode" => boolean!(choice_mode),
            "DefaultButton" => boolean!(default_button),
            "Hyperlink" => boolean!(hyperlink),
            "Picture" => match picture(n) {
                Some(p) => self.picture = Some(p),
                None => bad_value = true,
            },
            "Events" => {
                for e in n.children.iter().filter(|c| c.name == "Event") {
                    self.events.push((
                        e.attr("name").unwrap_or("").to_string(),
                        e.text.trim().to_string(),
                    ));
                }
            }
            _ => return false,
        }
        if bad_value {
            bump(bad, format!("{}.{tag}={txt}", kind.xml()));
        }
        true
    }
}
