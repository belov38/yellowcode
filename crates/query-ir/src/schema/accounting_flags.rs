use super::{AccountingFlagBinding, AccountingFlags, ObjMeta, SchemaError};
use brace::Guid;
use md_model::{Child, ChildTag, Configuration, ExternalObject, MdObject, Prop, Value};
use mdschema::MdKind;
use memory_budget::{BudgetError, BudgetedVec, Category, MemoryBudget, Reservation};
use std::alloc::Layout;

pub(super) trait Proof<'cfg> {
    type Error: From<SchemaError>;
    const ADMITTED: bool;
    fn unique(&mut self, cfg: &'cfg Configuration, id: Guid) -> Result<bool, Self::Error>;
}

pub(super) struct Legacy;

impl<'cfg> Proof<'cfg> for Legacy {
    type Error = SchemaError;
    const ADMITTED: bool = false;

    fn unique(&mut self, _: &'cfg Configuration, _: Guid) -> Result<bool, Self::Error> {
        Ok(false)
    }
}

#[derive(Debug)]
pub(super) enum Failure {
    Schema(SchemaError),
    Budget(BudgetError),
}

impl From<SchemaError> for Failure {
    fn from(error: SchemaError) -> Self {
        Self::Schema(error)
    }
}

impl From<BudgetError> for Failure {
    fn from(error: BudgetError) -> Self {
        Self::Budget(error)
    }
}

enum Frame<'cfg> {
    Objects(&'cfg [MdObject]),
    Children(&'cfg [Child]),
    External(&'cfg [ExternalObject]),
}

impl Frame<'_> {
    fn is_empty(&self) -> bool {
        match self {
            Self::Objects(v) => v.is_empty(),
            Self::Children(v) => v.is_empty(),
            Self::External(v) => v.is_empty(),
        }
    }
}

pub(super) struct Admitted<'cfg> {
    frames: BudgetedVec<Frame<'cfg>>,
    surcharge: Reservation,
}

