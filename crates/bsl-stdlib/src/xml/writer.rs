use super::{
    bad_char_error, is_ncname, shown_text, split_qname, types, write_error, write_error_in,
    xml_error,
};
use crate::binary::{SharedStream, stream_of, write_stream};
use crate::bind::{
    arg_bool, arg_str, arg_value, is_given, key_error, msg, type_mismatch, with_other,
};
use crate::collections::{
    ctor_not_implemented, method_name, not_implemented, property_name, property_ru,
};
use crate::enums::as_enum_value;
use crate::fs::{fs, fs_error};
use crate::globals::data::encoding_from_value;
use bsl_lib::codec::{self, CodePages, Encoding, EncodingName};
use bsl_lib::xml::{XmlWriteOptions, XmlWriter, xml_invalid_pos};
use bsl_prim::Str;
use bsl_vm::{
    Arg, CallResult, Execution, HostError, HostObject, MemberId, Registry, TypeId, TypeImpl, Value,
};
use platform_catalog::TypeIdx;
use std::any::Any;
use std::cell::{RefCell, RefMut};
use std::rc::Rc;
use std::sync::Arc;

const MAX_UNITS: u64 = 1 << 30;

const NS_XML: &str = "http://www.w3.org/XML/1998/namespace";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    pub encoding: String,
    pub version: String,
    pub indent: bool,
    pub indent_attributes: bool,
    pub indent_chars: String,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            encoding: "UTF-8".into(),
            version: "1.0".into(),
            indent: true,
            indent_attributes: false,
            indent_chars: "\t".into(),
        }
    }
}

struct SettingsObject(Settings);

impl SettingsObject {
    fn of(v: &Value) -> Option<&Settings> {
        v.as_object()?
            .as_any()
            .downcast_ref::<SettingsObject>()
            .map(|s| &s.0)
    }

    fn new_value(s: Settings) -> Value {
        Value::Object(Rc::new(SettingsObject(s)))
    }
}

fn read_only(t: TypeIdx, m: MemberId) -> bsl_vm::CallError {
    match property_name(t, m) {
        Some(_) => HostError::key(msg::PROP_NOT_WRITABLE)
            .with_arg(property_ru(t, m))
            .raw()
            .into(),
        None => not_implemented(t, m).into(),
    }
}

fn enum_encoding_name(ex: &Execution, name_en: &str) -> String {
    let pages = ex.env().code_pages();
    match name_en {
        "UTF8" => "UTF-8".into(),
        "UTF16" => "UTF-16".into(),
        "ANSI" => pages[0].into(),
        "OEM" => pages[1].into(),
        _ => pages[2].into(),
    }
}

fn encoding_arg(
    ex: &mut Execution,
    v: &Value,
    param: usize,
) -> CallResult<(String, Encoding, bool)> {
    match v {
        Value::String(s) => {
            let (e, bom) = codec::resolve(EncodingName::Named(s.units()), &CodePages::STAND)
                .map_err(|_| xml_error(msg::XML_ENCODING))?;
            Ok((s.to_string_lossy(), e, bom))
        }
        v => {
            let (e, bom) = encoding_from_value(ex, v, param)?;
            let name = match as_enum_value(v) {
                Some(ev) => enum_encoding_name(ex, ev.name_en()),
                None => "UTF-8".into(),
            };
            Ok((name, e, bom))
        }
    }
}

