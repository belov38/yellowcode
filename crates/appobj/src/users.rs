use crate::meta::{MdCtx, MdKey, MdPath, arg_value, key_with_app, member_name, value_of};
use crate::session::{SessionUser, call_ctx, internal};
use crate::session_info::outside_call;
use crate::{AppError, Application, InfoBaseUserCard, Result};
use brace::Node;
use bsl_stdlib::{InternalReader, InternalWriter, ValueCodec, register_codec};
use bsl_vm::{
    Arg, CallResult, Exception, Execution, HostError, HostObject, MemberId, ObjRef, Registry,
    TypeId, Value,
};
use md_model::MdKind;
use platform_catalog::{TypeIdx, catalog};
use std::any::Any;
use std::cell::RefCell;
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
    register_codec(types.roles, Arc::new(RolesCodec));
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
            (true, "ПолучитьПользователей") => self.list(ex),
            (true, "СоздатьПользователя") => Ok(self.new_user()),
            (_, name) => Err(HostError::not_implemented(MANAGER, name).into()),
        }
    }
}

impl UsersManager {
    fn user_object(&self, user: SessionUser) -> Value {
        let flags = if user.no_users {
            None
        } else {
            self.app
                .users()
                .and_then(|users| users.by_uuid(&user.uuid))
                .map(Flags::of)
        };
        Value::Object(Rc::new(InfoBaseUser {
            types: self.types,
            app: Some(self.app.clone()),
            user,
            flags,
            restored: None,
            created: None,
        }))
    }

    fn card_object(&self, card: &InfoBaseUserCard) -> Value {
        Value::Object(Rc::new(InfoBaseUser {
            types: self.types,
            app: Some(self.app.clone()),
            user: card.user.clone(),
            flags: Some(Flags::of(card)),
            restored: None,
            created: None,
        }))
    }

    fn list(&self, ex: &mut Execution) -> CallResult<Value> {
        let user = call_ctx(ex).ok_or_else(outside_call)?.sess.user().clone();
        let users = (!user.no_users).then(|| self.app.users()).flatten();
        let Some(users) = users else {
            return Err(HostError::not_implemented(MANAGER, "ПолучитьПользователей").into());
        };
        if !crate::access_right::is_admin(&self.app, &user) {
            if ex.effective_flags().privileged {
                return Err(HostError::not_implemented(
                    MANAGER,
                    "ПолучитьПользователей: привилегированный режим сеанса без административных прав",
                )
                .into());
            }
            return Err(access_denied().into());
        }
        Ok(bsl_stdlib::new_array(
            users.cards().map(|c| self.card_object(c)).collect(),
        ))
    }

    fn new_user(&self) -> Value {
        Value::Object(Rc::new(InfoBaseUser {
            types: self.types,
            app: Some(self.app.clone()),
            user: SessionUser {
                uuid: crate::model::Uuid16::NIL,
                name: String::new(),
                full_name: String::new(),
                roles: Vec::new(),
                no_users: false,
            },
            flags: None,
            restored: None,
            created: Some(Rc::new(RefCell::new(Vec::new()))),
        }))
    }

