use super::xml_type_name;
use crate::TestBase;
use memory_budget::{
    BudgetError, BudgetedText, Category, Charged, MemoryBudget, TextCount, TextOutput,
};
use query_ir::managed_value::{ManagedValue, ValueRef};
use query_ir::{QType, QValue, uuid_of};
use query_syntax::Lang;
use query_testkit::repr::{platform_text, write_type_repr_budgeted};
use std::fmt;

pub fn repr_budgeted(
    value: &QValue,
    base: &TestBase,
    budget: &MemoryBudget,
    category: Category,
    limit: u64,
) -> Result<Charged<String>, BudgetError> {
    repr_lang_budgeted(value, base, Lang::Ru, budget, category, limit)
}

pub fn repr_lang_budgeted(
    value: &QValue,
    base: &TestBase,
    lang: Lang,
    budget: &MemoryBudget,
    category: Category,
    limit: u64,
) -> Result<Charged<String>, BudgetError> {
    let mut output = BudgetedText::new(budget, category, limit)?;
    ValueWriter {
        base,
        lang,
        budget,
        category,
    }
    .write(Input::Native(value), &mut output, 0)?;
    output.into_string()
}

pub fn repr_managed_budgeted(
    value: &ManagedValue,
    base: &TestBase,
    budget: &MemoryBudget,
    category: Category,
    limit: u64,
) -> Result<Charged<String>, BudgetError> {
    repr_managed_lang_budgeted(value, base, Lang::Ru, budget, category, limit)
}

pub fn repr_managed_lang_budgeted(
    value: &ManagedValue,
    base: &TestBase,
    lang: Lang,
    budget: &MemoryBudget,
    category: Category,
    limit: u64,
) -> Result<Charged<String>, BudgetError> {
    let mut output = BudgetedText::new(budget, category, limit)?;
    write_managed_repr(value, base, lang, budget, category, &mut output)?;
    output.into_string()
}

pub fn write_managed_repr(
    value: &ManagedValue,
    base: &TestBase,
    lang: Lang,
    budget: &MemoryBudget,
    category: Category,
    output: &mut impl TextOutput,
) -> Result<(), BudgetError> {
    ValueWriter {
        base,
        lang,
        budget,
        category,
    }
    .write(Input::Managed(value), output, 0)
}

pub fn measure_managed_repr(
    value: &ManagedValue,
    base: &TestBase,
    lang: Lang,
    budget: &MemoryBudget,
    limit: u64,
) -> Result<u64, BudgetError> {
    let mut output = TextCount::new(Category::Row, limit);
    ValueWriter {
        base,
        lang,
        budget,
        category: Category::Execution,
    }
    .write(Input::Managed(value), &mut output, 0)?;
    Ok(output.len())
}

pub fn measure_native_repr(
    value: &QValue,
    base: &TestBase,
    lang: Lang,
    budget: &MemoryBudget,
    limit: u64,
) -> Result<u64, BudgetError> {
    let mut output = TextCount::new(Category::Row, limit);
    ValueWriter {
        base,
        lang,
        budget,
        category: Category::Execution,
    }
    .write(Input::Native(value), &mut output, 0)?;
    Ok(output.len())
}

pub fn write_native_repr(
    value: &QValue,
    base: &TestBase,
    lang: Lang,
    budget: &MemoryBudget,
    output: &mut impl TextOutput,
) -> Result<(), BudgetError> {
    ValueWriter {
        base,
        lang,
        budget,
        category: Category::Execution,
    }
    .write(Input::Native(value), output, 0)
}