fn settings_ctor(ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if args.len() > 5 {
        return Err(ctor_not_implemented(types().settings).into());
    }
    let mut s = Settings::default();
    let enc = arg_value(ex, args.first())?;
    if !matches!(enc, Value::Undefined) {
        s.encoding = encoding_arg(ex, &enc, 1)?.0;
    }
    if is_given(args.get(1)) && !matches!(arg_value(ex, args.get(1))?, Value::Undefined) {
        let v = arg_str(ex, args.get(1))?.to_string_lossy();
        if !matches!(v.as_str(), "1.0" | "1.1") {
            return Err(xml_error(msg::XML_VERSION).into());
        }
        s.version = v;
    }
    if is_given(args.get(2)) && !matches!(arg_value(ex, args.get(2))?, Value::Undefined) {
        s.indent = arg_bool(ex, args.get(2))?;
    }
    if is_given(args.get(3)) && !matches!(arg_value(ex, args.get(3))?, Value::Undefined) {
        s.indent_attributes = arg_bool(ex, args.get(3))?;
    }
    if is_given(args.get(4)) && !matches!(arg_value(ex, args.get(4))?, Value::Undefined) {
        let c = arg_str(ex, args.get(4))?.to_string_lossy();
        if !c.chars().all(|ch| matches!(ch, ' ' | '\t' | '\r' | '\n')) {
            return Err(xml_error(msg::XML_INDENT_CHARS).into());
        }
        s.indent_chars = c;
    }
    Ok(SettingsObject::new_value(s))
}

impl HostObject for SettingsObject {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().settings)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(crate::collections::type_name(ex, types().settings)))
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().settings;
        let s = &self.0;
        match property_name(t, m) {
            Some("Encoding") => Some(Value::from(s.encoding.as_str())),
            Some("Version") => Some(Value::from(s.version.as_str())),
            Some("Indent") => Some(Value::Bool(s.indent)),
            Some("IndentAttributes") => Some(Value::Bool(s.indent_attributes)),
            Some("IndentChars") => Some(Value::from(s.indent_chars.as_str())),
            _ => None,
        }
        .ok_or_else(|| not_implemented(t, m).into())
    }

    fn set(&self, _ex: &mut Execution, m: MemberId, _v: Value) -> CallResult<()> {
        Err(read_only(types().settings, m))
    }

    fn call(&self, _ex: &mut Execution, m: MemberId, _args: &mut [Arg]) -> CallResult<Value> {
        Err(not_implemented(types().settings, m).into())
    }
}

struct FileTarget {
    dest: Dest,
    enc: Encoding,
    bom: bool,
}

enum Dest {
    File(String),
    Stream(SharedStream),
}

pub(crate) struct XmlWriterState {
    w: XmlWriter,
    settings: Settings,
    settings_value: Value,
    depth: u64,
    started: bool,
    root_done: bool,
    tag_open: bool,
    last_text: bool,
    attrs: Vec<(String, String)>,
    generated: u64,
    scopes: Vec<(u64, String, String)>,
    size: u64,
    declared: Option<String>,
    file: Option<FileTarget>,
}

fn kernel_options(s: &Settings) -> XmlWriteOptions {
    XmlWriteOptions {
        indent: s.indent,
        indent_chars: s.indent_chars.clone(),
        newline: "\n".into(),
    }
}

enum Declare<'a> {
    Default,
    Plain,
    Prefix(&'a str),
}

pub(crate) fn check_chars(s: &str) -> CallResult<()> {
    let u: Vec<u16> = s.encode_utf16().collect();
    match xml_invalid_pos(&u, false) {
        0 => Ok(()),
        p => Err(bad_char_error(p as usize - 1, s).into()),
    }
}

fn bad_name() -> bsl_vm::CallError {
    xml_error(msg::XML_BAD_NAME).into()
}

impl XmlWriterState {
    fn new(settings: Settings, settings_value: Value, declared: Option<String>) -> XmlWriterState {
        XmlWriterState {
            w: XmlWriter::new(kernel_options(&settings)),
            settings,
            settings_value,
            depth: 0,
            started: false,
            root_done: false,
            tag_open: false,
            last_text: false,
            attrs: Vec::new(),
            generated: 0,
            scopes: Vec::new(),
            size: 0,
            declared,
            file: None,
        }
    }

    pub(crate) fn depth(&self) -> u64 {
        self.depth
    }

    #[allow(dead_code)]
    pub(crate) fn kernel(&mut self) -> &mut XmlWriter {
        self.started = true;
        &mut self.w
    }

    pub(crate) fn lookup_uri(&self, prefix: &str) -> Option<&str> {
        if prefix == "xml" {
            return Some(NS_XML);
        }
        self.scopes
            .iter()
            .rev()
            .find(|(_, p, _)| p == prefix)
            .map(|(_, _, u)| u.as_str())
    }

