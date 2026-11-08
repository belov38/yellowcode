use super::{Occurrence, Root};
use crate::host::Access;
use crate::ir::{ColSel, Expr, ExprKind, Rel, SrcId, SrcKind, StmtS};
use crate::schema::{FieldRole, QuerySchema, StdRole};
use std::collections::HashMap;

const NODE_LIMIT: usize = 256;
const EDGE_LIMIT: usize = 512;
pub(in crate::bind) const VISIT_LIMIT: usize = 16384;
const NONE: u16 = u16::MAX;
use crate::bind::temp_provenance::{BALANCE, SAFE, TURNOVERS, UNKNOWN};
const BLOCKED: u8 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Key {
    Column(SrcId, ColSel),
    Consumer(SrcId),
    Output(u16),
}
impl Key {
    pub(super) const fn column(src: SrcId, column: u16) -> Self {
        Self::Column(src, ColSel::Column(column))
    }
    fn hash(self) -> usize {
        let (src, slot) = match self {
            Self::Column(src, ColSel::Column(i)) => (src.0, u32::from(i)),
            Self::Column(src, ColSel::Field(i)) => (src.0, u32::from(i.0) | 0x10000),
            Self::Consumer(src) => (src.0, 0x20000),
            Self::Output(i) => (u32::MAX, u32::from(i) | 0x30000),
        };
        (src.wrapping_mul(0x9e3779b1) ^ slot.wrapping_mul(0x85ebca6b)) as usize & 511
    }
}
#[derive(Clone, Copy)]
struct Node {
    key: Key,
    owner: Option<SrcId>,
    head: u16,
    flags: u8,
    defined: bool,
    state: u8,
    value: u8,
}
const EMPTY_NODE: Node = Node {
    key: Key::column(SrcId(0), 0),
    owner: None,
    head: NONE,
    flags: 0,
    defined: false,
    state: 0,
    value: UNKNOWN,
};
#[derive(Clone, Copy)]
struct Edge {
    to: u16,
    next: u16,
}
#[derive(Clone, Copy)]
struct Eval {
    node: u16,
    edge: u16,
    flags: u8,
}
#[derive(Clone, Copy)]
enum Work<'s> {
    Empty,
    Scan(&'s Rel, bool),
    Output(&'s Rel, u16, Key),
}

pub(in crate::bind) struct Proof {
    nodes: [Node; NODE_LIMIT],
    edges: [Edge; EDGE_LIMIT],
    index: [u16; 512],
    eval: [Eval; NODE_LIMIT],
    count: usize,
    edge_count: usize,
    visits: usize,
    exhausted: bool,
    disabled: bool,
    ordering: bool,
    balance_columns: bool,
    temp_enabled: bool,
    temp_complete: bool,
}
pub(in crate::bind) struct Workspace<'s> {
    pub(super) proof: Proof,
    work: [Work<'s>; NODE_LIMIT],
    work_count: usize,
    temps: Option<&'s dyn crate::bind::temp_provenance::TempOriginProvider>,
    version: u64,
}
impl std::ops::Deref for Workspace<'_> {
    type Target = Proof;
    fn deref(&self) -> &Proof {
        &self.proof
    }
}
impl std::ops::DerefMut for Workspace<'_> {
    fn deref_mut(&mut self) -> &mut Proof {
        &mut self.proof
    }
}
#[allow(
    clippy::indexing_slicing,
    reason = "Work indices are bounded by push/pop; every graph index comes from checked fixed-capacity insertion"
)]
impl<'s> Workspace<'s> {
    pub(in crate::bind) fn new() -> Self {
        Self {
            proof: Proof::new(),
            work: [Work::Empty; NODE_LIMIT],
            work_count: 0,
            temps: None,
            version: 0,
        }
    }
    pub(in crate::bind) fn with_phase(phase: crate::bind::PhaseContext<'_>) -> Self {
        let mut workspace = Self::new();
        workspace.proof.balance_columns = phase.enabled();
        workspace
    }
    fn push(&mut self, work: Work<'s>) {
        if self.work_count + 1 >= NODE_LIMIT {
            self.exhausted = true;
            return;
        }
        self.work[self.work_count] = work;
        self.work_count += 1;
    }
    pub(in crate::bind) fn set_temp_origins(
        &mut self,
        temps: Option<&'s dyn crate::bind::temp_provenance::TempOriginProvider>,
        version: u64,
    ) {
        self.temps = temps;
        self.version = version;
        self.proof.temp_enabled = temps.is_some();
    }
    pub(in crate::bind) fn record_outputs(
        &mut self,
        rel: &'s Rel,
        stmt: &'s StmtS,
        schema: &QuerySchema,
        width: usize,
    ) {
        for i in 0..width {
            let Ok(i) = u16::try_from(i) else {
                self.exhausted = true;
                break;
            };
            self.push(Work::Output(rel, i, Key::Output(i)));
        }
        self.drain(stmt, schema);
    }
    pub(in crate::bind) fn record_ir(
        &mut self,
        rel: &'s Rel,
        stmt: &'s StmtS,
        schema: &QuerySchema,
        membership: bool,
    ) {
        self.push(Work::Scan(rel, membership));
        self.drain(stmt, schema);
    }
    fn drain(&mut self, stmt: &'s StmtS, schema: &QuerySchema) {
        while self.work_count > 0 && !self.exhausted {
            if !self.spend(1) {
                break;
            }
            self.work_count -= 1;
            let work = std::mem::replace(&mut self.work[self.work_count], Work::Empty);
            match work {
                Work::Empty => {}
                Work::Scan(r, member) => match r {
                    Rel::TempScan { src, name } => {
                        if let Some(SrcKind::Temp { columns, stored }) =
                            stmt.sources.get(src.0 as usize).map(|s| &s.kind)
                        {
                            self.proof.import_temp(
                                *src,
                                name,
                                crate::bind::temp_provenance::TempShape::Ir { columns, stored },
                                self.temps,
                                self.version,
                            );
                        }
                    }
                    Rel::Subquery { src, input } => {
                        if let Some(SrcKind::Subquery { columns }) =
                            stmt.sources.get(src.0 as usize).map(|s| &s.kind)
                        {
                            for col in 0..columns.len() {
                                let Ok(col) = u16::try_from(col) else {
                                    self.exhausted = true;
                                    break;
                                };
                                let out = Key::column(*src, col);
                                let _ = self.find(out, true);
                                self.push(Work::Output(input, col, out));
                                if self.exhausted {
                                    break;
                                }
                            }
                        }
                        self.push(Work::Scan(input, member));
                    }
                    Rel::Join { l, r, .. } => {
                        self.push(Work::Scan(l, member));
                        self.push(Work::Scan(r, member));
                    }
                    Rel::Union { parts, .. } => {
                        for part in parts {
                            self.push(Work::Scan(part, member));
                            if self.exhausted {
                                break;
                            }
                        }
                    }
                    Rel::Filter { input, .. }
                    | Rel::Project { input, .. }
                    | Rel::Aggregate { input, .. }
                    | Rel::Sort { input, .. }
                    | Rel::Top { input, .. }
                    | Rel::Distinct(input) => self.push(Work::Scan(input, member)),
                    Rel::Hierarchy { input, .. } => {
                        self.disable();
                        self.push(Work::Scan(input, member));
                    }
                    Rel::RegisterVt(v)
                        if member
                            && (crate::lower::is_accounting_balance(schema, v.table)
                                || crate::lower::is_accounting_turnovers(schema, v.table)) =>
                    {
                        self.consumer(v.src)
                    }
                    _ => {}
                },
                Work::Output(r, col, out) => match r {
                    Rel::Project { input, exprs } => {
                        let keys = self.group_keys(input);
                        if let Some(e) = exprs.get(usize::from(col)) {
                            self.ir_value(out, e, keys, stmt, schema);
                        } else {
                            self.unknown(out);
                        }
                    }
                    Rel::Filter { input, .. }
                    | Rel::Sort { input, .. }
                    | Rel::Top { input, .. }
                    | Rel::Distinct(input) => self.push(Work::Output(input, col, out)),
                    Rel::Union { parts, .. } if !parts.is_empty() => {
                        for part in parts {
                            self.push(Work::Output(part, col, out));
                            if self.exhausted {
                                break;
                            }
                        }
                    }
                    _ => self.unknown(out),
                },
            }
        }
    }
}
const _: () = assert!(std::mem::size_of::<Workspace<'_>>() <= 32 * 1024);

#[allow(
    clippy::indexing_slicing,
    reason = "Node/edge indices come only from checked insertions; DFS frames and hash slots are bounded by their arrays"
)]
impl Proof {
    pub(in crate::bind) fn new() -> Self {
        Self {
            nodes: [EMPTY_NODE; NODE_LIMIT],
            edges: [Edge {
                to: NONE,
                next: NONE,
            }; EDGE_LIMIT],
            index: [NONE; 512],
            eval: [Eval {
                node: NONE,
                edge: NONE,
                flags: 0,
            }; NODE_LIMIT],
            count: 0,
            edge_count: 0,
            visits: 0,
            exhausted: false,
            disabled: false,
            ordering: false,
            balance_columns: false,
            temp_enabled: false,
            temp_complete: true,
        }
    }
    pub(in crate::bind) fn temp_origins_enabled(&self) -> bool {
        self.temp_enabled && self.temp_complete
    }
    pub(in crate::bind) fn failed(&self) -> bool {
        self.exhausted || self.disabled
    }
    pub(in crate::bind) fn export_output(&mut self, column: u16) -> u8 {
        self.solve(Key::Output(column), |_| Some(false))
    }
    pub(super) fn enable_temp_origins(&mut self, enabled: bool) {
        self.temp_enabled = enabled;
    }
    pub(super) fn import_temp(
        &mut self,
        src: SrcId,
        name: &str,
        shape: crate::bind::temp_provenance::TempShape<'_>,
        provider: Option<&dyn crate::bind::temp_provenance::TempOriginProvider>,
        version: u64,
    ) {
        use crate::bind::temp_provenance::ImportEvent;
        let width = shape.width();
        for i in 0..width {
            let Ok(column) = u16::try_from(i) else {
                self.exhausted = true;
                self.temp_complete = false;
                return;
            };
            self.null(Key::column(src, column));
        }
        let complete = provider.is_some_and(|provider| {
            crate::bind::temp_provenance::import_columns(
                provider,
                name,
                shape,
                version,
                &mut |event| match event {
                    ImportEvent::Step(n) => self.spend(n),
                    ImportEvent::Column(column, flags) => {
                        self.leaf(Key::column(src, column), flags, None);
                        !self.exhausted
                    }
                },
            )
        });
        if !complete {
            self.temp_complete = false;
            for i in 0..width {
                let Ok(column) = u16::try_from(i) else {
                    self.exhausted = true;
                    break;
                };
                self.unknown(Key::column(src, column));
            }
        }
    }
    pub(in crate::bind) fn disable(&mut self) {
        self.disabled = true;
    }
    pub(super) fn set_ordering(&mut self, active: bool) -> bool {
        std::mem::replace(&mut self.ordering, active)
    }
    pub(super) fn order_role(&mut self, role: StdRole) {
        if self.ordering && role == StdRole::Parent {
            self.disable();
        }
    }
    pub(super) fn spend(&mut self, n: usize) -> bool {
        if self.exhausted {
            return false;
        }
        match self.visits.checked_add(n) {
            Some(next) if next < VISIT_LIMIT => {
                self.visits = next;
                true
            }
            _ => {
                self.exhausted = true;
                false
            }
        }
    }
    fn find(&mut self, key: Key, insert: bool) -> Option<u16> {
        let start = key.hash();
        for offset in 0..self.index.len() {
            if !self.spend(1) {
                return None;
            }
            let slot = (start + offset) & 511;
            let id = self.index[slot];
            if id != NONE {
                if self.nodes[usize::from(id)].key == key {
                    return Some(id);
                }
            } else if insert {
                if self.count + 1 >= NODE_LIMIT {
                    self.exhausted = true;
                    return None;
                }
                let id = self.count as u16;
                self.nodes[self.count] = Node { key, ..EMPTY_NODE };
                self.index[slot] = id;
                self.count += 1;
                return Some(id);
            } else {
                return None;
            }
        }
        self.exhausted = true;
        None
    }
    fn leaf(&mut self, key: Key, flags: u8, owner: Option<SrcId>) {
        if let Some(id) = self.find(key, true) {
            let n = &mut self.nodes[usize::from(id)];
            n.defined = true;
            n.flags |= flags;
            n.owner = owner;
        }
    }
    pub(super) fn safe(&mut self, key: Key) {
        self.leaf(key, SAFE, None);
    }
    pub(super) fn null(&mut self, key: Key) {
        self.leaf(key, 0, None);
    }
    pub(super) fn unknown(&mut self, key: Key) {
        self.leaf(key, UNKNOWN, None);
    }
    fn account(&mut self, key: Key, src: SrcId, balance: bool) {
        self.leaf(key, if balance { BALANCE } else { TURNOVERS }, Some(src));
    }
    pub(super) fn copy(&mut self, output: Key, input: Key) {
        let (Some(parent), Some(child)) = (self.find(output, true), self.find(input, true)) else {
            return;
        };
        if self.edge_count + 1 >= EDGE_LIMIT {
            self.exhausted = true;
            return;
        }
        let p = &mut self.nodes[usize::from(parent)];
        self.edges[self.edge_count] = Edge {
            to: child,
            next: p.head,
        };
        p.head = self.edge_count as u16;
        p.defined = true;
        self.edge_count += 1;
    }
    pub(super) fn consumer(&mut self, src: SrcId) {
        self.safe(Key::Consumer(src));
    }

