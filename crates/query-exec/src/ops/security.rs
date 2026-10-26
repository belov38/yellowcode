use super::{BoxOp, Cx, Op};
use crate::source::Row;
use query_ir::{ExecError, QValue, RuntimeError};
use query_syntax::{MsgId, QueryError};

pub(crate) struct CheckAccess<'a> {
    input: BoxOp<'a>,
    signal: u16,
}
impl<'a> CheckAccess<'a> {
    pub(crate) fn new(input: BoxOp<'a>, signal: u16) -> Self {
        Self { input, signal }
    }
}
impl<'a> Op<'a> for CheckAccess<'a> {
    fn close(&mut self, cx: &mut Cx<'a>) {
        self.input.close(cx);
    }
    fn next(&mut self, cx: &mut Cx<'a>) -> Result<Option<Row>, ExecError> {
        let row = self.input.next(cx)?;
        if let Some(row) = &row {
            cx.tick()?;
            match row.get(usize::from(self.signal)) {
                Some(QValue::Bool(true) | QValue::Null) => {}
                Some(QValue::Bool(false)) => {
                    return Err(ExecError::Rights(RuntimeError {
                        chain: vec![
                            QueryError::new(MsgId::SELECT_ERROR_RUNTIMEERROR, None),
                            QueryError::new(MsgId::DBERROR_ACCESSDENIED, None),
                        ],
                    }));
                }
                _ => return Err(super::fatal("invalid hidden access signal")),
            }
        }
        Ok(row)
    }
}
