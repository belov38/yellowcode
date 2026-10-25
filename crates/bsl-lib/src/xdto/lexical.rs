use super::model::{
    Facets, Model, ObjectType, Package, QName, TypeDef, ValueType, Variety, WhiteSpace,
};
use crate::xml::{is_name_char, is_name_start};
use regex::{Regex, RegexBuilder};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

pub const XS_NS: &str = "http://www.w3.org/2001/XMLSchema";

const MAX_CHAIN: usize = 64;
const MAX_NEST: usize = 16;
pub(crate) const MAX_PATTERN_CHARS: usize = 4096;
pub(crate) const MAX_ENUMERATION: usize = 10_000;
pub(crate) const MAX_PATTERNS_PER_TYPE: usize = 64;
const REGEX_SIZE_LIMIT: usize = 2 << 20;
const REGEX_CACHE: usize = 128;
const ENUM_CACHE: usize = 256;
const WORK_BASE: usize = 10_000;
const WORK_PER_BYTE: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexError {
    pub lines: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rule {
    None,
    Decimal,
    Integer,
    Float,
    Double,
    Bool,
    DateTime,
    Date,
    Time,
    Duration,
    GYear,
    GYearMonth,
    GMonth,
    GMonthDay,
    GDay,
    Hex,
    B64,
    Uri,
    QName,
    Name,
    NCName,
    NMToken,
    Language,
}

struct Spec {
    name: &'static str,
    base: Option<&'static str>,
    rule: Rule,
    ws: Option<WhiteSpace>,
    min: Option<&'static str>,
    max: Option<&'static str>,
    list_item: Option<&'static str>,
}

const fn s(
    name: &'static str,
    base: Option<&'static str>,
    rule: Rule,
    ws: Option<WhiteSpace>,
) -> Spec {
    Spec {
        name,
        base,
        rule,
        ws,
        min: None,
        max: None,
        list_item: None,
    }
}

const fn range(
    name: &'static str,
    base: &'static str,
    min: Option<&'static str>,
    max: Option<&'static str>,
) -> Spec {
    Spec {
        name,
        base: Some(base),
        rule: Rule::None,
        ws: None,
        min,
        max,
        list_item: None,
    }
}

const fn list(name: &'static str, item: &'static str) -> Spec {
    Spec {
        name,
        base: Some("anySimpleType"),
        rule: Rule::None,
        ws: Some(WhiteSpace::Collapse),
        min: None,
        max: None,
        list_item: Some(item),
    }
}

use WhiteSpace::{Collapse, Preserve, Replace};

const SPECS: &[Spec] = &[
    s("anySimpleType", None, Rule::None, None),
    s("string", Some("anySimpleType"), Rule::None, Some(Preserve)),
    s(
        "normalizedString",
        Some("string"),
        Rule::None,
        Some(Replace),
    ),
    s(
        "token",
        Some("normalizedString"),
        Rule::None,
        Some(Collapse),
    ),
    s("language", Some("token"), Rule::Language, None),
    s("NMTOKEN", Some("token"), Rule::NMToken, None),
    s("Name", Some("token"), Rule::Name, None),
    s("NCName", Some("Name"), Rule::NCName, None),
    s("ID", Some("NCName"), Rule::None, None),
    s("IDREF", Some("NCName"), Rule::None, None),
    s("ENTITY", Some("NCName"), Rule::None, None),
    list("NMTOKENS", "NMTOKEN"),
    list("IDREFS", "IDREF"),
    list("ENTITIES", "ENTITY"),
    s(
        "NOTATION",
        Some("anySimpleType"),
        Rule::QName,
        Some(Collapse),
    ),
    s("QName", Some("anySimpleType"), Rule::QName, Some(Collapse)),
    s("anyURI", Some("anySimpleType"), Rule::Uri, Some(Collapse)),
    s("boolean", Some("anySimpleType"), Rule::Bool, Some(Collapse)),
    s(
        "base64Binary",
        Some("anySimpleType"),
        Rule::B64,
        Some(Collapse),
    ),
    s(
        "hexBinary",
        Some("anySimpleType"),
        Rule::Hex,
        Some(Collapse),
    ),
    s("float", Some("anySimpleType"), Rule::Float, Some(Collapse)),
    s(
        "double",
        Some("anySimpleType"),
        Rule::Double,
        Some(Collapse),
    ),
    s(
        "decimal",
        Some("anySimpleType"),
        Rule::Decimal,
        Some(Collapse),
    ),
    s("integer", Some("decimal"), Rule::Integer, None),
    range("nonPositiveInteger", "integer", None, Some("0")),
    range("negativeInteger", "nonPositiveInteger", None, Some("-1")),
    range(
        "long",
        "integer",
        Some("-9223372036854775808"),
        Some("9223372036854775807"),
    ),
    range("int", "long", Some("-2147483648"), Some("2147483647")),
    range("short", "int", Some("-32768"), Some("32767")),
    range("byte", "short", Some("-128"), Some("127")),
    range("nonNegativeInteger", "integer", Some("0"), None),
    range(
        "unsignedLong",
        "nonNegativeInteger",
        None,
        Some("18446744073709551615"),
    ),
    range("unsignedInt", "unsignedLong", None, Some("4294967295")),
    range("unsignedShort", "unsignedInt", None, Some("65535")),
    range("unsignedByte", "unsignedShort", None, Some("255")),
    range("positiveInteger", "nonNegativeInteger", Some("1"), None),
    s(
        "dateTime",
        Some("anySimpleType"),
        Rule::DateTime,
        Some(Collapse),
    ),
    s("date", Some("anySimpleType"), Rule::Date, Some(Collapse)),
    s("time", Some("anySimpleType"), Rule::Time, Some(Collapse)),
    s(
        "duration",
        Some("anySimpleType"),
        Rule::Duration,
        Some(Collapse),
    ),
    s("gYear", Some("anySimpleType"), Rule::GYear, Some(Collapse)),
    s(
        "gYearMonth",
        Some("anySimpleType"),
        Rule::GYearMonth,
        Some(Collapse),
    ),
    s(
        "gMonth",
        Some("anySimpleType"),
        Rule::GMonth,
        Some(Collapse),
    ),
    s(
        "gMonthDay",
        Some("anySimpleType"),
        Rule::GMonthDay,
        Some(Collapse),
    ),
    s("gDay", Some("anySimpleType"), Rule::GDay, Some(Collapse)),
];

struct Builtin {
    vt: ValueType,
    rule: Rule,
}

fn builtins() -> &'static HashMap<&'static str, Builtin> {
    static B: OnceLock<HashMap<&'static str, Builtin>> = OnceLock::new();
    B.get_or_init(|| {
        let q = |n: &str| QName::new(XS_NS, n);
        let mut m = HashMap::new();
        for sp in SPECS {
            let facets = Facets {
                white_space: sp.ws,
                min_inclusive: sp.min.map(str::to_string),
                max_inclusive: sp.max.map(str::to_string),
                ..Facets::default()
            };
            let variety = match sp.list_item {
                Some(i) => Variety::List { item: q(i) },
                None => Variety::Atomic,
            };
            m.insert(
                sp.name,
                Builtin {
                    vt: ValueType {
                        name: q(sp.name),
                        base: sp.base.map(q),
                        variety,
                        facets,
                    },
                    rule: sp.rule,
                },
            );
        }
        m
    })
}

pub fn builtin_types() -> Vec<TypeDef> {
    let mut v: Vec<TypeDef> = SPECS
        .iter()
        .filter_map(|sp| builtins().get(sp.name))
        .map(|b| TypeDef::Value(b.vt.clone()))
        .collect();
    v.push(TypeDef::Object(ObjectType {
        name: QName::new(XS_NS, "anyType"),
        base: None,
        open: true,
        abstract_: false,
        mixed: true,
        sequenced: false,
        ordered: true,
        properties: Vec::new(),
    }));
    v
}

pub fn is_builtin(name: &str) -> bool {
    name == "anyType" || builtins().contains_key(name)
}

#[derive(Clone, Copy)]
struct Step<'a> {
    vt: &'a ValueType,
    at: Option<(&'a Arc<Package>, usize)>,
}

struct Chain<'a> {
    steps: Vec<Step<'a>>,
}