    pub(crate) fn declared_here(&self, uri: &str) -> Option<&str> {
        let depth = self.depth;
        self.scopes
            .iter()
            .rev()
            .find(|(d, _, u)| *d == depth && u == uri)
            .map(|(_, p, _)| p.as_str())
    }

    pub(crate) fn prefix_of(&self, uri: &str) -> Option<&str> {
        self.w.prefix_of(uri)
    }

    pub(crate) fn grow(&mut self, ex: &mut Execution, add: u64) -> CallResult<()> {
        let extra = if self.settings.indent {
            self.depth
                .saturating_mul(self.settings.indent_chars.len() as u64)
                .saturating_add(4)
        } else {
            0
        };
        let total = self.size.saturating_add(add).saturating_add(extra);
        if total > MAX_UNITS {
            return Err(ex.out_of_memory().into());
        }
        ex.charge(add.saturating_add(extra) / 64)?;
        ex.reserve(total.saturating_mul(3))?;
        self.size = total;
        Ok(())
    }

    pub(crate) fn declaration(&mut self, ex: &mut Execution) -> CallResult<()> {
        self.grow(ex, 64)?;
        let enc = self.declared.clone();
        let version = self.settings.version.clone();
        self.w
            .declaration_version(&version, enc.as_deref())
            .map_err(write_error)?;
        self.started = true;
        Ok(())
    }

    pub(crate) fn start_element(
        &mut self,
        ex: &mut Execution,
        local: &str,
        ns: Option<&str>,
    ) -> CallResult<()> {
        self.begin(ex, local, ns, Declare::Default)
    }

    pub(crate) fn start_element_prefixed(
        &mut self,
        ex: &mut Execution,
        local: &str,
        uri: &str,
        prefix: &str,
    ) -> CallResult<()> {
        if !is_ncname(prefix) {
            return Err(bad_name());
        }
        self.begin(ex, local, Some(uri), Declare::Prefix(prefix))
    }

    pub(crate) fn start_element_full(&mut self, ex: &mut Execution, full: &str) -> CallResult<()> {
        let Some((prefix, local)) = split_qname(full) else {
            return Err(bad_name());
        };
        let ns = match prefix {
            "" => self.lookup_uri("").filter(|u| !u.is_empty()),
            p => self.lookup_uri(p),
        }
        .map(str::to_string);
        if !prefix.is_empty() && ns.is_none() {
            return self.begin(ex, full, None, Declare::Plain);
        }
        self.begin(ex, local, ns.as_deref(), Declare::Plain)
    }

    fn begin(
        &mut self,
        ex: &mut Execution,
        local: &str,
        ns: Option<&str>,
        declare: Declare<'_>,
    ) -> CallResult<()> {
        let full_name = matches!(declare, Declare::Plain) && split_qname(local).is_some();
        if !is_ncname(local) && !full_name {
            return Err(bad_name());
        }
        if let Some(u) = ns {
            check_chars(u)?;
        }
        if self.depth == 0 && self.root_done {
            return Err(write_error(bsl_lib::xml::XmlWriteError::Order).into());
        }
        self.grow(ex, (local.len() as u64).saturating_mul(2).saturating_add(8))?;
        let ns = ns.filter(|u| !u.is_empty());
        let decl = match ns {
            Some(uri) if self.w.prefix_of(uri).is_none() => Some((
                match declare {
                    Declare::Prefix(p) => p.to_string(),
                    _ => String::new(),
                },
                uri.to_string(),
            )),
            None if matches!(declare, Declare::Default | Declare::Prefix(_))
                && self.lookup_uri("").is_some_and(|u| !u.is_empty()) =>
            {
                Some((String::new(), String::new()))
            }
            _ => None,
        };
        self.w.start_element(local, ns).map_err(write_error)?;
        self.started = true;
        self.depth += 1;
        self.tag_open = true;
        self.last_text = false;
        self.attrs.clear();
        self.generated = 0;
        if let Some((prefix, uri)) = decl {
            self.w.namespace(&prefix, &uri).map_err(write_error)?;
            self.scopes.push((self.depth, prefix, uri));
        }
        Ok(())
    }

