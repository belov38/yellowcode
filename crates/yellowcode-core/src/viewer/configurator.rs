pub mod borrowed;

use std::collections::HashMap;
use std::sync::OnceLock;

use mdconf::MdKind;
use serde::Deserialize;

use super::out::Lang;

const WINDOWS: &str = include_str!("configurator/windows.json");
const PROPS: &str = include_str!("configurator/props.json");
const RIGHTS: &str = include_str!("configurator/rights.json");
const EDITOR: &str = include_str!("configurator/editor.json");

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabItem {
    Prop(String),
    List(String),
    Caption {
        ru: String,
        en: String,
        control: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowTab {
    pub title_ru: String,
    pub title_en: String,
    pub items: Vec<TabItem>,
}

impl WindowTab {
    pub fn title(&self, lang: Lang) -> &str {
        match lang {
            Lang::Ru => &self.title_ru,
            Lang::En => &self.title_en,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteProp {
    pub name: String,
    pub caption_ru: String,
    pub caption_en: String,
    pub tooltip_ru: String,
    pub view: String,
    pub tab: Option<String>,
    pub category: String,
}

impl PaletteProp {
    pub fn caption(&self, lang: Lang) -> &str {
        match lang {
            Lang::Ru => &self.caption_ru,
            Lang::En => &self.caption_en,
        }
    }

    pub fn group(&self, lang: Lang) -> Option<&'static str> {
        category_title(&self.category, lang)
    }
}

#[derive(Deserialize)]
struct RawItem {
    t: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    ru: String,
    #[serde(default)]
    en: String,
    #[serde(default)]
    control: String,
}

#[derive(Deserialize)]
struct RawTab {
    ru: String,
    en: String,
    items: Vec<RawItem>,
}

#[derive(Deserialize)]
struct RawProp {
    name: String,
    ru: String,
    en: String,
    #[serde(default)]
    tip: String,
    #[serde(default)]
    view: String,
    #[serde(default)]
    tab: Option<String>,
    cat: String,
}

#[derive(Deserialize)]
struct RawCategory {
    en: String,
    ru: String,
}

#[derive(Deserialize)]
struct RawProps {
    categories: Vec<RawCategory>,
    contexts: HashMap<String, Vec<RawProp>>,
}

#[derive(Deserialize)]
struct RawRight {
    ru: String,
    en: String,
    ru_hint: String,
    en_hint: String,
}

#[derive(Deserialize)]
struct RawColor {
    key: String,
    color: String,
}

#[derive(Deserialize)]
struct RawFont {
    face: String,
    size_pt: f64,
}

#[derive(Deserialize)]
struct RawEditor {
    colors: Vec<RawColor>,
    font: RawFont,
    tab_size: u32,
    replace_tab_on_input: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EditorSettings {
    pub font_face: String,
    pub font_pt: f64,
    pub tab_size: u32,
    pub replace_tab_on_input: bool,
}

struct Data {
    windows: HashMap<String, Vec<WindowTab>>,
    main_tab: Vec<WindowTab>,
    props: HashMap<String, Vec<PaletteProp>>,
    categories: Vec<(String, String)>,
    rights: HashMap<String, RawRight>,
    colors: Vec<(String, String)>,
    editor: EditorSettings,
}

fn parse<'a, T: Deserialize<'a>>(text: &'a str, what: &str) -> Option<T> {
    serde_json::from_str(text)
        .map_err(|e| eprintln!("onec: embedded {what}: {e}"))
        .ok()
}

fn data() -> &'static Data {
    static DATA: OnceLock<Data> = OnceLock::new();
    DATA.get_or_init(|| {
        let windows = parse::<HashMap<String, Vec<RawTab>>>(WINDOWS, "windows.json")
            .unwrap_or_default()
            .into_iter()
            .map(|(k, tabs)| {
                let tabs = tabs
                    .into_iter()
                    .map(|t| WindowTab {
                        title_ru: t.ru,
                        title_en: t.en,
                        items: t.items.into_iter().filter_map(item).collect(),
                    })
                    .collect();
                (k, tabs)
            })
            .collect();
        let raw = parse::<RawProps>(PROPS, "props.json");
        let (categories, props) = match raw {
            Some(r) => (
                r.categories.into_iter().map(|c| (c.en, c.ru)).collect(),
                r.contexts
                    .into_iter()
                    .map(|(k, v)| {
                        let v = v
                            .into_iter()
                            .map(|p| PaletteProp {
                                name: p.name,
                                caption_ru: p.ru,
                                caption_en: p.en,
                                tooltip_ru: p.tip,
                                view: p.view,
                                tab: p.tab,
                                category: p.cat,
                            })
                            .collect();
                        (k, v)
                    })
                    .collect(),
            ),
            None => Default::default(),
        };
        let rights = parse(RIGHTS, "rights.json").unwrap_or_default();
        let (colors, editor) = match parse::<RawEditor>(EDITOR, "editor.json") {
            Some(e) => (
                e.colors.into_iter().map(|c| (c.key, c.color)).collect(),
                EditorSettings {
                    font_face: e.font.face,
                    font_pt: e.font.size_pt,
                    tab_size: e.tab_size,
                    replace_tab_on_input: e.replace_tab_on_input,
                },
            ),
            None => (
                Vec::new(),
                EditorSettings {
                    font_face: "Courier New".into(),
                    font_pt: 10.0,
                    tab_size: 4,
                    replace_tab_on_input: false,
                },
            ),
        };
        Data {
            windows,
            main_tab: vec![WindowTab {
                title_ru: "Основные".into(),
                title_en: "Main".into(),
                items: Vec::new(),
            }],
            props,
            categories,
            rights,
            colors,
            editor,
        }
    })
}

fn item(r: RawItem) -> Option<TabItem> {
    match r.t.as_str() {
        "prop" => Some(TabItem::Prop(r.name)),
        "list" => Some(TabItem::List(r.kind)),
        "cap" => Some(TabItem::Caption {
            ru: r.ru,
            en: r.en,
            control: r.control,
        }),
        _ => None,
    }
}

pub fn has_window(kind: MdKind) -> bool {
    data().windows.contains_key(kind.as_str())
}

pub fn window_tabs(kind: MdKind) -> &'static [WindowTab] {
    let d = data();
    d.windows
        .get(kind.as_str())
        .map_or(d.main_tab.as_slice(), Vec::as_slice)
}

pub fn palette_prop(context: &str, name: &str) -> Option<&'static PaletteProp> {
    let d = data();
    let mut ctx = context;
    loop {
        if let Some(p) = d
            .props
            .get(ctx)
            .and_then(|v| v.iter().find(|p| p.name == name))
        {
            return Some(p);
        }
        ctx = ctx.split_once('.')?.1;
    }
}

pub fn category_title(category: &str, lang: Lang) -> Option<&'static str> {
    data()
        .categories
        .iter()
        .find(|(en, _)| en == category)
        .map(|(en, ru)| match lang {
            Lang::Ru => ru.as_str(),
            Lang::En => en.as_str(),
        })
}

pub fn categories() -> &'static [(String, String)] {
    &data().categories
}

pub fn right_title(right: &str, lang: Lang) -> Option<&'static str> {
    data().rights.get(right).map(|r| match lang {
        Lang::Ru => r.ru.as_str(),
        Lang::En => r.en.as_str(),
    })
}

pub fn right_hint(right: &str, lang: Lang) -> Option<&'static str> {
    data().rights.get(right).map(|r| match lang {
        Lang::Ru => r.ru_hint.as_str(),
        Lang::En => r.en_hint.as_str(),
    })
}

pub fn editor_colors() -> &'static [(String, String)] {
    &data().colors
}

pub fn editor_settings() -> &'static EditorSettings {
    &data().editor
}