struct Work<'a> {
    left: usize,
    stack: Vec<&'a QName>,
    item_failure: Option<Vec<String>>,
}

fn rule_of(vt: &ValueType) -> Rule {
    if vt.name.ns == XS_NS {
        builtins()
            .get(vt.name.name.as_str())
            .map_or(Rule::None, |b| b.rule)
    } else {
        Rule::None
    }
}

fn lookup<'a>(m: &'a Model, q: &QName) -> Option<Step<'a>> {
    if q.ns == XS_NS
        && let Some(b) = builtins().get(q.name.as_str())
    {
        return Some(Step {
            vt: &b.vt,
            at: None,
        });
    }
    let (p, ti) = m.type_at(q)?;
    match &p.types[ti] {
        TypeDef::Value(v) => Some(Step {
            vt: v,
            at: Some((p, ti)),
        }),
        TypeDef::Object(_) => None,
    }
}

fn chain<'a>(m: &'a Model, t: &QName) -> Option<Chain<'a>> {
    let mut steps = Vec::new();
    let mut cur = lookup(m, t)?;
    loop {
        steps.push(cur);
        if steps.len() > MAX_CHAIN {
            return None;
        }
        match &cur.vt.base {
            Some(b) => match lookup(m, b) {
                Some(n) => cur = n,
                None => break,
            },
            None => break,
        }
    }
    Some(Chain { steps })
}

pub fn check_lexical(m: &Model, t: &QName, text: &str) -> Result<String, LexError> {
    let mut w = Work {
        left: WORK_BASE + WORK_PER_BYTE * text.len(),
        stack: Vec::new(),
        item_failure: None,
    };
    match check_type(m, t, text, 0, &mut w) {
        Ok(c) => Ok(c),
        Err(mut detail) => {
            if let Some(lines) = w.item_failure.take() {
                return Err(LexError { lines });
            }
            let mut lines = vec![head(text, t)];
            lines.append(&mut detail);
            Err(LexError { lines })
        }
    }
}

fn head(text: &str, t: &QName) -> String {
    format!(
        "Значение: '{text}' не соответствует простому типу: {}",
        t.clark()
    )
}

fn check_type<'a>(
    m: &'a Model,
    t: &'a QName,
    text: &str,
    depth: usize,
    w: &mut Work<'a>,
) -> Result<String, Vec<String>> {
    if depth > MAX_NEST || w.left == 0 || w.stack.contains(&t) {
        return Err(Vec::new());
    }
    w.left -= 1;
    w.stack.push(t);
    let r = check_inner(m, t, text, depth, w);
    w.stack.pop();
    r
}

