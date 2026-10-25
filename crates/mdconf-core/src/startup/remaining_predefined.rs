use super::{
    GuidMap, ManagedIndex, ManagedPredefined, ManagedValueContext,
    index::text,
    source_copy::copy_node,
    values::{append, empty},
};
use crate::{
    MdRef,
    parts::{
        AccountData, AccountType, CalcTypeData, ExtDimensionType, Predefined, PredefinedCode,
        PredefinedItem,
    },
};
use brace::{Guid, Node};
use memory_budget::{BudgetError, BudgetedText, BudgetedVec, Category, Charged, MemoryBudget};
#[derive(Debug)]
pub(super) enum Error<'a> {
    Memory(BudgetError),
    Semantic(&'static str, Option<&'a str>),
}
impl From<BudgetError> for Error<'_> {
    fn from(e: BudgetError) -> Self {
        Self::Memory(e)
    }
}
type Result<'a, T> = std::result::Result<T, Error<'a>>;
fn malformed<'a>() -> Error<'a> {
    Error::Semantic("malformed", None)
}
fn unrepresentable<'a>() -> Error<'a> {
    Error::Semantic("unrepresentable", None)
}
fn bad<'a>(why: &'static str) -> Error<'a> {
    Error::Semantic(why, None)
}
fn list(node: Option<&Node>) -> Result<'_, &[Node]> {
    node.and_then(Node::as_list).ok_or_else(malformed)
}
fn txt(node: Option<&Node>) -> Result<'_, &str> {
    node.and_then(Node::text).ok_or_else(malformed)
}
fn num(node: Option<&Node>) -> Result<'_, usize> {
    txt(node)?.trim().parse().map_err(|_| malformed())
}
fn vector<T>(budget: &MemoryBudget, n: usize) -> std::result::Result<Charged<Vec<T>>, BudgetError> {
    Ok(BudgetedVec::try_with_capacity(budget, Category::Metadata, n)?.into_charged_vec())
}
fn scalar(node: Option<&Node>) -> Result<'_, Option<&str>> {
    let node = node.ok_or_else(malformed)?;
    match txt(node.at(0))? {
        "S" => Ok(Some(node.at(1).and_then(Node::text).unwrap_or(""))),
        "N" => Ok(Some(txt(node.at(1))?)),
        "B" => Ok(Some(if txt(node.at(1))? == "1" {
            "true"
        } else {
            "false"
        })),
        "U" => Ok(None),
        code => Err(Error::Semantic("scalar ", Some(code))),
    }
}
fn bool_of(node: Option<&Node>) -> Result<'_, bool> {
    match scalar(node)? {
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        _ => Err(unrepresentable()),
    }
}
fn text_of<'a>(budget: &MemoryBudget, node: Option<&'a Node>) -> Result<'a, Charged<String>> {
    Ok(text(budget, &[scalar(node)?.unwrap_or_default()])?.into_string()?)
}
fn ref_text(node: Option<&Node>) -> Result<'_, &str> {
    let node = node.ok_or_else(malformed)?;
    if node.at(0).ok_or_else(malformed)?.text() != Some("#") {
        return Err(bad("ref"));
    }
    txt(node.at(2).and_then(|n| n.at(1)))
}
const FIELDS0: &[(&str, &str)] = &[
    ("0", "Ref"),
    ("1", "IsFolder"),
    ("2", "Parent"),
    ("3", "Name"),
    ("4", "Code"),
    ("5", "Description"),
    ("6", "ExtState"),
];
const FIELDS1: &[(&str, &str)] = &[
    ("1", "Ref"),
    ("2", "IsFolder"),
    ("3", "Name"),
    ("4", "Code"),
    ("5", "Description"),
    ("6", "Type"),
    ("7", "ExtState"),
];
const FIELDS2: &[(&str, &str)] = &[
    ("0", "Ref"),
    ("1", "Name"),
    ("2", "Code"),
    ("3", "Description"),
    ("4", "AccountType"),
    ("5", "OffBalance"),
    ("6", "ExtDimensionTypes"),
    ("10000", "Order"),
    ("20000", "ExtState"),
];
const FIELDS9: &[(&str, &str)] = &[
    ("1", "Ref"),
    ("2", "Name"),
    ("3", "Code"),
    ("4", "Description"),
    ("5", "ActionPeriodIsBase"),
    ("6", "Displaced"),
    ("7", "Base"),
    ("8", "Leading"),
    ("9", "ExtState"),
];
fn columns(kind: u32) -> Option<&'static [(&'static str, &'static str)]> {
    match kind {
        0 => Some(FIELDS0),
        1 => Some(FIELDS1),
        2 => Some(FIELDS2),
        9 => Some(FIELDS9),
        _ => None,
    }
}
fn lookup<V: Copy>(items: &[(&str, V)], key: &str) -> Option<V> {
    items.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}
