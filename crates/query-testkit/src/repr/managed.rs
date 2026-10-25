use super::{platform_text, presentation_text};
use memory_budget::{
    BudgetError, BudgetedText, BudgetedVec, Category, Charged, MemoryBudget, TextOutput,
};
use query_ir::schema::atoms::TypeAtom;
use query_ir::{DateQ, Other, QType, QuerySchema};
use query_syntax::Lang;
use std::fmt;

pub fn type_repr_budgeted(
    schema: &QuerySchema,
    ty: &QType,
    lang: Lang,
    budget: &MemoryBudget,
    category: Category,
    limit: u64,
) -> Result<Charged<String>, BudgetError> {
    let mut output = BudgetedText::new(budget, category, limit)?;
    write_type_repr_budgeted(schema, ty, lang, budget, category, &mut output, true)?;
    output.into_string()
}

pub fn write_type_repr_budgeted(
    schema: &QuerySchema,
    ty: &QType,
    lang: Lang,
    budget: &MemoryBudget,
    category: Category,
    output: &mut impl TextOutput,
    wrapped: bool,
) -> Result<(), BudgetError> {
    const INLINE: usize = 8;
    let mut count = 0usize;
    schema.try_visit_type_atoms(ty, |_, _| {
        count += 1;
        Ok::<(), BudgetError>(())
    })?;
    if count <= INLINE {
        let mut atoms = [None; INLINE];
        let mut filled = 0;
        schema.try_visit_type_atoms(ty, |uuid, atom| {
            let slot = atoms.get_mut(filled).ok_or(BudgetError::Overflow)?;
            *slot = Some((uuid, filled, atom));
            filled += 1;
            Ok::<(), BudgetError>(())
        })?;
        let atoms = &mut atoms[..filled];
        atoms.sort_unstable_by_key(|atom| atom.map(|(uuid, index, _)| (uuid, index)));
        write_atoms(
            schema,
            lang,
            output,
            wrapped,
            atoms.iter().flatten().map(|(_, _, value)| *value),
        )
    } else {
        let mut atoms = BudgetedVec::try_with_capacity(budget, category, count)?;
        schema.try_visit_type_atoms(ty, |uuid, atom| {
            if atoms.len() == atoms.capacity() {
                return Err(BudgetError::Overflow);
            }
            atoms.try_push((uuid, atoms.len(), atom))
        })?;
        atoms
            .as_mut_slice()
            .sort_unstable_by_key(|(uuid, index, _)| (*uuid, *index));
        write_atoms(
            schema,
            lang,
            output,
            wrapped,
            atoms.iter().map(|(_, _, value)| *value),
        )
    }
}

fn write_atoms(
    schema: &QuerySchema,
    lang: Lang,
    output: &mut impl TextOutput,
    wrapped: bool,
    atoms: impl Iterator<Item = TypeAtom>,
) -> Result<(), BudgetError> {
    let result = (|| {
        if wrapped {
            output.write_str("ОписаниеТипов(")?;
        }
        for (index, value) in atoms.enumerate() {
            if index != 0 {
                output.write_str(", ")?;
            }
            write_atom(output, schema, value, lang)?;
        }
        if wrapped {
            output.write_char(')')?;
        }
        Ok::<_, fmt::Error>(())
    })();
    result.map_err(|_| output.error().copied().unwrap_or(BudgetError::Overflow))
}

fn pick<'a>(lang: Lang, ru: &'a str, en: &'a str) -> &'a str {
    match lang {
        Lang::Ru => ru,
        Lang::En => en,
    }
}

fn write_atom(
    output: &mut impl TextOutput,
    schema: &QuerySchema,
    atom: TypeAtom,
    lang: Lang,
) -> fmt::Result {
    match atom {
        TypeAtom::Null => output.write_str(platform_text("Null", lang)),
        TypeAtom::Boolean => output.write_str(platform_text("Boolean", lang)),
        TypeAtom::Number(value) => write!(
            output,
            "Число({},{}{})",
            value.digits,
            value.scale,
            if value.nonneg {
                ",Неотрицательный"
            } else {
                ""
            }
        ),
        TypeAtom::String(value) => write!(
            output,
            "Строка({},{})",
            value.len,
            if value.fixed {
                pick(lang, "Фиксированная", "Fixed-length")
            } else {
                pick(lang, "Переменная", "Variable-length")
            }
        ),
        TypeAtom::Date(value) => write!(
            output,
            "Дата({})",
            match value {
                DateQ::Date => pick(lang, "Дата", "Date"),
                DateQ::Time => pick(lang, "Время", "Time"),
                DateQ::DateTime => pick(lang, "Дата и время", "Date and time"),
            }
        ),
        TypeAtom::Reference(value) => output.write_str(&schema.ref_info(value).xml_name),
        TypeAtom::Other(Other::RoutePoint(value)) => {
            let name = &schema.ref_info(value).xml_name;
            if let Some((before, after)) = name.split_once("BusinessProcessRef") {
                output.write_str(before)?;
                output.write_str("BusinessProcessRoutePointRef")?;
                output.write_str(after)
            } else {
                output.write_str(name)
            }
        }
        TypeAtom::Other(Other::SysEnum(index)) => output.write_str(presentation_text(
            platform_catalog::catalog().type_info(index),
            lang,
        )),
        TypeAtom::Other(value) => output.write_str(platform_text(
            match value {
                Other::Uuid => "UUID",
                Other::Binary => "BinaryData",
                Other::Storage => "ValueStorage",
                Other::Type => "Type",
                Other::TypeDescription => "TypeDescription",
                Other::PointInTime => "PointInTime",
                Other::Boundary => "Boundary",
                Other::QueryResult => "QueryResult",
                Other::ValueTable => "ValueTable",
                Other::RoutePoint(_) | Other::SysEnum(_) => "",
            },
            lang,
        )),
    }
}
