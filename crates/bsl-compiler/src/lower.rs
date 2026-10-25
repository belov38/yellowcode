use crate::app::AppMethodSig;
use crate::bytecode::{CompiledModule, Handler, MethodCode, Op, ParamCode};
use crate::ir::{
    Callee, Const, GlobalRef, IrArg, IrArm, IrBody, IrExpr, IrExprKind, IrMethod, IrModule, IrStmt,
    IrStmtKind, Place,
};
use crate::sym::Sym;
use bsl_prim::Number;
use bsl_syntax::ast::{BinOp, MethodKind, UnOp};
use std::collections::HashMap;
use std::hash::Hash;

pub(crate) fn lower(m: IrModule) -> CompiledModule {
    let mut pools = Pools::default();
    let methods = m.methods.iter().map(|x| method(&mut pools, x)).collect();
    let body = m.body.as_ref().map(|b| module_body(&mut pools, b));
    let IrModule {
        name,
        kind,
        context,
        compat,
        vars,
        source_lines,
        outer,
        caller_methods,
        caller_vars,
        ..
    } = m;
    CompiledModule {
        name,
        kind,
        context,
        compat,
        consts: pools.consts.list,
        syms: pools.syms.list,
        globals: pools.globals.list,
        app_methods: pools.app_methods.list,
        vars,
        methods,
        body,
        source_lines,
        outer,
        caller_methods,
        caller_vars,
    }
}