fn check_inner<'a>(
    m: &'a Model,
    t: &QName,
    text: &str,
    depth: usize,
    w: &mut Work<'a>,
) -> Result<String, Vec<String>> {
    if t.ns == XS_NS && t.name == "anyType" {
        return Ok(text.to_string());
    }
    let Some(ch) = chain(m, t) else {
        return Err(Vec::new());
    };
    let ws = ch
        .steps
        .iter()
        .filter(|s| s.at.is_none())
        .find_map(|s| s.vt.facets.white_space)
        .unwrap_or(Preserve);
    let v = apply_ws(text, ws);
    let variety = ch
        .steps
        .iter()
        .find(|s| !matches!(s.vt.variety, Variety::Atomic))
        .map(|s| &s.vt.variety);
    let kind = ch
        .steps
        .iter()
        .rev()
        .map(|s| rule_of(s.vt))
        .find(|r| *r != Rule::None)
        .unwrap_or(Rule::None);
    let mut items = 0usize;
    let canon = match variety {
        Some(Variety::List { item }) => {
            let mut out = Vec::new();
            for part in v.split(' ').filter(|p| !p.is_empty()) {
                match check_type(m, item, part, depth + 1, w) {
                    Ok(c) => out.push(c),
                    Err(detail) => {
                        let mut lines = vec![head(part, item)];
                        lines.extend(detail);
                        w.item_failure = Some(lines);
                        return Err(Vec::new());
                    }
                }
            }
            items = out.len();
            out.join(" ")
        }
        Some(Variety::Union { members }) => {
            let mut found = None;
            for mem in members {
                if let Ok(c) = check_type(m, mem, &v, depth + 1, w) {
                    found = Some(c);
                    break;
                }
            }
            match found {
                Some(c) => {
                    w.item_failure = None;
                    c
                }
                None => {
                    w.item_failure = None;
                    return Err(Vec::new());
                }
            }
        }
        _ => {
            for st in &ch.steps {
                check_rule(rule_of(st.vt), &v)?;
            }
            canonical(kind, &v)
        }
    };
    let is_list = matches!(variety, Some(Variety::List { .. }));
    for st in &ch.steps {
        if let Some(line) = check_facets(st, &v, &canon, kind, is_list, items)? {
            return Err(vec![line]);
        }
    }
    Ok(canon)
}

fn enum_key(kind: Rule, s: &str) -> String {
    match kind {
        Rule::Decimal | Rule::Integer => {
            Dec::parse(s).map_or_else(|| s.to_string(), |d| d.render())
        }
        Rule::Float | Rule::Double => match float_value(s) {
            Some(x) if x.is_nan() => "NaN".to_string(),
            Some(0.0) => "0".to_string(),
            Some(x) => format!("{x:e}"),
            None => s.to_string(),
        },
        Rule::DateTime
        | Rule::Date
        | Rule::Time
        | Rule::GYear
        | Rule::GYearMonth
        | Rule::GMonth
        | Rule::GMonthDay
        | Rule::GDay => time_key(kind, s).map_or_else(|| s.to_string(), |k| format!("{k:?}")),
        Rule::Bool | Rule::Hex => canonical(kind, s),
        _ => s.to_string(),
    }
}

type EnumKey = (usize, usize, u8);
type EnumCache = HashMap<EnumKey, (Arc<Package>, Arc<HashSet<String>>)>;

fn enum_set(step: &Step, kind: Rule) -> Arc<HashSet<String>> {
    let build = || -> HashSet<String> {
        step.vt
            .facets
            .enumeration
            .iter()
            .map(|e| enum_key(kind, e))
            .collect()
    };
    let Some((pkg, ti)) = step.at else {
        return Arc::new(build());
    };
    static CACHE: OnceLock<Mutex<EnumCache>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (Arc::as_ptr(pkg) as usize, ti, kind as u8);
    if let Ok(g) = cache.lock()
        && let Some((_, set)) = g.get(&key)
    {
        return set.clone();
    }
    let set = Arc::new(build());
    if let Ok(mut g) = cache.lock() {
        if g.len() >= ENUM_CACHE
            && let Some(k) = g.keys().next().copied()
        {
            g.remove(&k);
        }
        g.insert(key, (pkg.clone(), set.clone()));
    }
    set
}

fn apply_ws(s: &str, ws: WhiteSpace) -> String {
    match ws {
        WhiteSpace::Preserve => s.to_string(),
        WhiteSpace::Replace => s
            .chars()
            .map(|c| {
                if matches!(c, '\t' | '\n' | '\r') {
                    ' '
                } else {
                    c
                }
            })
            .collect(),
        WhiteSpace::Collapse => {
            let mut out = String::with_capacity(s.len());
            for part in s.split([' ', '\t', '\n', '\r']).filter(|p| !p.is_empty()) {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(part);
            }
            out
        }
    }
}

