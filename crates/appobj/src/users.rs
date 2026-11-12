use crate::meta::{MdCtx, MdKey, MdPath, arg_value, key_with_app, member_name, value_of};
use crate::session::{SessionUser, call_ctx, internal};
use crate::session_info::outside_call;
use crate::{AppError, Application, Result};
use brace::Node;
use bsl_stdlib::{InternalReader, InternalWriter, ValueCodec, register_codec};
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, ObjRef, Registry,
    TypeId, Value,
};
use md_model::MdKind;
use platform_catalog::{TypeIdx, catalog};
use std::any::Any;
use std::rc::Rc;
use std::sync::Arc;

const MANAGER: &str = "МенеджерПользователейИнформационнойБазы";
const USER: &str = "ПользовательИнформационнойБазы";
const ROLES: &str = "РолиПользователя";

#[derive(Clone, Copy)]
struct Types {
    manager: TypeIdx,
    user: TypeIdx,
    roles: TypeIdx,
}

impl Types {
    fn find() -> Result<Types> {
        let find = |name: &str| {
            catalog()
                .find_type(name)
                .ok_or_else(|| AppError::Unsupported(format!("the catalog has no type {name}")))
        };
        Ok(Types {
            manager: find(MANAGER)?,
            user: find(USER)?,
            roles: find(ROLES)?,
        })
    }
}

pub(crate) fn register(app: &Arc<Application>, reg: &mut Registry) -> Result<()> {
    let types = Types::find()?;
    register_codec(types.user, Arc::new(UserCodec));
    let app = app.clone();
    reg.global_prop(
        "ПользователиИнформационнойБазы",
        "InfoBaseUsers",
        Arc::new(move |_| {
            Ok(Value::Object(Rc::new(UsersManager {
                types,
                app: app.clone(),
            })))
        }),
    );
    Ok(())
}

fn member_of(ty: TypeIdx, m: MemberId) -> std::result::Result<(bool, &'static str), HostError> {
    member_name(catalog(), ty, m)
        .map(|(is_method, ru, _)| (is_method, ru))
        .ok_or_else(|| internal("unknown member of an infobase user object"))
}

struct UsersManager {
    types: Types,
    app: Arc<Application>,
}

impl HostObject for UsersManager {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.types.manager)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        let (_, name) = member_of(self.types.manager, m)?;
        Err(HostError::not_implemented(MANAGER, name).into())
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(HostError::key(bsl_stdlib::bind::msg::PROP_NOT_WRITABLE)
            .raw()
            .into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match member_of(self.types.manager, m)? {
            (true, "ТекущийПользователь") => {
                let user = call_ctx(ex).ok_or_else(outside_call)?.sess.user().clone();
                Ok(self.user_object(user))
            }
            (true, "НайтиПоУникальномуИдентификатору") => {
                self.find_by_uuid(ex, args)
            }
            (true, "НайтиПоИмени") => self.find_by_name(ex, args),
            (_, name) => Err(HostError::not_implemented(MANAGER, name).into()),
        }
    }
}

impl UsersManager {
    fn user_object(&self, user: SessionUser) -> Value {
        Value::Object(Rc::new(InfoBaseUser {
            types: self.types,
            app: Some(self.app.clone()),
            user,
            restored: None,
        }))
    }

    fn find_by_name(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let user = call_ctx(ex).ok_or_else(outside_call)?.sess.user().clone();
        let refuse =
            |what: &str| HostError::not_implemented(MANAGER, &format!("НайтиПоИмени{what}"));
        if !user.no_users {
            return Err(refuse("").into());
        }
        match arg_value(ex, args, 0)? {
            Value::String(name) if name.to_string_lossy().is_empty() => {
                Ok(self.user_object(SessionUser {
                    name: String::new(),
                    full_name: String::new(),
                    ..user
                }))
            }
            Value::String(_) => Ok(Value::Undefined),
            _ => Err(refuse(": параметр не Строка").into()),
        }
    }

    fn find_by_uuid(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let user = call_ctx(ex).ok_or_else(outside_call)?.sess.user().clone();
        if !user.no_users {
            return Err(
                HostError::not_implemented(MANAGER, "НайтиПоУникальномуИдентификатору").into(),
            );
        }
        let wanted = bsl_stdlib::values::uuid_bytes(&arg_value(ex, args, 0)?);
        let Some(wanted) = wanted else {
            return Err(HostError::not_implemented(
                MANAGER,
                "НайтиПоУникальномуИдентификатору: параметр не УникальныйИдентификатор",
            )
            .into());
        };
        Ok(if wanted == *anonymous_uuid().as_bytes() {
            self.user_object(user)
        } else {
            Value::Undefined
        })
    }
}