fn insert<'a, V>(
    items: &mut BudgetedVec<(&'a str, V)>,
    key: &'a str,
    value: V,
) -> std::result::Result<(), BudgetError> {
    if let Some(slot) = items.iter_mut().find(|(k, _)| *k == key) {
        slot.1 = value;
    } else {
        items.try_push((key, value))?;
    }
    Ok(())
}
enum Body<'a> {
    Tree(&'a Node),
    Flat(&'a [Node]),
}
struct Table<'a> {
    kind: u32,
    columns: BudgetedVec<(&'a str, &'a str)>,
    positions: BudgetedVec<&'a str>,
    body: Body<'a>,
}
fn table<'a>(budget: &MemoryBudget, node: &'a Node, flat: bool) -> Result<'a, Table<'a>> {
    let kind = u32::try_from(num(node.at(0))?).map_err(|_| malformed())?;
    let body = if flat {
        node
    } else {
        node.at(1).ok_or_else(malformed)?
    };
    let cols = list(body.at(1))?;
    let count = num(cols.first())?;
    let cols = cols.get(1..).ok_or_else(malformed)?;
    let mut columns =
        BudgetedVec::try_with_capacity(budget, Category::Metadata, count.min(cols.len()))?;
    for col in cols.iter().take(count) {
        let id = txt(col.at(0))?;
        let name = txt(col.at(1))?;
        col.at(2).ok_or_else(malformed)?;
        insert(&mut columns, id, name)?;
    }
    let positions_raw = list(body.at(2))?;
    let n = num(positions_raw.get(1))?;
    if n > positions_raw.len().saturating_sub(2) / 2 {
        return Err(malformed());
    }
    let mut pos = BudgetedVec::try_with_capacity(budget, Category::Metadata, n)?;
    pos.try_resize(n, None)?;
    for i in 0..n {
        let at = num(positions_raw.get(2 + 2 * i))?;
        let id = txt(positions_raw.get(3 + 2 * i))?;
        *pos.get_mut(at).ok_or_else(malformed)? = Some(id);
    }
    let mut positions = BudgetedVec::try_with_capacity(budget, Category::Metadata, n)?;
    for value in pos.iter() {
        positions.try_push(value.ok_or_else(malformed)?)?;
    }
    let rows = positions_raw.get(2 + 2 * n).ok_or_else(malformed)?;
    let body = if flat {
        let rows = list(Some(rows))?;
        let n = num(rows.get(1))?;
        let rows = rows.get(2..).ok_or_else(malformed)?;
        Body::Flat(rows.get(..n.min(rows.len())).ok_or_else(malformed)?)
    } else {
        Body::Tree(rows.at(2).ok_or_else(malformed)?)
    };
    Ok(Table {
        kind,
        columns,
        positions,
        body,
    })
}
fn cells<'a>(
    budget: &MemoryBudget,
    row: &'a Node,
    positions: &[&'a str],
) -> Result<'a, BudgetedVec<(&'a str, &'a Node)>> {
    let row = list(Some(row))?;
    let n = num(row.get(2))?;
    let values = row
        .get(3..)
        .and_then(|v| v.get(..n))
        .ok_or_else(malformed)?;
    let mut out = BudgetedVec::try_with_capacity(budget, Category::Metadata, values.len())?;
    for (at, value) in values.iter().enumerate() {
        insert(&mut out, positions.get(at).ok_or_else(malformed)?, value)?;
    }
    Ok(out)
}
struct Context<'a> {
    index: &'a ManagedIndex,
    values: ManagedValueContext<'a>,
    full: &'a GuidMap<BudgetedText>,
}
impl Context<'_> {
    fn budget(&self) -> &MemoryBudget {
        self.index.memory_budget()
    }
    fn item_ref<'a>(&self, raw: &'a str, prefix: &'static str) -> Result<'a, Charged<MdRef>> {
        let id = Guid::parse(raw);
        let name = id
            .and_then(|id| self.full.get(&id))
            .ok_or(Error::Semantic(prefix, Some(raw)))?;
        Ok(text(self.budget(), &[name.as_str()])?
            .into_string()?
            .map(|name| MdRef { id, name }))
    }
    fn flag_ref<'a>(&self, raw: &'a str, prefix: &'static str) -> Result<'a, Charged<MdRef>> {
        let id = Guid::parse(raw);
        let name = id
            .and_then(|id| self.index.names.get(&id))
            .ok_or(Error::Semantic(prefix, Some(raw)))?;
        Ok(text(self.budget(), &[name.as_str()])?
            .into_string()?
            .map(|name| MdRef { id, name }))
    }
}
fn flat_rows<'a>(
    cx: &Context<'_>,
    table: &Table<'a>,
) -> Result<'a, BudgetedVec<BudgetedVec<(&'a str, &'a Node)>>> {
    let Body::Flat(rows) = table.body else {
        return Err(malformed());
    };
    let mut out = BudgetedVec::try_with_capacity(cx.budget(), Category::Metadata, rows.len())?;
    for row in rows {
        out.try_push(cells(cx.budget(), row, &table.positions)?)?;
    }
    Ok(out)
}
fn ext_dims<'a>(cx: &Context<'_>, node: &'a Node) -> Result<'a, Charged<Vec<ExtDimensionType>>> {
    if node.at(0).ok_or_else(malformed)?.text() != Some("#")
        || node.at(1).ok_or_else(malformed)?.text() != Some("acf6192e-81ca-46ef-93a6-5a6968b78663")
    {
        return Err(bad("ext dims value"));
    }
    let table = table(cx.budget(), node.at(2).ok_or_else(malformed)?, true)?;
    let rows = flat_rows(cx, &table)?;
    let mut output = vector(cx.budget(), rows.len())?;
    for row in rows.iter() {
        let kind = cx.item_ref(ref_text(lookup(row, "0"))?, "ext dim type ")?;
        let turnover = bool_of(lookup(row, "1"))?;
        for (cid, _) in row.iter().filter(|(cid, _)| !matches!(*cid, "0" | "1")) {
            lookup(&table.columns, cid).ok_or_else(malformed)?;
        }
        let mut flags = vector(cx.budget(), row.len())?;
        for (cid, value) in row.iter().filter(|(cid, _)| !matches!(*cid, "0" | "1")) {
            let raw = lookup(&table.columns, cid).ok_or_else(malformed)?;
            let flag = cx.flag_ref(raw, "ext dim flag ")?;
            let value = bool_of(Some(value))?;
            flags = append(flags, flag.map(|flag| (flag, value)))?;
        }
        output = append(
            output,
            kind.try_zip(flags)?.map(|(kind, flags)| ExtDimensionType {
                kind,
                turnover,
                flags,
            }),
        )?;
    }
    Ok(output)
}
fn calc_refs<'a>(cx: &Context<'_>, node: Option<&'a Node>) -> Result<'a, Charged<Vec<MdRef>>> {
    let node = node.ok_or_else(malformed)?;
    if node.at(0).ok_or_else(malformed)?.text() != Some("#") {
        return Ok(vector(cx.budget(), 0)?);
    }
    let table = table(cx.budget(), node.at(2).ok_or_else(malformed)?, true)?;
    let rows = flat_rows(cx, &table)?;
    if rows.is_empty() || table.columns.is_empty() {
        return Ok(vector(cx.budget(), 0)?);
    }
    let mut last = None;
    for (cid, _) in table.columns.iter() {
        let key = cid.trim().parse::<i64>().map_err(|_| malformed())?;
        if last.is_none_or(|(old, _)| key > old) {
            last = Some((key, *cid));
        }
    }
    let (_, last) = last.ok_or_else(malformed)?;
    let mut output = vector(cx.budget(), rows.len())?;
    for row in rows.iter() {
        let raw = ref_text(Some(lookup(row, last).ok_or_else(malformed)?))?;
        let value = cx.item_ref(raw, "").map_err(|error| match error {
            Error::Semantic(_, _) => bad("calc type ref"),
            other => other,
        })?;
        output = append(output, value)?;
    }
    Ok(output)
}
fn core<'a>(
    cx: &Context<'_>,
    get: impl Fn(&str) -> Option<&'a Node>,
) -> Result<'a, Charged<PredefinedItem>> {
    let id = Guid::parse(ref_text(get("Ref"))?).ok_or_else(unrepresentable)?;
    let name = text_of(cx.budget(), get("Name"))?;
    let numeric = get("Code").and_then(|n| n.at(0)).and_then(Node::text) == Some("N");
    let code = text_of(cx.budget(), get("Code"))?.map(|code| {
        if numeric {
            PredefinedCode::Number(code)
        } else {
            PredefinedCode::Str(code)
        }
    });
    let description = text_of(cx.budget(), get("Description"))?;
    Ok(name
        .try_zip(code)?
        .try_zip(description)?
        .map(|((name, code), description)| PredefinedItem {
            id,
            name,
            code,
            description,
            ..Default::default()
        }))
}
fn calc_items<'a>(cx: &Context<'_>, node: &'a Node) -> Result<'a, Charged<Vec<PredefinedItem>>> {
    let table = table(cx.budget(), node, true)?;
    let rows = flat_rows(cx, &table)?;
    let mut output = vector(cx.budget(), rows.len())?;
    for row in rows.iter() {
        let get = |field: &str| {
            FIELDS9
                .iter()
                .find(|(_, f)| *f == field)
                .and_then(|(cid, _)| lookup(row, cid))
        };
        let item = core(cx, get)?;
        let action_period_is_base = bool_of(get("ActionPeriodIsBase"))?;
        let base = calc_refs(cx, get("Base"))?;
        let leading = calc_refs(cx, get("Leading"))?;
        let displaced = calc_refs(cx, get("Displaced"))?;
        let calc =
            base.try_zip(leading)?
                .try_zip(displaced)?
                .map(|((base, leading), displaced)| CalcTypeData {
                    action_period_is_base,
                    base,
                    leading,
                    displaced,
                });
        output = append(
            output,
            item.try_zip(calc)?.map(|(mut item, calc)| {
                item.calc = Some(calc);
                item
            }),
        )?;
    }
    Ok(output)
}
struct Tree<'a> {
    table: Table<'a>,
    columns: &'static [(&'static str, &'static str)],
    flags: BudgetedVec<(i64, usize, &'a str, &'a str)>,
    bad_flags: bool,
}
fn tree_item<'a>(
    cx: &Context<'_>,
    tree: &Tree<'a>,
    row: &'a Node,
    states: &mut bool,
    depth: usize,
) -> Result<'a, Charged<PredefinedItem>> {
    if depth > 128 {
        return Err(bad("too_deep"));
    }
    let vals = cells(cx.budget(), row, &tree.table.positions)?;
    let mut fields = BudgetedVec::try_with_capacity(cx.budget(), Category::Metadata, vals.len())?;
    for (cid, value) in vals.iter() {
        if let Some((_, field)) = tree.columns.iter().find(|(id, _)| id == cid) {
            insert(&mut fields, field, *value)?;
        } else if tree.table.kind == 2
            && lookup(&tree.table.columns, cid).is_some_and(|s| !s.is_empty())
        {
        } else {
            return Err(Error::Semantic("column ", Some(cid)));
        }
    }
    if tree.table.kind == 2 && tree.bad_flags {
        return Err(malformed());
    }
    let get = |field: &str| lookup(&fields, field);
    let mut item = core(cx, get)?;
    if tree.table.kind == 1 {
        let pattern = get("Type").and_then(|n| n.at(2)).ok_or_else(malformed)?;
        let ty = cx.values.type_desc(pattern)?.ok_or_else(|| bad("type"))?;
        item = item.try_zip(ty)?.map(|(mut item, ty)| {
            item.type_desc = Some(ty);
            item
        });
    }
    if tree.table.kind != 2 {
        let is_folder = bool_of(get("IsFolder"))?;
        item = item.map(|mut item| {
            item.is_folder = is_folder;
            item
        });
    } else {
        let account_type = match get("AccountType")
            .and_then(|n| n.at(1))
            .and_then(Node::text)
        {
            Some("0") => AccountType::Active,
            Some("1") => AccountType::Passive,
            Some("2") => AccountType::ActivePassive,
            _ => return Err(malformed()),
        };
        let off_balance = bool_of(get("OffBalance"))?;
        let order = text_of(cx.budget(), get("Order"))?;
        let mut flags = vector(cx.budget(), tree.flags.len())?;
        for (_, _, cid, raw) in tree.flags.iter() {
            let flag = cx.flag_ref(raw, "flag ")?;
            let val = match lookup(&vals, cid) {
                Some(node) => bool_of(Some(node))?,
                None => false,
            };
            flags = append(flags, flag.map(|flag| (flag, val)))?;
        }
        let ext = match get("ExtDimensionTypes") {
            Some(node) if node.at(0).ok_or_else(malformed)?.text() != Some("U") => {
                ext_dims(cx, node)?.map(Some)
            }
            _ => empty(cx.budget(), None)?,
        };
        let account =
            order
                .try_zip(flags)?
                .try_zip(ext)?
                .map(|((order, flags), ext_dimension_types)| AccountData {
                    account_type,
                    off_balance,
                    order,
                    flags,
                    ext_dimension_types,
                });
        item = item.try_zip(account)?.map(|(mut item, account)| {
            item.account = Some(account);
            item
        });
    }
    if let Some(state) = get("ExtState")
        && matches!(state.at(1).ok_or_else(malformed)?.text(), Some("1" | "2"))
    {
        *states = true;
    }
    let children = crate::predefined::iter_children(row).ok_or_else(malformed)?;
    let mut actual = vector(cx.budget(), children.len())?;
    for child in children {
        actual = append(actual, tree_item(cx, tree, child, states, depth + 1)?)?;
    }
    Ok(item.try_zip(actual)?.map(|(mut item, children)| {
        item.children = children;
        item
    }))
}
pub(super) fn decode<'a>(
    index: &ManagedIndex,
    predef: &ManagedPredefined,
    full: &GuidMap<BudgetedText>,
    node: &'a Node,
    keep: bool,
) -> Result<'a, Charged<Predefined>> {
    let cx = Context {
        index,
        values: ManagedValueContext::new(index, predef)?,
        full,
    };
    let mut states = false;
    let items = if node.at(0).ok_or_else(malformed)?.text() == Some("9") {
        calc_items(&cx, node)?
    } else {
        let table = table(cx.budget(), node, false)?;
        let columns = columns(table.kind).ok_or_else(malformed)?;
        let Body::Tree(root) = table.body else {
            return Err(malformed());
        };
        let mut flags = BudgetedVec::new(cx.budget(), Category::Metadata)?;
        let mut bad_flags = false;
        if table.kind == 2 {
            for (ordinal, (cid, name)) in table.columns.iter().enumerate() {
                if columns.iter().any(|(id, _)| id == cid) || name.is_empty() {
                    continue;
                }
                if let Ok(key) = cid.trim().parse::<i64>() {
                    flags.try_push((key, ordinal, *cid, *name))?;
                } else {
                    bad_flags = true;
                }
            }
            flags.sort_unstable_by_key(|(key, ordinal, _, _)| (*key, *ordinal));
        }
        let tree = Tree {
            table,
            columns,
            flags,
            bad_flags,
        };
        let rows = crate::predefined::iter_children(root).ok_or_else(malformed)?;
        let mut output = vector(cx.budget(), rows.len())?;
        for row in rows {
            output = append(output, tree_item(&cx, &tree, row, &mut states, 1)?)?;
        }
        output
    };
    let source = if keep || states {
        copy_node(cx.budget(), node)?
    } else {
        empty(cx.budget(), Node::empty())?
    };
    Ok(items.try_zip(source)?.map(|(items, source)| Predefined {
        items,
        source: Some(source),
    }))
}
