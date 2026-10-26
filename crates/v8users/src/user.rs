use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use brace::Node;

use crate::password::{Hashes, hashes};
use crate::{BslDate, Error, Uuid16, data};

pub const SERVICE_ROW_ID: &str = "6e3ee1de-e796-4023-952a-5019eae6a1f7";

const F_ID: usize = 0;
const F_NAME: usize = 1;
const F_PASSWORD_SET: usize = 2;
const F_FULL_NAME: usize = 3;
const F_ROLES: usize = 5;
const F_STD_AUTH: usize = 7;
const F_SHOW: usize = 8;
const F_HASH: usize = 12;
const F_HASH_UPPER: usize = 13;
const F_ROLESID: usize = 15;
const F_CHANGED: usize = 16;
const F_PASSWORD_CHANGED: usize = 29;
const F_SECRET: usize = 33;

const PASSWORD_SET: &str = "\u{FFFD}";
const F14_VALUE: i64 = 2;

#[derive(Clone, PartialEq, Eq)]
pub struct NewUser {
    pub id: Uuid16,
    pub name: String,
    pub full_name: String,
    pub roles: Vec<Uuid16>,
    pub std_auth: bool,
    pub show_in_list: bool,
    pub password: Option<String>,
    pub rolesid: u32,
    pub changed_local: BslDate,
    pub password_changed_utc: Option<BslDate>,
    pub secret32: [u8; 32],
}

impl fmt::Debug for NewUser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const HIDDEN: &str = "<redacted>";
        f.debug_struct("NewUser")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("full_name", &self.full_name)
            .field("roles", &self.roles)
            .field("std_auth", &self.std_auth)
            .field("show_in_list", &self.show_in_list)
            .field("password", &self.password.as_ref().map(|_| HIDDEN))
            .field("rolesid", &self.rolesid)
            .field("changed_local", &self.changed_local)
            .field("password_changed_utc", &self.password_changed_utc)
            .field("secret32", &HIDDEN)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct UserData {
    pub tree: Node,
}

impl fmt::Debug for UserData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserData")
            .field("fields", &self.tree.as_list().map_or(0, <[Node]>::len))
            .finish_non_exhaustive()
    }
}

fn flag(b: bool) -> Node {
    Node::int(i64::from(b))
}

fn str_or_empty(n: Option<&Node>) -> String {
    n.and_then(Node::as_str).unwrap_or_default().to_owned()
}

fn roles_node(roles: &[Uuid16]) -> Node {
    let n = i64::try_from(roles.len()).unwrap_or(i64::MAX);
    let mut items = Vec::with_capacity(roles.len().saturating_add(1));
    items.push(Node::int(n));
    items.extend(roles.iter().map(|r| Node::guid(*r)));
    Node::list(items)
}

fn date_node(d: BslDate) -> Node {
    let p = d.parts();
    Node::atom(format!(
        "{:04}{:02}{:02}{:02}{:02}{:02}",
        p.year, p.month, p.day, p.hour, p.minute, p.second
    ))
}

use crate::layout::{Layout, bound_text, parse_date};

struct Head<'a> {
    id: Uuid16,
    name: &'a str,
    password_set: bool,
    full_name: &'a str,
    roles: &'a [Uuid16],
    std_auth: bool,
    show: bool,
    hashes: &'a Hashes,
    rolesid: u32,
    changed: BslDate,
}

fn head(h: &Head<'_>) -> Vec<Node> {
    let zero = || Node::int(0);
    let list0 = || Node::list(vec![Node::int(0)]);
    vec![
        Node::guid(h.id),
        Node::str(h.name),
        Node::str(if h.password_set { PASSWORD_SET } else { "" }),
        Node::str(h.full_name),
        Node::guid(Uuid16::NIL),
        roles_node(h.roles),
        Node::guid(Uuid16::NIL),
        flag(h.std_auth),
        flag(h.show),
        Node::empty(),
        zero(),
        zero(),
        Node::str(&h.hashes.plain),
        Node::str(&h.hashes.upper),
        Node::int(F14_VALUE),
        Node::int(i64::from(h.rolesid)),
        date_node(h.changed),
        zero(),
        zero(),
        list0(),
        Node::int(1),
        list0(),
        Node::int(1),
        zero(),
        Node::int(1),
        Node::str(""),
        zero(),
        zero(),
        Node::str(""),
    ]
}

fn tail(password_changed: BslDate, hash: Option<&str>, secret: &str) -> Vec<Node> {
    let mut f = vec![date_node(password_changed)];
    match hash {
        Some(h) => f.extend([Node::int(1), Node::str(h)]),
        None => f.push(Node::int(0)),
    }
    f.extend([
        Node::int(0),
        Node::str(secret),
        Node::int(0),
        Node::int(0),
        Node::int(0),
        Node::str(""),
    ]);
    f
}

impl UserData {
    pub fn parse(text: &str) -> Result<UserData, Error> {
        bound_text(text)?;
        let tree = brace::parse(text)?;
        Layout::read(tree.as_list().ok_or(Error::NotList)?)?;
        Ok(UserData { tree })
    }

