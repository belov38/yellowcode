use super::{PResult, Parser, reserved_prefix};
use crate::ast::AccessForm;
use crate::diag::MsgArg;
use crate::kw::Kw;
use crate::lexer::TokKind;
use crate::msg::MsgId;

impl Parser<'_> {
    pub fn condition(&mut self) -> PResult<AccessForm> {
        let form = match self.tok().kind {
            TokKind::Word { kws, .. } if kws.contains(Kw::Where) => {
                let where_kw = self.bump()?.span;
                let cond = self.expr()?;
                AccessForm::Short { where_kw, cond }
            }
            TokKind::Word { kws, .. } if !reserved_prefix(kws) => {
                let alias = self.word_ident()?;
                if !self.at_kw(Kw::From) {
                    return Err(self.unexpected());
                }
                let from = self.from_clause()?;
                let where_ = self.clause(Kw::Where)?;
                AccessForm::Full {
                    alias,
                    from,
                    where_,
                }
            }
            _ => {
                return Err(self
                    .err_here(MsgId::SELECT_ERROR_TOKENEXPECTED)
                    .with_quoted(MsgArg::Kw(Kw::Where)));
            }
        };
        if !self.at_eof() {
            return Err(self.unexpected());
        }
        Ok(form)
    }
}
