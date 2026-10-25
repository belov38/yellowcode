use brace::Node;

const MAGIC: &[u8] = b"MOXCEL\0";
pub(crate) const HEAD: [u8; 6] = [8, 0, 1, 0, 12, 0];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MoxelError(pub String);

impl From<super::tree::TreeError> for MoxelError {
    fn from(error: super::tree::TreeError) -> Self {
        Self(match error {
            super::tree::TreeError::Memory(error) => error.to_string(),
            super::tree::TreeError::Brace(error) => format!("bad brace: {error}"),
            super::tree::TreeError::Semantic(message) => message.to_string(),
        })
    }
}

impl std::fmt::Display for MoxelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for MoxelError {}

pub(crate) type MoxelRow = (i64, i64, Vec<(i64, Node)>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MoxelDoc {
    pub head: [u8; 6],
    pub bom: bool,
    pub lead: String,
    pub trailer: String,
    pub version: Vec<Node>,
    pub lang: Node,
    pub default_format: Node,
    pub lines: Node,
    pub u6: Node,
    pub headers: Vec<Node>,
    pub u13: Vec<Node>,
    pub rows: Vec<MoxelRow>,
    pub columns: Vec<Node>,
    pub height: i64,
    pub row_columns: Vec<(i64, i64)>,
    pub drawing_next: Node,
    pub drawings: Vec<Node>,
    pub vg: Vec<(Node, Node)>,
    pub hg: Vec<(Node, Node)>,
    pub vbreaks: Vec<Node>,
    pub hbreaks: Vec<(Node, Node)>,
    pub merges: Node,
    pub row_merges: Node,
    pub col_merges: Node,
    pub named: Node,
    pub print_settings_name: Node,
    pub print_settings: Node,
    pub print_area: Node,
    pub u_after_pa: Vec<Node>,
    pub formats: Vec<Node>,
    pub fonts: Vec<Node>,
    pub format_strings: Vec<Node>,
    pub value_types: Vec<Node>,
    pub uuids: Vec<Node>,
    pub colors: Vec<Node>,
    pub pictures: Vec<Node>,
    pub save_view: i64,
    pub read_only: i64,
    pub view_settings: Node,
    pub embedded: Vec<(Node, Node)>,
    pub used_file_name: Node,
    pub style_colors: Node,
    pub masks: Vec<Node>,
    pub tail: Vec<Node>,
}

#[cfg_attr(not(test), allow(dead_code))]
fn push_counted(t: &mut Vec<Node>, items: &[Node]) {
    t.push(Node::int(items.len() as i64));
    t.extend(items.iter().cloned());
}

#[cfg_attr(not(test), allow(dead_code))]
fn push_pairs(t: &mut Vec<Node>, items: &[(Node, Node)]) {
    t.push(Node::int(items.len() as i64));
    for (a, b) in items {
        t.push(a.clone());
        t.push(b.clone());
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn write_tree(d: &MoxelDoc) -> Vec<Node> {
    let mut t: Vec<Node> = d.version.clone();
    t.extend([
        d.lang.clone(),
        d.default_format.clone(),
        d.lines.clone(),
        d.u6.clone(),
    ]);
    t.extend(d.headers.iter().cloned());
    t.extend(d.u13.iter().cloned());
    t.push(Node::int(d.rows.len() as i64));
    for (idx, fmt, cells) in &d.rows {
        t.extend([
            Node::int(*idx),
            Node::int(*fmt),
            Node::int(cells.len() as i64),
        ]);
        for (col, cell) in cells {
            t.extend([Node::int(*col), cell.clone()]);
        }
    }
    let (first, rest) = d
        .columns
        .split_first()
        .map_or((None, &[][..]), |(f, r)| (Some(f), r));
    t.extend(first.cloned());
    t.push(Node::int(d.height));
    push_counted(&mut t, rest);
    let rc: Vec<(Node, Node)> = d
        .row_columns
        .iter()
        .map(|(a, b)| (Node::int(*a), Node::int(*b)))
        .collect();
    push_pairs(&mut t, &rc);
    t.push(d.drawing_next.clone());
    push_counted(&mut t, &d.drawings);
    push_pairs(&mut t, &d.vg);
    push_pairs(&mut t, &d.hg);
    push_counted(&mut t, &d.vbreaks);
    push_pairs(&mut t, &d.hbreaks);
    t.extend([
        d.merges.clone(),
        d.row_merges.clone(),
        d.col_merges.clone(),
        d.named.clone(),
        d.print_settings_name.clone(),
        d.print_settings.clone(),
        d.print_area.clone(),
    ]);
    t.extend(d.u_after_pa.iter().cloned());
    for tab in [
        &d.formats,
        &d.fonts,
        &d.format_strings,
        &d.value_types,
        &d.uuids,
        &d.colors,
        &d.pictures,
    ] {
        push_counted(&mut t, tab);
    }
    t.push(Node::int(d.save_view));
    if d.save_view != 0 {
        t.extend([Node::int(d.read_only), d.view_settings.clone()]);
        push_pairs(&mut t, &d.embedded);
        t.push(d.used_file_name.clone());
    } else {
        push_pairs(&mut t, &d.embedded);
        t.extend([
            d.used_file_name.clone(),
            Node::int(d.read_only),
            d.view_settings.clone(),
        ]);
    }
    t.push(d.style_colors.clone());
    push_counted(&mut t, &d.masks);
    t.extend(d.tail.iter().cloned());
    t
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn write_doc(d: &MoxelDoc) -> Vec<u8> {
    let doc = brace::Doc {
        root: Node::List(write_tree(d)),
        bom: d.bom,
        lead: d.lead.clone(),
        trailer: d.trailer.clone(),
    };
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&d.head);
    out.extend(brace::write_doc(&doc));
    out
}
