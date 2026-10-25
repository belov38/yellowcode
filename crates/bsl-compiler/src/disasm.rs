use crate::bytecode::{CompiledModule, MethodCode, Op};
use crate::ir_dump::{constant, global_name};
use crate::options::ModuleKind;
use crate::sym::Sym;
use bsl_syntax::ast::MethodKind;
use std::collections::BTreeSet;
use std::fmt::Write;

pub(crate) fn module(m: &CompiledModule) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "module {} ({}, {:?}, {})",
        m.name,
        kind_name(m.kind),
        m.context,
        m.compat.xml_name()
    );
    for v in &m.vars {
        s.push_str("var ");
        s.push_str(&v.name.name);
        if v.export {
            s.push_str(" export");
        }
        s.push('\n');
    }
    for c in &m.methods {
        header(&mut s, c);
        code(&mut s, m, c);
    }
    if let Some(b) = &m.body {
        s.push_str("body\n");
        code(&mut s, m, b);
    }
    s
}

fn kind_name(k: ModuleKind) -> &'static str {
    match k {
        ModuleKind::Common(_) => "Common",
        ModuleKind::Object => "Object",
        ModuleKind::Manager => "Manager",
        ModuleKind::RecordSet => "RecordSet",
        ModuleKind::ValueManager => "ValueManager",
        ModuleKind::Command => "Command",
        ModuleKind::Form => "Form",
        ModuleKind::Session => "Session",
        ModuleKind::ManagedApplication => "ManagedApplication",
        ModuleKind::OrdinaryApplication => "OrdinaryApplication",
        ModuleKind::ExternalConnection => "ExternalConnection",
        ModuleKind::HttpService => "HttpService",
        ModuleKind::WebService => "WebService",
    }
}

fn header(s: &mut String, c: &MethodCode) {
    s.push_str("method ");
    s.push_str(&c.name.name);
    s.push('(');
    for (i, p) in c.params.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        if p.by_val {
            s.push_str("Знач ");
        }
        s.push_str(&p.name.name);
        if let Some(d) = p.default {
            let _ = write!(s, " = c{d}");
        }
    }
    s.push_str(match c.kind {
        MethodKind::Procedure => ") procedure",
        MethodKind::Function => ") function",
    });
    if c.export {
        s.push_str(" export");
    }
    if c.is_async {
        s.push_str(" async");
    }
    if c.callable_only {
        s.push_str(" callable-only");
    }
    s.push('\n');
}

fn slot(names: &[Sym], i: u32) -> String {
    match names.get(i as usize) {
        Some(n) if !n.name.is_empty() => n.name.to_string(),
        _ => format!("${i}"),
    }
}

fn const_text(m: &CompiledModule, i: u32) -> String {
    let mut s = String::new();
    match m.consts.get(i as usize) {
        Some(c) => constant(&mut s, c),
        None => s.push('?'),
    }
    s
}

fn code(s: &mut String, m: &CompiledModule, c: &MethodCode) {
    if !c.slot_names.is_empty() {
        s.push_str("  slots:");
        for i in 0..c.slot_names.len() {
            s.push(' ');
            s.push_str(&slot(&c.slot_names, u32::try_from(i).unwrap_or(u32::MAX)));
        }
        s.push('\n');
    }
    let mut used: BTreeSet<u32> = c.params.iter().filter_map(|p| p.default).collect();
    used.extend(c.code.iter().filter_map(|op| match op {
        Op::Const(i) => Some(*i),
        _ => None,
    }));
    if !used.is_empty() {
        s.push_str("  consts:");
        for i in used {
            let _ = write!(s, " c{i}={}", const_text(m, i));
        }
        s.push('\n');
    }
    if !c.handlers.is_empty() {
        s.push_str("  handlers: ");
        for (i, h) in c.handlers.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            let _ = write!(
                s,
                "[{:04}..{:04}) -> {:04} depth {}",
                h.start, h.end, h.target, h.depth
            );
        }
        s.push('\n');
    }
    let mut lines = c.lines.iter().peekable();
    for (pc, op) in c.code.iter().enumerate() {
        while let Some(&&(at, line)) = lines.peek()
            && at as usize <= pc
        {
            if at as usize == pc {
                let _ = writeln!(s, "  ; line {line}");
            }
            lines.next();
        }
        let _ = writeln!(s, "  {pc:04} {}", op_text(m, c, *op));
    }
}

fn call(op: &str, target: &str, argc: u32, want: bool) -> String {
    let want = if want { "" } else { " want=false" };
    format!("{op} {target} argc={argc}{want}")
}

