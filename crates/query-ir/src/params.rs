use crate::temp::TempSchema;
use crate::types::QType;
use crate::value::QValue;
use std::sync::Arc;

pub trait ParamTypes: Sync {
    fn shape(&self, name: &str) -> Option<ParamShape>;

    fn constant(&self, _name: &str) -> Option<ParamConst> {
        None
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ParamConst {
    Value(QValue),
    List(Vec<QValue>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ParamShape {
    Value(QType),
    List(QType),
    Table(Arc<TempSchema>),
    TypeDescription,
    Structure,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoParams;

impl ParamTypes for NoParams {
    fn shape(&self, _name: &str) -> Option<ParamShape> {
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamInfo {
    pub name: String,
    pub ty: QType,
}

pub trait ManagedParamTypes: Sync {
    fn shape_lowercase(&self, name: &str) -> Option<&ParamShape>;

    fn constant_lowercase(&self, _name: &str) -> Option<&ParamConst> {
        None
    }
}
impl ManagedParamTypes for NoParams {
    fn shape_lowercase(&self, _name: &str) -> Option<&ParamShape> {
        None
    }
}

pub(crate) type ParamView<'a, T> = std::borrow::Cow<'a, T>;

pub(crate) enum Provider<'a> {
    Legacy(&'a dyn ParamTypes),
    Managed {
        arena: &'a crate::managed::CompileArena,
        params: &'a dyn ManagedParamTypes,
    },
}
impl Provider<'_> {
    pub(crate) fn shape(&self, name: &str) -> crate::sema::R<Option<ParamView<'_, ParamShape>>> {
        match self {
            Self::Legacy(params) => Ok(params.shape(name).map(ParamView::Owned)),
            Self::Managed { arena, params } => {
                let key = crate::managed::NameKey::parameter(Some(arena), name)?;
                Ok(params
                    .shape_lowercase(key.as_str())
                    .map(ParamView::Borrowed))
            }
        }
    }
    pub(crate) fn constant(&self, name: &str) -> crate::sema::R<Option<ParamView<'_, ParamConst>>> {
        match self {
            Self::Legacy(params) => Ok(params.constant(name).map(ParamView::Owned)),
            Self::Managed { arena, params } => {
                let key = crate::managed::NameKey::parameter(Some(arena), name)?;
                Ok(params
                    .constant_lowercase(key.as_str())
                    .map(ParamView::Borrowed))
            }
        }
    }
}
