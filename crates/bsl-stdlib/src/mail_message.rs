use crate::bind::arg_value;
use crate::collections::{
    ctor_not_implemented, method_name, not_implemented, property_name, type_name_ru,
};
use crate::enums::{as_enum_value, enum_value, type_by_id};
use crate::serial::codec::{InternalReader, InternalWriter, ValueCodec};
use brace::Node;
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, ObjRef, Registry,
    TypeId, TypeImpl, Value,
};
use platform_catalog::TypeIdx;
use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

struct Types {
    message: TypeIdx,
    addresses: TypeIdx,
    address: TypeIdx,
    texts: TypeIdx,
    text: TypeIdx,
    attachments: TypeIdx,
    text_type: TypeIdx,
}

fn types() -> Option<&'static Types> {
    static T: OnceLock<Option<Types>> = OnceLock::new();
    T.get_or_init(|| {
        Some(Types {
            message: type_by_id("InternetMailMessage")?,
            addresses: type_by_id("InternetMailAddresses")?,
            address: type_by_id("InternetMailAddress")?,
            texts: type_by_id("InternetMailTexts")?,
            text: type_by_id("InternetMailText")?,
            attachments: type_by_id("InternetMailAttachments")?,
            text_type: type_by_id("InternetMailTextType")?,
        })
    })
    .as_ref()
}

fn refuse(t: TypeIdx, what: &str) -> HostError {
    HostError::not_implemented(type_name_ru(t), what)
}

struct Message {
    t: &'static Types,
    subject: RefCell<Str>,
    sender: RefCell<Value>,
    recipients: Value,
    texts: Value,
    attachments: Value,
}

impl HostObject for Message {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.t.message)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        match property_name(self.t.message, m) {
            Some("Subject") => Ok(Value::String(self.subject.borrow().clone())),
            Some("From") => Ok(self.sender.borrow().clone()),
            Some("To") => Ok(self.recipients.clone()),
            Some("Texts") => Ok(self.texts.clone()),
            Some("Attachments") => Ok(self.attachments.clone()),
            _ => Err(not_implemented(self.t.message, m).into()),
        }
    }
    fn set(&self, _: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        match (property_name(self.t.message, m), v) {
            (Some("Subject"), Value::String(s)) => {
                *self.subject.borrow_mut() = s;
                Ok(())
            }
            (Some("From"), Value::String(s))
                if !s.is_empty() && s.to_string_lossy().chars().all(char::is_alphanumeric) =>
            {
                *self.sender.borrow_mut() = new_address(self.t, true);
                Ok(())
            }
            _ => Err(not_implemented(self.t.message, m).into()),
        }
    }
    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(self.t.message, m).into())
    }
    fn presentation(&self, _: &mut Execution) -> CallResult<Option<bsl_prim::Str>> {
        Err(refuse(self.t.message, "Строка").into())
    }
}

fn new_message(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    let Some(t) = types() else {
        return Err(HostError::not_implemented("ИнтернетПочтовоеСообщение", "Новый").into());
    };
    if !args.is_empty() {
        return Err(ctor_not_implemented(t.message).into());
    }
    ex.charge(1)?;
    let coll = |ct: TypeIdx| -> Value {
        Value::Object(Rc::new(Collection {
            t,
            ct,
            items: RefCell::new(Vec::new()),
        }))
    };
    Ok(Value::Object(Rc::new(Message {
        t,
        subject: RefCell::new(Str::from("")),
        sender: RefCell::new(new_address(t, false)),
        recipients: coll(t.addresses),
        texts: coll(t.texts),
        attachments: coll(t.attachments),
    })))
}

struct Address {
    t: &'static Types,
    observed: bool,
}

fn new_address(t: &'static Types, observed: bool) -> Value {
    Value::Object(Rc::new(Address { t, observed }))
}

impl HostObject for Address {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.t.address)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        match property_name(self.t.address, m) {
            Some("Address" | "DisplayName") if self.observed => Ok(Value::from("")),
            _ => Err(not_implemented(self.t.address, m).into()),
        }
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(not_implemented(self.t.address, m).into())
    }
    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(self.t.address, m).into())
    }
    fn presentation(&self, _: &mut Execution) -> CallResult<Option<Str>> {
        if self.observed {
            return Ok(Some(Str::from(type_name_ru(self.t.address))));
        }
        Err(refuse(self.t.address, "Строка").into())
    }
}

struct Collection {
    t: &'static Types,
    ct: TypeIdx,
    items: RefCell<Vec<Value>>,
}

