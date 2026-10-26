use crate::bind::{arg_value, global, is_given};
use bsl_vm::{Registry, UserMessage, Value};

pub(crate) fn register(reg: &mut Registry) {
    global(reg, "Сообщить", "Message", |ex, args| {
        let text = arg_value(ex, args.first())?;
        let text = ex.present(&text)?.to_string_lossy();
        let status = if is_given(args.get(1)) {
            let s = arg_value(ex, args.get(1))?;
            Some(ex.present(&s)?.to_string_lossy())
        } else {
            None
        };
        ex.push_message(UserMessage { text, status });
        Ok(Value::Undefined)
    });
}