fn op_text(m: &CompiledModule, c: &MethodCode, op: Op) -> String {
    let slot = |i: u32| slot(&c.slot_names, i);
    let var = |i: u32| {
        let name = m.vars.get(i as usize).map(|v| &v.name);
        name.or(m.caller_vars.get(i as usize))
            .map_or_else(|| format!("#{i}"), |v| v.name.to_string())
    };
    let outer = |i: u32| self::slot(&m.outer, i);
    let sym = |i: u32| {
        m.syms
            .get(i as usize)
            .map_or_else(|| format!("#{i}"), |x| x.name.to_string())
    };
    let global = |i: u32| {
        m.globals
            .get(i as usize)
            .map_or_else(|| format!("#{i}"), global_name)
    };
    let ty = |t: u16| {
        platform_catalog::catalog()
            .types()
            .get(usize::from(t))
            .map_or_else(|| format!("#{t}"), |t| t.name_ru.clone())
    };
    match op {
        Op::Const(i) => format!("Const {i} ; {}", const_text(m, i)),
        Op::LoadLocal(i) => format!("LoadLocal {i} ; {}", slot(i)),
        Op::StoreLocal(i) => format!("StoreLocal {i} ; {}", slot(i)),
        Op::RefLocal(i) => format!("RefLocal {i} ; {}", slot(i)),
        Op::LoadModVar(i) => format!("LoadModVar {i} ; {}", var(i)),
        Op::StoreModVar(i) => format!("StoreModVar {i} ; {}", var(i)),
        Op::RefModVar(i) => format!("RefModVar {i} ; {}", var(i)),
        Op::LoadOuter(i) => format!("LoadOuter {i} ; {}", outer(i)),
        Op::StoreOuter(i) => format!("StoreOuter {i} ; {}", outer(i)),
        Op::RefOuter(i) => format!("RefOuter {i} ; {}", outer(i)),
        Op::LoadGlobal(i) => format!("LoadGlobal {i} ; {}", global(i)),
        Op::StoreGlobal(i) => format!("StoreGlobal {i} ; {}", global(i)),
        Op::LoadContext(i) => format!("LoadContext {i} ; {}", sym(i)),
        Op::StoreContext(i) => format!("StoreContext {i} ; {}", sym(i)),
        Op::GetMember(i) => format!("GetMember {i} ; {}", sym(i)),
        Op::SetMember(i) => format!("SetMember {i} ; {}", sym(i)),
        Op::RefMember(i) => format!("RefMember {i} ; {}", sym(i)),
        Op::CallLocal { method, argc, want } => {
            let name = m
                .methods
                .get(method as usize)
                .map(|x| &x.name)
                .or(m.caller_methods.get(method as usize))
                .map_or_else(|| format!("#{method}"), |s| s.name.to_string());
            call("CallLocal", &name, argc, want)
        }
        Op::CallApp { method, argc, want } => {
            let name = m.app_methods.get(method as usize).map_or_else(
                || format!("#{method}"),
                |x| format!("{}.{}", x.module, x.name),
            );
            call("CallApp", &name, argc, want)
        }
        Op::CallPlatform { method, argc, want } => {
            let name = platform_catalog::catalog()
                .global_methods()
                .get(usize::from(method))
                .map_or_else(|| format!("#{method}"), |g| g.info.name_ru.clone());
            call("CallPlatform", &name, argc, want)
        }
        Op::CallContext { name, argc, want } => call("CallContext", &sym(name), argc, want),
        Op::CallMember { name, argc, want } => call("CallMember", &sym(name), argc, want),
        Op::New { ty: t, argc } => format!("New {} argc={argc}", ty(t)),
        Op::NewNoArgs { ty: t } => format!("NewNoArgs {}", ty(t)),
        Op::NewDynamic { argc } => format!("NewDynamic argc={argc}"),
        Op::Jump(t) => format!("Jump {t:04}"),
        Op::JumpIfFalse(t) => format!("JumpIfFalse {t:04}"),
        Op::JumpIfTrue(t) => format!("JumpIfTrue {t:04}"),
        Op::IterStart(i) => format!("IterStart {i} ; {}", slot(i)),
        Op::IterNext { slot: i, end } => format!("IterNext {i} {end:04} ; {}", slot(i)),
        Op::RaiseExt(n) => format!("RaiseExt {n}"),
        Op::LeaveHandler(n) => format!("LeaveHandler {n}"),
        Op::Undef
        | Op::Null
        | Op::True
        | Op::False
        | Op::Pop
        | Op::Dup
        | Op::GetIndex
        | Op::SetIndex
        | Op::RefIndex
        | Op::Omitted
        | Op::Return
        | Op::ReturnValue
        | Op::Neg
        | Op::Pos
        | Op::Not
        | Op::Add
        | Op::Sub
        | Op::Mul
        | Op::Div
        | Op::Mod
        | Op::Eq
        | Op::Ne
        | Op::Lt
        | Op::Le
        | Op::Gt
        | Op::Ge
        | Op::ToBool
        | Op::Raise
        | Op::Reraise
        | Op::EnterHandler
        | Op::Execute
        | Op::Evaluate
        | Op::Await
        | Op::AddHandler
        | Op::RemoveHandler => format!("{op:?}"),
    }
}
