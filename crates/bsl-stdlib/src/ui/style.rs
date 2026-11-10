use super::{color, font};
use crate::bind::msg;
use brace::Node;
use bsl_prim::{Str, fold_name};
use bsl_vm::{
    Arg, CallResult, Exception, Execution, GlobalProp, HostError, HostObject, MemberId, MemberKind,
    TypeId, Value,
};
use platform_catalog::{TypeIdx, catalog, messages::Lang};
use std::any::Any;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

const STYLE_PREFIX: &str = "стиль: ";
const NOT_INDEXED: &str = "bsl:IDS_RTE_NOTINDEXED";
const ITEM_BASE: u32 = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StyleItemKind {
    Color,
    Font,
}

#[derive(Clone, Debug)]
pub struct StyleItem {
    pub name: String,
    pub synonym: String,
    pub uuid: String,
    pub kind: StyleItemKind,
}

#[derive(Debug, Default)]
pub struct StyleItems {
    items: Vec<StyleItem>,
    by_name: HashMap<(StyleItemKind, String), u32>,
    builtin: HashMap<String, String>,
    web: HashMap<String, String>,
}

impl StyleItems {
    pub fn new(
        items: Vec<StyleItem>,
        builtin: impl IntoIterator<Item = (String, String)>,
        web: impl IntoIterator<Item = (String, String)>,
    ) -> StyleItems {
        let mut by_name = HashMap::new();
        for (k, item) in items.iter().enumerate() {
            by_name
                .entry((item.kind, fold_name(&item.name)))
                .or_insert(k as u32);
        }
        StyleItems {
            items,
            by_name,
            builtin: builtin.into_iter().collect(),
            web: web.into_iter().collect(),
        }
    }

    pub(super) fn item(&self, k: u32) -> Option<&StyleItem> {
        self.items.get(k as usize)
    }

    pub(super) fn find_uuid(&self, uuid: &str) -> Option<u32> {
        let k = self
            .items
            .iter()
            .position(|i| i.uuid.eq_ignore_ascii_case(uuid))?;
        u32::try_from(k).ok()
    }

    fn find(&self, kind: StyleItemKind, name: &str) -> Option<u32> {
        self.by_name.get(&(kind, fold_name(name))).copied()
    }
}

pub(super) fn items(ex: &Execution) -> Option<Arc<StyleItems>> {
    ex.registry_data::<StyleItems>()
}

pub(super) fn item_ref(ex: &Execution, k: u32) -> Option<Vec<Node>> {
    let all = items(ex)?;
    Some(vec![Node::int(0), Node::atom(all.item(k)?.uuid.clone())])
}

pub(super) fn item_by_uuid(ex: &Execution, uuid: &str) -> Option<u32> {
    items(ex)?.find_uuid(uuid)
}

fn styled(ex: &Execution, text: &str) -> Option<String> {
    (ex.lang() == Lang::Ru).then(|| format!("{STYLE_PREFIX}{text}"))
}

pub(super) fn item_text(
    ex: &Execution,
    builtin: Option<&str>,
    item: Option<u32>,
) -> Option<String> {
    let all = items(ex)?;
    let text = match (builtin, item) {
        (Some(name), _) => all.builtin.get(name)?,
        (None, Some(k)) => &all.item(k)?.synonym,
        _ => return None,
    };
    styled(ex, text)
}

pub(super) fn web_text(ex: &Execution, name: &str) -> Option<Str> {
    let all = items(ex)?;
    let text = all.web.get(name)?;
    (ex.lang() == Lang::Ru).then(|| Str::from(text.as_str()))
}

pub(super) fn manager(t: TypeIdx, kind: StyleItemKind) -> GlobalProp {
    Arc::new(move |_| Ok(Value::Object(Rc::new(StyleManager { t, kind }))))
}

struct StyleManager {
    t: TypeIdx,
    kind: StyleItemKind,
}

fn no_field(name: &str) -> HostError {
    HostError::key(msg::PROP_NOT_FOUND).with_arg(name).raw()
}

impl StyleManager {
    fn builtin_name(&self, i: usize) -> &str {
        catalog()
            .type_info(self.t)
            .values
            .get(i)
            .map_or("", |v| v.name_ru.as_str())
    }

    fn name_of(&self, ex: &Execution, m: MemberId) -> String {
        match m.0.checked_sub(ITEM_BASE) {
            Some(k) => items(ex)
                .and_then(|all| all.item(k).map(|i| i.name.clone()))
                .unwrap_or_default(),
            None => self.builtin_name(m.0 as usize).to_string(),
        }
    }

    fn member(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let value = match (m.0.checked_sub(ITEM_BASE), self.kind) {
            (Some(k), StyleItemKind::Color) => Some(color::item_color(k)),
            (Some(k), StyleItemKind::Font) => Some(font::item_font(k)),
            (None, StyleItemKind::Color) => color::style_member(self.t, m.0 as usize),
            (None, StyleItemKind::Font) => font::style_member(self.t, m.0 as usize),
        };
        value.ok_or_else(|| no_field(&self.name_of(ex, m)).into())
    }
}

impl HostObject for StyleManager {
    fn retained_memory(&self, _: &mut dyn bsl_vm::RetainedMemory) -> Result<bool, Exception> {
        Ok(true)
    }

    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.t)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        self.member(ex, m)
    }

    fn set(&self, ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(HostError::key(msg::PROP_NOT_WRITABLE)
            .with_arg(self.name_of(ex, m))
            .raw()
            .into())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        Err(HostError::key(msg::METHOD_NOT_FOUND)
            .with_arg(self.name_of(ex, m))
            .raw()
            .into())
    }

    fn find_member(&self, name: &str) -> Option<MemberId> {
        u32::try_from(catalog().find_enum_value(self.t, name)?)
            .ok()
            .map(MemberId)
    }

    fn find_member_guarded(
        &self,
        ex: &mut Execution,
        name: &str,
    ) -> Result<Option<MemberId>, Exception> {
        if let Some(m) = self.find_member(name) {
            return Ok(Some(m));
        }
        Ok(items(ex)
            .and_then(|all| all.find(self.kind, name))
            .and_then(|k| ITEM_BASE.checked_add(k))
            .map(MemberId))
    }

    fn member_kind(&self, _m: MemberId) -> Option<MemberKind> {
        Some(MemberKind::Property {
            readable: true,
            writable: false,
        })
    }

    fn index_get(&self, ex: &mut Execution, key: &Value) -> CallResult<Value> {
        let name = match key {
            Value::String(s) => s.to_string_lossy(),
            Value::Number(_) => return Err(HostError::key(NOT_INDEXED).raw().into()),
            other => ex.present(other)?.to_string_lossy(),
        };
        match self.find_member_guarded(ex, &name)? {
            Some(m) => self.member(ex, m),
            None => Err(no_field(&name).into()),
        }
    }
}