impl<'cfg> Admitted<'cfg> {
    pub(super) fn new(budget: &MemoryBudget) -> Result<Self, BudgetError> {
        let wrapper = std::mem::size_of::<Self>()
            .checked_sub(std::mem::size_of::<BudgetedVec<Frame<'cfg>>>())
            .ok_or(BudgetError::Overflow)?;
        let surcharge = budget.try_reserve(
            Category::Metadata,
            u64::try_from(wrapper).map_err(|_| BudgetError::Overflow)?,
        )?;
        let frames = BudgetedVec::new(budget, Category::Metadata)?;
        Ok(Self { frames, surcharge })
    }

    fn overhead(capacity: usize) -> Result<u64, BudgetError> {
        if capacity == 0 {
            return Ok(0);
        }
        let layout = Layout::array::<Frame<'cfg>>(capacity).map_err(|_| BudgetError::Overflow)?;
        let body = u64::try_from(layout.size()).map_err(|_| BudgetError::Overflow)?;
        body.checked_add(15)
            .map(|n| n & !15)
            .and_then(|n| n.checked_add(16))
            .and_then(|n| n.checked_sub(body))
            .ok_or(BudgetError::Overflow)
    }

    fn push(&mut self, frame: Frame<'cfg>) -> Result<(), BudgetError> {
        if frame.is_empty() {
            return Ok(());
        }
        let old = self.frames.capacity();
        let required = self
            .frames
            .len()
            .checked_add(1)
            .ok_or(BudgetError::Overflow)?;
        if required <= old {
            return self.frames.try_push(frame);
        }
        let preferred = old.checked_mul(2).unwrap_or(required).max(required);
        let old_extra = Self::overhead(old)?;
        let new_extra = Self::overhead(preferred)?.max(Self::overhead(required)?);
        self.surcharge.try_grow(new_extra)?;
        #[cfg(test)]
        super::item_calibration_trace::mark(
            "frame-growth-before",
            0,
            [
                super::item_calibration_trace::Backing {
                    pointer: self.frames.as_slice().as_ptr() as usize,
                    capacity: self.frames.capacity(),
                    len: self.frames.len(),
                    element: size_of::<Frame<'cfg>>(),
                },
                Default::default(),
                Default::default(),
            ],
        );
        if let Err(error) = self.frames.try_push(frame) {
            self.surcharge.shrink(new_extra)?;
            return Err(error);
        }
        #[cfg(test)]
        super::item_calibration_trace::mark(
            "frame-growth-after",
            0,
            [
                super::item_calibration_trace::Backing {
                    pointer: self.frames.as_slice().as_ptr() as usize,
                    capacity: self.frames.capacity(),
                    len: self.frames.len(),
                    element: size_of::<Frame<'cfg>>(),
                },
                Default::default(),
                Default::default(),
            ],
        );
        let actual_extra = Self::overhead(self.frames.capacity())?;
        let excess = old_extra
            .checked_add(new_extra)
            .and_then(|n| n.checked_sub(actual_extra))
            .ok_or(BudgetError::Overflow)?;
        self.surcharge.shrink(excess)?;
        Ok(())
    }

    fn count(&mut self, cfg: &'cfg Configuration, id: Guid) -> Result<u8, BudgetError> {
        self.frames.clear();
        let result = self.count_inner(cfg, id);
        self.frames.clear();
        result
    }

    fn count_inner(&mut self, cfg: &'cfg Configuration, id: Guid) -> Result<u8, BudgetError> {
        let mut count = u8::from(cfg.id == id);
        self.push(Frame::Objects(&cfg.objects))?;
        while let Some(frame) = self.frames.pop() {
            match frame {
                Frame::Objects(objects) => {
                    let Some((object, rest)) = objects.split_first() else {
                        continue;
                    };
                    count = (count + u8::from(object.id == id)).min(2);
                    for sub in object.forms.iter().chain(&object.templates) {
                        count = (count + u8::from(sub.id == id)).min(2);
                    }
                    for recalculation in &object.recalculations {
                        count = (count + u8::from(recalculation.id == id)).min(2);
                        for dimension in &recalculation.dimensions {
                            count = (count + u8::from(dimension.id == id)).min(2);
                        }
                    }
                    if count == 2 {
                        return Ok(2);
                    }
                    self.push(Frame::Objects(rest))?;
                    self.push(Frame::Objects(&object.subsystems))?;
                    self.push(Frame::External(&object.external))?;
                    self.push(Frame::Children(&object.children))?;
                }
                Frame::Children(children) => {
                    let Some((child, rest)) = children.split_first() else {
                        continue;
                    };
                    count = (count + u8::from(child.id == id)).min(2);
                    if count == 2 {
                        return Ok(2);
                    }
                    self.push(Frame::Children(rest))?;
                    self.push(Frame::Children(&child.children))?;
                }
                Frame::External(objects) => {
                    let Some((object, rest)) = objects.split_first() else {
                        continue;
                    };
                    count = (count + u8::from(object.id == id)).min(2);
                    for sub in object.forms.iter().chain(&object.templates) {
                        count = (count + u8::from(sub.id == id)).min(2);
                    }
                    if count == 2 {
                        return Ok(2);
                    }
                    self.push(Frame::External(rest))?;
                    self.push(Frame::External(&object.nested))?;
                    self.push(Frame::Children(&object.children))?;
                }
            }
        }
        Ok(count)
    }
}

impl<'cfg> Proof<'cfg> for Admitted<'cfg> {
    type Error = Failure;
    const ADMITTED: bool = true;

    fn unique(&mut self, cfg: &'cfg Configuration, id: Guid) -> Result<bool, Self::Error> {
        Ok(!id.is_nil() && self.count(cfg, id)? == 1)
    }
}

#[derive(Clone, Copy)]
enum AccountingTarget {
    Chart,
    AccountingFlag,
    ExtDimensionAccountingFlag,
}

impl AccountingTarget {
    fn property(self) -> &'static str {
        match self {
            Self::Chart => "ChartOfAccounts",
            Self::AccountingFlag => "AccountingFlag",
            Self::ExtDimensionAccountingFlag => "ExtDimensionAccountingFlag",
        }
    }

    fn child_tag(self) -> Option<ChildTag> {
        match self {
            Self::Chart => None,
            Self::AccountingFlag => Some(ChildTag::AccountingFlag),
            Self::ExtDimensionAccountingFlag => Some(ChildTag::ExtDimensionAccountingFlag),
        }
    }
}

