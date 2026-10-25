use crate::QType;
use std::sync::Arc;
mod bytes;
mod shared;
pub use bytes::QBytes;
pub use shared::QShared;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct QStr(pub QStrData);

#[derive(Clone)]
pub struct QStrData(QStrBacking);
#[derive(Clone)]
enum QStrBacking {
    Legacy(Arc<[u16]>),
    Managed(memory_budget::Shared<Vec<u16>>),
}
impl std::ops::Deref for QStrData {
    type Target = [u16];
    fn deref(&self) -> &[u16] {
        match &self.0 {
            QStrBacking::Legacy(units) => units,
            QStrBacking::Managed(units) => units.as_slice(),
        }
    }
}
impl AsRef<[u16]> for QStrData {
    fn as_ref(&self) -> &[u16] {
        self
    }
}
impl std::fmt::Debug for QStrData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&**self, f)
    }
}
impl PartialEq for QStrData {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}
impl Eq for QStrData {}
impl std::hash::Hash for QStrData {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&**self, state);
    }
}
impl From<Arc<[u16]>> for QStrData {
    fn from(units: Arc<[u16]>) -> Self {
        Self(QStrBacking::Legacy(units))
    }
}
impl From<Vec<u16>> for QStrData {
    fn from(units: Vec<u16>) -> Self {
        Self(QStrBacking::Legacy(units.into()))
    }
}
impl From<&[u16]> for QStrData {
    fn from(units: &[u16]) -> Self {
        Self(QStrBacking::Legacy(units.into()))
    }
}

impl QStr {
    pub fn try_clone_budgeted(
        &self,
        budget: &memory_budget::MemoryBudget,
    ) -> Result<Self, memory_budget::BudgetError> {
        if let QStrBacking::Managed(units) = &self.0.0
            && units.reservation().budget().same_instance(budget)
        {
            return Ok(self.clone());
        }
        let mut units = memory_budget::BudgetedVec::try_with_capacity(
            budget,
            memory_budget::Category::Execution,
            self.0.len(),
        )?;
        units.try_extend_from_slice(&self.0)?;
        Self::try_from_budgeted_units(units)
    }

    pub(crate) fn managed_reservation(&self) -> Option<&memory_budget::Reservation> {
        match &self.0.0 {
            QStrBacking::Legacy(_) => None,
            QStrBacking::Managed(units) => Some(units.reservation()),
        }
    }

    pub fn try_from_str_budgeted(
        text: &str,
        budget: &memory_budget::MemoryBudget,
    ) -> Result<Self, memory_budget::BudgetError> {
        let capacity = text.encode_utf16().count();
        let mut units = memory_budget::BudgetedVec::try_with_capacity(
            budget,
            memory_budget::Category::Execution,
            capacity,
        )?;
        for unit in text.encode_utf16() {
            units.try_push(unit)?;
        }
        Self::try_from_budgeted_units(units)
    }
    pub fn try_from_budgeted_units(
        units: memory_budget::BudgetedVec<u16>,
    ) -> Result<Self, memory_budget::BudgetError> {
        Ok(Self(QStrData(QStrBacking::Managed(
            units.try_into_shared()?,
        ))))
    }
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> QStr {
        QStr(s.encode_utf16().collect::<Vec<u16>>().into())
    }

    pub fn to_string_lossy(&self) -> String {
        String::from_utf16_lossy(&self.0)
    }

    pub fn len_utf16(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

pub type RefId = [u8; 16];

#[derive(Clone, Debug, PartialEq)]
pub enum QValue {
    Null,
    Undefined,
    Bool(bool),
    Number(bsl_prim::Number),
    String(QStr),
    Date(bsl_prim::BslDate),
    Uuid([u8; 16]),
    Ref { table: u32, id: RefId },
    Type(QType),
    Binary(QBytes),
    Storage(QBytes),
    SysEnum(platform_catalog::TypeIdx, u32),
    Nested(QShared<NestedResult>),
    PointInTime(Box<Moment>),
    Boundary(Box<Boundary>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Moment {
    pub date: bsl_prim::BslDate,
    pub reference: Option<(u32, RefId)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Boundary {
    pub point: QValue,
    pub include: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NestedResult {
    pub columns: Vec<crate::ColumnInfo>,
    pub rows: Vec<Vec<QValue>>,
}