fn index(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn argc(n: usize) -> Option<u32> {
    u32::try_from(n).ok()
}

struct Pool<T> {
    list: Vec<T>,
    index: HashMap<T, u32>,
}

impl<T> Default for Pool<T> {
    fn default() -> Self {
        Pool {
            list: Vec::new(),
            index: HashMap::new(),
        }
    }
}

impl<T: Clone + Eq + Hash> Pool<T> {
    fn get(&mut self, v: &T) -> u32 {
        if let Some(&i) = self.index.get(v) {
            return i;
        }
        let i = index(self.list.len());
        self.list.push(v.clone());
        self.index.insert(v.clone(), i);
        i
    }
}

#[derive(Default)]
struct Pools {
    consts: Pool<Const>,
    syms: Pool<Sym>,
    globals: Pool<GlobalRef>,
    app_methods: Pool<AppMethodSig>,
}

fn method(pools: &mut Pools, m: &IrMethod) -> MethodCode {
    let params = m
        .params
        .iter()
        .map(|p| ParamCode {
            name: p.name.clone(),
            by_val: p.by_val,
            default: p.default.as_ref().map(|c| pools.consts.get(c)),
        })
        .collect();
    let (code, lines, handlers) = if m.callable_only {
        (Vec::new(), Vec::new(), Vec::new())
    } else {
        Gen::new(pools).body(&m.body.stmts, m.kind)
    };
    MethodCode {
        name: m.name.clone(),
        kind: m.kind,
        export: m.export,
        is_async: m.is_async,
        directive: m.directive,
        intercept: m.intercept.clone(),
        params,
        slot_names: m.body.slots.clone(),
        code,
        lines,
        handlers,
        callable_only: m.callable_only,
    }
}

fn module_body(pools: &mut Pools, b: &IrBody) -> MethodCode {
    let (code, lines, handlers) = Gen::new(pools).body(&b.stmts, MethodKind::Procedure);
    let empty: std::sync::Arc<str> = "".into();
    MethodCode {
        name: Sym {
            folded: empty.clone(),
            name: empty,
        },
        kind: MethodKind::Procedure,
        export: false,
        is_async: false,
        directive: None,
        intercept: None,
        params: Vec::new(),
        slot_names: b.slots.clone(),
        code,
        lines,
        handlers,
        callable_only: false,
    }
}

#[derive(Clone, Copy)]
struct Label(u32);

const UNBOUND: u32 = u32::MAX;

#[derive(Clone, Copy)]
enum Cond {
    Always,
    IfFalse,
    IfTrue,
}

struct Loop {
    exit: Label,
    next: Label,
    depth: u32,
}

enum Task<'a> {
    Expr(&'a IrExpr),
    Arg(&'a IrArg),
    EventOperand(&'a IrExpr),
    Op(Op),
    Jump(Cond, Label),
    Bind(Label),
}

struct Gen<'p> {
    pools: &'p mut Pools,
    code: Vec<Op>,
    lines: Vec<(u32, u32)>,
    line: Option<u32>,
    handlers: Vec<Handler>,
    labels: Vec<u32>,
    fixups: Vec<(u32, Label)>,
    loops: Vec<Loop>,
    depth: u32,
    goto_labels: HashMap<u32, (Label, u32)>,
}

fn scan_labels(stmts: &[IrStmt], depth: u32, out: &mut HashMap<u32, u32>) {
    for s in stmts {
        match &s.kind {
            IrStmtKind::Label(l) => {
                out.insert(*l, depth);
            }
            IrStmtKind::If { arms, otherwise } => {
                for a in arms {
                    scan_labels(&a.body, depth, out);
                }
                scan_labels(otherwise, depth, out);
            }
            IrStmtKind::While { body, .. }
            | IrStmtKind::For { body, .. }
            | IrStmtKind::ForEach { body, .. } => scan_labels(body, depth, out),
            IrStmtKind::Try { body, handler } => {
                scan_labels(body, depth, out);
                scan_labels(handler, depth.saturating_add(1), out);
            }
            _ => {}
        }
    }
}

impl<'p> Gen<'p> {
    fn new(pools: &'p mut Pools) -> Gen<'p> {
        Gen {
            pools,
            code: Vec::new(),
            lines: Vec::new(),
            line: None,
            handlers: Vec::new(),
            labels: Vec::new(),
            fixups: Vec::new(),
            loops: Vec::new(),
            depth: 0,
            goto_labels: HashMap::new(),
        }
    }

    fn body(
        mut self,
        stmts: &[IrStmt],
        kind: MethodKind,
    ) -> (Vec<Op>, Vec<(u32, u32)>, Vec<Handler>) {
        let mut depths = HashMap::new();
        scan_labels(stmts, 0, &mut depths);
        for (id, depth) in depths {
            let l = self.label();
            self.goto_labels.insert(id, (l, depth));
        }
        self.block(stmts);
        self.line = None;
        match kind {
            MethodKind::Function => {
                self.emit(Op::Undef);
                self.emit(Op::ReturnValue);
            }
            MethodKind::Procedure => {
                self.emit(Op::Return);
            }
        }
        self.patch();
        (self.code, self.lines, self.handlers)
    }

    fn pc(&self) -> u32 {
        index(self.code.len())
    }

    fn emit(&mut self, op: Op) {
        if let Some(line) = self.line.take() {
            let pc = self.pc();
            if self.lines.last().is_some_and(|&(p, _)| p == pc) {
                self.lines.pop();
            }
            if self.lines.last().is_none_or(|&(_, l)| l != line) {
                self.lines.push((pc, line));
            }
        }
        self.code.push(op);
    }

    fn label(&mut self) -> Label {
        let l = Label(index(self.labels.len()));
        self.labels.push(UNBOUND);
        l
    }

    fn bind(&mut self, l: Label) {
        let pc = self.pc();
        if let Some(slot) = self.labels.get_mut(l.0 as usize) {
            *slot = pc;
        }
    }

    fn jump(&mut self, c: Cond, l: Label) {
        self.fixups.push((self.pc(), l));
        self.emit(match c {
            Cond::Always => Op::Jump(UNBOUND),
            Cond::IfFalse => Op::JumpIfFalse(UNBOUND),
            Cond::IfTrue => Op::JumpIfTrue(UNBOUND),
        });
    }

    fn patch(&mut self) {
        let last = index(self.code.len().saturating_sub(1));
        for &(pc, l) in &self.fixups {
            let target = match self.labels.get(l.0 as usize) {
                Some(&t) if t != UNBOUND => t,
                _ => last,
            };
            match self.code.get_mut(pc as usize) {
                Some(Op::Jump(t) | Op::JumpIfFalse(t) | Op::JumpIfTrue(t)) => *t = target,
                Some(Op::IterNext { end, .. }) => *end = target,
                _ => {}
            }
        }
    }

    fn leave_to(&mut self, depth: u32) {
        let n = self.depth.saturating_sub(depth);
        if n > 0 {
            self.emit(Op::LeaveHandler(u16::try_from(n).unwrap_or(u16::MAX)));
        }
    }

    fn block(&mut self, stmts: &[IrStmt]) {
        for s in stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &IrStmt) {
        self.line = Some(s.line);
        match &s.kind {
            IrStmtKind::Assign { target, value } => self.assign(target, value),
            IrStmtKind::Eval(e) => self.run(vec![Task::Op(Op::Pop), Task::Expr(e)]),
            IrStmtKind::If { arms, otherwise } => self.if_stmt(arms, otherwise),
            IrStmtKind::While { cond, body } => self.while_stmt(cond, body),
            IrStmtKind::For {
                var,
                from,
                to,
                bound_slot,
                body,
            } => self.for_stmt(s.line, var, from, to, *bound_slot, body),
            IrStmtKind::ForEach {
                var,
                collection,
                iter_slot,
                body,
            } => self.foreach_stmt(var, collection, *iter_slot, body),
            IrStmtKind::Break => self.loop_jump(true),
            IrStmtKind::Continue => self.loop_jump(false),
            IrStmtKind::Return(e) => self.return_stmt(e.as_ref()),
            IrStmtKind::Try { body, handler } => self.try_stmt(body, handler),
            IrStmtKind::Raise(e) => self.run(vec![Task::Op(Op::Raise), Task::Expr(e)]),
            IrStmtKind::RaiseCall(args) => self.raise_call(args),
            IrStmtKind::Reraise => self.emit(Op::Reraise),
            IrStmtKind::Goto(id) => {
                if let Some(&(l, depth)) = self.goto_labels.get(id) {
                    self.leave_to(depth);
                    self.jump(Cond::Always, l);
                }
            }
            IrStmtKind::Label(id) => {
                if let Some(&(l, _)) = self.goto_labels.get(id) {
                    self.bind(l);
                }
            }
            IrStmtKind::Execute(e) => self.run(vec![Task::Op(Op::Execute), Task::Expr(e)]),
            IrStmtKind::AddHandler { event, handler } => self.run(vec![
                Task::Op(Op::AddHandler),
                Task::EventOperand(handler),
                Task::EventOperand(event),
            ]),
            IrStmtKind::RemoveHandler { event, handler } => self.run(vec![
                Task::Op(Op::RemoveHandler),
                Task::EventOperand(handler),
                Task::EventOperand(event),
            ]),
            IrStmtKind::Await(e) => {
                self.run(vec![Task::Op(Op::Pop), Task::Op(Op::Await), Task::Expr(e)])
            }
        }
    }

    #[inline(never)]
    fn raise_call(&mut self, args: &[IrArg]) {
        let Some(n) = argc(args.len()) else {
            self.emit(Op::Undef);
            self.emit(Op::Raise);
            return;
        };
        let mut tasks = vec![Task::Op(Op::RaiseExt(n))];
        tasks.extend(args.iter().rev().map(Task::Arg));
        self.run(tasks);
    }

    #[inline(never)]
    fn assign(&mut self, target: &Place, value: &IrExpr) {
        let tasks = match target {
            Place::Member { object, name } => {
                let sym = self.pools.syms.get(name);
                vec![
                    Task::Op(Op::SetMember(sym)),
                    Task::Expr(value),
                    Task::Expr(object),
                ]
            }
            Place::Index { object, index } => vec![
                Task::Op(Op::SetIndex),
                Task::Expr(value),
                Task::Expr(index),
                Task::Expr(object),
            ],
            simple => {
                let store = self.store_op(simple);
                vec![Task::Op(store), Task::Expr(value)]
            }
        };
        self.run(tasks);
    }

    #[inline(never)]
    fn if_stmt(&mut self, arms: &[IrArm], otherwise: &[IrStmt]) {
        let end = self.label();
        for (i, arm) in arms.iter().enumerate() {
            self.line = Some(arm.line);
            let next = self.label();
            self.run(vec![Task::Jump(Cond::IfFalse, next), Task::Expr(&arm.cond)]);
            self.block(&arm.body);
            if i + 1 < arms.len() || !otherwise.is_empty() {
                self.jump(Cond::Always, end);
            }
            self.bind(next);
        }
        self.block(otherwise);
        self.bind(end);
    }

    #[inline(never)]
    fn while_stmt(&mut self, cond: &IrExpr, body: &[IrStmt]) {
        let (top, exit) = (self.label(), self.label());
        self.bind(top);
        self.run(vec![Task::Jump(Cond::IfFalse, exit), Task::Expr(cond)]);
        self.loop_body(exit, top, body);
        self.jump(Cond::Always, top);
        self.bind(exit);
    }

    #[inline(never)]
    fn for_stmt(
        &mut self,
        line: u32,
        var: &Place,
        from: &IrExpr,
        to: &IrExpr,
        bound: u32,
        body: &[IrStmt],
    ) {
        let (top, next, exit) = (self.label(), self.label(), self.label());
        let (load, store) = (self.load_op(var), self.store_op(var));
        self.run(vec![
            Task::Op(store),
            Task::Op(Op::StoreLocal(bound)),
            Task::Op(Op::Pos),
            Task::Expr(to),
            Task::Expr(from),
        ]);
        self.bind(top);
        self.emit(load);
        self.emit(Op::LoadLocal(bound));
        self.emit(Op::Le);
        self.jump(Cond::IfFalse, exit);
        self.loop_body(exit, next, body);
        self.bind(next);
        self.line = Some(line);
        self.emit(load);
        self.emit(Op::Pos);
        let one = self.pools.consts.get(&Const::Number(Number::one()));
        self.emit(Op::Const(one));
        self.emit(Op::Add);
        self.emit(store);
        self.jump(Cond::Always, top);
        self.bind(exit);
    }

    #[inline(never)]
    fn foreach_stmt(&mut self, var: &Place, coll: &IrExpr, iter: u32, body: &[IrStmt]) {
        let (top, exit) = (self.label(), self.label());
        let store = self.store_op(var);
        self.run(vec![Task::Op(Op::IterStart(iter)), Task::Expr(coll)]);
        self.bind(top);
        self.fixups.push((self.pc(), exit));
        self.emit(Op::IterNext {
            slot: iter,
            end: UNBOUND,
        });
        self.emit(store);
        self.loop_body(exit, top, body);
        self.jump(Cond::Always, top);
        self.bind(exit);
    }

    fn loop_body(&mut self, exit: Label, next: Label, body: &[IrStmt]) {
        self.loops.push(Loop {
            exit,
            next,
            depth: self.depth,
        });
        self.block(body);
        self.loops.pop();
    }

    fn loop_jump(&mut self, exit: bool) {
        let Some(lp) = self.loops.last() else {
            return;
        };
        let (target, depth) = (if exit { lp.exit } else { lp.next }, lp.depth);
        self.leave_to(depth);
        self.jump(Cond::Always, target);
    }

    #[inline(never)]
    fn return_stmt(&mut self, value: Option<&IrExpr>) {
        match value {
            Some(e) => {
                self.run(vec![Task::Expr(e)]);
                self.leave_to(0);
                self.emit(Op::ReturnValue);
            }
            None => {
                self.leave_to(0);
                self.emit(Op::Return);
            }
        }
    }

    #[inline(never)]
    fn try_stmt(&mut self, body: &[IrStmt], handler: &[IrStmt]) {
        let start = self.pc();
        self.block(body);
        let end = self.pc();
        let done = self.label();
        self.jump(Cond::Always, done);
        self.handlers.push(Handler {
            start,
            end,
            target: self.pc(),
            depth: self.depth,
        });
        self.emit(Op::EnterHandler);
        self.depth = self.depth.saturating_add(1);
        self.block(handler);
        self.depth = self.depth.saturating_sub(1);
        self.emit(Op::LeaveHandler(1));
        self.bind(done);
    }

    fn load_op(&mut self, p: &Place) -> Op {
        match p {
            Place::Local(i) => Op::LoadLocal(*i),
            Place::ModVar(i) => Op::LoadModVar(*i),
            Place::Outer(i) => Op::LoadOuter(*i),
            Place::Global(g) => Op::LoadGlobal(self.pools.globals.get(g)),
            Place::Context(s) => Op::LoadContext(self.pools.syms.get(s)),
            Place::Member { .. } | Place::Index { .. } => Op::Undef,
        }
    }

    fn store_op(&mut self, p: &Place) -> Op {
        match p {
            Place::Local(i) => Op::StoreLocal(*i),
            Place::ModVar(i) => Op::StoreModVar(*i),
            Place::Outer(i) => Op::StoreOuter(*i),
            Place::Global(g) => Op::StoreGlobal(self.pools.globals.get(g)),
            Place::Context(s) => Op::StoreContext(self.pools.syms.get(s)),
            Place::Member { .. } | Place::Index { .. } => Op::Pop,
        }
    }

    fn konst(&mut self, c: &Const) -> Op {
        match c {
            Const::Bool(true) => Op::True,
            Const::Bool(false) => Op::False,
            Const::Undefined => Op::Undef,
            Const::Null => Op::Null,
            _ => Op::Const(self.pools.consts.get(c)),
        }
    }

    fn run<'a>(&mut self, mut tasks: Vec<Task<'a>>) {
        while let Some(t) = tasks.pop() {
            match t {
                Task::Expr(e) => self.expr(e, &mut tasks),
                Task::Arg(a) => self.arg(a, &mut tasks),
                Task::EventOperand(e) => match &e.kind {
                    IrExprKind::Read(Place::Member { object, name }) => {
                        let sym = self.pools.syms.get(name);
                        tasks.extend([Task::Op(Op::RefMember(sym)), Task::Expr(object)]);
                    }
                    _ => tasks.push(Task::Expr(e)),
                },
                Task::Op(op) => self.emit(op),
                Task::Jump(c, l) => self.jump(c, l),
                Task::Bind(l) => self.bind(l),
            }
        }
    }

    fn expr<'a>(&mut self, e: &'a IrExpr, tasks: &mut Vec<Task<'a>>) {
        match &e.kind {
            IrExprKind::Const(c) => {
                let op = self.konst(c);
                self.emit(op);
            }
            IrExprKind::Read(p) => match p {
                Place::Member { object, name } => {
                    let sym = self.pools.syms.get(name);
                    tasks.extend([Task::Op(Op::GetMember(sym)), Task::Expr(object)]);
                }
                Place::Index { object, index } => tasks.extend([
                    Task::Op(Op::GetIndex),
                    Task::Expr(index),
                    Task::Expr(object),
                ]),
                simple => {
                    let op = self.load_op(simple);
                    self.emit(op);
                }
            },
            IrExprKind::Call {
                callee,
                args,
                want_value,
            } => {
                let Some(n) = argc(args.len()) else {
                    return self.emit(Op::Undef);
                };
                let want = *want_value;
                let op = match callee {
                    Callee::Local(i) => Op::CallLocal {
                        method: *i,
                        argc: n,
                        want,
                    },
                    Callee::App(sig) => Op::CallApp {
                        method: self.pools.app_methods.get(sig),
                        argc: n,
                        want,
                    },
                    Callee::Platform(i) => Op::CallPlatform {
                        method: *i,
                        argc: n,
                        want,
                    },
                    Callee::Context(s) => Op::CallContext {
                        name: self.pools.syms.get(s),
                        argc: n,
                        want,
                    },
                };
                tasks.push(Task::Op(op));
                push_args(tasks, args);
            }
            IrExprKind::CallMember {
                object,
                name,
                args,
                want_value,
            } => {
                let Some(n) = argc(args.len()) else {
                    return self.emit(Op::Undef);
                };
                let op = Op::CallMember {
                    name: self.pools.syms.get(name),
                    argc: n,
                    want: *want_value,
                };
                tasks.push(Task::Op(op));
                push_args(tasks, args);
                tasks.push(Task::Expr(object));
            }
            IrExprKind::New { ty, args } => match args.as_deref() {
                None | Some([]) => self.emit(Op::NewNoArgs { ty: ty.0 }),
                Some(args) => {
                    let Some(n) = argc(args.len()) else {
                        return self.emit(Op::Undef);
                    };
                    tasks.push(Task::Op(Op::New { ty: ty.0, argc: n }));
                    push_args(tasks, args);
                }
            },
            IrExprKind::NewDynamic { args } => {
                let Some(n) = argc(args.len()) else {
                    return self.emit(Op::Undef);
                };
                tasks.push(Task::Op(Op::NewDynamic { argc: n }));
                push_args(tasks, args);
            }
            IrExprKind::Unary { op, operand } => {
                let op = match op {
                    UnOp::Neg => Op::Neg,
                    UnOp::Pos => Op::Pos,
                    UnOp::Not => Op::Not,
                };
                tasks.extend([Task::Op(op), Task::Expr(operand)]);
            }
            IrExprKind::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs, tasks),
            IrExprKind::Ternary {
                cond,
                then,
                otherwise,
            } => {
                let (other, end) = (self.label(), self.label());
                tasks.extend([
                    Task::Bind(end),
                    Task::Expr(otherwise),
                    Task::Bind(other),
                    Task::Jump(Cond::Always, end),
                    Task::Expr(then),
                    Task::Jump(Cond::IfFalse, other),
                    Task::Expr(cond),
                ]);
            }
            IrExprKind::Evaluate(x) => tasks.extend([Task::Op(Op::Evaluate), Task::Expr(x)]),
            IrExprKind::Await(x) => tasks.extend([Task::Op(Op::Await), Task::Expr(x)]),
        }
    }

    fn binary<'a>(
        &mut self,
        op: BinOp,
        lhs: &'a IrExpr,
        rhs: &'a IrExpr,
        tasks: &mut Vec<Task<'a>>,
    ) {
        let op = match op {
            BinOp::And => return self.short(Cond::IfFalse, Op::False, lhs, rhs, tasks),
            BinOp::Or => return self.short(Cond::IfTrue, Op::True, lhs, rhs, tasks),
            BinOp::Eq => Op::Eq,
            BinOp::Ne => Op::Ne,
            BinOp::Lt => Op::Lt,
            BinOp::Le => Op::Le,
            BinOp::Gt => Op::Gt,
            BinOp::Ge => Op::Ge,
            BinOp::Add => Op::Add,
            BinOp::Sub => Op::Sub,
            BinOp::Mul => Op::Mul,
            BinOp::Div => Op::Div,
            BinOp::Mod => Op::Mod,
        };
        tasks.extend([Task::Op(op), Task::Expr(rhs), Task::Expr(lhs)]);
    }

    fn short<'a>(
        &mut self,
        cond: Cond,
        short: Op,
        lhs: &'a IrExpr,
        rhs: &'a IrExpr,
        tasks: &mut Vec<Task<'a>>,
    ) {
        let (taken, end) = (self.label(), self.label());
        tasks.extend([
            Task::Bind(end),
            Task::Op(short),
            Task::Bind(taken),
            Task::Jump(Cond::Always, end),
            Task::Op(Op::ToBool),
            Task::Expr(rhs),
            Task::Jump(cond, taken),
            Task::Expr(lhs),
        ]);
    }

    fn arg<'a>(&mut self, a: &'a IrArg, tasks: &mut Vec<Task<'a>>) {
        match a {
            IrArg::Omitted => self.emit(Op::Omitted),
            IrArg::Value(e) => tasks.push(Task::Expr(e)),
            IrArg::ByRef(p) => match p {
                Place::Local(i) => self.emit(Op::RefLocal(*i)),
                Place::ModVar(i) => self.emit(Op::RefModVar(*i)),
                Place::Outer(i) => self.emit(Op::RefOuter(*i)),
                Place::Member { object, name } => {
                    let sym = self.pools.syms.get(name);
                    tasks.extend([Task::Op(Op::RefMember(sym)), Task::Expr(object)]);
                }
                Place::Index { object, index } => tasks.extend([
                    Task::Op(Op::RefIndex),
                    Task::Expr(index),
                    Task::Expr(object),
                ]),
                Place::Global(_) | Place::Context(_) => {
                    let op = self.load_op(p);
                    self.emit(op);
                }
            },
        }
    }
}

fn push_args<'a>(tasks: &mut Vec<Task<'a>>, args: &'a [IrArg]) {
    tasks.extend(args.iter().rev().map(Task::Arg));
}
