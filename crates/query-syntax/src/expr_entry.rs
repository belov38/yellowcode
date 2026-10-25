use crate::ast::{Expr, ExprKind, Lit};
use crate::source::{SourceMap, Span};
use crate::{INLINE_DEPTH, ParseMode, SyntaxError, with_deep_stack};

#[derive(Debug)]
pub struct ExprTree {
    pub expr: Expr,
    pub map: SourceMap,
    pub max_depth: u32,
}

pub fn parse_expr(text: &str, mode: ParseMode) -> Result<ExprTree, SyntaxError> {
    crate::parse_with(text, mode, |mut p| {
        let r = p.expr().and_then(|e| {
            if p.at_eof() {
                Ok(e)
            } else {
                Err(p.unexpected())
            }
        });
        let crate::parser::Parser { map, max_depth, .. } = p;
        (
            r.map(|expr| ExprTree {
                expr,
                map,
                max_depth,
            }),
            Vec::new(),
        )
    })
    .0
}

impl Drop for ExprTree {
    fn drop(&mut self) {
        if self.max_depth > INLINE_DEPTH {
            let null = Expr {
                kind: ExprKind::Lit(Lit::Null),
                span: Span::new(0, 0),
            };
            let e = std::mem::replace(&mut self.expr, null);
            with_deep_stack(self.max_depth, move || drop(e));
        }
    }
}
