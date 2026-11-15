use std::borrow::Cow;

use roxmltree::Node;

use super::{
    MAX_DEPTH, NS_XSI, XmlError, escape_attr, escape_text, is_nil, leaf_text, push_value_as,
    qname_of, resolve_qname, value_in,
};
use crate::tabular::{
    AllowedLength, AllowedSign, Column, DateFractions, DateQualifiers, ListItem, NS_UI,
    NumberQualifiers, Picture, StringQualifiers, TreeRow, TypeDescription, ValueList, ValueTable,
    ValueTree,
};
use crate::value::{NS_CORE, NS_XS, TypeName};

fn shape(msg: impl Into<String>) -> XmlError {
    XmlError::Shape(msg.into())
}

fn attribute(n: Node, name: &str) -> XmlError {
    shape(format!("attribute {name} on <{}>", n.tag_name().name()))
}

fn need<'a, 'i>(
    n: Option<Node<'a, 'i>>,
    local: &str,
    owner: &str,
) -> Result<Node<'a, 'i>, XmlError> {
    n.ok_or_else(|| shape(format!("<{owner}> without <{local}> in its place")))
}

struct Children<'a, 'i> {
    items: Vec<Node<'a, 'i>>,
    at: usize,
}

impl<'a, 'i> Children<'a, 'i> {
    fn of(n: Node<'a, 'i>) -> Result<Children<'a, 'i>, XmlError> {
        Ok(Children {
            items: super::elements(n)?,
            at: 0,
        })
    }

    fn bare(n: Node<'a, 'i>) -> Result<Children<'a, 'i>, XmlError> {
        if let Some(a) = n.attributes().next() {
            return Err(attribute(n, a.name()));
        }
        Children::of(n)
    }

    fn is_next(&self, local: &str) -> bool {
        self.items.get(self.at).is_some_and(|c| {
            c.tag_name().namespace() == Some(NS_CORE) && c.tag_name().name() == local
        })
    }

    fn take(&mut self, local: &str) -> Option<Node<'a, 'i>> {
        let next = self.is_next(local).then(|| self.items[self.at]);
        if next.is_some() {
            self.at += 1;
        }
        next
    }

    fn take_all(&mut self, local: &str) -> Vec<Node<'a, 'i>> {
        let mut out = Vec::new();
        while let Some(c) = self.take(local) {
            out.push(c);
        }
        out
    }

    fn require(&mut self, local: &str, owner: &str) -> Result<Node<'a, 'i>, XmlError> {
        need(self.take(local), local, owner)
    }

    fn finish(&self, owner: &str, unsupported: &[&str]) -> Result<(), XmlError> {
        let Some(c) = self.items.get(self.at) else {
            return Ok(());
        };
        let local = c.tag_name().name();
        if c.tag_name().namespace() == Some(NS_CORE) && unsupported.contains(&local) {
            return Err(XmlError::UnsupportedPart(format!("{owner}.{local}")));
        }
        Err(shape(format!("unexpected <{local}> in <{owner}>")))
    }
}

fn schema_text<'a>(n: Node<'a, '_>, types: &[&str]) -> Result<Cow<'a, str>, XmlError> {
    let local = n.tag_name().name();
    if is_nil(n) {
        return Err(shape(format!("nil <{local}>")));
    }
    if let Some(a) = n
        .attributes()
        .find(|a| a.namespace() != Some(NS_XSI) || a.name() != "type")
    {
        return Err(attribute(n, a.name()));
    }
    if let Some(q) = n.attribute((NS_XSI, "type")) {
        let ok = resolve_qname(n, q).is_some_and(|t| t.ns() == NS_XS && types.contains(&t.local()));
        if !ok {
            return Err(shape(format!("<{local}> of type {q}")));
        }
    }
    leaf_text(n)
}