    pub(super) fn field(
        &mut self,
        key: Key,
        src: SrcId,
        table: crate::QTableId,
        field: crate::QFieldId,
        schema: &QuerySchema,
    ) {
        let Some(f) = schema.field(table, field) else {
            self.unknown(key);
            return;
        };
        let balance = crate::lower::is_accounting_balance(schema, table);
        let turnovers = crate::lower::is_accounting_turnovers(schema, table);
        if (balance || turnovers) && f.role == FieldRole::Std(StdRole::Account) {
            self.account(key, src, balance);
        } else if self.balance_columns
            && balance
            && f.role == FieldRole::Std(StdRole::ExtDimension(1))
        {
            self.unknown(key);
        } else {
            self.safe(key);
        }
    }
    fn ir_column(&mut self, key: Key, stmt: &StmtS, schema: &QuerySchema) {
        let Key::Column(src, col) = key else {
            self.unknown(key);
            return;
        };
        match (stmt.sources.get(src.0 as usize).map(|s| &s.kind), col) {
            (
                Some(SrcKind::Table(table) | SrcKind::Virtual { table, .. }),
                ColSel::Field(field),
            ) => {
                self.field(key, src, *table, field, schema);
            }
            (Some(SrcKind::Virtual { table, cols }), ColSel::Column(i)) => {
                if let Some(field) = cols.get(usize::from(i)).copied().flatten() {
                    self.field(key, src, *table, field, schema);
                } else {
                    self.unknown(key);
                }
            }
            (Some(SrcKind::Subquery { .. } | SrcKind::Temp { .. }), _) => {
                let _ = self.find(key, true);
            }
            _ => self.unknown(key),
        }
    }
    fn ir_value<'e>(
        &mut self,
        out: Key,
        mut e: &'e Expr,
        keys: Option<&'e [Expr]>,
        stmt: &StmtS,
        schema: &QuerySchema,
    ) {
        loop {
            if !self.spend(1) {
                return;
            }
            match &e.kind {
                ExprKind::Cast { x, .. } => e = x,
                ExprKind::LocalKey { orig, .. } | ExprKind::OuterKey { orig, .. } => e = orig,
                ExprKind::KeyRef(i) => {
                    let Some(key) = keys.and_then(|ks| ks.get(usize::from(*i))) else {
                        self.unknown(out);
                        return;
                    };
                    e = key;
                }
                ExprKind::Col { src, col } => {
                    let input = Key::Column(*src, *col);
                    self.ir_column(input, stmt, schema);
                    self.copy(out, input);
                    return;
                }
                ExprKind::Lit(crate::QValue::Null) => {
                    self.null(out);
                    return;
                }
                ExprKind::Lit(_) | ExprKind::Param(_) | ExprKind::Value(_) => {
                    self.safe(out);
                    return;
                }
                _ => {
                    self.unknown(out);
                    return;
                }
            }
        }
    }
    fn group_keys<'r>(&mut self, mut r: &'r Rel) -> Option<&'r [Expr]> {
        loop {
            if !self.spend(1) {
                return None;
            }
            match r {
                Rel::Aggregate { keys, .. } => return Some(keys),
                Rel::Filter { input, .. } => r = input,
                _ => return None,
            }
        }
    }

    fn solve(&mut self, key: Key, mut restricted: impl FnMut(SrcId) -> Option<bool>) -> u8 {
        if self.disabled || self.exhausted {
            return UNKNOWN;
        }
        for i in 0..self.count {
            if !self.spend(1) {
                return UNKNOWN;
            }
            self.nodes[i].state = 0;
        }
        let Some(root) = self.find(key, false) else {
            return UNKNOWN;
        };
        let mut depth = 0usize;
        self.eval[0] = Eval {
            node: root,
            edge: NONE,
            flags: 0,
        };
        loop {
            if !self.spend(1) {
                return UNKNOWN;
            }
            let id = usize::from(self.eval[depth].node);
            if self.nodes[id].state == 0 {
                let n = self.nodes[id];
                let mut flags = if n.defined { n.flags } else { UNKNOWN };
                if let Some(owner) = n.owner {
                    flags = match restricted(owner) {
                        Some(true) => flags | BLOCKED,
                        Some(false) => flags,
                        None => UNKNOWN,
                    };
                }
                self.nodes[id].state = 1;
                self.eval[depth].edge = n.head;
                self.eval[depth].flags = flags;
            }
            let next = self.eval[depth].edge;
            if next == NONE {
                let value = self.eval[depth].flags;
                let value = if value & UNKNOWN != 0 { UNKNOWN } else { value };
                self.nodes[id].state = 2;
                self.nodes[id].value = value;
                if depth == 0 {
                    return value;
                }
                depth -= 1;
                self.eval[depth].flags |= value;
                continue;
            }
            let edge = self.edges[usize::from(next)];
            let child = usize::from(edge.to);
            match self.nodes[child].state {
                0 => {
                    if depth + 1 >= NODE_LIMIT {
                        self.exhausted = true;
                        return UNKNOWN;
                    }
                    self.eval[depth].edge = edge.next;
                    depth += 1;
                    self.eval[depth] = Eval {
                        node: edge.to,
                        edge: NONE,
                        flags: 0,
                    };
                }
                state => {
                    self.eval[depth].flags |= if state == 1 {
                        UNKNOWN
                    } else {
                        self.nodes[child].value
                    };
                    self.eval[depth].edge = edge.next;
                }
            }
        }
    }
    pub(super) fn permits(
        &mut self,
        root: &Root,
        target_restricted: bool,
        keys: &[Occurrence],
        decisions: &[Access],
        aliases: &HashMap<SrcId, SrcId>,
    ) -> bool {
        let Root::Column(src, ColSel::Column(column)) = root else {
            return false;
        };
        if self.disabled || self.exhausted {
            return false;
        }
        for i in 0..self.count {
            if let Key::Consumer(src) = self.nodes[i].key {
                if !self.spend(keys.len()) {
                    return false;
                }
                if live_restricted(src, true, keys, decisions, aliases) != Some(false) {
                    return false;
                }
            }
        }
        let Some(lookup_cost) = self.count.checked_mul(keys.len()) else {
            self.exhausted = true;
            return false;
        };
        if !self.spend(lookup_cost) {
            return false;
        }
        let facts = self.solve(Key::column(*src, *column), |src| {
            live_restricted(src, false, keys, decisions, aliases)
        });
        facts & (UNKNOWN | BLOCKED) == 0
            && !(target_restricted && facts & (BALANCE | TURNOVERS) != 0)
    }
}
fn live_restricted(
    src: SrcId,
    chart_child: bool,
    keys: &[Occurrence],
    decisions: &[Access],
    aliases: &HashMap<SrcId, SrcId>,
) -> Option<bool> {
    let src = aliases.get(&src).copied().unwrap_or(src);
    let mut found = false;
    let mut restricted = false;
    for (key, access) in keys.iter().zip(decisions) {
        let matched = match key {
            Occurrence::Direct(id) => *id == src,
            Occurrence::BalanceChart(key) if chart_child => {
                matches!(&**key, Occurrence::Direct(id) if *id == src)
            }
            _ => false,
        };
        if matched {
            found = true;
            restricted |= !matches!(access, Access::Unrestricted);
        }
    }
    found.then_some(restricted)
}
