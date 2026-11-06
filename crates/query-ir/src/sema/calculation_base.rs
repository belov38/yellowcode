use super::{AnalyzeMode, Analyzer, R};
use crate::CompileError;
use crate::managed::{CompileArena, NameKey};
use crate::params::ParamConst;
use crate::schema::{FieldRole, QTableId};
use crate::value::QValue;
use query_syntax::ast::{self, ExprKind};
use query_syntax::{MsgArg, MsgId, QueryError, ScriptVariant};

fn parameter_identifier(memory: Option<&CompileArena>, caption: &str) -> R<NameKey> {
    let mut name = NameKey::raw(memory, "")?;
    let mut capital = true;
    for ch in caption.chars() {
        if ch == ' ' {
            capital = true;
            continue;
        }
        let mut encoded = [0; 4];
        if capital {
            for upper in ch.to_uppercase() {
                name.push_str(memory, upper.encode_utf8(&mut encoded))?;
            }
        } else {
            name.push_str(memory, ch.encode_utf8(&mut encoded))?;
        }
        capital = false;
    }
    Ok(name)
}

impl Analyzer<'_> {
    pub(super) fn base_main_dimensions(
        &mut self,
        table: QTableId,
        path: &ast::Path,
        args: Option<&ast::VtArgs>,
    ) -> R<()> {
        if self.opts.mode != AnalyzeMode::Execute {
            return Ok(());
        }
        let first = args.and_then(|a| a.args.first()).and_then(Option::as_ref);
        let invalid = if let Some(arg) = first {
            let ExprKind::Param(parameter) = &arg.expr.kind else {
                return Ok(());
            };
            let constant = self.params.constant(&parameter.name.text)?;
            let Some(value) = constant.as_deref() else {
                return Ok(());
            };
            let mut invalid = None;
            if let ParamConst::List(values) = value
                && values.iter().all(|v| matches!(v, QValue::String(_)))
                && let Some(owner) = self.schema.table(table).owner
            {
                for (index, value) in values.iter().enumerate() {
                    let QValue::String(value) = value else {
                        continue;
                    };
                    let mut text = NameKey::raw(self.memory, "")?;
                    for ch in char::decode_utf16(value.0.iter().copied()) {
                        let mut encoded = [0; 4];
                        text.push_str(
                            self.memory,
                            ch.unwrap_or(char::REPLACEMENT_CHARACTER)
                                .encode_utf8(&mut encoded),
                        )?;
                    }
                    let key = NameKey::folded(self.memory, &text)?;
                    let dimension = self
                        .schema
                        .find_field_folded(owner, &key)
                        .and_then(|f| self.schema.field(owner, f))
                        .is_some_and(|f| f.role == FieldRole::Dimension);
                    if !dimension {
                        invalid = Some(index);
                        break;
                    }
                }
            }
            drop(constant);
            self.note_value_dep(&parameter.name.text)?;
            let Some(index) = invalid else { return Ok(()) };
            Some(index)
        } else {
            None
        };
        let Some(parameter) = self.schema.table(table).params.first() else {
            return Ok(());
        };
        let caption = match self.schema.script() {
            ScriptVariant::Russian => &parameter.name_ru,
            ScriptVariant::English => &parameter.name_en,
        };
        let parameter = parameter_identifier(self.memory, caption)?;
        Err(self.base_main_dimensions_error(path, &parameter, invalid)?)
    }

    fn base_main_dimensions_error(
        &self,
        path: &ast::Path,
        parameter: &str,
        invalid: Option<usize>,
    ) -> R<CompileError> {
        let cause = if invalid.is_some() {
            MsgId::MAIN_REGISTER_DIMS_PARAMETER_VALUE_MISMATCH
        } else {
            MsgId::MAIN_REGISTER_DIMS_PARAMETER_UNDEFINED
        };
        let number = invalid
            .map(|index| NameKey::formatted(self.memory, |out| write!(out, "{index}")))
            .transpose()?;
        let arguments = std::iter::once(parameter).chain(number.as_ref().map(NameKey::as_str));
        if let Some(memory) = self.memory {
            return Ok(memory.quoted_parts_cause_error(
                MsgId::SELECT_ERROR_ERRORPROCESSINGVIEW,
                Some(self.map.at(self.text, path.span().start)),
                path.parts.iter().map(|part| &*part.text),
                ".",
                cause,
                arguments,
            ));
        }
        let mut cause = QueryError::new(cause, None);
        for argument in arguments {
            cause = cause.with_subst(MsgArg::text(argument));
        }
        Ok(self
            .err(MsgId::SELECT_ERROR_ERRORPROCESSINGVIEW, path.span().start)
            .with_quoted(MsgArg::text(&path.dotted()))
            .with_cause(cause))
    }
}
