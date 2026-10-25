use std::collections::BTreeMap;

use mdconf::{Child, Configuration, MdKind, MdObject, Module, Value};
use serde::Serialize;

#[derive(Debug, Default, Serialize)]
pub struct Stats {
    pub objects: BTreeMap<String, usize>,
    pub nested_subsystems: usize,
    pub modules: usize,
    pub module_lines: u64,
    pub module_bytes: u64,
    pub forms: usize,
    pub form_modules: usize,
    pub templates: usize,
}

fn is_managed(props: &[mdconf::Prop]) -> bool {
    props
        .iter()
        .find(|p| p.name == "FormType")
        .is_some_and(|p| matches!(&p.value, Value::Text(t) if t == "Managed"))
}

fn lines(m: &Module) -> u64 {
    let b = m.source.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&m.source);
    if b.is_empty() {
        return 0;
    }
    let breaks = b.iter().filter(|&&c| c == b'\n').count() as u64;
    breaks + u64::from(b.last() != Some(&b'\n'))
}

impl Stats {
    fn module(&mut self, m: &Module, form: bool) {
        if form {
            self.form_modules += 1;
        } else {
            self.modules += 1;
        }
        self.module_lines += lines(m);
        self.module_bytes += m.source.len() as u64;
    }

    fn child(&mut self, c: &Child) {
        if let Some(m) = &c.module {
            self.module(m, false);
        }
        for c2 in &c.children {
            self.child(c2);
        }
    }

    fn object(&mut self, o: &MdObject, top: bool) {
        if top {
            *self.objects.entry(o.kind.as_str().to_string()).or_default() += 1;
        } else {
            self.nested_subsystems += 1;
        }
        for m in &o.modules {
            self.module(m, m.kind == mdconf::ModuleKind::FormModule);
        }
        if o.kind == MdKind::CommonForm && is_managed(&o.props) {
            self.forms += 1;
        }
        if o.kind == MdKind::CommonTemplate {
            self.templates += 1;
        }
        for c in &o.children {
            self.child(c);
        }
        for f in &o.forms {
            if is_managed(&f.props) {
                self.forms += 1;
            }
            if let Some(m) = &f.module {
                self.module(m, true);
            }
        }
        self.templates += o.templates.len();
        for r in &o.recalculations {
            if let Some(m) = &r.module {
                self.module(m, false);
            }
        }
        for s in &o.subsystems {
            self.object(s, false);
        }
    }
}

pub fn stats(cfg: &Configuration) -> Stats {
    let mut s = Stats::default();
    for o in &cfg.objects {
        s.object(o, true);
    }
    for m in &cfg.modules {
        s.module(m, false);
    }
    s
}

pub fn render_text(
    s: &Stats,
    out: &mut dyn std::io::Write,
    lang: super::out::Lang,
) -> crate::Result<()> {
    use super::names::kind_name;
    let ru = lang == super::out::Lang::Ru;
    let l = |ru_s: &'static str, en: &'static str| if ru { ru_s } else { en };
    let mut rows: Vec<(usize, String, usize)> = s
        .objects
        .iter()
        .map(|(k, n)| match MdKind::from_name(k) {
            Some(kind) => {
                let rank = super::tree::kind_order()
                    .iter()
                    .position(|x| *x == kind)
                    .unwrap_or(usize::MAX);
                (rank, kind_name(kind, lang), *n)
            }
            None => (usize::MAX, k.clone(), *n),
        })
        .collect();
    rows.sort_by_key(|r| r.0);
    for (_, name, n) in rows {
        writeln!(out, "{name}: {n}")?;
    }
    writeln!(
        out,
        "{}: {}",
        l("Вложенные подсистемы", "Nested subsystems"),
        s.nested_subsystems
    )?;
    writeln!(out, "{}: {}", l("Модули", "Modules"), s.modules)?;
    writeln!(
        out,
        "{}: {}",
        l("Модули форм", "Form modules"),
        s.form_modules
    )?;
    writeln!(
        out,
        "{}: {}",
        l("Строк кода", "Lines of code"),
        s.module_lines
    )?;
    writeln!(
        out,
        "{}: {}",
        l("Байт кода", "Bytes of code"),
        s.module_bytes
    )?;
    writeln!(
        out,
        "{}: {}",
        l("Управляемые формы", "Managed forms"),
        s.forms
    )?;
    writeln!(out, "{}: {}", l("Макеты", "Templates"), s.templates)?;
    Ok(())
}