struct AccountingReference<'a> {
    id: Option<Guid>,
    chart_name: Option<&'a str>,
    child_name: Option<&'a str>,
}

fn accounting_fold(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(one), None) => one,
        _ => c,
    }
}

fn accounting_name_eq(a: &str, b: &str) -> bool {
    a.chars()
        .map(accounting_fold)
        .eq(b.chars().map(accounting_fold))
}

fn accounting_segment(name: &str) -> bool {
    !name.is_empty() && !name.contains('.') && !name.chars().any(char::is_whitespace)
}

fn accounting_single<T>(mut values: impl Iterator<Item = T>) -> Option<T> {
    let first = values.next()?;
    values.next().is_none().then_some(first)
}

fn accounting_property<'a>(props: &'a [Prop], name: &str) -> Result<Option<&'a Value>, ()> {
    let mut found = props.iter().filter(|p| p.name == name);
    let value = found.next().map(|p| &p.value);
    if found.next().is_some() {
        return Err(());
    }
    Ok(value)
}

fn accounting_reference(
    props: &[Prop],
    target: AccountingTarget,
) -> Result<Option<AccountingReference<'_>>, ()> {
    let Some(value) = accounting_property(props, target.property())? else {
        return Ok(None);
    };
    let (id, name) = match value {
        Value::Text(name) => (None, name.as_str()),
        Value::Ref(reference) => (reference.id, reference.name.as_str()),
        Value::RefList(list) => match list.as_slice() {
            [] => return Ok(None),
            [reference] => (reference.id, reference.name.as_str()),
            _ => return Err(()),
        },
        _ => return Err(()),
    };
    if id.is_some_and(|id| id.is_nil()) {
        return Err(());
    }
    if name.is_empty() {
        return Ok(id.map(|id| AccountingReference {
            id: Some(id),
            chart_name: None,
            child_name: None,
        }));
    }
    let mut parts = name.split('.');
    let kind = parts.next().ok_or(())?;
    let chart = parts.next().filter(|n| accounting_segment(n)).ok_or(())?;
    if !accounting_name_eq(kind, MdKind::ChartOfAccounts.as_str()) {
        return Err(());
    }
    let child_name = match target.child_tag() {
        None => None,
        Some(_) => {
            let tag = parts.next().ok_or(())?;
            if !accounting_name_eq(tag, target.property()) {
                return Err(());
            }
            Some(parts.next().filter(|n| accounting_segment(n)).ok_or(())?)
        }
    };
    if parts.next().is_some() {
        return Err(());
    }
    Ok(Some(AccountingReference {
        id,
        chart_name: Some(chart),
        child_name,
    }))
}

fn accounting_boolean(child: &Child) -> bool {
    let Ok(Some(Value::Type(td))) = accounting_property(&child.props, "Type") else {
        return false;
    };
    !td.types.is_empty()
        && td.types.iter().all(|atom| {
            !atom.set
                && atom.type_id.is_none()
                && atom.name.fixed_local(mdschema::FixedNamespace::Xs) == Some("boolean")
        })
        && td.number.is_none()
        && td.string.is_none()
        && td.date.is_none()
        && td.binary.is_none()
}

fn parent<'cfg, P: Proof<'cfg>>(
    cfg: &'cfg Configuration,
    register: &'cfg MdObject,
    proof: &mut P,
) -> Result<Option<&'cfg MdObject>, P::Error> {
    if !P::ADMITTED || register.kind != MdKind::AccountingRegister {
        return Ok(None);
    }
    let reference = match accounting_reference(&register.props, AccountingTarget::Chart) {
        Ok(Some(reference)) => reference,
        _ => return Ok(None),
    };
    if !proof.unique(cfg, register.id)? {
        return Ok(None);
    }
    let by_id = match reference.id {
        Some(id) => match accounting_single(cfg.objects.iter().filter(|o| o.id == id)) {
            Some(object) => Some(object),
            None => return Ok(None),
        },
        None => None,
    };
    let by_name = match reference.chart_name {
        Some(name) => match accounting_single(
            cfg.objects
                .iter()
                .filter(|o| o.kind == MdKind::ChartOfAccounts && accounting_name_eq(&o.name, name)),
        ) {
            Some(object) => Some(object),
            None => return Ok(None),
        },
        None => None,
    };
    let chart = match (by_id, by_name) {
        (Some(a), Some(b)) if std::ptr::eq(a, b) => a,
        (Some(chart), None) | (None, Some(chart)) => chart,
        _ => return Ok(None),
    };
    if chart.kind != MdKind::ChartOfAccounts
        || chart.id.is_nil()
        || !accounting_segment(&chart.name)
        || accounting_single(cfg.objects.iter().filter(|o| {
            o.kind == MdKind::ChartOfAccounts && accounting_name_eq(&o.name, &chart.name)
        }))
        .is_none()
        || !proof.unique(cfg, chart.id)?
    {
        return Ok(None);
    }
    Ok(Some(chart))
}