    fn find_by_name(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let user = call_ctx(ex).ok_or_else(outside_call)?.sess.user().clone();
        let refuse =
            |what: &str| HostError::not_implemented(MANAGER, &format!("НайтиПоИмени{what}"));
        if !user.no_users {
            return self.find_in_table(ex, args, &user);
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

    fn find_in_table(
        &self,
        ex: &mut Execution,
        args: &[Arg],
        user: &SessionUser,
    ) -> CallResult<Value> {
        let refuse =
            |what: &str| HostError::not_implemented(MANAGER, &format!("НайтиПоИмени{what}"));
        let Some(users) = self.app.users() else {
            return Err(refuse("").into());
        };
        let Value::String(name) = arg_value(ex, args, 0)? else {
            return Err(refuse(": параметр не Строка").into());
        };
        let name = name.to_string_lossy();
        if name.trim_end_matches(' ').is_empty() {
            return Err(refuse(": пустое имя в базе с пользователями").into());
        }
        let Ok(found) = users.by_name(&name) else {
            return Err(refuse(": имя без ключа сравнения").into());
        };
        let admin = crate::access_right::is_admin(&self.app, user);
        if !admin && ex.effective_flags().privileged {
            return Err(
                refuse(": привилегированный режим сеанса без административных прав").into(),
            );
        }
        match found {
            Some(card) if admin || card.user.uuid == user.uuid => Ok(self.card_object(card)),
            Some(_) => Err(access_denied().into()),
            None if admin => Ok(Value::Undefined),
            None => {
                Err(refuse(": нет такого пользователя, сеанс без административных прав").into())
            }
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

fn access_denied() -> HostError {
    let mut e = HostError::text(
        "Пользователю недостаточно прав для выполнения операции.",
        "The user doesn't have appropriate rights to perform this operation.",
    );
    e.categories = Some(bsl_vm::CategorySet::of(&[
        bsl_vm::Category::AllErrors,
        bsl_vm::Category::ScriptRuntimeError,
        bsl_vm::Category::OtherError,
    ]));
    e
}

fn anonymous_uuid() -> crate::model::Uuid16 {
    evlog::UserRef::nobody().uuid
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UserValue {
    Anonymous,
    OfSessionWithUsers,
    Restored,
    Created,
}

pub(crate) fn user_value(v: &Value) -> Option<UserValue> {
    let user = v.as_object()?.as_any().downcast_ref::<InfoBaseUser>()?;
    Some(if user.created.is_some() {
        UserValue::Created
    } else if user.app.is_none() {
        UserValue::Restored
    } else if user.user.no_users {
        UserValue::Anonymous
    } else {
        UserValue::OfSessionWithUsers
    })
}

pub(crate) fn user_roles(v: &Value) -> Option<(Vec<crate::model::Uuid16>, Arc<Application>)> {
    let user = v.as_object()?.as_any().downcast_ref::<InfoBaseUser>()?;
    if user_value(v) != Some(UserValue::OfSessionWithUsers) {
        return None;
    }
    Some((user.user.roles.clone(), user.app.clone()?))
}

#[derive(Clone, Copy)]
struct Flags {
    standard_auth: bool,
    password_set: bool,
    show_in_list: bool,
}

impl Flags {
    fn of(card: &InfoBaseUserCard) -> Flags {
        Flags {
            standard_auth: card.standard_auth,
            password_set: card.password_set,
            show_in_list: card.show_in_list,
        }
    }
}

struct InfoBaseUser {
    types: Types,
    app: Option<Arc<Application>>,
    user: SessionUser,
    flags: Option<Flags>,
    restored: Option<[u8; 16]>,
    created: Option<Rc<RefCell<Vec<MdKey>>>>,
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
            items: Rc::new(RefCell::new(items)),
            editable: false,
        })))
    }

    fn created_roles(&self, items: &Rc<RefCell<Vec<MdKey>>>) -> CallResult<Value> {
        let cx = self
            .app
            .as_ref()
            .and_then(|app| app.md_ctx())
            .ok_or_else(|| internal("the metadata context is gone"))?;
        Ok(Value::Object(Rc::new(UserRoles {
            ty: self.types.roles,
            cx,
            items: items.clone(),
            editable: true,
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
        if let Some(items) = &self.created {
            roles_memory(items, true, visit)?;
        }
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
        if let Some(items) = &self.created {
            return match en {
                "Roles" => self.created_roles(items),
                _ => Err(
                    HostError::not_implemented(USER, &format!("{ru}: новый пользователь")).into(),
                ),
            };
        }
        match en {
            "Name" => Ok(Value::from(self.user.name.as_str())),
            "FullName" => Ok(Value::from(self.user.full_name.as_str())),
            "UUID" => Ok(bsl_stdlib::values::new_uuid(self.uuid_bytes())),
            "Roles" => self.roles(),
            "OSUser" if self.user.no_users => Ok(Value::from("")),
            _ => match (ru, self.flags) {
                ("АутентификацияСтандартная", Some(f)) => {
                    Ok(Value::Bool(f.standard_auth))
                }
                ("ПарольУстановлен", Some(f)) => Ok(Value::Bool(f.password_set)),
                ("ПоказыватьВСпискеВыбора", Some(f)) => {
                    Ok(Value::Bool(f.show_in_list))
                }
                _ => Err(HostError::not_implemented(USER, ru).into()),
            },
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
        if self.created.is_some() {
            return Err(HostError::not_implemented(USER, "Строка: новый пользователь").into());
        }
        Ok(Some(bsl_prim::Str::from(if self.user.no_users {
            ""
        } else {
            self.user.name.as_str()
        })))
    }
}

fn roles_memory(
    items: &Rc<RefCell<Vec<MdKey>>>,
    editable: bool,
    visit: &mut dyn bsl_vm::RetainedMemory,
) -> std::result::Result<(), Exception> {
    visit.work(1)?;
    if editable {
        visit.mutable();
    }
    let cell = std::mem::size_of::<RefCell<Vec<MdKey>>>() + 2 * std::mem::size_of::<usize>();
    if visit.allocation(Rc::as_ptr(items) as usize, cell as u64)? {
        let roles = items.borrow();
        if roles.capacity() != 0 {
            visit.allocation(
                roles.as_ptr() as usize,
                crate::read_budget::slots::<MdKey>(roles.capacity()),
            )?;
        }
    }
    Ok(())
}

struct UserRoles {
    ty: TypeIdx,
    cx: Arc<MdCtx>,
    items: Rc<RefCell<Vec<MdKey>>>,
    editable: bool,
}

fn role_key<'v>(v: &'v Value, method: &str) -> CallResult<(MdKey, &'v Arc<Application>)> {
    let refuse = |what: &str| HostError::not_implemented(ROLES, &format!("{method}: {what}"));
    let Some((key, owner)) = key_with_app(v) else {
        return Err(refuse("не объект метаданных").into());
    };
    let is_role = matches!(
        key,
        MdKey::Path(MdPath::Other(i))
            if owner.model().config.objects.get(i).is_some_and(|o| o.kind == MdKind::Role)
    );
    if !is_role {
        return Err(refuse("не роль").into());
    }
    Ok((key, owner))
}

impl UserRoles {
    fn contains(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let v = arg_value(ex, args, 0)?;
        let (key, owner) = role_key(&v, "Содержит")?;
        Ok(Value::Bool(
            Arc::ptr_eq(owner, &self.cx.app) && self.items.borrow().contains(&key),
        ))
    }

    fn add(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<Value> {
        let v = arg_value(ex, args, 0)?;
        let (key, owner) = role_key(&v, "Добавить")?;
        if !Arc::ptr_eq(owner, &self.cx.app) {
            return Err(
                HostError::not_implemented(ROLES, "Добавить: роль другой конфигурации").into(),
            );
        }
        let mut items = self.items.borrow_mut();
        if items.contains(&key) {
            return Err(HostError::not_implemented(ROLES, "Добавить: роль уже в коллекции").into());
        }
        items.push(key);
        Ok(Value::Undefined)
    }
}

impl HostObject for UserRoles {
    fn retained_memory(
        &self,
        visit: &mut dyn bsl_vm::RetainedMemory,
    ) -> std::result::Result<bool, Exception> {
        roles_memory(&self.items, self.editable, visit)?;
        Ok(true)
    }
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
            (true, "Добавить") if self.editable => self.add(ex, args),
            (_, name) => Err(HostError::not_implemented(ROLES, name).into()),
        }
    }
    fn iter_len(&self, _: &mut Execution) -> Option<usize> {
        Some(self.items.borrow().len())
    }
    fn iter_get(&self, ex: &mut Execution, i: usize) -> CallResult<Value> {
        let items = self.items.borrow();
        if self.editable && items.len() > 1 {
            return Err(HostError::not_implemented(
                ROLES,
                "Для Каждого: несколько ролей нового пользователя",
            )
            .into());
        }
        let key = items.get(i).copied();
        drop(items);
        match key {
            Some(key) => Ok(value_of(&self.cx, ex, key)),
            None => Err(HostError::key(bsl_stdlib::bind::msg::INDEX_OFFBOUND).into()),
        }
    }
}

struct RolesCodec;

impl ValueCodec for RolesCodec {
    fn internal_write(
        &self,
        ex: &mut Execution,
        v: &ObjRef,
        w: &mut InternalWriter,
    ) -> std::result::Result<Option<Vec<Node>>, Exception> {
        let Some(roles) = v.as_any().downcast_ref::<UserRoles>() else {
            return Err(ex.error(internal("the codec of the user roles got another object")));
        };
        let role = match roles.items.borrow().as_slice() {
            [MdKey::Path(MdPath::Other(i))] => *i,
            [_] => {
                return Err(ex.error(internal(
                    "a user role that is no object of the configuration",
                )));
            }
            _ => {
                return Err(ex.error(HostError::not_implemented(
                    ROLES,
                    "ЗначениеВСтрокуВнутр: не одна роль",
                )));
            }
        };
        let Some(object) = roles.cx.model().config.objects.get(role) else {
            return Err(ex.error(internal("a user role outside of the configuration")));
        };
        w.account(ex, 64)?;
        Ok(Some(vec![
            Node::list(vec![
                Node::int(1),
                Node::atom(bsl_stdlib::values::uuid_string(object.id.as_bytes())),
            ]),
            Node::list(vec![Node::int(0)]),
        ]))
    }

    fn internal_read(
        &self,
        ex: &mut Execution,
        _: &[Node],
        _: &mut InternalReader,
    ) -> std::result::Result<Value, Exception> {
        Err(ex.error(HostError::not_implemented(ROLES, "ЗначениеИзСтрокиВнутр")))
    }

    fn storable(&self) -> bool {
        false
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
        if user.created.is_some() {
            return Err(ex.error(HostError::not_implemented(
                USER,
                "ЗначениеВСтрокуВнутр: новый пользователь",
            )));
        }
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
            flags: None,
            restored: Some(uid),
            created: None,
        })))
    }

    fn storable(&self) -> bool {
        false
    }
}
