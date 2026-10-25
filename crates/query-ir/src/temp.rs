use crate::types::{NumQ, QType};
use bsl_prim::fold_name;
use std::collections::HashMap;
use std::sync::Arc;

pub trait TempSchemas: Sync {
    fn get(&self, name: &str) -> Option<Arc<TempSchema>>;
}

pub trait ManagedTempSchemas: Sync {
    fn get_folded(&self, name: &str) -> Option<&Arc<TempSchema>>;

    fn get_managed_folded(&self, _name: &str) -> Option<&crate::QShared<TempSchema>> {
        None
    }
}

pub(crate) enum Provider<'a> {
    Legacy(&'a dyn TempSchemas),
    Managed {
        arena: &'a crate::managed::CompileArena,
        tables: &'a dyn ManagedTempSchemas,
    },
}
impl Provider<'_> {
    pub(crate) fn get(&self, name: &str) -> crate::sema::R<Option<crate::QShared<TempSchema>>> {
        match self {
            Self::Legacy(tables) => Ok(tables.get(name).map(Into::into)),
            Self::Managed { arena, tables } => {
                let key = crate::managed::NameKey::folded(Some(arena), name)?;
                Ok(tables.get_managed_folded(&key).cloned().or_else(|| {
                    tables
                        .get_folded(&key)
                        .map(|schema| Arc::clone(schema).into())
                }))
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TempSchema {
    pub name: String,
    pub columns: Vec<TempColumn>,
    pub open: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TempColumn {
    pub name: String,
    pub ty: Option<QType>,
    pub unknown: bool,
    pub stored: Option<NumQ>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoTempTables;

impl TempSchemas for NoTempTables {
    fn get(&self, _name: &str) -> Option<Arc<TempSchema>> {
        None
    }
}

impl ManagedTempSchemas for NoTempTables {
    fn get_folded(&self, _name: &str) -> Option<&Arc<TempSchema>> {
        None
    }
}

#[derive(Clone, Debug, Default)]
pub struct TempMap(HashMap<String, Arc<TempSchema>>);

impl TempMap {
    pub fn new() -> TempMap {
        TempMap::default()
    }

    pub fn insert(&mut self, t: Arc<TempSchema>) -> Option<Arc<TempSchema>> {
        self.0.insert(fold_name(&t.name), t)
    }

    pub fn remove(&mut self, name: &str) -> Option<Arc<TempSchema>> {
        self.0.remove(&fold_name(name))
    }

    pub fn get(&self, name: &str) -> Option<Arc<TempSchema>> {
        self.0.get(&fold_name(name)).cloned()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<TempSchema>> {
        self.0.values()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.0.contains_key(&fold_name(name))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl TempSchemas for TempMap {
    fn get(&self, name: &str) -> Option<Arc<TempSchema>> {
        TempMap::get(self, name)
    }
}

impl ManagedTempSchemas for TempMap {
    fn get_folded(&self, name: &str) -> Option<&Arc<TempSchema>> {
        self.0.get(name)
    }
}