    pub(crate) fn namespace(
        &mut self,
        ex: &mut Execution,
        prefix: &str,
        uri: &str,
    ) -> CallResult<()> {
        if !prefix.is_empty() && !is_ncname(prefix) {
            return Err(bad_name());
        }
        check_chars(uri)?;
        if !self.tag_open {
            return Err(write_error(bsl_lib::xml::XmlWriteError::Order).into());
        }
        self.grow(ex, (prefix.len() + uri.len()) as u64 + 10)?;
        self.w.namespace(prefix, uri).map_err(write_error)?;
        self.scopes
            .push((self.depth, prefix.to_string(), uri.to_string()));
        Ok(())
    }

    pub(crate) fn attribute(
        &mut self,
        ex: &mut Execution,
        local: &str,
        ns: Option<&str>,
        value: &str,
    ) -> CallResult<()> {
        if !is_ncname(local) {
            return Err(bad_name());
        }
        check_chars(value)?;
        let ns = ns.filter(|u| !u.is_empty());
        if let Some(u) = ns {
            check_chars(u)?;
        }
        if !self.tag_open {
            if self.depth > 0 && !self.last_text {
                return Ok(());
            }
            return Err(write_error(bsl_lib::xml::XmlWriteError::Order).into());
        }
        let key = (ns.unwrap_or_default().to_string(), local.to_string());
        if self.attrs.contains(&key) {
            return Err(xml_error(msg::XML_DUP_ATTR).into());
        }
        self.grow(ex, (local.len() + value.len()) as u64 + 8)?;
        if let Some(uri) = ns
            && self.w.prefix_of(uri).is_none_or(str::is_empty)
        {
            self.generated += 1;
            let prefix = format!("d{}p{}", self.depth, self.generated);
            self.w.namespace(&prefix, uri).map_err(write_error)?;
            self.scopes.push((self.depth, prefix, uri.to_string()));
        }
        self.w.attribute(local, ns, value).map_err(write_error)?;
        self.attrs.push(key);
        Ok(())
    }

    pub(crate) fn attribute_full(
        &mut self,
        ex: &mut Execution,
        full: &str,
        value: &str,
    ) -> CallResult<()> {
        if full == "xmlns" {
            return self.namespace(ex, "", value);
        }
        if let Some(p) = full.strip_prefix("xmlns:") {
            return self.namespace(ex, p, value);
        }
        let Some((prefix, local)) = split_qname(full) else {
            return Err(bad_name());
        };
        if prefix.is_empty() {
            return self.attribute(ex, local, None, value);
        }
        let uri = self
            .lookup_uri(prefix)
            .ok_or_else(|| xml_error(msg::XML_NS_MAPPING))?
            .to_string();
        self.attribute(ex, local, Some(&uri), value)
    }

    fn content_written(&mut self, text: bool) {
        self.tag_open = false;
        self.last_text = text;
    }

    pub(crate) fn text(&mut self, ex: &mut Execution, s: &str) -> CallResult<()> {
        self.grow(ex, (s.len() as u64).saturating_mul(5))?;
        self.w.text(s).map_err(|e| write_error_in(e, s))?;
        if !s.is_empty() {
            self.content_written(true);
        }
        Ok(())
    }

    pub(crate) fn raw(&mut self, ex: &mut Execution, s: &str) -> CallResult<()> {
        self.grow(ex, s.len() as u64)?;
        self.w.raw(s).map_err(|e| write_error_in(e, s))?;
        if !s.is_empty() {
            self.content_written(true);
        }
        Ok(())
    }

    pub(crate) fn comment(&mut self, ex: &mut Execution, s: &str) -> CallResult<()> {
        check_chars(s)?;
        self.grow(ex, s.len() as u64 + 8)?;
        self.w.comment(s).map_err(|e| write_error_in(e, s))?;
        self.started = true;
        self.content_written(false);
        Ok(())
    }

    pub(crate) fn cdata(&mut self, ex: &mut Execution, s: &str) -> CallResult<()> {
        self.grow(ex, (s.len() as u64).saturating_mul(2).saturating_add(16))?;
        self.w.cdata(s).map_err(|e| write_error_in(e, s))?;
        self.content_written(true);
        Ok(())
    }