fn check_facets(
    step: &Step,
    v: &str,
    canon: &str,
    kind: Rule,
    is_list: bool,
    items: usize,
) -> Result<Option<String>, Vec<String>> {
    let f = &step.vt.facets;
    let len = if is_list {
        items as u64
    } else {
        value_length(kind, v)
    };
    let fail = |name: &str, val: &str| Ok(Some(format!("Несоответствие фасету {name} = '{val}'")));
    if !f.patterns.is_empty() {
        let mut any = false;
        for p in &f.patterns {
            if let Ok(re) = compile_pattern(p)
                && re.is_match(v)
            {
                any = true;
                break;
            }
        }
        if !any {
            return Ok(Some("Значение не соответствует образцу".to_string()));
        }
    }
    if !f.enumeration.is_empty() && !enum_set(step, kind).contains(&enum_key(kind, canon)) {
        return Ok(Some(
            "Значение не соответствует значениям фасета перечисления".to_string(),
        ));
    }
    if let Some(n) = f.length
        && len != n
    {
        return fail("Length", &n.to_string());
    }
    if let Some(n) = f.min_length
        && len < n
    {
        return fail("MinLength", &n.to_string());
    }
    if let Some(n) = f.max_length
        && len > n
    {
        return fail("MaxLength", &n.to_string());
    }
    type Bound<'a> = (&'a Option<String>, &'a str, fn(Ordering) -> bool);
    let bounds: [Bound; 4] = [
        (&f.min_inclusive, "MinInclusive", |o| o != Ordering::Less),
        (&f.max_inclusive, "MaxInclusive", |o| o != Ordering::Greater),
        (&f.min_exclusive, "MinExclusive", |o| o == Ordering::Greater),
        (&f.max_exclusive, "MaxExclusive", |o| o == Ordering::Less),
    ];
    for (b, name, ok) in bounds {
        if let Some(b) = b
            && let Some(o) = compare(kind, canon, b)
            && !ok(o)
        {
            return fail(name, b);
        }
    }
    if matches!(kind, Rule::Decimal)
        && let Some(d) = Dec::parse(canon)
    {
        if let Some(n) = f.total_digits
            && d.total_digits() > n as usize
        {
            return fail("TotalDigits", &n.to_string());
        }
        if let Some(n) = f.fraction_digits
            && d.frac.len() > n as usize
        {
            return fail("FractionDigits", &n.to_string());
        }
    }
    Ok(None)
}

fn value_length(kind: Rule, v: &str) -> u64 {
    match kind {
        Rule::Hex => (v.len() / 2) as u64,
        Rule::B64 => {
            let s: Vec<u8> = v.bytes().filter(|b| *b != b' ').collect();
            let pad = s.iter().rev().take_while(|b| **b == b'=').count();
            (s.len() / 4 * 3).saturating_sub(pad) as u64
        }
        _ => v.chars().count() as u64,
    }
}

#[derive(Debug)]
struct Dec {
    neg: bool,
    int: String,
    frac: String,
}

impl Dec {
    fn parse(s: &str) -> Option<Dec> {
        let (neg, rest) = match s.as_bytes().first()? {
            b'-' => (true, &s[1..]),
            b'+' => (false, &s[1..]),
            _ => (false, s),
        };
        let (i, f) = match rest.split_once('.') {
            Some((i, f)) => (i, f),
            None => (rest, ""),
        };
        if (i.is_empty() && f.is_empty())
            || !i.bytes().all(|b| b.is_ascii_digit())
            || !f.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let int = i.trim_start_matches('0');
        let frac = f.trim_end_matches('0');
        let zero = int.is_empty() && frac.is_empty();
        Some(Dec {
            neg: neg && !zero,
            int: if int.is_empty() {
                "0".into()
            } else {
                int.into()
            },
            frac: frac.into(),
        })
    }

    fn render(&self) -> String {
        let mut s = String::new();
        if self.neg {
            s.push('-');
        }
        s.push_str(&self.int);
        if !self.frac.is_empty() {
            s.push('.');
            s.push_str(&self.frac);
        }
        s
    }

    fn total_digits(&self) -> usize {
        let i = if self.int == "0" { 0 } else { self.int.len() };
        (i + self.frac.len()).max(1)
    }

    fn cmp(&self, o: &Dec) -> Ordering {
        let mag = |a: &Dec, b: &Dec| {
            a.int
                .len()
                .cmp(&b.int.len())
                .then_with(|| a.int.cmp(&b.int))
                .then_with(|| a.frac.cmp(&b.frac))
        };
        match (self.neg, o.neg) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => mag(self, o),
            (true, true) => mag(o, self),
        }
    }
}

fn canonical(kind: Rule, v: &str) -> String {
    match kind {
        Rule::Decimal => Dec::parse(v).map_or_else(|| v.to_string(), |d| d.render()),
        Rule::Bool => match v.to_ascii_lowercase().as_str() {
            "1" | "true" => "true".into(),
            _ => "false".into(),
        },
        Rule::Float | Rule::Double if matches!(v, "NaN" | "INF" | "-INF" | "+INF") => "0".into(),
        Rule::Hex => v.to_ascii_uppercase(),
        Rule::B64 => v.chars().filter(|c| *c != ' ').collect(),
        _ => v.to_string(),
    }
}

fn compare(kind: Rule, a: &str, b: &str) -> Option<Ordering> {
    match kind {
        Rule::Decimal | Rule::Integer => Some(Dec::parse(a)?.cmp(&Dec::parse(b)?)),
        Rule::Float | Rule::Double => {
            let (x, y) = (float_value(a)?, float_value(b)?);
            x.partial_cmp(&y)
        }
        Rule::DateTime
        | Rule::Date
        | Rule::Time
        | Rule::GYear
        | Rule::GYearMonth
        | Rule::GMonth
        | Rule::GMonthDay
        | Rule::GDay => Some(time_key(kind, a)?.cmp(&time_key(kind, b)?)),
        Rule::Bool => Some(canonical(kind, a).cmp(&canonical(kind, b))),
        Rule::Hex => Some(a.to_ascii_uppercase().cmp(&b.to_ascii_uppercase())),
        _ => (a == b).then_some(Ordering::Equal),
    }
}

