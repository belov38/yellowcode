pub mod rules;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NumQ {
    pub digits: u8,
    pub scale: u8,
    pub nonneg: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StrQ {
    pub len: u32,
    pub fixed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DateQ {
    Date,
    Time,
    DateTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RefAtom(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Other {
    Uuid,
    Binary,
    Storage,
    Type,
    TypeDescription,
    PointInTime,
    Boundary,
    QueryResult,
    ValueTable,
    RoutePoint(RefAtom),
    SysEnum(platform_catalog::TypeIdx),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct QType {
    pub null: bool,
    pub boolean: bool,
    pub number: Option<NumQ>,
    pub string: Option<StrQ>,
    pub date: Option<DateQ>,
    pub refs: Vec<RefAtom>,
    pub others: Vec<Other>,
}

const REF_WIDTH: u32 = 25;
const UNLIMITED_WIDTH: u32 = 32;

impl QType {
    pub fn undefined() -> QType {
        QType::default()
    }

    pub fn null_only() -> QType {
        QType {
            null: true,
            ..QType::default()
        }
    }

    pub fn num(q: NumQ) -> QType {
        QType {
            number: Some(q),
            ..QType::default()
        }
    }

    pub fn str_(q: StrQ) -> QType {
        QType {
            string: Some(q),
            ..QType::default()
        }
    }

    pub fn date(q: DateQ) -> QType {
        QType {
            date: Some(q),
            ..QType::default()
        }
    }

    pub fn boolean() -> QType {
        QType {
            boolean: true,
            ..QType::default()
        }
    }

    pub fn reference(r: RefAtom) -> QType {
        QType {
            refs: vec![r],
            ..QType::default()
        }
    }

    pub fn other(o: Other) -> QType {
        QType {
            others: vec![o],
            ..QType::default()
        }
    }

    pub fn with_null(mut self) -> QType {
        self.null = true;
        self
    }

    pub fn without_null(mut self) -> QType {
        self.null = false;
        self
    }

    pub fn merge(&self, o: &QType) -> QType {
        let mut merged = self.merged_qualifiers(o);
        merged.refs = union_atoms(&self.refs, &o.refs);
        merged.others = union_atoms(&self.others, &o.others);
        merged
    }

    pub fn try_clone_budgeted(
        &self,
        budget: &memory_budget::MemoryBudget,
    ) -> Result<memory_budget::Charged<QType>, memory_budget::BudgetError> {
        let refs = copy_atoms_budgeted(&self.refs, budget)?;
        let others = copy_atoms_budgeted(&self.others, budget)?;
        Ok(refs.try_zip(others)?.map(|(refs, others)| QType {
            null: self.null,
            boolean: self.boolean,
            number: self.number,
            string: self.string,
            date: self.date,
            refs,
            others,
        }))
    }

    pub fn try_from_atom_budgeted(
        atom: Option<crate::schema::atoms::TypeAtom>,
        budget: &memory_budget::MemoryBudget,
    ) -> Result<memory_budget::Charged<QType>, memory_budget::BudgetError> {
        use crate::schema::atoms::TypeAtom;
        let mut qualifiers = Self::undefined();
        let mut reference = None;
        let mut other = None;
        match atom {
            None => {}
            Some(TypeAtom::Null) => qualifiers.null = true,
            Some(TypeAtom::Boolean) => qualifiers.boolean = true,
            Some(TypeAtom::Number(value)) => qualifiers.number = Some(value),
            Some(TypeAtom::String(value)) => qualifiers.string = Some(value),
            Some(TypeAtom::Date(value)) => qualifiers.date = Some(value),
            Some(TypeAtom::Reference(value)) => reference = Some(value),
            Some(TypeAtom::Other(value)) => other = Some(value),
        }
        let refs = copy_atoms_budgeted(reference.as_slice(), budget)?;
        let others = copy_atoms_budgeted(other.as_slice(), budget)?;
        Ok(refs.try_zip(others)?.map(|(refs, others)| QType {
            refs,
            others,
            ..qualifiers
        }))
    }

    pub fn try_merge_budgeted(
        &self,
        other: &QType,
        budget: &memory_budget::MemoryBudget,
    ) -> Result<memory_budget::Charged<QType>, memory_budget::BudgetError> {
        let refs = union_atoms_budgeted(&self.refs, &other.refs, budget)?;
        let others = union_atoms_budgeted(&self.others, &other.others, budget)?;
        let qualifiers = self.merged_qualifiers(other);
        Ok(refs.try_zip(others)?.map(|(refs, others)| QType {
            refs,
            others,
            ..qualifiers
        }))
    }

    pub(crate) fn merged_qualifiers(&self, o: &QType) -> QType {
        let number = match (self.number, o.number) {
            (Some(a), Some(b)) if a == b => Some(a),
            (Some(_), Some(_)) => Some(NumQ {
                digits: 0,
                scale: 0,
                nonneg: false,
            }),
            (a, b) => a.or(b),
        };
        let string = match (self.string, o.string) {
            (Some(a), Some(b)) => Some(StrQ {
                len: if a.len == 0 || b.len == 0 {
                    0
                } else {
                    a.len.max(b.len)
                },
                fixed: a.fixed && b.fixed && a.len == b.len,
            }),
            (a, b) => a.or(b),
        };
        let date = match (self.date, o.date) {
            (Some(a), Some(b)) if a != b => Some(DateQ::DateTime),
            (a, b) => a.or(b),
        };
        QType {
            null: self.null || o.null,
            boolean: self.boolean || o.boolean,
            number,
            string,
            date,
            refs: Vec::new(),
            others: Vec::new(),
        }
    }

    pub fn is_subtype_of(&self, o: &QType) -> bool {
        (!self.boolean || o.boolean)
            && (self.number.is_none() || o.number.is_some())
            && (self.string.is_none() || o.string.is_some())
            && (self.date.is_none() || o.date.is_some())
            && self.refs.iter().all(|r| o.refs.binary_search(r).is_ok())
            && self
                .others
                .iter()
                .all(|x| o.others.binary_search(x).is_ok())
    }

    pub fn atom_count(&self) -> usize {
        usize::from(self.boolean)
            + usize::from(self.number.is_some())
            + usize::from(self.string.is_some())
            + usize::from(self.date.is_some())
            + self.refs.len()
            + self.others.len()
    }

    pub fn is_undefined(&self) -> bool {
        !self.null && self.atom_count() == 0
    }

    pub fn is_composite(&self) -> bool {
        self.atom_count() > 1
    }

    pub fn is_composite_beyond_refs(&self) -> bool {
        self.is_composite()
            && (self.boolean
                || self.number.is_some()
                || self.string.is_some()
                || self.date.is_some()
                || self
                    .others
                    .iter()
                    .any(|o| !matches!(o, Other::SysEnum(_) | Other::RoutePoint(_))))
    }

    pub fn has_unlimited_string(&self) -> bool {
        self.string.is_some_and(|q| q.len == 0)
    }

    pub fn only_number(&self) -> bool {
        self.number.is_some() && self.atom_count() == 1
    }

    pub fn only_string(&self) -> bool {
        self.string.is_some() && self.atom_count() == 1
    }

    pub fn only_date(&self) -> bool {
        self.date.is_some() && self.atom_count() == 1
    }

    pub fn only_bool(&self) -> bool {
        self.boolean && self.atom_count() == 1
    }

    pub fn has_number(&self) -> bool {
        self.number.is_some()
    }

    pub fn has_string(&self) -> bool {
        self.string.is_some()
    }

    pub fn width(&self) -> u32 {
        let number = self.number.map(|q| match q.digits {
            0 => UNLIMITED_WIDTH,
            p => u32::from(p),
        });
        let string = self.string.map(|q| match q.len {
            0 => UNLIMITED_WIDTH,
            n => n,
        });
        let date = self.date.map(|q| match q {
            DateQ::Date => 10,
            DateQ::Time | DateQ::DateTime => 19,
        });
        let boolean = self.boolean.then_some(2);
        let non_primitive = (!self.refs.is_empty() || !self.others.is_empty()).then_some(REF_WIDTH);
        [number, string, date, boolean, non_primitive]
            .into_iter()
            .flatten()
            .max()
            .unwrap_or(1)
    }
}

fn copy_atoms_budgeted<T: Copy>(
    values: &[T],
    budget: &memory_budget::MemoryBudget,
) -> Result<memory_budget::Charged<Vec<T>>, memory_budget::BudgetError> {
    let mut out = memory_budget::BudgetedVec::try_with_capacity(
        budget,
        memory_budget::Category::Execution,
        values.len(),
    )?;
    out.try_extend_from_slice(values)?;
    Ok(out.into_charged_vec())
}
fn union_atoms_budgeted<T: Ord + Copy>(
    left: &[T],
    right: &[T],
    budget: &memory_budget::MemoryBudget,
) -> Result<memory_budget::Charged<Vec<T>>, memory_budget::BudgetError> {
    let capacity = left
        .len()
        .checked_add(right.len())
        .ok_or(memory_budget::BudgetError::Overflow)?;
    let mut out = memory_budget::BudgetedVec::try_with_capacity(
        budget,
        memory_budget::Category::Execution,
        capacity,
    )?;
    out.try_extend_from_slice(left)?;
    out.try_extend_from_slice(right)?;
    Ok(out.into_charged_vec().map(|mut values| {
        values.sort_unstable();
        values.dedup();
        values
    }))
}

fn union_atoms<T: Ord + Copy>(a: &[T], b: &[T]) -> Vec<T> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    out.extend_from_slice(a);
    out.extend_from_slice(b);
    out.sort_unstable();
    out.dedup();
    out
}
