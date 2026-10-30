use crate::Console;
use crate::run::{ErrorOut, Phase};
use memory_budget::{BudgetError, Category, MemoryLimit, Reservation, TextCount};
use query_ir::{Other, QType, QValue, uuid_of};
use query_syntax::{Lang, ParseMode};
use query_testhost::BatchMemory;
use query_testhost::repr_lang;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamKind {
    Number,
    String,
    Date,
    Bool,
    Ref,
    List,
    Type,
    Table,
    Any,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ParamInfoOut {
    pub name: String,
    pub ty: String,
    pub kind: ParamKind,
    pub refs: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Choice {
    pub id: String,
    pub text: String,
}

#[derive(Debug)]
pub struct RetainedList<T> {
    values: Vec<T>,
    _retained: Option<Reservation>,
}
pub type Parameters = RetainedList<ParamInfoOut>;
pub type RefChoices = RetainedList<Choice>;
impl<T> std::ops::Deref for RetainedList<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.values
    }
}
impl<T: Serialize> Serialize for RetainedList<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.values.serialize(serializer)
    }
}
impl<'a, T> IntoIterator for &'a RetainedList<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.values.iter()
    }
}

fn memory_error(error: MemoryLimit) -> ErrorOut {
    ErrorOut::new(Phase::Runtime, error.public_text())
}
fn allocation_error(error: BudgetError) -> ErrorOut {
    match error {
        BudgetError::Limit(limit) => memory_error(limit),
        _ => ErrorOut::new(Phase::Runtime, "MEMORY_ALLOCATION_FAILED"),
    }
}
fn cancelled(cancel: &AtomicBool) -> Result<(), ErrorOut> {
    if cancel.load(Ordering::Acquire) {
        Err(ErrorOut::new(Phase::Runtime, "Выполнение прервано"))
    } else {
        Ok(())
    }
}
fn structure_error(limit: query_syntax::PublicParseLimit) -> ErrorOut {
    let limit = match limit {
        query_syntax::PublicParseLimit::Statements => memory_budget::StructureLimit::Statements,
        query_syntax::PublicParseLimit::Columns => memory_budget::StructureLimit::Columns,
        query_syntax::PublicParseLimit::Depth => memory_budget::StructureLimit::Depth,
    };
    ErrorOut::new(
        Phase::Runtime,
        query_ir::ExecError::Structure(limit).to_string(),
    )
}

pub const MAX_CHOICES: usize = 50;

pub(crate) fn param_kind(ty: &QType) -> ParamKind {
    if ty.others.contains(&Other::ValueTable) {
        return ParamKind::Table;
    }
    let atoms = usize::from(ty.boolean)
        + usize::from(ty.number.is_some())
        + usize::from(ty.string.is_some())
        + usize::from(ty.date.is_some())
        + usize::from(!ty.refs.is_empty())
        + ty.others.len();
    if atoms != 1 {
        return ParamKind::Any;
    }
    if ty.boolean {
        ParamKind::Bool
    } else if ty.number.is_some() {
        ParamKind::Number
    } else if ty.string.is_some() {
        ParamKind::String
    } else if ty.date.is_some() {
        ParamKind::Date
    } else if !ty.refs.is_empty() {
        ParamKind::Ref
    } else if ty.others == [Other::Type] {
        ParamKind::Type
    } else {
        ParamKind::Any
    }
}

impl Console {
    pub fn parameters(&self, text: &str) -> Result<Parameters, ErrorOut> {
        self.parameters_with_cancel(text, &AtomicBool::new(false))
    }

    pub fn parameters_with_cancel(
        &self,
        text: &str,
        cancel: &AtomicBool,
    ) -> Result<Parameters, ErrorOut> {
        self.independent(cancel, |cancel| self.parameters_active(text, cancel))?
    }