fn anonymous_uuid() -> crate::model::Uuid16 {
    evlog::UserRef::nobody().uuid
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UserValue {
    Anonymous,
    OfSessionWithUsers,
    Restored,
}

pub(crate) fn user_value(v: &Value) -> Option<UserValue> {
    let user = v.as_object()?.as_any().downcast_ref::<InfoBaseUser>()?;
    Some(if user.app.is_none() {
        UserValue::Restored
    } else if user.user.no_users {
        UserValue::Anonymous
    } else {
        UserValue::OfSessionWithUsers
    })
}

struct InfoBaseUser {
    types: Types,
    app: Option<Arc<Application>>,
    user: SessionUser,
    restored: Option<[u8; 16]>,
}

impl InfoBaseUser {
    fn uuid(&self) -> crate::model::Uuid16 {
        if self.user.no_users {
            anonymous_uuid()
        } else {
            self.user.uuid
        }
    }

    fn uuid_bytes(&self) -> [u8; 16] {
        self.restored.unwrap_or_else(|| *self.uuid().as_bytes())
    }

    fn roles(&self) -> CallResult<Value> {
        let Some(app) = &self.app else {
            return Err(HostError::not_implemented(
                USER,
                "Роли: объект восстановлен из внутренней строки",
            )
            .into());
        };
        let cx = app
            .md_ctx()
            .ok_or_else(|| internal("the metadata context is gone"))?;
        let objects = &cx.model().config.objects;
        let mut items = Vec::with_capacity(self.user.roles.len());
        for id in &self.user.roles {
            let index = cx
                .cfg_index
                .get(id)
                .copied()
                .filter(|i| objects.get(*i).is_some_and(|o| o.kind == MdKind::Role));
            let Some(index) = index else {
                return Err(HostError::not_implemented(USER, "Роли: роль вне конфигурации").into());
            };
            items.push(MdKey::Path(MdPath::Other(index)));
        }
        Ok(Value::Object(Rc::new(UserRoles {
            ty: self.types.roles,
            cx,
            items,
        })))
    }
}

impl HostObject for InfoBaseUser {
    fn retained_memory(
        &self,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> std::result::Result<bool, Exception> {
        let text = self.user.name.capacity() + self.user.full_name.capacity();
        let roles = self.user.roles.capacity() * std::mem::size_of::<crate::model::Uuid16>();
        visit.bytes((text + roles) as u64)?;
        Ok(true)
    }
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.types.user)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        let Some((false, ru, en)) = member_name(catalog(), self.types.user, m) else {
            return Err(internal("unknown property of an infobase user").into());
        };
        match en {
            "Name" => Ok(Value::from(self.user.name.as_str())),
            "FullName" => Ok(Value::from(self.user.full_name.as_str())),
            "UUID" => Ok(bsl_stdlib::values::new_uuid(self.uuid_bytes())),
            "Roles" => self.roles(),
            "OSUser" if self.user.no_users => Ok(Value::from("")),
            _ => Err(HostError::not_implemented(USER, ru).into()),
        }
    }
    fn set(&self, _: &mut Execution, m: MemberId, _: Value) -> CallResult<()> {
        let (_, name) = member_of(self.types.user, m)?;
        Err(HostError::not_implemented(USER, &format!("{name} (запись)")).into())
    }
    fn call(&self, _: &mut Execution, m: MemberId, _: &mut [Arg]) -> CallResult<Value> {
        let (_, name) = member_of(self.types.user, m)?;
        Err(HostError::not_implemented(USER, name).into())
    }
    fn presentation(&self, _: &mut Execution) -> CallResult<Option<bsl_prim::Str>> {
        Ok(self.user.no_users.then(|| bsl_prim::Str::from("")))
    }
}

struct UserRoles {
    ty: TypeIdx,
    cx: Arc<MdCtx>,
    items: Vec<MdKey>,
}

impl UserRoles {
    fn contains(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let v = arg_value(ex, args, 0)?;
        let Some((key, owner)) = key_with_app(&v) else {
            return Err(HostError::not_implemented(ROLES, "Содержит: не объект метаданных").into());
        };
        let is_role = matches!(
            key,
            MdKey::Path(MdPath::Other(i))
                if owner.model().config.objects.get(i).is_some_and(|o| o.kind == MdKind::Role)
        );
        if !is_role {
            return Err(HostError::not_implemented(ROLES, "Содержит: не роль").into());
        }
        Ok(Value::Bool(
            Arc::ptr_eq(owner, &self.cx.app) && self.items.contains(&key),
        ))
    }
}

