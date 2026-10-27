use super::MAX_LIST;
use crate::viewer::{
    Lang,
    body::{self, ViewerBody},
    configurator::borrowed::{self, TabItem},
    module::{self, ManagedModuleError},
    names::{self, CHILD_TAGS, ManagedNames, child_tag_en, sub_kind_en},
    path::{ManagedTarget, RECALC_EN, RECALC_RU},
    show::{self, ManagedProperties, ManagedShowError, Property},
};
use mdconf::{
    LocalString, ManagedEntrySource, MdKind, ModuleKind,
    startup::{
        ChildFacts, ManagedModuleKey, ManagedSelectionError, ManagedViewerOpen, ObjectFacts,
    },
};
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, MemoryBudget};
use serde::{Serialize, Serializer, ser::SerializeMap};
type Result<T> = std::result::Result<T, BudgetError>;

pub fn object_window_managed<S: ManagedEntrySource>(
    open: &ManagedViewerOpen<S>,
    names: &ManagedNames<'_>,
    path: &str,
    lang: Lang,
    wire_limit: u64,
) -> std::result::Result<ViewerBody, ManagedShowError<S::Error>> {
    show::with_properties(open, names, path, lang, |target, props| {
        let keys = module::modules_managed(open, names, path).map_err(|e| match e {
            ManagedModuleError::Path(e) => ManagedShowError::Path(e),
            ManagedModuleError::Memory(e) => ManagedShowError::Memory(e),
            ManagedModuleError::Selection(_) | ManagedModuleError::InvalidInventory => {
                ManagedShowError::Read(ManagedSelectionError::MissingValidated)
            }
        })?;
        let budget = open.context().index().memory_budget();
        let canonical = canonical(open, target)?;
        let ctx = Context {
            open,
            names,
            target,
            props: &props,
            keys: &keys,
            canonical: canonical.as_str(),
            lang,
            budget,
        };
        let window = ctx.build()?;
        Ok(body::json(budget, wire_limit, &window)?)
    })
}
struct Window<'a> {
    kind: &'a str,
    name: &'a str,
    modules: BudgetedVec<&'static str>,
    tabs: BudgetedVec<Tab<'a>>,
}
impl Serialize for Window<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(4))?;
        m.serialize_entry("kind", self.kind)?;
        m.serialize_entry("name", self.name)?;
        m.serialize_entry("modules", self.modules.as_slice())?;
        m.serialize_entry("tabs", self.tabs.as_slice())?;
        m.end()
    }
}
struct Tab<'a> {
    title: &'static str,
    other: bool,
    items: BudgetedVec<Item<'a>>,
}
impl Serialize for Tab<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(2))?;
        m.serialize_entry("title", self.title)?;
        m.serialize_entry("items", self.items.as_slice())?;
        m.end()
    }
}
enum Item<'a> {
    Prop(&'a Property<'a>),
    List(List<'a>),
    Caption {
        caption: &'static str,
        control: &'static str,
        open: Option<Open<'a>>,
    },
}
#[derive(Serialize)]
struct Open<'a> {
    path: &'a str,
    module: &'static str,
}
impl Serialize for Item<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Prop(p) => p.serialize(s),
            Self::List(l) => {
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("list", l)?;
                m.end()
            }
            Self::Caption {
                caption,
                control,
                open,
            } => {
                let mut m = s.serialize_map(Some(2 + usize::from(open.is_some())))?;
                m.serialize_entry("caption", caption)?;
                m.serialize_entry("control", control)?;
                if let Some(open) = open {
                    m.serialize_entry("open", open)?;
                }
                m.end()
            }
        }
    }
}
struct List<'a> {
    kind: &'static str,
    title: &'a str,
    items: BudgetedVec<ListItem<'a>>,
    total: usize,
}
impl Serialize for List<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let truncated = self.total > MAX_LIST;
        let mut m = s.serialize_map(Some(if truncated { 5 } else { 3 }))?;
        m.serialize_entry("kind", self.kind)?;
        m.serialize_entry("title", self.title)?;
        m.serialize_entry("items", self.items.as_slice())?;
        if truncated {
            m.serialize_entry("truncated", &true)?;
            m.serialize_entry("total", &self.total)?;
        }
        m.end()
    }
}
struct ListItem<'a> {
    name: &'a str,
    path: BudgetedText,
    icon: BudgetedText,
    synonym: Option<&'a str>,
}
impl Serialize for ListItem<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(3 + usize::from(self.synonym.is_some())))?;
        m.serialize_entry("name", self.name)?;
        m.serialize_entry("path", self.path.as_str())?;
        m.serialize_entry("icon", self.icon.as_str())?;
        if let Some(syn) = self.synonym {
            m.serialize_entry("synonym", syn)?;
        }
        m.end()
    }
}
fn text(budget: &MemoryBudget, parts: &[&str]) -> Result<BudgetedText> {
    let mut out = BudgetedText::new(budget, Category::Metadata, u64::MAX)?;
    for part in parts {
        out.try_push_str(part)?;
    }
    Ok(out)
}
fn synonym(value: &LocalString, lang: Lang) -> Option<&str> {
    value
        .0
        .iter()
        .find(|(code, _)| code == if lang == Lang::Ru { "ru" } else { "en" })
        .or_else(|| value.0.first())
        .map(|(_, value)| value.as_str())
        .filter(|value| !value.is_empty())
}
impl<'a> List<'a> {
    fn push(
        &mut self,
        budget: &MemoryBudget,
        owner: &str,
        tag: &str,
        name: &'a str,
        icon: &[&str],
        synonym: Option<&'a str>,
    ) -> Result<()> {
        self.total = self.total.checked_add(1).ok_or(BudgetError::Overflow)?;
        if self.items.len() < MAX_LIST {
            self.items.try_push(ListItem {
                name,
                path: text(budget, &[owner, ".", tag, ".", name])?,
                icon: text(budget, icon)?,
                synonym,
            })?;
        }
        Ok(())
    }
}
struct Context<'a, S: ManagedEntrySource> {
    open: &'a ManagedViewerOpen<S>,
    names: &'a ManagedNames<'a>,
    target: &'a ManagedTarget<'a>,
    props: &'a ManagedProperties<'a>,
    keys: &'a [ManagedModuleKey],
    canonical: &'a str,
    lang: Lang,
    budget: &'a MemoryBudget,
}
impl<'a, S: ManagedEntrySource> Context<'a, S> {
    fn holder(&self) -> (&'a [ChildFacts], Option<(usize, &'a ObjectFacts)>) {
        match self.target {
            ManagedTarget::Object { ordinal, object } => {
                (object.children.as_slice(), Some((*ordinal, object)))
            }
            ManagedTarget::Child { chain, .. } => {
                (chain.last().map_or(&[], |c| c.children.as_slice()), None)
            }
            _ => (&[], None),
        }
    }
    fn identity(&self) -> (&'a str, &'a str) {
        match self.target {
            ManagedTarget::Configuration(c) => (
                self.names.kind_name(MdKind::Configuration, self.lang),
                c.name.as_str(),
            ),
            ManagedTarget::Object { object, .. } => {
                (self.names.kind_name(object.kind, self.lang), &object.name)
            }
            ManagedTarget::Child { chain, .. } => chain.last().map_or(("", ""), |c| {
                (self.names.child_tag_name(c.tag, self.lang), c.name.as_str())
            }),
            ManagedTarget::Sub { sub, .. } => {
                (self.names.sub_kind_name(sub.kind, self.lang), &sub.name)
            }
            ManagedTarget::Recalc { recalc, .. } => (
                if self.lang == Lang::Ru {
                    RECALC_RU
                } else {
                    RECALC_EN
                },
                &recalc.name,
            ),
        }
    }
    fn kind(&self, key: ManagedModuleKey) -> Result<ModuleKind> {
        self.open
            .modules()
            .get(key.ordinal())
            .map(|m| m.slot.kind)
            .ok_or(BudgetError::Overflow)
    }
    fn open_module(&self, control: &str) -> Result<Option<Open<'a>>> {
        let mut found = None;
        for key in self.keys {
            let kind = self.kind(*key)?;
            let wanted = match control {
                "BtnOpenManagerModule" | "BtnOpenStorageModule" => {
                    kind == ModuleKind::ManagerModule
                }
                "BtnOpenRecordSetModule" => kind == ModuleKind::RecordSetModule,
                "BtnOpenModule" => kind != ModuleKind::ManagerModule,
                _ => false,
            };
            if wanted {
                found = Some(Open {
                    path: self.canonical,
                    module: names::module_label(kind, Lang::En),
                });
                break;
            }
        }
        Ok(found)
    }
    fn list(&self, kind: &'static str) -> Result<List<'a>> {
        let (children, object) = self.holder();
        let mut list = List {
            kind,
            title: match kind {
                "Form" => names::group_label("Forms", self.lang),
                "Template" => names::group_label("Templates", self.lang),
                "Recalculation" => names::group_label("Recalculations", self.lang),
                "Subsystem" => self.names.section_name(MdKind::Subsystem, self.lang),
                tag => names::group_label(tag, self.lang),
            },
            items: BudgetedVec::new(self.budget, Category::Metadata)?,
            total: 0,
        };
        match kind {
            "Form" | "Template" => {
                if let Some((_, o)) = object {
                    let rows = if kind == "Form" {
                        o.forms.as_slice()
                    } else {
                        o.templates.as_slice()
                    };
                    for row in rows {
                        let tag = sub_kind_en(row.kind);
                        list.push(
                            self.budget,
                            self.canonical,
                            tag,
                            &row.name,
                            &["sub:", tag],
                            None,
                        )?;
                    }
                }
            }
            "Recalculation" => {
                if let Some((_, o)) = object {
                    for row in o.recalculations.iter() {
                        list.push(
                            self.budget,
                            self.canonical,
                            "Recalculation",
                            &row.name,
                            &["recalc"],
                            synonym(&row.synonym, self.lang),
                        )?;
                    }
                }
            }
            "Subsystem" => {
                if let Some((ordinal, _)) = object {
                    for row in self
                        .open
                        .objects()
                        .iter()
                        .filter(|r| r.parent == Some(ordinal))
                    {
                        list.push(
                            self.budget,
                            self.canonical,
                            "Subsystem",
                            &row.object.name,
                            &["object:", row.object.kind.as_str()],
                            synonym(&row.object.synonym, self.lang),
                        )?;
                    }
                }
            }
            tag => {
                let wanted = CHILD_TAGS.iter().copied().find(|t| child_tag_en(*t) == tag);
                for row in children.iter().filter(|c| Some(c.tag) == wanted) {
                    list.push(
                        self.budget,
                        self.canonical,
                        tag,
                        &row.name,
                        &["child:", child_tag_en(row.tag)],
                        synonym(&row.synonym, self.lang),
                    )?;
                }
            }
        }
        Ok(list)
    }
    fn item(&self, item: &TabItem) -> Result<Option<Item<'a>>> {
        Ok(match item {
            TabItem::Prop(name) => self
                .props
                .rows()
                .iter()
                .find(|p| p.name() == *name)
                .map(Item::Prop),
            TabItem::List(kind) => Some(Item::List(self.list(kind)?)),
            TabItem::Caption { ru, en, control } => Some(Item::Caption {
                caption: if self.lang == Lang::Ru || en.is_empty() {
                    ru
                } else {
                    en
                },
                control,
                open: self.open_module(control)?,
            }),
        })
    }
    fn build(&self) -> Result<Window<'a>> {
        let (kind, name) = self.identity();
        let mut modules =
            BudgetedVec::try_with_capacity(self.budget, Category::Metadata, self.keys.len())?;
        for key in self.keys {
            modules.try_push(names::module_label(self.kind(*key)?, Lang::En))?;
        }
        let mut tabs = BudgetedVec::new(self.budget, Category::Metadata)?;
        let configured = match self.target {
            ManagedTarget::Object { object, .. } if borrowed::has_window(object.kind) => {
                Some(borrowed::window_tabs(object.kind))
            }
            _ => None,
        };
        if let Some(configured) = configured {
            for tab in configured {
                let mut items = BudgetedVec::new(self.budget, Category::Metadata)?;
                for item in tab.items {
                    if let Some(item) = self.item(item)? {
                        items.try_push(item)?;
                    }
                }
                tabs.try_push(Tab {
                    title: tab.title(self.lang),
                    other: tab.title_ru == "Прочее",
                    items,
                })?;
            }
            let destination = tabs
                .iter()
                .position(|tab| tab.other)
                .unwrap_or(tabs.len().saturating_sub(1));
            if let Some(tab) = tabs.get_mut(destination) {
                for prop in self.props.rows() {
                    let placed = configured
                        .iter()
                        .flat_map(|t| t.items)
                        .any(|it| matches!(it,TabItem::Prop(name) if *name==prop.name()));
                    if !placed {
                        tab.items.try_push(Item::Prop(prop))?;
                    }
                }
            }
        } else {
            let mut items = BudgetedVec::new(self.budget, Category::Metadata)?;
            for prop in self.props.rows() {
                if let Some(first) = self.props.rows().iter().find(|p| p.name() == prop.name()) {
                    items.try_push(Item::Prop(first))?;
                }
            }
            let (children, object) = self.holder();
            for tag in CHILD_TAGS {
                if children.iter().any(|c| c.tag == tag) {
                    items.try_push(Item::List(self.list(child_tag_en(tag))?))?;
                }
            }
            if let Some((ordinal, o)) = object {
                for (present, kind) in [
                    (!o.forms.is_empty(), "Form"),
                    (!o.templates.is_empty(), "Template"),
                    (!o.recalculations.is_empty(), "Recalculation"),
                    (
                        self.open
                            .objects()
                            .iter()
                            .any(|r| r.parent == Some(ordinal)),
                        "Subsystem",
                    ),
                ] {
                    if present {
                        items.try_push(Item::List(self.list(kind)?))?;
                    }
                }
            }
            let title = borrowed::window_tabs(MdKind::Configuration)
                .first()
                .map_or("", |t| t.title(self.lang));
            tabs.try_push(Tab {
                title,
                other: false,
                items,
            })?;
        }
        Ok(Window {
            kind,
            name,
            modules,
            tabs,
        })
    }
}
pub(crate) fn canonical<S: ManagedEntrySource>(
    open: &ManagedViewerOpen<S>,
    target: &ManagedTarget<'_>,
) -> Result<BudgetedText> {
    let budget = open.context().index().memory_budget();
    let ordinal = match target {
        ManagedTarget::Configuration(_) => return text(budget, &["Configuration"]),
        ManagedTarget::Object { ordinal, .. }
        | ManagedTarget::Child { ordinal, .. }
        | ManagedTarget::Sub { ordinal, .. }
        | ManagedTarget::Recalc { ordinal, .. } => *ordinal,
    };
    let mut up = BudgetedVec::new(budget, Category::Metadata)?;
    let mut at = Some(ordinal);
    while let Some(i) = at {
        if up.len() >= open.objects().len() {
            return Err(BudgetError::Overflow);
        }
        let row = open.objects().get(i).ok_or(BudgetError::Overflow)?;
        up.try_push(i)?;
        at = row.parent;
    }
    let mut out = BudgetedText::new(budget, Category::Metadata, u64::MAX)?;
    for (at, i) in up.iter().rev().enumerate() {
        if at != 0 {
            out.try_push_str(".")?;
        }
        let row = open.objects().get(*i).ok_or(BudgetError::Overflow)?;
        out.try_push_str(row.object.kind.as_str())?;
        out.try_push_str(".")?;
        out.try_push_str(&row.object.name)?;
    }
    match target {
        ManagedTarget::Child { chain, .. } => {
            for child in chain.iter() {
                out.try_push_str(".")?;
                out.try_push_str(child_tag_en(child.tag))?;
                out.try_push_str(".")?;
                out.try_push_str(&child.name)?;
            }
        }
        ManagedTarget::Sub { sub, .. } => {
            out.try_push_str(".")?;
            out.try_push_str(sub_kind_en(sub.kind))?;
            out.try_push_str(".")?;
            out.try_push_str(&sub.name)?;
        }
        ManagedTarget::Recalc { recalc, .. } => {
            out.try_push_str(".Recalculation.")?;
            out.try_push_str(&recalc.name)?;
        }
        _ => {}
    }
    Ok(out)
}
