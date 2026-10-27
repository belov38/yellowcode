use super::{ManagedShowError, with_properties};
use crate::viewer::{
    Lang,
    body::{self, ViewerBody},
    configurator::borrowed,
    module::{self, ManagedModuleError},
    names::{ManagedNames, child_tag_en, module_label, sub_kind_en},
    path::{ManagedTarget, RECALC_EN, RECALC_RU},
};
use mdconf::{ChildTag, ManagedEntrySource, MdKind, startup::ManagedViewerOpen};
use memory_budget::{BudgetError, BudgetedText, Category};
use serde::{
    Serialize, Serializer,
    ser::{SerializeMap, SerializeSeq},
};

pub fn show_http_managed<S: ManagedEntrySource>(
    open: &ManagedViewerOpen<S>,
    names: &ManagedNames<'_>,
    path: &str,
    lang: Lang,
    wire_limit: u64,
) -> Result<ViewerBody, ManagedShowError<S::Error>> {
    with_properties(open, names, path, lang, |target, props| {
        let budget = names.memory_budget();
        let keys = module::modules_managed(open, names, path).map_err(|error| match error {
            ManagedModuleError::Path(e) => ManagedShowError::Path(e),
            ManagedModuleError::Memory(e) => ManagedShowError::Memory(e),
            _ => ManagedShowError::Read(mdconf::startup::ManagedSelectionError::MissingValidated),
        })?;
        let mut modules = memory_budget::BudgetedVec::new(budget, Category::Metadata)?;
        for key in keys.iter() {
            let descriptor = open.module_descriptor(*key)?;
            modules.try_push(module_label(descriptor.slot.kind, Lang::En))?;
        }
        let (kind, name) = match target {
            ManagedTarget::Configuration(c) => (
                names.kind_name(MdKind::Configuration, lang),
                c.name.as_str(),
            ),
            ManagedTarget::Object { object, .. } => {
                (names.kind_name(object.kind, lang), object.name.as_str())
            }
            ManagedTarget::Child { chain, .. } => chain.last().map_or(("", ""), |c| {
                (names.child_tag_name(c.tag, lang), c.name.as_str())
            }),
            ManagedTarget::Sub { sub, .. } => {
                (names.sub_kind_name(sub.kind, lang), sub.name.as_str())
            }
            ManagedTarget::Recalc { recalc, .. } => (
                if lang == Lang::Ru {
                    RECALC_RU
                } else {
                    RECALC_EN
                },
                recalc.name.as_str(),
            ),
        };
        let mut forms = memory_budget::BudgetedVec::new(budget, Category::Metadata)?;
        let mut templates = memory_budget::BudgetedVec::new(budget, Category::Metadata)?;
        let mut commands = memory_budget::BudgetedVec::new(budget, Category::Metadata)?;
        if let ManagedTarget::Object { object, .. } = target {
            let join = |kind: &str, name: &str| -> Result<BudgetedText, BudgetError> {
                let mut out = BudgetedText::new(budget, Category::Metadata, u64::MAX)?;
                for (i, part) in [
                    names.kind_name(object.kind, Lang::En),
                    object.name.as_str(),
                    kind,
                    name,
                ]
                .iter()
                .enumerate()
                {
                    if i != 0 {
                        out.try_push_str(".")?;
                    }
                    out.try_push_str(part)?;
                }
                Ok(out)
            };
            for sub in object.forms.iter() {
                forms.try_push(Item {
                    name: &sub.name,
                    path: join(sub_kind_en(sub.kind), &sub.name)?,
                })?;
            }
            for sub in object.templates.iter() {
                templates.try_push(Item {
                    name: &sub.name,
                    path: join(sub_kind_en(sub.kind), &sub.name)?,
                })?;
            }
            for child in object
                .children
                .iter()
                .filter(|c| c.tag == ChildTag::Command)
            {
                commands.try_push(Item {
                    name: &child.name,
                    path: join(child_tag_en(child.tag), &child.name)?,
                })?;
            }
        }
        body::json(
            budget,
            wire_limit,
            &Show {
                kind,
                name,
                properties: &props,
                groups: Groups(lang),
                forms: forms.as_slice(),
                templates: templates.as_slice(),
                modules: modules.as_slice(),
                commands: commands.as_slice(),
            },
        )
        .map_err(Into::into)
    })
}
struct Item<'a> {
    name: &'a str,
    path: BudgetedText,
}
impl Serialize for Item<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(2))?;
        m.serialize_entry("name", self.name)?;
        m.serialize_entry("path", self.path.as_str())?;
        m.end()
    }
}
struct Groups(Lang);
impl Serialize for Groups {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(None)?;
        for title in borrowed::category_titles(self.0) {
            seq.serialize_element(title)?;
        }
        seq.end()
    }
}
#[derive(Serialize)]
struct Show<'a> {
    kind: &'a str,
    name: &'a str,
    properties: &'a super::ManagedProperties<'a>,
    groups: Groups,
    forms: &'a [Item<'a>],
    templates: &'a [Item<'a>],
    modules: &'a [&'static str],
    commands: &'a [Item<'a>],
}
