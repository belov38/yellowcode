use super::Analyzer;
use crate::CompileError;
use query_syntax::ast::{self, BinOp, ExprKind, Ident, Path};
use query_syntax::{MsgArg, MsgId, QueryError, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Frame {
    Compare { op: u32 },
    Subquery,
    Barrier,
}

pub(crate) fn bare_name_at(frames: &[Frame], name: Span) -> u32 {
    let mut op = None;
    for f in frames.iter().rev().take_while(|f| **f != Frame::Subquery) {
        match f {
            Frame::Barrier => return name.start,
            Frame::Compare { op: o } => {
                op.get_or_insert(*o);
            }
            Frame::Subquery => {}
        }
    }
    op.unwrap_or(name.start)
}

pub(crate) fn root_at(e: &ast::Expr) -> u32 {
    match &e.kind {
        ExprKind::Compare { op_span, .. } => op_span.start,
        _ => e.span.start,
    }
}

pub(crate) fn elem_at(e: &ast::Expr) -> u32 {
    match &e.kind {
        ExprKind::Chain { rest, .. } => rest
            .first()
            .filter(|o| !matches!(o.op, BinOp::And | BinOp::Or))
            .map_or(e.span.start, |o| o.op_span.start),
        _ => root_at(e),
    }
}

pub(crate) fn path_prefix(parts: &[Ident], last: usize) -> String {
    let mut out = String::new();
    for (i, p) in parts.iter().take(last.saturating_add(1)).enumerate() {
        if i > 0 {
            out.push('.');
        }
        out.push_str(&p.text);
    }
    out
}

impl Analyzer<'_> {
    pub(crate) fn err(&self, msg: MsgId, offset: u32) -> CompileError {
        QueryError::new(msg, Some(self.map.at(self.text, offset)))
    }

    pub(crate) fn message_error(&self, msg: MsgId, offset: u32) -> CompileError {
        if let Some(memory) = self.memory {
            return memory.semantic_error(msg, Some(self.map.at(self.text, offset)), None);
        }
        self.err(msg, offset)
    }

    pub(crate) fn quoted_argument_error(
        &self,
        msg: MsgId,
        argument: crate::managed::Argument<'_>,
        offset: u32,
    ) -> CompileError {
        if let Some(memory) = self.memory {
            return memory.semantic_error(
                msg,
                Some(self.map.at(self.text, offset)),
                Some(argument),
            );
        }
        self.err(msg, offset).with_quoted(argument.legacy())
    }

    pub(crate) fn quoted_text_error(&self, msg: MsgId, text: &str, offset: u32) -> CompileError {
        self.quoted_argument_error(msg, crate::managed::Argument::Text(text), offset)
    }

    pub(crate) fn quoted_path_error(&self, msg: MsgId, path: &Path, offset: u32) -> CompileError {
        if let Some(memory) = self.memory {
            return memory.quoted_parts_error(
                msg,
                self.map.at(self.text, offset),
                path.parts.iter().map(|part| &*part.text),
                ".",
            );
        }
        self.quoted_text_error(msg, &path.dotted(), offset)
    }

    pub(crate) fn field_not_found(&self, quoted: &str, offset: u32) -> CompileError {
        if let Some(memory) = self.memory {
            return memory.quoted_error(
                MsgId::SELECT_ERROR_FIELDNOTFOUND,
                self.map.at(self.text, offset),
                quoted,
            );
        }
        self.err(MsgId::SELECT_ERROR_FIELDNOTFOUND, offset)
            .with_quoted(MsgArg::text(quoted))
    }

    pub(crate) fn table_not_found(&self, path: &Path) -> CompileError {
        if let Some(memory) = self.memory {
            return memory.quoted_parts_error(
                MsgId::SELECT_ERROR_TABLENOTFOUND,
                self.map.at(self.text, path.span().start),
                path.parts.iter().map(|part| &*part.text),
                ".",
            );
        }
        self.err(MsgId::SELECT_ERROR_TABLENOTFOUND, path.span().start)
            .with_quoted(MsgArg::text(&path.dotted()))
    }

    pub(crate) fn ambiguous_field(&self, name: &Ident) -> CompileError {
        if let Some(memory) = self.memory {
            return memory.quoted_error(
                MsgId::SELECT_ERROR_AMBIGOUSFIELD,
                self.map.at(self.text, name.span.start),
                &name.text,
            );
        }
        self.err(MsgId::SELECT_ERROR_AMBIGOUSFIELD, name.span.start)
            .with_quoted(MsgArg::text(&name.text))
    }

    pub(crate) fn duplicated_alias(&self, alias: &str, offset: u32) -> CompileError {
        if let Some(memory) = self.memory {
            return memory.quoted_error(
                MsgId::SELECT_ERROR_DUPLICATEDALIAS,
                self.map.at(self.text, offset),
                alias,
            );
        }
        self.err(MsgId::SELECT_ERROR_DUPLICATEDALIAS, offset)
            .with_quoted(MsgArg::text(alias))
    }

    pub(crate) fn unsupported(&self, what: &str, offset: u32) -> CompileError {
        if let Some(memory) = self.memory {
            return memory.quoted_parts_error(
                MsgId::SELECT_ERROR_SYNTAXERROR,
                self.map.at(self.text, offset),
                ["internal: ", what],
                "",
            );
        }
        self.err(MsgId::SELECT_ERROR_SYNTAXERROR, offset)
            .with_quoted(MsgArg::text(&format!("internal: {what}")))
    }
}