    fn parameters_active(&self, text: &str, cancel: &AtomicBool) -> Result<Parameters, ErrorOut> {
        cancelled(cancel)?;
        let memory = BatchMemory::new(&self.base)
            .transpose()
            .map_err(memory_error)?;
        #[cfg(test)]
        self.query_checkpoint("parameters", cancel);
        cancelled(cancel)?;
        let schema = self.base.schema();
        let fail = |error: &query_ir::CompileError| {
            let failure =
                query_testhost::compile_failure_budgeted(error, text, Lang::Ru, &self.base);
            ErrorOut::from_chain(Phase::Compile, failure.chain, failure.at)
        };
        let batch = match &memory {
            Some(memory) => {
                let pre = query_syntax::public_preflight(text).map_err(structure_error)?;
                cancelled(cancel)?;
                memory
                    .compile
                    .try_grow_or_evict(memory.model.parse(&pre, text.len()), memory.evictor())
                    .map_err(memory_error)?;
                let batch = query_ir::compile_batch_public(text, ParseMode::Query, schema)
                    .map_err(|query_ir::CompileRefusal::Structure(limit)| structure_error(limit))?
                    .map_err(|error| fail(&error))?;
                for i in 0..batch.stmts.len() {
                    cancelled(cancel)?;
                    let bytes = memory
                        .model
                        .sema(
                            &pre,
                            i,
                            memory.max_ref_fanout,
                            memory.max_table_width,
                            batch.stmts.len(),
                        )
                        .saturating_mul(2);
                    memory
                        .compile
                        .try_grow_or_evict(bytes, memory.evictor())
                        .map_err(memory_error)?;
                }
                batch
            }
            None => query_ir::compile_batch(text, ParseMode::Query, schema)
                .map_err(|error| fail(&error))?,
        };
        cancelled(cancel)?;
        let found = query_ir::find_parameters(&batch, text, schema, &query_ir::NoTempTables)
            .map_err(|error| fail(&error))?;
        cancelled(cancel)?;
        let mut bytes = (found.len() as u64)
            .saturating_mul(std::mem::size_of::<ParamInfoOut>() as u64)
            .saturating_add(128);
        let mut scratch_bytes = 0u64;
        if let Some(memory) = &memory {
            for parameter in &found {
                cancelled(cancel)?;
                let mut count = TextCount::new(Category::Result, u64::MAX);
                query_testkit::repr::write_type_repr_budgeted(
                    schema,
                    &parameter.ty,
                    Lang::Ru,
                    &memory.memory.budget,
                    Category::Execution,
                    &mut count,
                    true,
                )
                .map_err(allocation_error)?;
                bytes = bytes
                    .saturating_add((parameter.name.len() as u64).saturating_mul(2))
                    .saturating_add(count.len().saturating_mul(2))
                    .saturating_add(128);
                scratch_bytes = scratch_bytes.saturating_add(
                    (parameter.ty.refs.len() as u64)
                        .saturating_add(8)
                        .saturating_mul(128),
                );
                for reference in &parameter.ty.refs {
                    bytes = bytes
                        .saturating_add(
                            (schema.ref_info(*reference).xml_name.len() as u64).saturating_mul(2),
                        )
                        .saturating_add(128);
                }
            }
        }
        let budget = self.base.memory_budget();
        let retained = budget
            .map(|budget| budget.try_reserve(Category::Result, bytes))
            .transpose()
            .map_err(memory_error)?;
        let _format_scratch = budget
            .map(|budget| budget.try_reserve(Category::Execution, scratch_bytes))
            .transpose()
            .map_err(memory_error)?;
        let mut values = Vec::with_capacity(found.len());
        for parameter in &found {
            cancelled(cancel)?;
            values.push(ParamInfoOut {
                name: parameter.name.clone(),
                ty: query_testkit::repr::type_repr(schema, &parameter.ty, Lang::Ru),
                kind: param_kind(&parameter.ty),
                refs: parameter
                    .ty
                    .refs
                    .iter()
                    .map(|reference| schema.ref_info(*reference).xml_name.clone())
                    .collect(),
            });
        }
        cancelled(cancel)?;
        Ok(RetainedList {
            values,
            _retained: retained,
        })
    }

    pub fn ref_choices(&self, kind: &str, limit: usize) -> Result<RefChoices, ErrorOut> {
        self.ref_choices_with_cancel(kind, limit, &AtomicBool::new(false))
    }

    pub fn ref_choices_with_cancel(
        &self,
        kind: &str,
        limit: usize,
        cancel: &AtomicBool,
    ) -> Result<RefChoices, ErrorOut> {
        self.independent(cancel, |cancel| {
            self.ref_choices_active(kind, limit, cancel)
        })?
    }

    fn ref_choices_active(
        &self,
        kind: &str,
        limit: usize,
        cancel: &AtomicBool,
    ) -> Result<RefChoices, ErrorOut> {
        cancelled(cancel)?;
        let limit = limit.min(MAX_CHOICES);
        let budget = self.base.memory_budget();
        let _scratch = budget
            .map(|budget| {
                budget.try_reserve(
                    Category::Execution,
                    (limit as u64).saturating_mul(64).saturating_add(1024),
                )
            })
            .transpose()
            .map_err(memory_error)?;
        #[cfg(test)]
        self.query_checkpoint("choices", cancel);
        cancelled(cancel)?;
        let mut number = None;
        for (index, (candidate, name)) in self.base.ref_kinds_borrowed().enumerate() {
            if index.is_multiple_of(64) {
                cancelled(cancel)?;
            }
            if name == kind {
                number = Some(candidate);
                break;
            }
        }
        cancelled(cancel)?;
        let mut selected = Vec::with_capacity(limit);
        if let Some(number) = number.filter(|_| limit > 0) {
            for (index, candidate) in self.base.named_refs_borrowed(number).enumerate() {
                if index.is_multiple_of(64) {
                    cancelled(cancel)?;
                }
                let position = selected.partition_point(|(id, name): &([u8; 16], &str)| {
                    name.cmp(&candidate.1).then(id.cmp(&candidate.0)).is_lt()
                });
                if selected.len() < limit {
                    selected.insert(position, candidate);
                } else if position < limit {
                    selected.pop();
                    selected.insert(position, candidate);
                }
            }
        }
        cancelled(cancel)?;
        let mut bytes = (selected.len() as u64)
            .saturating_mul(std::mem::size_of::<Choice>() as u64)
            .saturating_add(128);
        if let (Some(number), Some(budget)) = (number, budget) {
            for (id, _) in &selected {
                cancelled(cancel)?;
                let len = query_testhost::measure_native_repr(
                    &QValue::Ref {
                        table: number,
                        id: *id,
                    },
                    &self.base,
                    Lang::Ru,
                    budget,
                    u64::MAX,
                )
                .map_err(allocation_error)?;
                bytes = bytes
                    .saturating_add(len.saturating_mul(2))
                    .saturating_add(256);
            }
        }
        let retained = budget
            .map(|budget| budget.try_reserve(Category::Result, bytes))
            .transpose()
            .map_err(memory_error)?;
        let mut values = Vec::with_capacity(selected.len());
        if let Some(number) = number {
            for (id, _) in selected {
                cancelled(cancel)?;
                values.push(Choice {
                    id: uuid_of(&id).to_string(),
                    text: repr_lang(&QValue::Ref { table: number, id }, &self.base, Lang::Ru),
                });
            }
        }
        cancelled(cancel)?;
        Ok(RetainedList {
            values,
            _retained: retained,
        })
    }
}
