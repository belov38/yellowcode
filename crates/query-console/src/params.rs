use crate::Console;
use crate::run::{ErrorOut, Phase};
use query_ir::{Other, QType, QValue, uuid_of};
use query_syntax::{Lang, ParseMode};
use query_testhost::repr_lang;
use serde::{Deserialize, Serialize};

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
    pub fn parameters(&self, text: &str) -> Result<Vec<ParamInfoOut>, ErrorOut> {
        let schema = self.base.schema();
        let fail = |e: &query_ir::CompileError| {
            let script = schema.script();
            let mut chain = vec![e.render(text, Lang::Ru, script)];
            let mut c = e.cause.as_deref();
            while let Some(x) = c {
                chain.push(x.message(Lang::Ru, script));
                c = x.cause.as_deref();
            }
            ErrorOut {
                phase: Phase::Compile,
                chain,
                at: e.at.map(|a| (a.pos.line, a.pos.col)),
            }
        };
        let batch =
            query_ir::compile_batch(text, ParseMode::Query, schema).map_err(|e| fail(&e))?;
        let found = query_ir::find_parameters(&batch, text, schema, &query_ir::NoTempTables)
            .map_err(|e| fail(&e))?;
        Ok(found
            .iter()
            .map(|p| ParamInfoOut {
                name: p.name.clone(),
                ty: query_testkit::repr::type_repr(schema, &p.ty, Lang::Ru),
                kind: param_kind(&p.ty),
                refs: p
                    .ty
                    .refs
                    .iter()
                    .map(|r| schema.ref_info(*r).xml_name.clone())
                    .collect(),
            })
            .collect())
    }

    pub fn ref_choices(&self, kind: &str, limit: usize) -> Vec<Choice> {
        let base = &*self.base;
        let Some((number, _)) = base.ref_kinds().into_iter().find(|(_, n)| *n == kind) else {
            return Vec::new();
        };
        base.named_refs(number)
            .into_iter()
            .take(limit.min(MAX_CHOICES))
            .map(|(id, _)| {
                let v = QValue::Ref { table: number, id };
                Choice {
                    id: uuid_of(&id).to_string(),
                    text: repr_lang(&v, base, Lang::Ru),
                }
            })
            .collect()
    }
}