impl HostObject for UserRoles {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(self.ty)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn get(&self, _: &mut Execution, m: MemberId) -> CallResult<Value> {
        let (_, name) = member_of(self.ty, m)?;
        Err(HostError::not_implemented(ROLES, name).into())
    }
    fn set(&self, _: &mut Execution, _: MemberId, _: Value) -> CallResult<()> {
        Err(HostError::key(bsl_stdlib::bind::msg::PROP_NOT_WRITABLE)
            .raw()
            .into())
    }
    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        match member_of(self.ty, m)? {
            (true, "Содержит") => self.contains(ex, args),
            (_, name) => Err(HostError::not_implemented(ROLES, name).into()),
        }
    }
    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(self.items.len())
    }
    fn iter_get(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        match self.items.get(i) {
            Some(key) => Ok(value_of(&self.cx, ex, *key)),
            None => Err(HostError::key(bsl_stdlib::bind::msg::INDEX_OFFBOUND).into()),
        }
    }
}

const USER_ITEMS: usize = 35;

fn user_template(uid: &[u8; 16]) -> Node {
    let nil = || Node::atom(bsl_stdlib::values::uuid_string(&[0; 16]));
    let empty = || Node::str("");
    let zero = || Node::list(vec![Node::int(0)]);
    let no_date = || Node::atom("00010101000000");
    Node::list(vec![
        Node::atom(bsl_stdlib::values::uuid_string(uid)),
        empty(),
        empty(),
        empty(),
        nil(),
        zero(),
        zero(),
        nil(),
        Node::int(1),
        Node::int(1),
        Node::list(vec![Node::atom("#base64:")]),
        Node::int(0),
        Node::int(0),
        Node::int(2),
        Node::int(0),
        zero(),
        empty(),
        empty(),
        no_date(),
        Node::int(0),
        Node::int(1),
        Node::int(0),
        Node::int(1),
        empty(),
        Node::int(0),
        zero(),
        empty(),
        no_date(),
        Node::int(0),
        Node::int(0),
        empty(),
        Node::int(0),
        Node::int(0),
        Node::int(0),
        empty(),
    ])
}

struct UserCodec;

impl ValueCodec for UserCodec {
    fn internal_write(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        w: &mut InternalWriter,
    ) -> std::result::Result<Option<Vec<Node>>, Exception> {
        let Some(user) = v.as_any().downcast_ref::<InfoBaseUser>() else {
            return Err(ex.error(internal(
                "the codec of the infobase user got another object",
            )));
        };
        if !user.user.no_users {
            return Err(ex.error(HostError::not_implemented(
                USER,
                "ЗначениеВСтрокуВнутр: пользователь таблицы",
            )));
        }
        w.account(ex, USER_ITEMS as u64 * 64)?;
        Ok(Some(vec![user_template(&user.uuid_bytes())]))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        items: &[Node],
        r: &mut InternalReader,
    ) -> std::result::Result<Value, Exception> {
        let uid = match items {
            [Node::List(content)] if content.len() == USER_ITEMS => content
                .first()
                .and_then(Node::as_atom)
                .map(|text| text.encode_utf16().collect::<Vec<u16>>())
                .and_then(|units| bsl_stdlib::values::parse_uuid(&units)),
            _ => None,
        };
        let Some(uid) = uid else {
            return Err(r.format_error(ex));
        };
        let mut text = items.first().cloned();
        if let Some(Node::List(content)) = text.as_mut()
            && let Some(Node::Atom(atom)) = content.first_mut()
        {
            atom.make_ascii_lowercase();
        }
        if text.as_ref() != Some(&user_template(&uid)) {
            return Err(ex.error(HostError::not_implemented(
                USER,
                "ЗначениеИзСтрокиВнутр: пользователь, отличный от пользователя базы без пользователей",
            )));
        }
        let types =
            Types::find().map_err(|_| ex.error(internal("the catalog lost the user type")))?;
        Ok(Value::Object(Rc::new(InfoBaseUser {
            types,
            app: None,
            user: SessionUser {
                uuid: anonymous_uuid(),
                name: String::new(),
                full_name: String::new(),
                roles: Vec::new(),
                no_users: true,
            },
            restored: Some(uid),
        })))
    }

    fn storable(&self) -> bool {
        false
    }
}