fn float_value(s: &str) -> Option<f64> {
    match s {
        "INF" => Some(f64::INFINITY),
        "-INF" => Some(f64::NEG_INFINITY),
        "NaN" => Some(f64::NAN),
        _ => s.parse().ok(),
    }
}

fn check_rule(r: Rule, v: &str) -> Result<(), Vec<String>> {
    let ok = match r {
        Rule::None => true,
        Rule::Decimal => Dec::parse(v).is_some(),
        Rule::Integer => {
            let d = v.strip_prefix(['+', '-']).unwrap_or(v);
            let (i, f) = d.split_once('.').unwrap_or((d, ""));
            !i.is_empty() && i.bytes().all(|b| b.is_ascii_digit()) && f.bytes().all(|b| b == b'0')
        }
        Rule::Float | Rule::Double => is_float(v),
        Rule::Bool => matches!(
            v.to_ascii_lowercase().as_str(),
            "true" | "false" | "1" | "0"
        ),
        Rule::Hex => v.len().is_multiple_of(2) && v.bytes().all(|b| b.is_ascii_hexdigit()),
        Rule::B64 | Rule::Uri | Rule::Name | Rule::NCName => true,
        Rule::QName => {
            let mut p = v.split(':');
            match (p.next(), p.next(), p.next()) {
                (Some(a), None, _) => is_ncname(a),
                (Some(a), Some(b), None) => is_ncname(a) && is_ncname(b),
                _ => false,
            }
        }
        Rule::NMToken => !v.is_empty() && v.chars().all(is_name_char),
        Rule::Language => is_language(v),
        Rule::Duration => {
            if is_duration(v) {
                true
            } else {
                return Err(duration_detail(v));
            }
        }
        Rule::DateTime | Rule::Date => {
            if time_key(r, v).is_some() {
                true
            } else {
                return Err(date_detail(v));
            }
        }
        Rule::Time
        | Rule::GYear
        | Rule::GYearMonth
        | Rule::GMonth
        | Rule::GMonthDay
        | Rule::GDay => time_key(r, v).is_some(),
    };
    if ok { Ok(()) } else { Err(Vec::new()) }
}

fn duration_detail(v: &str) -> Vec<String> {
    let s = v.strip_prefix('-').unwrap_or(v);
    let Some(s) = s.strip_prefix('P') else {
        return Vec::new();
    };
    let date = s.split_once('T').map_or(s, |(d, _)| d);
    if date
        .chars()
        .any(|c| c.is_alphabetic() && !"YMD".contains(c))
    {
        vec!["(Duration has invalid stuff before 'T'!)".to_string()]
    } else {
        Vec::new()
    }
}

fn date_detail(v: &str) -> Vec<String> {
    let date = v.split_once('T').map_or(v, |(d, _)| d);
    let date = date.strip_prefix('-').unwrap_or(date);
    let parts: Vec<&str> = date.split('-').collect();
    let digits =
        |p: &str, n: usize| !p.is_empty() && p.len() <= n && p.bytes().all(|b| b.is_ascii_digit());
    if parts.len() == 3
        && digits(parts[0], 18)
        && digits(parts[1], 2)
        && digits(parts[2], 2)
        && (parts[1].len() < 2 || parts[2].len() < 2)
    {
        vec!["(Incomplete Date!)".to_string()]
    } else {
        Vec::new()
    }
}

fn is_name(v: &str) -> bool {
    let mut it = v.chars();
    it.next().is_some_and(is_name_start) && it.all(is_name_char)
}

fn is_ncname(v: &str) -> bool {
    is_name(v) && !v.contains(':')
}

fn is_language(v: &str) -> bool {
    let mut parts = v.split('-');
    let first = parts.next().unwrap_or("");
    (1..=8).contains(&first.len())
        && first.bytes().all(|b| b.is_ascii_alphabetic())
        && parts.all(|p| (1..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()))
}

fn is_float(v: &str) -> bool {
    if matches!(v, "INF" | "-INF" | "NaN") {
        return true;
    }
    let b = v.as_bytes();
    let mut i = usize::from(matches!(b.first(), Some(b'+' | b'-')));
    let d0 = i;
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    let mut digits = i - d0;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let f0 = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        digits += i - f0;
    }
    if digits == 0 {
        return false;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let e0 = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == e0 {
            return false;
        }
    }
    i == b.len()
}

