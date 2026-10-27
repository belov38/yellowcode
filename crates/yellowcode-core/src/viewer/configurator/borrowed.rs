use crate::viewer::Lang;
use mdconf::MdKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteProp {
    pub name: &'static str,
    pub caption_ru: &'static str,
    pub caption_en: &'static str,
    pub tooltip_ru: &'static str,
    pub view: &'static str,
    pub tab: Option<&'static str>,
    pub category: &'static str,
}
impl PaletteProp {
    pub fn caption(&self, lang: Lang) -> &'static str {
        match lang {
            Lang::Ru => self.caption_ru,
            Lang::En => self.caption_en,
        }
    }
    pub fn group(&self, lang: Lang) -> Option<&'static str> {
        category_title(self.category, lang)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabItem {
    Prop(&'static str),
    List(&'static str),
    Caption {
        ru: &'static str,
        en: &'static str,
        control: &'static str,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowTab {
    pub title_ru: &'static str,
    pub title_en: &'static str,
    pub items: &'static [TabItem],
}
impl WindowTab {
    pub fn title(&self, lang: Lang) -> &'static str {
        match lang {
            Lang::Ru => self.title_ru,
            Lang::En => self.title_en,
        }
    }
}
include!(concat!(env!("OUT_DIR"), "/viewer_configurator.rs"));
const MAIN: &[WindowTab] = &[WindowTab {
    title_ru: "Основные",
    title_en: "Main",
    items: &[],
}];
pub fn palette_prop(context: &str, name: &str) -> Option<&'static PaletteProp> {
    let mut context = context;
    loop {
        if let Some(row) = PROPS
            .iter()
            .find(|row| row.0 == context)
            .and_then(|row| row.1.iter().find(|row| row.name == name))
        {
            return Some(row);
        }
        context = context.split_once('.')?.1;
    }
}
pub fn category_title(category: &str, lang: Lang) -> Option<&'static str> {
    CATEGORIES
        .iter()
        .find(|row| row.0 == category)
        .map(|row| match lang {
            Lang::Ru => row.1,
            Lang::En => row.0,
        })
}
pub fn has_window(kind: MdKind) -> bool {
    WINDOWS.iter().any(|row| row.0 == kind.as_str())
}
pub fn window_tabs(kind: MdKind) -> &'static [WindowTab] {
    WINDOWS
        .iter()
        .find(|row| row.0 == kind.as_str())
        .map_or(MAIN, |row| row.1)
}
pub fn right_title(right: &str, lang: Lang) -> Option<&'static str> {
    RIGHTS
        .iter()
        .find(|row| row.0 == right)
        .map(|row| match lang {
            Lang::Ru => row.1[0],
            Lang::En => row.1[1],
        })
}
pub fn right_hint(right: &str, lang: Lang) -> Option<&'static str> {
    RIGHTS
        .iter()
        .find(|row| row.0 == right)
        .map(|row| match lang {
            Lang::Ru => row.1[2],
            Lang::En => row.1[3],
        })
}

pub fn category_titles(lang: Lang) -> impl Iterator<Item = &'static str> {
    CATEGORIES
        .iter()
        .map(move |row| if lang == Lang::Ru { row.1 } else { row.0 })
}
