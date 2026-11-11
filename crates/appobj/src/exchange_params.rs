use crate::meta::member_name;
use crate::values::refs::as_ref;
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, RetainedMemory, TypeId,
    Value,
};
use platform_catalog::{TypeIdx, catalog};
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const PARAMETERS: &str = "ПараметрыОбменаДанными";
const NODE_SET: &str = "НаборУзлов";

pub(crate) struct ExchangeParams {
    ty: TypeIdx,
    load: Cell<bool>,
    sender: RefCell<Value>,
    recipients: RefCell<Option<Value>>,
}

impl ExchangeParams {
    pub(crate) fn new() -> Result<Rc<ExchangeParams>, HostError> {
        let ty = catalog()
            .find_type(PARAMETERS)
            .ok_or_else(|| HostError::not_implemented("СправочникОбъект", "ОбменДанными"))?;
        Ok(Rc::new(ExchangeParams {
            ty,
            load: Cell::new(false),
            sender: RefCell::new(Value::Undefined),
            recipients: RefCell::new(None),
        }))
    }

    pub(crate) fn loading(&self) -> bool {
        self.load.get()
    }

    fn property(&self, m: MemberId) -> Option<&'static str> {
        match member_name(catalog(), self.ty, m) {
            Some((false, _, "Load")) => Some("Load"),
            Some((false, _, "Sender")) => Some("Sender"),
            Some((false, _, "Recipients")) => Some("Recipients"),
            _ => None,
        }
    }

    fn not_implemented(member: &str) -> HostError {
        HostError::not_implemented(PARAMETERS, member)
    }
}

impl HostObject for ExchangeParams {
    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        visit.work(1)?;
        visit.mutable();
        visit.value(&self.sender.borrow())?;
        if let Some(v) = self.recipients.borrow().as_ref() {
            visit.value(v)?;
        }
        Ok(true)
    }

    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        match self.property(m) {
            Some("Load") => Ok(Value::Bool(self.load.get())),
            Some("Sender") => Ok(self.sender.borrow().clone()),
            Some("Recipients") => {
                if let Some(v) = self.recipients.borrow().as_ref() {
                    return Ok(v.clone());
                }
                let ty = catalog()
                    .find_type(NODE_SET)
                    .ok_or_else(|| Self::not_implemented("Получатели"))?;
                let set = Value::Object(Rc::new(NodeSet { ty }));
                *self.recipients.borrow_mut() = Some(set.clone());
                Ok(set)
            }
            _ => Err(Self::not_implemented(&m.0.to_string()).into()),
        }
    }

    fn set(&self, _: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        match self.property(m) {
            Some("Load") => match v {
                Value::Bool(b) => {
                    self.load.set(b);
                    Ok(())
                }
                _ => Err(Self::not_implemented("Загрузка: значение не Булево").into()),
            },
            Some("Sender") => {
                let node = matches!(v, Value::Undefined) || as_ref(&v).is_some();
                if !node {
                    return Err(Self::not_implemented("Отправитель: значение").into());
                }
                *self.sender.borrow_mut() = v;
                Ok(())
            }
            _ => Err(crate::objtab::not_writable(
                &member_name(catalog(), self.ty, m).map_or_else(String::new, |n| n.1.to_string()),
            )
            .into()),
        }
    }

    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(Self::not_implemented(&m.0.to_string()).into())
    }
}

struct NodeSet {
    ty: TypeIdx,
}

impl HostObject for NodeSet {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn retained_memory(&self, visit: &mut dyn RetainedMemory) -> Result<bool, Exception> {
        visit.work(1)?;
        Ok(true)
    }

    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(HostError::not_implemented(NODE_SET, &m.0.to_string()).into())
    }

    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(HostError::not_implemented(NODE_SET, &m.0.to_string()).into())
    }

    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        let name =
            member_name(catalog(), self.ty, m).map_or_else(|| m.0.to_string(), |n| n.1.to_string());
        Err(HostError::not_implemented(NODE_SET, &name).into())
    }
}