    pub(crate) fn instruction(
        &mut self,
        ex: &mut Execution,
        target: &str,
        data: &str,
    ) -> CallResult<()> {
        if !is_ncname(target) {
            return Err(bad_name());
        }
        if target.eq_ignore_ascii_case("xml") {
            return Err(with_other(
                key_error(msg::XML_PI_TARGET, Vec::new(), format!(": {target}")),
                false,
            )
            .into());
        }
        check_chars(data)?;
        self.grow(ex, (target.len() + data.len()) as u64 + 8)?;
        self.w
            .instruction(target, data)
            .map_err(|e| write_error_in(e, data))?;
        self.started = true;
        self.content_written(false);
        Ok(())
    }

    pub(crate) fn end_element(&mut self, ex: &mut Execution) -> CallResult<()> {
        self.grow(ex, 8)?;
        self.w.end_element().map_err(write_error)?;
        let depth = self.depth;
        self.scopes.retain(|(d, _, _)| *d < depth);
        self.depth -= 1;
        self.tag_open = false;
        self.last_text = false;
        if self.depth == 0 {
            self.root_done = true;
        }
        Ok(())
    }

    fn set_indent(&mut self, on: bool) {
        self.settings.indent = on;
        self.settings_value = SettingsObject::new_value(self.settings.clone());
        self.w.set_indent(on);
    }
}

pub(crate) struct XmlWriterObject {
    st: RefCell<Option<XmlWriterState>>,
}

fn no_target() -> HostError {
    xml_error(msg::XML_NO_TARGET)
}

fn state_of(w: &XmlWriterObject) -> CallResult<RefMut<'_, XmlWriterState>> {
    let g = w.st.try_borrow_mut().map_err(|_| no_target())?;
    RefMut::filter_map(g, Option::as_mut).map_err(|_| no_target().into())
}

pub(crate) fn xml_writer<'a>(
    _ex: &mut Execution,
    v: &'a Value,
) -> CallResult<RefMut<'a, XmlWriterState>> {
    let Some(w) = v
        .as_object()
        .and_then(|o| o.as_any().downcast_ref::<XmlWriterObject>())
    else {
        return Err(type_mismatch(1).into());
    };
    state_of(w)
}

impl XmlWriterObject {
    fn settings_arg(
        ex: &mut Execution,
        a: Option<&Arg>,
        param: usize,
    ) -> CallResult<(Settings, Value, Encoding, bool)> {
        let v = arg_value(ex, a)?;
        if let Some(s) = SettingsObject::of(&v) {
            let (_, enc, bom) = encoding_arg(ex, &Value::from(s.encoding.as_str()), param)?;
            return Ok((s.clone(), v.clone(), enc, bom));
        }
        let (name, enc, bom) = match v {
            Value::Undefined => ("UTF-8".to_string(), Encoding::Utf8, true),
            v => encoding_arg(ex, &v, param)?,
        };
        let s = Settings {
            encoding: name,
            ..Settings::default()
        };
        Ok((s.clone(), SettingsObject::new_value(s), enc, bom))
    }

    fn check_not_writing(&self) -> CallResult<()> {
        let g = self.st.try_borrow().map_err(|_| no_target())?;
        if g.as_ref().is_some_and(|s| s.started) {
            return Err(xml_error(msg::XML_WRITING).into());
        }
        Ok(())
    }

    fn set_string(&self, ex: &mut Execution, a: Option<&Arg>) -> CallResult<()> {
        self.check_not_writing()?;
        let given = !matches!(arg_value(ex, a)?, Value::Undefined);
        let (s, sv, _, _) = Self::settings_arg(ex, a, 1)?;
        let declared = given.then(|| s.encoding.clone());
        *self.st.try_borrow_mut().map_err(|_| no_target())? =
            Some(XmlWriterState::new(s, sv, declared));
        Ok(())
    }