fn parse_uint(text: &str) -> Option<u64> {
    let canonical = !text.is_empty()
        && text.bytes().all(|c| c.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    if canonical { text.parse().ok() } else { None }
}

fn parse_int(text: &str) -> Option<i64> {
    match text.strip_prefix('-') {
        Some(digits) if digits != "0" => parse_uint(digits)
            .and_then(|v| i64::try_from(v).ok())
            .map(|v| -v),
        Some(_) => None,
        None => parse_uint(text).and_then(|v| i64::try_from(v).ok()),
    }
}

fn uint_of(n: Node, types: &[&str]) -> Result<u64, XmlError> {
    let text = schema_text(n, types)?;
    parse_uint(&text).ok_or_else(|| shape(format!("<{}> {text:?}", n.tag_name().name())))
}

fn u32_of(n: Node) -> Result<u32, XmlError> {
    let v = uint_of(n, &["decimal"])?;
    u32::try_from(v).map_err(|_| shape(format!("<{}> {v} out of range", n.tag_name().name())))
}

fn enum_of<T: Copy>(n: Node, names: &[(&str, T)]) -> Result<T, XmlError> {
    let text = schema_text(n, &[])?;
    names
        .iter()
        .find(|(name, _)| *name == text.as_ref())
        .map(|(_, v)| *v)
        .ok_or_else(|| shape(format!("<{}> {text:?}", n.tag_name().name())))
}

const SIGNS: [(&str, AllowedSign); 2] = [
    ("Any", AllowedSign::Any),
    ("Nonnegative", AllowedSign::Nonnegative),
];
const LENGTHS: [(&str, AllowedLength); 2] = [
    ("Variable", AllowedLength::Variable),
    ("Fixed", AllowedLength::Fixed),
];
const FRACTIONS: [(&str, DateFractions); 3] = [
    ("Date", DateFractions::Date),
    ("Time", DateFractions::Time),
    ("DateTime", DateFractions::DateTime),
];

fn name_of<T: PartialEq>(names: &[(&'static str, T)], v: T) -> &'static str {
    names
        .iter()
        .find(|(_, x)| *x == v)
        .map_or("", |(name, _)| name)
}

fn type_description_of(n: Node) -> Result<TypeDescription, XmlError> {
    let mut cur = Children::bare(n)?;
    let mut td = TypeDescription::default();
    for t in cur.take_all("Type") {
        let text = schema_text(t, &["QName"])?;
        let ty = resolve_qname(t, &text).ok_or_else(|| shape(format!("<Type> {text:?}")))?;
        if td.types.contains(&ty) {
            return Err(shape(format!("repeated <Type> {text}")));
        }
        td.types.push(ty);
    }
    if let Some(q) = cur.take("NumberQualifiers") {
        let mut f = Children::bare(q)?;
        td.number = Some(NumberQualifiers {
            digits: u32_of(f.require("Digits", "NumberQualifiers")?)?,
            fraction_digits: u32_of(f.require("FractionDigits", "NumberQualifiers")?)?,
            sign: enum_of(f.require("AllowedSign", "NumberQualifiers")?, &SIGNS)?,
        });
        f.finish("NumberQualifiers", &[])?;
    }
    if let Some(q) = cur.take("StringQualifiers") {
        let mut f = Children::bare(q)?;
        td.string = Some(StringQualifiers {
            length: u32_of(f.require("Length", "StringQualifiers")?)?,
            allowed: enum_of(f.require("AllowedLength", "StringQualifiers")?, &LENGTHS)?,
        });
        f.finish("StringQualifiers", &[])?;
    }
    if let Some(q) = cur.take("DateQualifiers") {
        let mut f = Children::bare(q)?;
        td.date = Some(DateQualifiers {
            fractions: enum_of(f.require("DateFractions", "DateQualifiers")?, &FRACTIONS)?,
        });
        f.finish("DateQualifiers", &[])?;
    }
    cur.finish(
        "TypeDescription",
        &["TypeSet", "TypeId", "BinaryDataQualifiers"],
    )?;
    Ok(td)
}

fn column_of(n: Node) -> Result<Column, XmlError> {
    let mut cur = Children::bare(n)?;
    let (name, value_type) = (cur.take("Name"), cur.take("ValueType"));
    let (title, width) = (cur.take("Title"), cur.take("Width"));
    cur.finish("column", &[])?;
    let name = schema_text(need(name, "Name", "column")?, &["string", "NMTOKEN"])?;
    let value_type = type_description_of(need(value_type, "ValueType", "column")?)?;
    let title = match title {
        Some(t) => schema_text(t, &["string"])?.into_owned(),
        None => String::new(),
    };
    let width = match width {
        Some(w) => uint_of(w, &["decimal", "nonNegativeInteger"])?,
        None => 0,
    };
    Ok(Column {
        name: name.to_string(),
        value_type,
        title,
        width,
    })
}

fn cells_of(
    values: &[Node],
    columns: &[Column],
    depth: usize,
) -> Result<Vec<crate::value::Value>, XmlError> {
    if values.len() != columns.len() {
        return Err(crate::value::ValueError::RowWidth {
            columns: columns.len(),
            values: values.len(),
        }
        .into());
    }
    values
        .iter()
        .zip(columns)
        .map(|(v, c)| value_in(*v, depth, c.value_type.single()))
        .collect()
}

pub(super) fn table_of(n: Node, depth: usize) -> Result<ValueTable, XmlError> {
    let mut cur = Children::of(n)?;
    let (columns, indexes, rows) = (
        cur.take_all("column"),
        cur.take_all("index"),
        cur.take_all("row"),
    );
    cur.finish("ValueTable", &[])?;
    let columns = columns
        .into_iter()
        .map(column_of)
        .collect::<Result<Vec<_>, _>>()?;
    let mut table = ValueTable::new(columns)?;
    for index in indexes {
        let mut f = Children::bare(index)?;
        let names = f
            .take_all("column")
            .into_iter()
            .map(|c| schema_text(c, &["string", "NMTOKEN"]).map(Cow::into_owned))
            .collect::<Result<Vec<_>, _>>()?;
        f.finish("index", &[])?;
        table.add_index(names)?;
    }
    for row in rows {
        let mut f = Children::bare(row)?;
        let values = f.take_all("Value");
        f.finish("row", &[])?;
        let cells = cells_of(&values, table.columns(), depth)?;
        table.push_row(cells)?;
    }
    Ok(table)
}

fn tree_row_of(n: Node, columns: &[Column], depth: usize) -> Result<TreeRow, XmlError> {
    let mut cur = Children::bare(n)?;
    let children = cur.take_all("row");
    let values = cur.take_all("Value");
    cur.finish("row", &[])?;
    let mut row = TreeRow::new(cells_of(&values, columns, depth)?);
    for c in children {
        if depth + 1 >= MAX_DEPTH {
            return Err(XmlError::TooDeep);
        }
        row.push_child(tree_row_of(c, columns, depth + 1)?);
    }
    Ok(row)
}

pub(super) fn tree_of(n: Node, depth: usize) -> Result<ValueTree, XmlError> {
    let mut cur = Children::of(n)?;
    let (columns, rows) = (cur.take_all("column"), cur.take_all("row"));
    cur.finish("ValueTree", &[])?;
    let columns = columns
        .into_iter()
        .map(column_of)
        .collect::<Result<Vec<_>, _>>()?;
    let mut tree = ValueTree::new(columns)?;
    for row in rows {
        let row = tree_row_of(row, tree.columns(), depth)?;
        tree.push_row(row)?;
    }
    Ok(tree)
}

fn picture_of(n: Node) -> Result<Picture, XmlError> {
    let ty = n
        .attribute((NS_XSI, "type"))
        .and_then(|q| resolve_qname(n, q));
    if ty != Some(TypeName::new(NS_UI, "Picture")) {
        return Err(XmlError::UnsupportedPart(format!(
            "picture of type {}",
            ty.map_or("(none)".to_string(), |t| t.to_string())
        )));
    }
    let mut picture = Picture {
        t: None,
        reference: None,
    };
    for a in n.attributes() {
        match (a.namespace(), a.name()) {
            (Some(NS_XSI), "type") => {}
            (None, "t") => picture.t = Some(a.value().to_string()),
            (None, "ref") => {
                picture.reference = Some(
                    resolve_qname(n, a.value())
                        .ok_or_else(|| shape(format!("picture ref {:?}", a.value())))?,
                );
            }
            (_, name) => {
                return Err(XmlError::UnsupportedPart(format!(
                    "picture attribute {name}"
                )));
            }
        }
    }
    let content = n
        .children()
        .any(|c| c.is_element() || (c.is_text() && !c.text().unwrap_or("").trim().is_empty()));
    if content {
        return Err(XmlError::UnsupportedPart("picture content".to_string()));
    }
    Ok(picture)
}

pub(super) fn list_of(n: Node, depth: usize) -> Result<ValueList, XmlError> {
    let mut cur = Children::of(n)?;
    let (value_type, last, items) = (
        cur.take("valueType"),
        cur.take("lastId"),
        cur.take_all("item"),
    );
    cur.finish("ValueListType", &["availableValues"])?;
    let value_type = type_description_of(need(value_type, "valueType", "ValueListType")?)?;
    let last_text = schema_text(need(last, "lastId", "ValueListType")?, &["decimal", "long"])?;
    let last_id = parse_int(&last_text).ok_or_else(|| shape(format!("<lastId> {last_text:?}")))?;
    let mut list = ValueList::new(value_type);
    for item in items {
        let mut f = Children::bare(item)?;
        let (value, presentation, check, picture, id) = (
            f.take("value"),
            f.take("presentation"),
            f.take("checkState"),
            f.take("picture"),
            f.take("id"),
        );
        f.finish(
            "item",
            &["formatPresentationSpecified", "formatPresentation"],
        )?;
        let mut it = ListItem::new(value_in(need(value, "value", "item")?, depth, None)?);
        if let Some(p) = presentation {
            it.presentation = schema_text(p, &["string"])?.into_owned();
        }
        if let Some(c) = check {
            let text = schema_text(c, &["decimal"])?;
            if text != "1" {
                return Err(shape(format!("<checkState> {text:?}")));
            }
            it.check = true;
        }
        if let Some(p) = picture {
            it.picture = Some(picture_of(p)?);
        }
        let id = uint_of(need(id, "id", "item")?, &["decimal", "long"])?;
        list.push_with_id(it, id)?;
    }
    list.set_last_id(last_id)?;
    Ok(list)
}

fn push_type_description(r: &mut String, elem: &str, td: &TypeDescription) {
    if td.is_empty() {
        r.push_str(&format!("<{elem}/>"));
        return;
    }
    r.push_str(&format!("<{elem}>"));
    for ty in &td.types {
        let (decl, qname) = qname_of(ty, "d5p1");
        r.push_str(&format!("<v8:Type{decl}>{}</v8:Type>", escape_text(&qname)));
    }
    if let Some(q) = td.number {
        r.push_str(&format!(
            "<v8:NumberQualifiers><v8:Digits>{}</v8:Digits><v8:FractionDigits>{}</v8:FractionDigits>\
             <v8:AllowedSign>{}</v8:AllowedSign></v8:NumberQualifiers>",
            q.digits,
            q.fraction_digits,
            name_of(&SIGNS, q.sign)
        ));
    }
    if let Some(q) = td.string {
        r.push_str(&format!(
            "<v8:StringQualifiers><v8:Length>{}</v8:Length><v8:AllowedLength>{}</v8:AllowedLength>\
             </v8:StringQualifiers>",
            q.length,
            name_of(&LENGTHS, q.allowed)
        ));
    }
    if let Some(q) = td.date {
        r.push_str(&format!(
            "<v8:DateQualifiers><v8:DateFractions>{}</v8:DateFractions></v8:DateQualifiers>",
            name_of(&FRACTIONS, q.fractions)
        ));
    }
    r.push_str(&format!("</{elem}>"));
}

fn push_columns(r: &mut String, columns: &[Column]) {
    for c in columns {
        r.push_str(&format!(
            "<v8:column><v8:Name xsi:type=\"xs:string\">{}</v8:Name>",
            escape_text(&c.name)
        ));
        push_type_description(r, "v8:ValueType", &c.value_type);
        if !c.title.is_empty() {
            r.push_str(&format!("<v8:Title>{}</v8:Title>", escape_text(&c.title)));
        }
        if c.width != 0 {
            r.push_str(&format!(
                "<v8:Width xsi:type=\"xs:decimal\">{}</v8:Width>",
                c.width
            ));
        }
        r.push_str("</v8:column>");
    }
}

fn push_cells(r: &mut String, values: &[crate::value::Value], columns: &[Column]) {
    for (v, c) in values.iter().zip(columns) {
        push_value_as(r, "v8:Value", v, c.value_type.single());
    }
}

pub(super) fn push_table(r: &mut String, t: &ValueTable) {
    push_columns(r, t.columns());
    for index in t.indexes() {
        r.push_str("<v8:index>");
        for name in index {
            r.push_str(&format!(
                "<v8:column xsi:type=\"xs:string\">{}</v8:column>",
                escape_text(name)
            ));
        }
        r.push_str("</v8:index>");
    }
    for row in t.rows() {
        if row.is_empty() {
            r.push_str("<v8:row/>");
            continue;
        }
        r.push_str("<v8:row>");
        push_cells(r, row, t.columns());
        r.push_str("</v8:row>");
    }
}

fn push_tree_row(r: &mut String, row: &TreeRow, columns: &[Column]) {
    if row.rows().is_empty() && row.values().is_empty() {
        r.push_str("<v8:row/>");
        return;
    }
    r.push_str("<v8:row>");
    for child in row.rows() {
        push_tree_row(r, child, columns);
    }
    push_cells(r, row.values(), columns);
    r.push_str("</v8:row>");
}

pub(super) fn push_tree(r: &mut String, t: &ValueTree) {
    push_columns(r, t.columns());
    for row in t.rows() {
        push_tree_row(r, row, t.columns());
    }
}

fn push_picture(r: &mut String, p: &Picture) {
    let mut decls = format!(" xmlns:d4p1=\"{NS_UI}\"");
    let mut attrs = String::new();
    if let Some(t) = &p.t {
        attrs.push_str(&format!(" t=\"{}\"", escape_attr(t)));
    }
    if let Some(reference) = &p.reference {
        let qname = if reference.ns() == NS_UI {
            format!("d4p1:{}", reference.local())
        } else {
            let (decl, qname) = qname_of(reference, "d4p2");
            decls.push_str(&decl);
            qname
        };
        attrs.push_str(&format!(" ref=\"{}\"", escape_attr(&qname)));
    }
    r.push_str(&format!(
        "<v8:picture{decls} xsi:type=\"d4p1:Picture\"{attrs}/>"
    ));
}

pub(super) fn push_list(r: &mut String, l: &ValueList) {
    push_type_description(r, "v8:valueType", &l.value_type);
    r.push_str(&format!(
        "<v8:lastId xsi:type=\"xs:decimal\">{}</v8:lastId>",
        l.last_id()
    ));
    for item in l.items() {
        r.push_str("<v8:item>");
        push_value_as(r, "v8:value", &item.value, None);
        if !item.presentation.is_empty() {
            r.push_str(&format!(
                "<v8:presentation>{}</v8:presentation>",
                escape_text(&item.presentation)
            ));
        }
        if item.check {
            r.push_str("<v8:checkState>1</v8:checkState>");
        }
        if let Some(p) = &item.picture {
            push_picture(r, p);
        }
        r.push_str(&format!(
            "<v8:id xsi:type=\"xs:decimal\">{}</v8:id></v8:item>",
            item.id()
        ));
    }
}