fn is_duration(v: &str) -> bool {
    let s = v.strip_prefix('-').unwrap_or(v);
    let Some(s) = s.strip_prefix('P') else {
        return false;
    };
    let (date, time) = match s.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (s, None),
    };
    let comps = |part: &str, units: &[u8], frac_last: bool| -> Option<usize> {
        let b = part.as_bytes();
        let (mut i, mut ui, mut n) = (0, 0, 0);
        while i < b.len() {
            let d0 = i;
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
            if i == d0 {
                return None;
            }
            let mut last = false;
            if frac_last && b.get(i) == Some(&b'.') {
                i += 1;
                let f0 = i;
                while b.get(i).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                }
                if i == f0 {
                    return None;
                }
                last = true;
            }
            let u = *b.get(i)?;
            let pos = units[ui..].iter().position(|x| *x == u)? + ui;
            if last && u != b'S' {
                return None;
            }
            ui = pos + 1;
            i += 1;
            n += 1;
        }
        Some(n)
    };
    let nd = comps(date, b"YMD", false);
    let nt = time.map(|t| comps(t, b"HMS", true));
    match (nd, nt) {
        (Some(d), None) => d > 0,
        (Some(d), Some(Some(t))) => t > 0 && d <= 3,
        _ => false,
    }
}