impl Collection {
    fn add_text(&self, ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
        if args.iter().skip(1).any(|a| !matches!(a, Arg::Omitted)) {
            return Err(refuse(self.ct, "Добавить: тип текста").into());
        }
        let Value::String(s) = arg_value(ex, args.first())? else {
            return Err(refuse(self.ct, "Добавить: значение не строка").into());
        };
        let units = s.units();
        let lf_unit = u16::from(b'\n');
        if units.contains(&u16::from(b'\r')) {
            return Err(refuse(self.ct, "Добавить: текст с возвратом каретки").into());
        }
        if units.contains(&lf_unit) {
            let text = s.to_string_lossy();
            let html = text.starts_with(crate::serial::opaque::HTML_HEAD)
                && text.ends_with(crate::serial::opaque::HTML_TAIL);
            if !html || units.windows(2).any(|w| w[0] == lf_unit && w[1] == lf_unit) {
                return Err(
                    refuse(self.ct, "Добавить: многострочный текст не HTML документа").into(),
                );
            }
        }
        let lf = units.iter().filter(|u| **u == u16::from(b'\n')).count();
        ex.reserve(
            (units.len() as u64)
                .saturating_add(lf as u64)
                .saturating_mul(2),
        )?;
        let mut out = Vec::with_capacity(units.len() + lf);
        for &u in units {
            if u == u16::from(b'\n') {
                out.push(u16::from(b'\r'));
            }
            out.push(u);
        }
        let text = Value::Object(Rc::new(Text {
            t: self.t,
            text: Str::from_vec(out),
            html: RefCell::new(false),
        }));
        self.items.borrow_mut().push(text.clone());
        Ok(text)
    }
}

impl HostObject for Collection {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ct)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        Err(not_implemented(self.ct, m).into())
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        Err(not_implemented(self.ct, m).into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match method_name(self.ct, m) {
            Some("Count") => Ok(Value::from(self.items.borrow().len() as i64)),
            Some("Add") if self.ct == self.t.texts => self.add_text(ex, args),
            _ => Err(not_implemented(self.ct, m).into()),
        }
    }
    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(self.items.borrow().len())
    }
    fn iter_keys(&self, _: &mut Execution) -> CallResult<Option<Vec<Value>>> {
        Err(refuse(self.ct, "Для Каждого").into())
    }
    fn iter_get(&self, _: &mut Execution, _: usize) -> CallResult<Value> {
        Err(refuse(self.ct, "Для Каждого").into())
    }
    fn index_get(&self, _: &mut Execution, key: &Value) -> CallResult<Value> {
        let item = match key {
            Value::Number(n) => n
                .to_i64()
                .and_then(|i| usize::try_from(i).ok())
                .and_then(|i| self.items.borrow().get(i).cloned()),
            _ => None,
        };
        item.ok_or_else(|| refuse(self.ct, "Получить по индексу").into())
    }
    fn presentation(&self, _: &mut Execution) -> CallResult<Option<Str>> {
        Err(refuse(self.ct, "Строка").into())
    }
}

struct Text {
    t: &'static Types,
    text: Str,
    html: RefCell<bool>,
}

impl HostObject for Text {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.t.text)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        match property_name(self.t.text, m) {
            Some("Text") => Ok(Value::String(self.text.clone())),
            Some("TextType") => {
                let name = if *self.html.borrow() {
                    "HTML"
                } else {
                    "PlainText"
                };
                enum_value(self.t.text_type, name)
                    .ok_or_else(|| refuse(self.t.text, "ТипТекста").into())
            }
            _ => Err(not_implemented(self.t.text, m).into()),
        }
    }
    fn set(&self, _: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        let html = as_enum_value(&v)
            .is_some_and(|e| e.type_idx() == self.t.text_type && e.name_en() == "HTML");
        match property_name(self.t.text, m) {
            Some("TextType") if html => {
                *self.html.borrow_mut() = true;
                Ok(())
            }
            _ => Err(not_implemented(self.t.text, m).into()),
        }
    }
    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(self.t.text, m).into())
    }
    fn presentation(&self, _: &mut Execution) -> CallResult<Option<Str>> {
        Err(refuse(self.t.text, "Строка").into())
    }
}

struct NoInternal(TypeIdx);

impl ValueCodec for NoInternal {
    fn internal_write(
        &self,
        ex: &mut Execution,
        _v: &ObjRef,
        _w: &mut InternalWriter,
    ) -> Result<Option<Vec<Node>>, Exception> {
        Err(ex.error(refuse(self.0, "ЗначениеВСтрокуВнутр")))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        _items: &[Node],
        _r: &mut InternalReader,
    ) -> Result<Value, Exception> {
        Err(ex.error(refuse(self.0, "ЗначениеИзСтрокиВнутр")))
    }
}

pub(crate) fn codecs() -> Vec<(TypeIdx, Arc<dyn ValueCodec>)> {
    let Some(t) = types() else {
        return Vec::new();
    };
    [
        t.message,
        t.addresses,
        t.address,
        t.texts,
        t.text,
        t.attachments,
    ]
    .into_iter()
    .map(|x| (x, Arc::new(NoInternal(x)) as Arc<dyn ValueCodec>))
    .collect()
}

pub(crate) fn register(reg: &mut Registry) {
    if let Some(t) = types() {
        reg.type_impl(
            t.message,
            TypeImpl {
                ctor: Some(Arc::new(new_message)),
            },
        );
    }
}