fn binding<'cfg, P: Proof<'cfg>>(
    cfg: &'cfg Configuration,
    chart: Option<&'cfg MdObject>,
    props: &[Prop],
    target: AccountingTarget,
    proof: &mut P,
) -> Result<AccountingFlagBinding, P::Error> {
    use AccountingFlagBinding::{Absent, Present, Unresolved};
    let reference = match accounting_reference(props, target) {
        Ok(None) => return Ok(Absent),
        Ok(Some(reference)) => reference,
        Err(()) => return Ok(Unresolved),
    };
    let (Some(chart), Some(tag)) = (chart, target.child_tag()) else {
        return Ok(Unresolved);
    };
    if reference
        .chart_name
        .is_some_and(|name| !accounting_name_eq(name, &chart.name))
    {
        return Ok(Unresolved);
    }
    let by_id = match reference.id {
        Some(id) => match accounting_single(chart.children.iter().filter(|c| c.id == id)) {
            Some(child) => Some(child),
            None => return Ok(Unresolved),
        },
        None => None,
    };
    let by_name = match reference.child_name {
        Some(name) => match accounting_single(
            chart
                .children
                .iter()
                .filter(|c| c.tag == tag && accounting_name_eq(&c.name, name)),
        ) {
            Some(child) => Some(child),
            None => return Ok(Unresolved),
        },
        None => None,
    };
    let child = match (by_id, by_name) {
        (Some(a), Some(b)) if std::ptr::eq(a, b) => a,
        (Some(child), None) | (None, Some(child)) => child,
        _ => return Ok(Unresolved),
    };
    if child.tag != tag
        || child.id.is_nil()
        || !accounting_segment(&child.name)
        || accounting_single(
            chart
                .children
                .iter()
                .filter(|c| c.tag == tag && accounting_name_eq(&c.name, &child.name)),
        )
        .is_none()
        || !accounting_boolean(child)
        || !proof.unique(cfg, child.id)?
    {
        return Ok(Unresolved);
    }
    Ok(Present {
        chart: chart.id,
        child: child.id,
    })
}

pub(super) fn populate<'cfg, P: Proof<'cfg>>(
    cfg: &'cfg Configuration,
    register: &'cfg MdObject,
    meta: &mut ObjMeta,
    proof: &mut P,
) -> Result<(), P::Error> {
    if register.kind != MdKind::AccountingRegister {
        return Ok(());
    }
    let chart = parent(cfg, register, proof)?;
    for (tag, items) in [
        (ChildTag::Dimension, &mut meta.dims),
        (ChildTag::Resource, &mut meta.resources),
        (ChildTag::Attribute, &mut meta.attrs),
    ] {
        for item in items {
            let source = accounting_single(register.children.iter().filter(|c| c.id == item.id))
                .filter(|c| c.tag == tag);
            let (accounting, ext_dimension) = match source {
                Some(child) => (
                    binding(
                        cfg,
                        chart,
                        &child.props,
                        AccountingTarget::AccountingFlag,
                        proof,
                    )?,
                    binding(
                        cfg,
                        chart,
                        &child.props,
                        AccountingTarget::ExtDimensionAccountingFlag,
                        proof,
                    )?,
                ),
                None => (
                    AccountingFlagBinding::Unresolved,
                    AccountingFlagBinding::Unresolved,
                ),
            };
            item.accounting_flags = AccountingFlags {
                parent_valid: chart.is_some(),
                accounting,
                ext_dimension,
            };
        }
    }
    Ok(())
}