#[derive(Clone, Copy)]
enum Input<'a> {
    Native(&'a QValue),
    Managed(&'a ManagedValue),
}

struct ValueWriter<'a> {
    base: &'a TestBase,
    lang: Lang,
    budget: &'a MemoryBudget,
    category: Category,
}
impl ValueWriter<'_> {
    fn write(
        &self,
        value: Input<'_>,
        output: &mut impl TextOutput,
        depth: usize,
    ) -> Result<(), BudgetError> {
        if depth >= 128 {
            return Err(BudgetError::Overflow);
        }
        let value = match value {
            Input::Native(QValue::Nested(nested)) => {
                return self.nested(
                    nested
                        .columns
                        .iter()
                        .map(|column| (column.name.as_str(), &column.ty)),
                    nested.rows.iter().map(|row| row.iter().map(Input::Native)),
                    output,
                    depth,
                );
            }
            Input::Native(value) => match value {
                QValue::Null => ValueRef::Null,
                QValue::Undefined => ValueRef::Undefined,
                QValue::Bool(value) => ValueRef::Bool(*value),
                QValue::Number(value) => ValueRef::Number(value),
                QValue::String(value) => ValueRef::String(&value.0),
                QValue::Date(value) => ValueRef::Date(*value),
                QValue::Uuid(value) => ValueRef::Uuid(value),
                QValue::Ref { table, id } => ValueRef::Ref { table: *table, id },
                QValue::Type(value) => ValueRef::Type(value),
                QValue::Binary(value) => ValueRef::Binary(value),
                QValue::Storage(value) => ValueRef::Storage(value),
                QValue::SysEnum(ty, index) => ValueRef::SysEnum(*ty, *index),
                QValue::Nested(_) => return Err(BudgetError::Overflow),
                QValue::PointInTime(_) | QValue::Boundary(_) => return Err(BudgetError::Overflow),
            },
            Input::Managed(value) => value.view(),
        };
        if let ValueRef::Nested(nested) = value {
            return self.nested(
                nested
                    .columns
                    .iter()
                    .map(|column| (column.name.as_str(), &*column.ty)),
                nested.rows.iter().map(|row| row.iter().map(Input::Managed)),
                output,
                depth,
            );
        }
        if let ValueRef::Type(ty) = value {
            output.try_push_str("Тип(")?;
            self.type_name(ty, output)?;
            return output.try_push(')');
        }
        self.scalar(value, output)
            .map_err(|_| output.error().copied().unwrap_or(BudgetError::Overflow))
    }

    fn nested<'a, C, R, I>(
        &self,
        columns: C,
        rows: R,
        output: &mut impl TextOutput,
        depth: usize,
    ) -> Result<(), BudgetError>
    where
        C: IntoIterator<Item = (&'a str, &'a QType)>,
        R: IntoIterator<Item = I>,
        I: IntoIterator<Item = Input<'a>>,
    {
        output.try_push_str("РезультатЗапроса<")?;
        for (index, (name, ty)) in columns.into_iter().enumerate() {
            if index != 0 {
                output.try_push_str("; ")?;
            }
            output.try_push_str(name)?;
            output.try_push_str(": ")?;
            write_type_repr_budgeted(
                &self.base.schema,
                ty,
                self.lang,
                self.budget,
                self.category,
                output,
                true,
            )?;
        }
        output.try_push_str(">[")?;
        for (index, row) in rows.into_iter().enumerate() {
            if index != 0 {
                output.try_push_str(", ")?;
            }
            output.try_push('(')?;
            for (index, cell) in row.into_iter().enumerate() {
                if index != 0 {
                    output.try_push_str(", ")?;
                }
                self.write(cell, output, depth + 1)?;
            }
            output.try_push(')')?;
        }
        output.try_push(']')
    }

    fn type_name(&self, ty: &QType, output: &mut impl TextOutput) -> Result<(), BudgetError> {
        if let Some(reference) = ty.refs.first() {
            return output.try_push_str(&self.base.schema.ref_info(*reference).xml_name);
        }
        let name = if ty.null {
            "Null"
        } else if ty.boolean {
            "Boolean"
        } else if ty.number.is_some() {
            "Number"
        } else if ty.string.is_some() {
            "String"
        } else if ty.date.is_some() {
            "Date"
        } else if ty.others.is_empty() {
            return output.try_push_str(match self.lang {
                Lang::Ru => "Не определено",
                Lang::En => "Undefined",
            });
        } else {
            return write_type_repr_budgeted(
                &self.base.schema,
                ty,
                self.lang,
                self.budget,
                self.category,
                output,
                false,
            );
        };
        output.try_push_str(platform_text(name, self.lang))
    }

    fn scalar(&self, value: ValueRef<'_>, output: &mut impl TextOutput) -> fmt::Result {
        match value {
            ValueRef::Undefined => output.write_str("Неопределено"),
            ValueRef::Null => output.write_str("NULL"),
            ValueRef::Bool(value) => {
                output.write_str(if value { "Истина" } else { "Ложь" })
            }
            ValueRef::Number(value) => value.write_xml(output),
            ValueRef::String(value) => {
                output.write_char('"')?;
                for value in char::decode_utf16(value.iter().copied()) {
                    let value = value.unwrap_or('?');
                    output.write_char(value)?;
                    if value == '"' {
                        output.write_char('"')?;
                    }
                }
                output.write_char('"')
            }
            ValueRef::Date(value) => {
                let parts = value.parts();
                write!(
                    output,
                    "'{:04}-{:02}-{:02}T{:02}:{:02}:{:02}'",
                    parts.year, parts.month, parts.day, parts.hour, parts.minute, parts.second
                )
            }
            ValueRef::Uuid(bytes) => {
                output.write_str("УникальныйИдентификатор(")?;
                for (index, byte) in bytes.iter().enumerate() {
                    if matches!(index, 4 | 6 | 8 | 10) {
                        output.write_char('-')?;
                    }
                    write!(output, "{byte:02x}")?;
                }
                output.write_char(')')
            }
            ValueRef::Binary(bytes) => {
                output.write_str("ДвоичныеДанные(")?;
                for byte in bytes.iter() {
                    write!(output, "{byte:02X}")?;
                }
                output.write_char(')')
            }
            ValueRef::Storage(_) => output.write_str("<ХранилищеЗначения>"),
            ValueRef::SysEnum(ty, index) => {
                let info = platform_catalog::catalog().type_info(ty);
                let value = usize::try_from(index)
                    .ok()
                    .and_then(|index| info.values.get(index))
                    .map_or("?", |value| value.name_en.as_str());
                write!(output, "{}({value})", xml_type_name(&info.name_en))
            }
            ValueRef::Ref { table, id } => {
                let Some(atom) = self.base.by_number.get(&table) else {
                    return write!(output, "Ref#{table}({})", uuid_of(id));
                };
                let info = self.base.schema.ref_info(*atom);
                output.write_str(&info.xml_name)?;
                output.write_char('(')?;
                if let Some(name) = self.base.predefined_names.get(&(table, *id)) {
                    output.write_char('@')?;
                    output.write_str(name)?;
                } else if let Some(name) = self.base.enum_names.get(&(table, *id)) {
                    output.write_str(name)?;
                } else if *id != [0; 16] || info.kind != md_model::MdKind::Enum {
                    write!(output, "{}", uuid_of(id))?;
                }
                output.write_char(')')
            }
            ValueRef::Nested(_) | ValueRef::Type(_) => Err(fmt::Error),
        }
    }
}
