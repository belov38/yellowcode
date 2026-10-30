pub mod borrowed;

use super::resources::{self, Graph, GuiResourceError};
use memory_budget::{BudgetError, MemoryBudget};

use mdconf::MdKind;

use super::out::Lang;

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

struct Right {
    ru: String,
    en: String,
    ru_hint: String,
    en_hint: String,
}
mod descriptors {
    pub(super) enum Item {
        Prop(&'static str),
        List(&'static str),
        Caption(&'static str, &'static str, &'static str),
    }
    pub(super) struct Tab {
        pub ru: &'static str,
        pub en: &'static str,
        pub items: &'static [Item],
    }
    pub(super) struct Prop {
        pub name: &'static str,
        pub ru: &'static str,
        pub en: &'static str,
        pub tip: &'static str,
        pub view: &'static str,
        pub tab: Option<&'static str>,
        pub cat: &'static str,
    }
    include!("configurator/descriptors.rs");
}
struct FlatMap<V>(Vec<(String, V)>);
impl<V> FlatMap<V> {
    fn get(&self, key: &str) -> Option<&V> {
        let i = self.0.partition_point(|(k, _)| k.as_str() < key);
        self.0.get(i).filter(|(k, _)| k == key).map(|(_, v)| v)
    }
    fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
}
impl<'a, V> IntoIterator for &'a FlatMap<V> {
    type Item = &'a (String, V);
    type IntoIter = std::slice::Iter<'a, (String, V)>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EditorSettings {
    pub font_face: String,
    pub font_pt: f64,
    pub tab_size: u32,
    pub replace_tab_on_input: bool,
}

struct Data {
    windows: FlatMap<Vec<WindowTab>>,
    main_tab: Vec<WindowTab>,
    props: FlatMap<Vec<PaletteProp>>,
    categories: Vec<(String, String)>,
    rights: FlatMap<Right>,
    colors: Vec<(String, String)>,
    editor: EditorSettings,
}

static DATA: Graph<Data> = Graph::new();

fn descriptor_bytes() -> Result<u64, BudgetError> {
    use descriptors as d;
    let mut n = 0;
    resources::rows::<(String, Vec<WindowTab>)>(&mut n, d::WINDOWS.len())?;
    resources::rows::<WindowTab>(&mut n, 1)?;
    for s in ["Основные", "Main", d::FONT] {
        resources::add(&mut n, s.len())?;
    }
    for (key, tabs) in d::WINDOWS {
        resources::add(&mut n, key.len())?;
        resources::rows::<WindowTab>(&mut n, tabs.len())?;
        for tab in *tabs {
            resources::add(&mut n, tab.ru.len())?;
            resources::add(&mut n, tab.en.len())?;
            resources::rows::<TabItem>(&mut n, tab.items.len())?;
            for item in tab.items {
                match item {
                    d::Item::Prop(s) | d::Item::List(s) => resources::add(&mut n, s.len())?,
                    d::Item::Caption(ru, en, control) => {
                        for s in [ru, en, control] {
                            resources::add(&mut n, s.len())?;
                        }
                    }
                }
            }
        }
    }
    resources::rows::<(String, Vec<PaletteProp>)>(&mut n, d::PROPS.len())?;
    for (key, props) in d::PROPS {
        resources::add(&mut n, key.len())?;
        resources::rows::<PaletteProp>(&mut n, props.len())?;
        for p in *props {
            for s in [p.name, p.ru, p.en, p.tip, p.view, p.cat] {
                resources::add(&mut n, s.len())?;
            }
            if let Some(s) = p.tab {
                resources::add(&mut n, s.len())?;
            }
        }
    }
    resources::rows::<(String, Right)>(&mut n, d::RIGHTS.len())?;
    for (key, fields) in d::RIGHTS {
        resources::add(&mut n, key.len())?;
        for s in fields {
            resources::add(&mut n, s.len())?;
        }
    }
    for pairs in [d::CATEGORIES, d::COLORS] {
        resources::rows::<(String, String)>(&mut n, pairs.len())?;
        for (a, b) in pairs {
            resources::add(&mut n, a.len())?;
            resources::add(&mut n, b.len())?;
        }
    }
    Ok(n)
}
fn pairs(source: &[(&str, &str)]) -> Result<Vec<(String, String)>, BudgetError> {
    let mut v = resources::vec(source.len())?;
    for (a, b) in source {
        v.push((resources::text(a)?, resources::text(b)?));
    }
    Ok(v)
}
fn build() -> Result<Data, BudgetError> {
    use descriptors as d;
    let mut windows = resources::vec(d::WINDOWS.len())?;
    for (key, source) in d::WINDOWS {
        let mut tabs = resources::vec(source.len())?;
        for tab in *source {
            let mut items = resources::vec(tab.items.len())?;
            for item in tab.items {
                items.push(match item {
                    d::Item::Prop(s) => TabItem::Prop(resources::text(s)?),
                    d::Item::List(s) => TabItem::List(resources::text(s)?),
                    d::Item::Caption(ru, en, control) => TabItem::Caption {
                        ru: resources::text(ru)?,
                        en: resources::text(en)?,
                        control: resources::text(control)?,
                    },
                });
            }
            tabs.push(WindowTab {
                title_ru: resources::text(tab.ru)?,
                title_en: resources::text(tab.en)?,
                items,
            });
        }
        windows.push((resources::text(key)?, tabs));
    }
    let mut props = resources::vec(d::PROPS.len())?;
    for (key, source) in d::PROPS {
        let mut values = resources::vec(source.len())?;
        for p in *source {
            values.push(PaletteProp {
                name: resources::text(p.name)?,
                caption_ru: resources::text(p.ru)?,
                caption_en: resources::text(p.en)?,
                tooltip_ru: resources::text(p.tip)?,
                view: resources::text(p.view)?,
                tab: p.tab.map(resources::text).transpose()?,
                category: resources::text(p.cat)?,
            });
        }
        props.push((resources::text(key)?, values));
    }
    let mut rights = resources::vec(d::RIGHTS.len())?;
    for (key, [ru, en, ru_hint, en_hint]) in d::RIGHTS {
        rights.push((
            resources::text(key)?,
            Right {
                ru: resources::text(ru)?,
                en: resources::text(en)?,
                ru_hint: resources::text(ru_hint)?,
                en_hint: resources::text(en_hint)?,
            },
        ));
    }
    let mut main_tab = resources::vec(1)?;
    main_tab.push(WindowTab {
        title_ru: resources::text("Основные")?,
        title_en: resources::text("Main")?,
        items: Vec::new(),
    });
    Ok(Data {
        windows: FlatMap(windows),
        main_tab,
        props: FlatMap(props),
        categories: pairs(d::CATEGORIES)?,
        rights: FlatMap(rights),
        colors: pairs(d::COLORS)?,
        editor: EditorSettings {
            font_face: resources::text(d::FONT)?,
            font_pt: d::FONT_PT,
            tab_size: d::TAB_SIZE,
            replace_tab_on_input: d::REPLACE_TAB,
        },
    })
}
impl Data {
    fn backing_bytes(&self) -> Result<u64, BudgetError> {
        let mut n = 0;
        resources::rows::<(String, Vec<WindowTab>)>(&mut n, self.windows.0.capacity())?;
        for (key, _) in &self.windows {
            resources::add(&mut n, key.capacity())?;
        }
        for tabs in self
            .windows
            .0
            .iter()
            .map(|(_, v)| v)
            .chain(std::iter::once(&self.main_tab))
        {
            resources::rows::<WindowTab>(&mut n, tabs.capacity())?;
            for tab in tabs {
                resources::add(&mut n, tab.title_ru.capacity())?;
                resources::add(&mut n, tab.title_en.capacity())?;
                resources::rows::<TabItem>(&mut n, tab.items.capacity())?;
                for item in &tab.items {
                    match item {
                        TabItem::Prop(s) | TabItem::List(s) => {
                            resources::add(&mut n, s.capacity())?
                        }
                        TabItem::Caption { ru, en, control } => {
                            for s in [ru, en, control] {
                                resources::add(&mut n, s.capacity())?;
                            }
                        }
                    }
                }
            }
        }
        resources::rows::<(String, Vec<PaletteProp>)>(&mut n, self.props.0.capacity())?;
        for (key, props) in &self.props {
            resources::add(&mut n, key.capacity())?;
            resources::rows::<PaletteProp>(&mut n, props.capacity())?;
            for p in props {
                for s in [
                    &p.name,
                    &p.caption_ru,
                    &p.caption_en,
                    &p.tooltip_ru,
                    &p.view,
                    &p.category,
                ] {
                    resources::add(&mut n, s.capacity())?;
                }
                if let Some(s) = &p.tab {
                    resources::add(&mut n, s.capacity())?;
                }
            }
        }
        resources::rows::<(String, Right)>(&mut n, self.rights.0.capacity())?;
        for (key, r) in &self.rights {
            for s in [key, &r.ru, &r.en, &r.ru_hint, &r.en_hint] {
                resources::add(&mut n, s.capacity())?;
            }
        }
        for pairs in [&self.categories, &self.colors] {
            resources::rows::<(String, String)>(&mut n, pairs.capacity())?;
            for (a, b) in pairs {
                resources::add(&mut n, a.capacity())?;
                resources::add(&mut n, b.capacity())?;
            }
        }
        resources::add(&mut n, self.editor.font_face.capacity())?;
        Ok(n)
    }
}
pub fn prepare_with_memory(budget: &MemoryBudget) -> Result<(), GuiResourceError> {
    let root = budget.root_budget();
    DATA.prepare(Some(&root), descriptor_bytes, Data::backing_bytes, build)?;
    Ok(())
}
#[allow(
    clippy::panic,
    reason = "legacy infallible accessor; managed preparation is typed and fallible"
)]
fn data() -> &'static Data {
    match DATA.prepare(None, descriptor_bytes, Data::backing_bytes, build) {
        Ok(d) => d,
        Err(e) => panic!("configurator data: {e}"),
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

#[cfg(feature = "testing")]
#[doc(hidden)]
pub fn diagnostic_window_tabs(kind: &str) -> Option<&'static [WindowTab]> {
    data().windows.get(kind).map(Vec::as_slice)
}
