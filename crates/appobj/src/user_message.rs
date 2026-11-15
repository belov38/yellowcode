use crate::meta::member_name;
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, Registry,
    RetainedMemory, TypeId, TypeIdx, TypeImpl, UserMessage, Value, creation_number,
};
use platform_catalog::catalog;
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

const TYPE: &str = "СообщениеПользователю";

#[derive(Default)]
struct State {
    text: Option<Value>,
    field: Option<Value>,
    data_path: Option<Value>,
    data_key: Option<Value>,
    data: Option<Value>,
}

struct Message {
    ty: TypeIdx,
    state: RefCell<State>,
    hash: i32,
}

impl Message {
    fn refuse(&self, member: &str) -> HostError {
        HostError::not_implemented(TYPE, member)
    }

    fn name(&self, m: MemberId) -> Option<(bool, &'static str, &'static str)> {
        member_name(catalog(), self.ty, m)
    }
}

fn text_of(v: &Option<Value>) -> Value {
    v.clone().unwrap_or_else(|| Value::String(Str::from("")))
}

impl HostObject for Message {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn boost_hash(&self) -> Option<i32> {
        Some(self.hash)
    }

    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        visit.work(1)?;
        visit.mutable();
        let st = self.state.borrow();
        for v in [&st.text, &st.field, &st.data_path, &st.data_key, &st.data]
            .into_iter()
            .flatten()
        {
            visit.value(v)?;
        }
        Ok(true)
    }

    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        let st = self.state.borrow();
        match self.name(m) {
            Some((false, _, "Text")) => Ok(text_of(&st.text)),
            Some((false, _, "Field")) => Ok(text_of(&st.field)),
            Some((false, _, "DataPath")) => Ok(text_of(&st.data_path)),
            Some((false, _, "DataKey")) => Ok(st.data_key.clone().unwrap_or(Value::Undefined)),
            Some((_, ru, _)) => Err(self.refuse(ru).into()),
            None => Err(self.refuse(&m.0.to_string()).into()),
        }
    }

    fn set(&self, _: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        let name = self.name(m);
        let string = |v: Value, ru: &str| -> CallResult<Value> {
            match v {
                Value::String(_) => Ok(v),
                _ => Err(self.refuse(&format!("{ru}: значение не строка")).into()),
            }
        };
        let mut st = self.state.borrow_mut();
        match name {
            Some((false, ru, "Text")) => st.text = Some(string(v, ru)?),
            Some((false, ru, "Field")) => st.field = Some(string(v, ru)?),
            Some((false, ru, "DataPath")) => st.data_path = Some(string(v, ru)?),
            Some((false, _, "DataKey")) => st.data_key = Some(v),
            Some((_, ru, _)) => return Err(self.refuse(ru).into()),
            None => return Err(self.refuse(&m.0.to_string()).into()),
        }
        Ok(())
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match self.name(m) {
            Some((true, _, "SetData")) => {
                let v = crate::meta::arg_value(ex, args, 0)?;
                self.state.borrow_mut().data = Some(v);
                Ok(Value::Undefined)
            }
            Some((true, ru, "Message")) => {
                let (text, has_data) = {
                    let st = self.state.borrow();
                    let text = match text_of(&st.text) {
                        Value::String(s) => s.to_string_lossy(),
                        _ => String::new(),
                    };
                    (text, st.data.is_some())
                };
                if crate::events::in_fill_check() {
                    if !has_data {
                        return Err(self
                            .refuse(&format!("{ru}: без данных в проверке заполнения"))
                            .into());
                    }
                    crate::events::fail_fill_check();
                }
                ex.push_message(UserMessage { text, status: None });
                Ok(Value::Undefined)
            }
            Some((_, ru, _)) => Err(self.refuse(ru).into()),
            None => Err(self.refuse(&m.0.to_string()).into()),
        }
    }
}

pub(crate) fn register(reg: &mut Registry) {
    let Some(ty) = catalog().find_type(TYPE) else {
        return;
    };
    reg.type_impl(
        ty,
        TypeImpl {
            ctor: Some(Arc::new(move |_, _| {
                Ok(Value::Object(Rc::new(Message {
                    ty,
                    state: RefCell::new(State::default()),
                    hash: creation_number() as i32,
                })))
            })),
        },
    );
}