    fn open_file(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<()> {
        self.check_not_writing()?;
        let path = arg_str(ex, args.first())?.to_string_lossy();
        let (s, sv, enc, default_bom) = Self::settings_arg(ex, args.get(1), 2)?;
        let bom = if matches!(arg_value(ex, args.get(2))?, Value::Undefined) {
            default_bom
        } else {
            arg_bool(ex, args.get(2))?
        };
        let f = fs(ex, &path)?;
        f.get().write(&path, &[], false).map_err(fs_error)?;
        let mut st = XmlWriterState::new(s.clone(), sv, Some(s.encoding));
        st.file = Some(FileTarget {
            dest: Dest::File(path),
            enc,
            bom,
        });
        *self.st.try_borrow_mut().map_err(|_| no_target())? = Some(st);
        Ok(())
    }

    fn open_stream(&self, ex: &mut Execution, args: &[Arg]) -> CallResult<()> {
        self.check_not_writing()?;
        let first = arg_value(ex, args.first())?;
        let Some(stream) = stream_of(&first) else {
            return Err(type_mismatch(1).into());
        };
        let (s, sv, enc, default_bom) = Self::settings_arg(ex, args.get(1), 2)?;
        let bom = if matches!(arg_value(ex, args.get(2))?, Value::Undefined) {
            default_bom
        } else {
            arg_bool(ex, args.get(2))?
        };
        let mut st = XmlWriterState::new(s.clone(), sv, Some(s.encoding));
        st.file = Some(FileTarget {
            dest: Dest::Stream(stream),
            enc,
            bom,
        });
        *self.st.try_borrow_mut().map_err(|_| no_target())? = Some(st);
        Ok(())
    }

    fn close(&self, ex: &mut Execution) -> CallResult<Value> {
        let mut g = self.st.try_borrow_mut().map_err(|_| no_target())?;
        if let Some(st) = g.as_mut() {
            st.w.close_start_tag().map_err(write_error)?;
        }
        let Some(st) = g.take() else {
            return Ok(Value::Undefined);
        };
        drop(g);
        let target = st.file;
        let text = st.w.finish().map_err(write_error)?;
        let n = (text.len() as u64).saturating_mul(4);
        ex.charge(n / 64)?;
        ex.reserve(n)?;
        match target {
            None => Ok(Value::String(Str::from(text.as_str()))),
            Some(t) => {
                let units: Vec<u16> = text.encode_utf16().collect();
                drop(text);
                let bytes = codec::string_to_bytes(&units, t.enc, t.bom);
                match t.dest {
                    Dest::File(path) => {
                        let f = fs(ex, &path)?;
                        f.get().write(&path, &bytes, false).map_err(fs_error)?;
                    }
                    Dest::Stream(s) => write_stream(ex, &s, &bytes)?,
                }
                Ok(Value::Undefined)
            }
        }
    }
}

fn name_arg(ex: &mut Execution, a: Option<&Arg>) -> CallResult<String> {
    let s = arg_str(ex, a)?;
    ex.reserve((s.len() as u64).saturating_mul(3))?;
    Ok(s.to_string_lossy())
}

fn text_arg(ex: &mut Execution, a: Option<&Arg>) -> CallResult<String> {
    let s = arg_str(ex, a)?;
    ex.charge(s.len() as u64 / 64)?;
    match xml_invalid_pos(s.units(), false) {
        0 => {}
        p => return Err(bad_char_error(p as usize - 1, &shown_text(s.units())).into()),
    }
    ex.reserve((s.len() as u64).saturating_mul(3))?;
    Ok(s.to_string_lossy())
}

fn uri_arg(ex: &mut Execution, a: Option<&Arg>) -> CallResult<Option<String>> {
    let v = arg_value(ex, a)?;
    if matches!(v, Value::Undefined) {
        return Ok(None);
    }
    let s = text_arg(ex, a)?;
    Ok((!s.is_empty()).then_some(s))
}

impl HostObject for XmlWriterObject {
    fn type_id(&self) -> TypeId {
        TypeId::of_catalog(types().writer)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn presentation(&self, ex: &mut Execution) -> CallResult<Option<Str>> {
        Ok(Some(crate::collections::type_name(ex, types().writer)))
    }

    fn get(&self, _ex: &mut Execution, m: MemberId) -> CallResult<Value> {
        let t = types().writer;
        match property_name(t, m) {
            Some("Indent") => Ok(Value::Bool(state_of(self)?.settings.indent)),
            Some("Settings") => Ok(state_of(self)?.settings_value.clone()),
            _ => Err(not_implemented(t, m).into()),
        }
    }

    fn set(&self, ex: &mut Execution, m: MemberId, v: Value) -> CallResult<()> {
        let t = types().writer;
        match property_name(t, m) {
            Some("Indent") => {
                let on = bsl_vm::ops::to_bool(ex, &v)?;
                state_of(self)?.set_indent(on);
                Ok(())
            }
            _ => Err(read_only(t, m)),
        }
    }

    fn call(&self, ex: &mut Execution, m: MemberId, args: &mut [Arg]) -> CallResult<Value> {
        let t = types().writer;
        let Some(name) = method_name(t, m) else {
            return Err(not_implemented(t, m).into());
        };
        let args: &[Arg] = args;
        match name {
            "SetString" => self.set_string(ex, args.first())?,
            "OpenFile" => self.open_file(ex, args)?,
            "OpenStream" => self.open_stream(ex, args)?,
            "Close" => return self.close(ex),
            "WriteXMLDeclaration" => state_of(self)?.declaration(ex)?,
            "WriteStartElement" => {
                if args.len() >= 2 {
                    let local = name_arg(ex, args.first())?;
                    let ns = uri_arg(ex, args.get(1))?;
                    state_of(self)?.start_element(ex, &local, ns.as_deref())?;
                } else {
                    let full = name_arg(ex, args.first())?;
                    state_of(self)?.start_element_full(ex, &full)?;
                }
            }
            "WriteEndElement" => state_of(self)?.end_element(ex)?,
            "WriteAttribute" => {
                if args.len() >= 3 {
                    let local = name_arg(ex, args.first())?;
                    let ns = uri_arg(ex, args.get(1))?;
                    let value = text_arg(ex, args.get(2))?;
                    state_of(self)?.attribute(ex, &local, ns.as_deref(), &value)?;
                } else {
                    let full = name_arg(ex, args.first())?;
                    let value = text_arg(ex, args.get(1))?;
                    state_of(self)?.attribute_full(ex, &full, &value)?;
                }
            }
            "WriteNamespaceMapping" => {
                let prefix = name_arg(ex, args.first())?;
                let uri = text_arg(ex, args.get(1))?;
                state_of(self)?.namespace(ex, &prefix, &uri)?;
            }
            "WriteText" => {
                let s = text_arg(ex, args.first())?;
                state_of(self)?.text(ex, &s)?;
            }
            "WriteRaw" => {
                let s = text_arg(ex, args.first())?;
                state_of(self)?.raw(ex, &s)?;
            }
            "WriteComment" => {
                let s = text_arg(ex, args.first())?;
                state_of(self)?.comment(ex, &s)?;
            }
            "WriteCDATASection" => {
                let s = text_arg(ex, args.first())?;
                state_of(self)?.cdata(ex, &s)?;
            }
            "WriteProcessingInstruction" => {
                let target = name_arg(ex, args.first())?;
                let data = text_arg(ex, args.get(1))?;
                state_of(self)?.instruction(ex, &target, &data)?;
            }
            _ => return Err(not_implemented(t, m).into()),
        }
        Ok(Value::Undefined)
    }
}

fn ctor(_ex: &mut Execution, args: &mut [Arg]) -> CallResult<Value> {
    if !args.is_empty() {
        return Err(ctor_not_implemented(types().writer).into());
    }
    Ok(Value::Object(Rc::new(XmlWriterObject {
        st: RefCell::new(None),
    })))
}

pub(super) fn register(reg: &mut Registry) {
    reg.type_impl(
        types().writer,
        TypeImpl {
            ctor: Some(Arc::new(ctor)),
        },
    );
    reg.type_impl(
        types().settings,
        TypeImpl {
            ctor: Some(Arc::new(settings_ctor)),
        },
    );
}
