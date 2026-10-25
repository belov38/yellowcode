use crate::builtins::CatalogBuiltins;
use crate::bytecode::CompiledModule;
use crate::error::CompileError;
use crate::ir::{Callee, IrModule, Place};
use crate::lower::lower;
use crate::options::CompileOptions;
use crate::resolve::{Arity, GlobalLookup, Globals, resolve_fragment, select_error};
use crate::sym::Sym;
use bsl_syntax::ast::Ident;
use bsl_syntax::{ParseMode, ParseOptions, SourceText, parse};
use std::collections::HashMap;
use std::sync::Arc;

pub struct FragmentScope<'a> {
    pub caller_slots: &'a [Sym],
    pub module: Option<&'a CompiledModule>,
}

pub struct CompiledFragment {
    pub module: Arc<CompiledModule>,
}

struct FragmentGlobals<'a> {
    slots: HashMap<&'a str, u32>,
    vars: HashMap<&'a str, u32>,
    methods: HashMap<&'a str, (u32, Arity)>,
    rest: Globals<'a>,
}

fn first_index<'a>(names: impl Iterator<Item = &'a Sym>) -> HashMap<&'a str, u32> {
    let mut map = HashMap::new();
    for (i, s) in names.enumerate() {
        let i = u32::try_from(i).unwrap_or(u32::MAX);
        if !s.folded.is_empty() {
            map.entry(&*s.folded).or_insert(i);
        }
    }
    map
}

impl GlobalLookup for FragmentGlobals<'_> {
    fn value(&mut self, id: &Ident, sym: &Sym) -> Option<Place> {
        if let Some(&i) = self.slots.get(&*sym.folded) {
            return Some(Place::Outer(i));
        }
        if let Some(&i) = self.vars.get(&*sym.folded) {
            return Some(Place::ModVar(i));
        }
        self.rest.value(id, sym)
    }

    fn method(&mut self, id: &Ident, sym: &Sym) -> Option<(Callee, Arity)> {
        if let Some(&(i, a)) = self.methods.get(&*sym.folded) {
            return Some((Callee::Local(i), a));
        }
        self.rest.method(id, sym)
    }
}

pub fn compile_fragment_ir(
    src: &SourceText,
    scope: &FragmentScope<'_>,
    mode: ParseMode,
    o: &CompileOptions<'_>,
) -> Result<IrModule, CompileError> {
    let builtins = CatalogBuiltins(o.catalog);
    let out = parse(
        src,
        &ParseOptions {
            mode,
            context: o.context,
            methods_only: false,
            extension: false,
            builtins: &builtins,
        },
    );
    if let Some(e) = &out.error {
        return Err(CompileError::from_syntax(e));
    }
    let empty = Arc::new(CompiledModule {
        name: String::new(),
        kind: o.kind,
        context: o.context,
        compat: o.compat,
        consts: Vec::new(),
        syms: Vec::new(),
        globals: Vec::new(),
        app_methods: Vec::new(),
        vars: Vec::new(),
        methods: Vec::new(),
        body: None,
        source_lines: Arc::from(Vec::new()),
        outer: Vec::new(),
        caller_methods: Vec::new(),
        caller_vars: Vec::new(),
    });
    let m = scope.module.unwrap_or(&empty);
    let mut methods = HashMap::new();
    for (i, c) in m.methods.iter().enumerate() {
        let i = u32::try_from(i).unwrap_or(u32::MAX);
        methods
            .entry(&*c.name.folded)
            .or_insert((i, Arity::of_code(c)));
    }
    let mut globals = FragmentGlobals {
        slots: first_index(scope.caller_slots.iter()),
        vars: first_index(m.vars.iter().map(|v| &v.name)),
        methods,
        rest: Globals::new(o),
    };
    let r = resolve_fragment(src, o, &out.module.body, &mut globals);
    match select_error(None, &r.checks, &r.unknown) {
        Some(e) => Err(e),
        None => {
            let mut module = r.module;
            module.name = String::new();
            module.outer = scope.caller_slots.to_vec();
            module.caller_methods = m.methods.iter().map(|c| c.name.clone()).collect();
            module.caller_vars = m.vars.iter().map(|v| v.name.clone()).collect();
            Ok(module)
        }
    }
}

pub fn compile_fragment(
    src: &SourceText,
    scope: &FragmentScope<'_>,
    mode: ParseMode,
    o: &CompileOptions<'_>,
) -> Result<Arc<CompiledFragment>, CompileError> {
    let ir = compile_fragment_ir(src, scope, mode, o)?;
    Ok(Arc::new(CompiledFragment {
        module: Arc::new(lower(ir)),
    }))
}