fn time_key(kind: Rule, v: &str) -> Option<(i64, u32, u32, u32, u32, u32, String)> {
    let b = v.as_bytes();
    let mut i = 0;
    let digits = |i: &mut usize, n: usize| -> Option<u32> {
        let part = b.get(*i..*i + n)?;
        if !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        *i += n;
        std::str::from_utf8(part).ok()?.parse().ok()
    };
    let lit = |i: &mut usize, c: u8| -> Option<()> { (b.get(*i) == Some(&c)).then(|| *i += 1) };
    let (mut y, mut mo, mut d, mut h, mut mi, mut se, mut frac) =
        (0i64, 1u32, 1u32, 0u32, 0u32, 0u32, String::new());
    let has_year = matches!(
        kind,
        Rule::DateTime | Rule::Date | Rule::GYear | Rule::GYearMonth
    );
    if has_year {
        let neg = b.first() == Some(&b'-');
        if neg {
            i += 1;
        }
        let y0 = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i - y0 < 4 || i - y0 > 18 || (i - y0 > 4 && b[y0] == b'0') {
            return None;
        }
        y = v[y0..i].parse().ok()?;
        if y == 0 {
            return None;
        }
        if neg {
            y = -y;
        }
    }
    match kind {
        Rule::GYear => {}
        Rule::GMonth => {
            lit(&mut i, b'-')?;
            lit(&mut i, b'-')?;
            mo = digits(&mut i, 2)?;
        }
        Rule::GMonthDay => {
            lit(&mut i, b'-')?;
            lit(&mut i, b'-')?;
            mo = digits(&mut i, 2)?;
            lit(&mut i, b'-')?;
            d = digits(&mut i, 2)?;
        }
        Rule::GDay => {
            for _ in 0..3 {
                lit(&mut i, b'-')?;
            }
            d = digits(&mut i, 2)?;
        }
        Rule::GYearMonth => {
            lit(&mut i, b'-')?;
            mo = digits(&mut i, 2)?;
        }
        Rule::Date | Rule::DateTime => {
            lit(&mut i, b'-')?;
            mo = digits(&mut i, 2)?;
            lit(&mut i, b'-')?;
            d = digits(&mut i, 2)?;
        }
        _ => {}
    }
    if matches!(kind, Rule::DateTime | Rule::Time) {
        if kind == Rule::DateTime {
            lit(&mut i, b'T')?;
        }
        h = digits(&mut i, 2)?;
        lit(&mut i, b':')?;
        mi = digits(&mut i, 2)?;
        lit(&mut i, b':')?;
        se = digits(&mut i, 2)?;
        if b.get(i) == Some(&b'.') {
            i += 1;
            let f0 = i;
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
            if i == f0 {
                return None;
            }
            frac = v[f0..i].trim_end_matches('0').to_string();
        }
        if h > 24
            || mi > 59
            || se > 59
            || (h == 24 && (mi, se, !frac.is_empty() as u32) != (0, 0, 0))
        {
            return None;
        }
    }
    if i < b.len() {
        if b[i] == b'Z' {
            i += 1;
        } else if matches!(b[i], b'+' | b'-') {
            i += 1;
            let th = digits(&mut i, 2)?;
            lit(&mut i, b':')?;
            let tm = digits(&mut i, 2)?;
            if th > 14 || tm > 59 || (th == 14 && tm != 0) {
                return None;
            }
        }
    }
    if i != b.len() {
        return None;
    }
    if !(1..=12).contains(&mo) {
        return None;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let dim = match mo {
        2 => {
            if leap && has_year {
                29
            } else if has_year {
                28
            } else {
                29
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if d < 1 || d > dim {
        return None;
    }
    Some((y, mo, d, h, mi, se, frac))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelFault {
    pub code: &'static str,
    pub ty: QName,
    pub args: Vec<String>,
}

pub fn check_package_facets(m: &Model, ns: &str) -> Result<(), ModelFault> {
    let Some(pkg) = m.packages.iter().find(|p| p.ns == ns) else {
        return Ok(());
    };
    for def in &pkg.types {
        let TypeDef::Value(vt) = def else { continue };
        let f = &vt.facets;
        let Some(ch) = chain(m, &vt.name) else {
            continue;
        };
        if ch
            .steps
            .iter()
            .any(|s| !matches!(s.vt.variety, Variety::Atomic))
        {
            continue;
        }
        let kind = ch
            .steps
            .iter()
            .rev()
            .map(|s| rule_of(s.vt))
            .find(|r| *r != Rule::None)
            .unwrap_or(Rule::None);
        let ordered = matches!(
            kind,
            Rule::Decimal
                | Rule::Integer
                | Rule::Float
                | Rule::Double
                | Rule::DateTime
                | Rule::Date
                | Rule::Time
                | Rule::Duration
                | Rule::GYear
                | Rule::GYearMonth
                | Rule::GMonth
                | Rule::GMonthDay
                | Rule::GDay
        );
        let digits = matches!(kind, Rule::Decimal | Rule::Integer);
        let cannot = |name: &str| ModelFault {
            code: "xdto-valueType-5.1.3",
            ty: vt.name.clone(),
            args: vec![vt.name.clark(), name.to_string()],
        };
        let bounds = [
            (&f.min_inclusive, "minInclusive"),
            (&f.max_inclusive, "maxInclusive"),
            (&f.min_exclusive, "minExclusive"),
            (&f.max_exclusive, "maxExclusive"),
        ];
        if !ordered && let Some((_, n)) = bounds.iter().find(|(b, _)| b.is_some()) {
            return Err(cannot(n));
        }
        if !digits {
            if f.total_digits.is_some() {
                return Err(cannot("totalDigits"));
            }
            if f.fraction_digits.is_some() {
                return Err(cannot("fractionDigits"));
            }
        }
        let lows = [(&f.min_inclusive, false), (&f.min_exclusive, true)];
        let highs = [(&f.max_inclusive, false), (&f.max_exclusive, true)];
        for (lo, lo_x) in lows {
            for (hi, hi_x) in highs {
                if let (Some(lo), Some(hi)) = (lo, hi) {
                    let bad = match compare(kind, lo, hi) {
                        Some(Ordering::Greater) => true,
                        Some(Ordering::Equal) => lo_x || hi_x,
                        _ => false,
                    };
                    if bad {
                        return Err(ModelFault {
                            code: "xdto-facet-7.1",
                            ty: vt.name.clone(),
                            args: vec!["minExclusive".to_string(), "maxExclusive".to_string()],
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

const NAME_START: &str = "\\x{3A}\\x{41}-\\x{5A}\\x{5F}\\x{61}-\\x{7A}\\x{C0}-\\x{D6}\\x{D8}-\\x{F6}\\x{F8}-\\x{2FF}\\x{370}-\\x{37D}\\x{37F}-\\x{1FFF}\\x{200C}-\\x{200D}\\x{2070}-\\x{218F}\\x{2C00}-\\x{2FEF}\\x{3001}-\\x{D7FF}\\x{F900}-\\x{FDCF}\\x{FDF0}-\\x{FFFD}\\x{10000}-\\x{EFFFF}";
const NAME_EXTRA: &str = "\\x{2D}\\x{2E}\\x{30}-\\x{39}\\x{B7}\\x{300}-\\x{36F}\\x{203F}-\\x{2040}";

fn is_meta(c: char) -> bool {
    matches!(
        c,
        '\\' | '.'
            | '+'
            | '*'
            | '?'
            | '('
            | ')'
            | '|'
            | '['
            | ']'
            | '{'
            | '}'
            | '^'
            | '$'
            | '#'
            | '&'
            | '-'
            | '~'
    )
}

fn class_char(out: &mut String, c: char) {
    if is_meta(c) {
        out.push('\\');
    }
    out.push(c);
}

fn escape(cs: &[char], i: &mut usize, out: &mut String) -> Result<(), String> {
    let e = cs[*i];
    *i += 1;
    match e {
        'n' => out.push_str("\\n"),
        'r' => out.push_str("\\r"),
        't' => out.push_str("\\t"),
        '\\' | '|' | '.' | '-' | '^' | '?' | '*' | '+' | '{' | '}' | '(' | ')' | '[' | ']' => {
            out.push('\\');
            out.push(e);
        }
        's' => out.push_str("[ \\t\\n\\r]"),
        'S' => out.push_str("[^ \\t\\n\\r]"),
        'd' => out.push_str("\\p{Nd}"),
        'D' => out.push_str("\\P{Nd}"),
        'w' => out.push_str("[^\\p{P}\\p{Z}\\p{C}]"),
        'W' => out.push_str("[\\p{P}\\p{Z}\\p{C}]"),
        'i' => {
            out.push('[');
            out.push_str(NAME_START);
            out.push(']');
        }
        'I' => {
            out.push_str("[^");
            out.push_str(NAME_START);
            out.push(']');
        }
        'c' => {
            out.push('[');
            out.push_str(NAME_START);
            out.push_str(NAME_EXTRA);
            out.push(']');
        }
        'C' => {
            out.push_str("[^");
            out.push_str(NAME_START);
            out.push_str(NAME_EXTRA);
            out.push(']');
        }
        'p' | 'P' => {
            if cs.get(*i) != Some(&'{') {
                return Err("\\p without a category".into());
            }
            let end = cs[*i..]
                .iter()
                .position(|c| *c == '}')
                .ok_or("unterminated \\p{}")?;
            let name: String = cs[*i + 1..*i + end].iter().collect();
            *i += end + 1;
            let valid = matches!(
                name.as_str(),
                "L" | "Lu"
                    | "Ll"
                    | "Lt"
                    | "Lm"
                    | "Lo"
                    | "M"
                    | "Mn"
                    | "Mc"
                    | "Me"
                    | "N"
                    | "Nd"
                    | "Nl"
                    | "No"
                    | "P"
                    | "Pc"
                    | "Pd"
                    | "Ps"
                    | "Pe"
                    | "Pi"
                    | "Pf"
                    | "Po"
                    | "Z"
                    | "Zs"
                    | "Zl"
                    | "Zp"
                    | "S"
                    | "Sm"
                    | "Sc"
                    | "Sk"
                    | "So"
                    | "C"
                    | "Cc"
                    | "Cf"
                    | "Co"
                    | "Cn"
            );
            if !valid {
                return Err(format!(
                    "unsupported category or block `{name}` (blocks `Is…` are not supported)"
                ));
            }
            out.push('\\');
            out.push(e);
            out.push('{');
            out.push_str(&name);
            out.push('}');
        }
        other => return Err(format!("unsupported escape `\\{other}`")),
    }
    Ok(())
}

fn translate(p: &str) -> Result<String, String> {
    let cs: Vec<char> = p.chars().collect();
    let mut out = String::with_capacity(p.len() + 16);
    let (mut i, mut depth, mut prev_quant) = (0usize, 0usize, false);
    let mut first_in_class = false;
    while i < cs.len() {
        let c = cs[i];
        if depth == 0 {
            let mut quant = false;
            match c {
                '\\' => {
                    i += 1;
                    if i >= cs.len() {
                        return Err("trailing backslash".into());
                    }
                    escape(&cs, &mut i, &mut out)?;
                    prev_quant = false;
                    continue;
                }
                '[' => {
                    depth = 1;
                    out.push('[');
                    i += 1;
                    if cs.get(i) == Some(&'^') {
                        out.push('^');
                        i += 1;
                    }
                    first_in_class = true;
                    prev_quant = false;
                    continue;
                }
                '(' => {
                    if cs.get(i + 1) == Some(&'?') {
                        return Err("`(?` is not allowed".into());
                    }
                    out.push_str("(?:");
                }
                ')' => out.push(')'),
                '|' => out.push('|'),
                '.' => out.push_str("[^\\n\\r]"),
                '^' => out.push_str("\\^"),
                '$' => out.push_str("\\$"),
                '?' | '*' | '+' => {
                    if prev_quant {
                        return Err("a quantifier follows a quantifier".into());
                    }
                    out.push(c);
                    quant = true;
                }
                '{' => {
                    if prev_quant {
                        return Err("a quantifier follows a quantifier".into());
                    }
                    let end = cs[i..]
                        .iter()
                        .position(|c| *c == '}')
                        .ok_or("unterminated quantifier")?;
                    let body: String = cs[i + 1..i + end].iter().collect();
                    let mut it = body.splitn(2, ',');
                    let lo = it.next().unwrap_or("");
                    let hi = it.next();
                    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
                    if !digits(lo) || hi.is_some_and(|h| !h.is_empty() && !digits(h)) {
                        return Err("bad quantifier".into());
                    }
                    out.push('{');
                    out.push_str(&body);
                    out.push('}');
                    i += end;
                    quant = true;
                }
                '}' | ']' => return Err(format!("unescaped `{c}`")),
                _ => {
                    if is_meta(c) {
                        out.push('\\');
                    }
                    out.push(c);
                }
            }
            prev_quant = quant;
            i += 1;
        } else {
            match c {
                '\\' => {
                    i += 1;
                    if i >= cs.len() {
                        return Err("trailing backslash".into());
                    }
                    escape(&cs, &mut i, &mut out)?;
                    first_in_class = false;
                    continue;
                }
                '[' => return Err("unescaped `[` in a class".into()),
                ']' => {
                    out.push(']');
                    depth -= 1;
                }
                '-' => {
                    if cs.get(i + 1) == Some(&'[') {
                        out.push_str("--[");
                        i += 2;
                        depth += 1;
                        if cs.get(i) == Some(&'^') {
                            out.push('^');
                            i += 1;
                        }
                        first_in_class = true;
                        continue;
                    }
                    if cs.get(i + 1) == Some(&'-') || first_in_class {
                        out.push_str("\\-");
                    } else {
                        out.push('-');
                    }
                }
                _ => class_char(&mut out, c),
            }
            first_in_class = false;
            i += 1;
        }
    }
    if depth != 0 {
        return Err("unterminated class".into());
    }
    Ok(out)
}

fn build(p: &str) -> Result<Regex, String> {
    if p.chars().count() > MAX_PATTERN_CHARS {
        return Err("pattern is too long".into());
    }
    let t = translate(p)?;
    RegexBuilder::new(&format!("^(?:{t})$"))
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .nest_limit(64)
        .build()
        .map_err(|e| e.to_string())
}

pub(crate) fn validate_pattern(p: &str) -> Result<(), String> {
    compile_pattern(p).map(|_| ())
}

fn compile_pattern(p: &str) -> Result<Arc<Regex>, String> {
    type Cache = (HashMap<String, Arc<Regex>>, VecDeque<String>);
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new((HashMap::new(), VecDeque::new())));
    if let Ok(g) = cache.lock()
        && let Some(r) = g.0.get(p)
    {
        return Ok(r.clone());
    }
    let re = Arc::new(build(p)?);
    if let Ok(mut g) = cache.lock() {
        while g.0.len() >= REGEX_CACHE {
            match g.1.pop_front() {
                Some(old) => {
                    g.0.remove(&old);
                }
                None => break,
            }
        }
        if g.0.insert(p.to_string(), re.clone()).is_none() {
            g.1.push_back(p.to_string());
        }
    }
    Ok(re)
}