    pub fn build(u: &NewUser) -> UserData {
        let password = u.password.as_deref().unwrap_or("");
        let h = hashes(password);
        let mut f = head(&Head {
            id: u.id,
            name: &u.name,
            password_set: !password.is_empty(),
            full_name: &u.full_name,
            roles: &u.roles,
            std_auth: u.std_auth,
            show: u.show_in_list,
            hashes: &h,
            rolesid: u.rolesid,
            changed: u.changed_local,
        });
        f.extend(tail(
            u.password_changed_utc.unwrap_or(BslDate::EMPTY),
            Some(&h.plain),
            &STANDARD.encode(u.secret32),
        ));
        UserData {
            tree: Node::list(f),
        }
    }

    pub fn service_row(id: Uuid16, rolesid: u32, changed_local: BslDate) -> UserData {
        let mut f = head(&Head {
            id,
            name: "",
            password_set: false,
            full_name: "",
            roles: &[],
            std_auth: true,
            show: true,
            hashes: &Hashes::default(),
            rolesid,
            changed: changed_local,
        });
        f.extend(tail(BslDate::EMPTY, None, ""));
        UserData {
            tree: Node::list(f),
        }
    }

    pub fn to_text(&self) -> String {
        brace::to_text(&self.tree)
    }

    fn field(&self, i: usize) -> Option<&Node> {
        let fields = self.tree.as_list()?;
        Layout::read(fields).ok()?.at(fields, i)
    }

    fn checked_roles(&self) -> Option<Vec<Uuid16>> {
        let (n, rest) = self.field(F_ROLES)?.as_list()?.split_first()?;
        if usize::try_from(n.as_u32()?).ok()? != rest.len() {
            return None;
        }
        rest.iter().map(Node::as_guid).collect()
    }

    pub fn id(&self) -> Option<Uuid16> {
        self.field(F_ID)?.as_guid()
    }

    pub fn name(&self) -> Option<&str> {
        self.field(F_NAME)?.as_str()
    }

    pub fn full_name(&self) -> Option<&str> {
        self.field(F_FULL_NAME)?.as_str()
    }

    pub fn roles(&self) -> Vec<Uuid16> {
        self.checked_roles().unwrap_or_default()
    }

    pub fn std_auth(&self) -> bool {
        self.field(F_STD_AUTH).and_then(Node::as_bool) == Some(true)
    }

    pub fn show_in_list(&self) -> bool {
        self.field(F_SHOW).and_then(Node::as_bool) == Some(true)
    }

    pub fn password_set(&self) -> bool {
        self.field(F_PASSWORD_SET).and_then(Node::as_str) == Some(PASSWORD_SET)
    }

    pub fn has_password_material(&self) -> Result<bool, Error> {
        let fields = self.tree.as_list().ok_or(Error::NotList)?;
        let layout = Layout::read(fields)?;
        for logical in [F_PASSWORD_SET, F_HASH, F_HASH_UPPER, F_SECRET] {
            if let Some(node) = layout.at(fields, logical)
                && !node.as_str().ok_or(Error::BadField(logical))?.is_empty()
            {
                return Ok(true);
            }
        }
        Ok(layout.at(fields, 30).and_then(Node::as_u32).unwrap_or(0) != 0)
    }

    pub fn hashes(&self) -> Hashes {
        Hashes {
            plain: str_or_empty(self.field(F_HASH)),
            upper: str_or_empty(self.field(F_HASH_UPPER)),
        }
    }

    pub fn rolesid(&self) -> Option<u32> {
        self.field(F_ROLESID)?.as_u32()
    }

    pub fn changed_local(&self) -> Option<BslDate> {
        parse_date(self.field(F_CHANGED)?.as_atom()?)
    }

    pub fn password_changed_utc(&self) -> Option<BslDate> {
        parse_date(self.field(F_PASSWORD_CHANGED)?.as_atom()?)
    }

    pub fn secret32(&self) -> Option<[u8; 32]> {
        let b = STANDARD.decode(self.field(F_SECRET)?.as_str()?).ok()?;
        <[u8; 32]>::try_from(b).ok()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct UserRow {
    pub id: Uuid16,
    pub name: String,
    pub descr: String,
    pub osname: Option<String>,
    pub changed: BslDate,
    pub rolesid: u32,
    pub show: bool,
    pub data: Vec<u8>,
    pub eauth: bool,
    pub admrole: bool,
    pub ussprh: u32,
    pub email: String,
}

impl fmt::Debug for UserRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserRow")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("descr", &self.descr)
            .field("osname", &self.osname)
            .field("changed", &self.changed)
            .field("rolesid", &self.rolesid)
            .field("show", &self.show)
            .field("data", &format_args!("<{} bytes>", self.data.len()))
            .field("eauth", &self.eauth)
            .field("admrole", &self.admrole)
            .field("ussprh", &self.ussprh)
            .field("email", &self.email)
            .finish()
    }
}

impl UserRow {
    pub fn new(u: &NewUser, admin: bool, key: &[u8]) -> Result<UserRow, Error> {
        Ok(UserRow {
            id: u.id,
            name: u.name.clone(),
            descr: u.full_name.clone(),
            osname: None,
            changed: u.changed_local,
            rolesid: u.rolesid,
            show: u.show_in_list && u.std_auth,
            data: data::encode(&UserData::build(u).to_text(), key)?,
            eauth: u.std_auth,
            admrole: admin,
            ussprh: 0,
            email: String::new(),
        })
    }
}
