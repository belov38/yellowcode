use crate::serial::codec::InternalWriter;
use brace::{Newline, to_text_with};
use bsl_vm::{Exception, Execution, Value};

pub fn value_to_internal(ex: &mut Execution, v: &Value) -> Result<String, Exception> {
    let mut w = InternalWriter::new();
    let node = w.write_value(ex, v)?;
    ex.reserve(w.estimate().saturating_mul(2))?;
    Ok(to_text_with(&node, Newline::Lf))
}
