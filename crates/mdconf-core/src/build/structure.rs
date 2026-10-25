use crate::model::{Child, Configuration, MdObject, Module, Prop, Recalculation, SubObject};
use crate::parts::{ConfigParts, ObjectParts};
use crate::{Error, ModuleKind, Result};
use brace::{Guid, Node};
use std::collections::HashSet;

const LISTED: usize = 20;

const FORM: &str = "edit of the form body: not written before C7 Task 6 (a model loaded with private_forms \
                    also differs here: build_edit needs LoadOptions::default())";

pub(crate) type EditedProps = HashSet<(Guid, String)>;

pub(crate) fn load_options(model: &Configuration, base: &Configuration) -> Result<()> {
    let source = |c: &Configuration| {
        c.parts
            .dump_info
            .as_ref()
            .map(|d| d.source == Node::empty())
    };
    if source(base) == Some(false) && source(model) == Some(true) {
        return Err(Error::Build {
            object: "Configuration".into(),
            path: "load options".into(),
            what: "the model must come from LoadOptions::default() (keep_source true, no private_forms)"
                .into(),
        });
    }
    Ok(())
}

pub(crate) fn compare(model: &Configuration, base: &Configuration) -> Result<EditedProps> {
    let mut d = Diffs {
        cfg: model,
        schema: mdschema::schema()?,
        first: None,
        lines: Vec::new(),
        total: 0,
        edited: HashSet::new(),
    };
    d.configuration(model, base);
    match d.first {
        None => Ok(d.edited),
        Some((object, path, what)) => {
            let what = if d.total > 1 {
                format!("{what} ({} differences: {})", d.total, d.lines.join("; "))
            } else {
                what
            };
            Err(Error::Build { object, path, what })
        }
    }
}

struct Diffs<'c> {
    cfg: &'c Configuration,
    schema: &'static mdschema::Schema,
    first: Option<(String, String, String)>,
    lines: Vec<String>,
    total: usize,
    edited: EditedProps,
}

fn task7(part: &str) -> String {
    format!("edit of {part}: not written before C7 Task 7")
}

impl Diffs<'_> {
    fn note(&mut self, object: &str, path: &str, what: String) {
        self.total += 1;
        if self.lines.len() < LISTED {
            self.lines.push(format!("{object}: {path}: {what}"));
        }
        if self.first.is_none() {
            self.first = Some((object.to_string(), path.to_string(), what));
        }
    }

    fn full(&self, id: Guid, name: &str) -> String {
        self.cfg
            .full_name(id)
            .map_or_else(|| format!("{id} {name}"), str::to_string)
    }

    fn props(&mut self, id: Guid, m: &[Prop], b: &[Prop]) {
        for p in m {
            if b.iter().find(|q| q.name == p.name).map(|q| &q.value) != Some(&p.value) {
                self.edited.insert((id, p.name.clone()));
            }
        }
        for q in b {
            if !m.iter().any(|p| p.name == q.name) {
                self.edited.insert((id, q.name.clone()));
            }
        }
    }

    fn rename(&mut self, full: &str, m: &str, b: &str) {
        if m != b {
            self.note(full, "name", format!("rename {b:?} -> {m:?} (C9/M8)"));
        }
    }

    fn same_ids(
        &mut self,
        object: &str,
        path: &str,
        m: &[(String, Guid)],
        b: &[(String, Guid)],
    ) -> bool {
        if m == b {
            return true;
        }
        let (mi, bi): (HashSet<Guid>, HashSet<Guid>) = (
            m.iter().map(|x| x.1).collect(),
            b.iter().map(|x| x.1).collect(),
        );
        let mut any = false;
        for (label, id) in m.iter().filter(|x| !bi.contains(&x.1)) {
            self.note(
                object,
                path,
                format!("structural change: added {label} {id} (C9)"),
            );
            any = true;
        }
        for (label, id) in b.iter().filter(|x| !mi.contains(&x.1)) {
            self.note(
                object,
                path,
                format!("structural change: removed {label} {id} (C9)"),
            );
            any = true;
        }
        if !any {
            self.note(
                object,
                path,
                "structural change: moved elements (C9)".into(),
            );
        }
        false
    }

    fn configuration(&mut self, m: &Configuration, b: &Configuration) {
        let Configuration {
            id: _,
            root_info: _,
            xml_origin: _,
            header_id: _,
            compat,
            props,
            objects,
            modules,
            diagnostics: _,
            parts,
            settings,
            index: _,
            predef,
            profile: _,
        } = m;
        let at = "Configuration";
        if *compat != b.compat {
            self.note(
                at,
                "compat",
                format!(
                    "compat {compat} differs from the base {} (version entry is copied)",
                    b.compat
                ),
            );
        }
        if *props != b.props {
            self.note(at, "props", task7("the configuration properties"));
        }
        if *settings != b.settings {
            self.note(at, "settings", task7("the configuration settings"));
        }
        if *predef != b.predef {
            self.note(at, "predef", task7("predefined items"));
        }
        self.config_parts(parts, &b.parts);
        self.module_kinds(at, modules, &b.modules);
        self.objects(at, "objects", objects, &b.objects);
    }

    fn config_parts(&mut self, m: &ConfigParts, b: &ConfigParts) {
        let ConfigParts {
            command_interface,
            main_section,
            home_page,
            client_interface,
            contained_objects,
            dump_info: _,
            pictures,
            help,
            binaries,
        } = m;
        let at = "Configuration";
        let checks = [
            (
                "parts.command_interface",
                *command_interface != b.command_interface,
            ),
            ("parts.main_section", *main_section != b.main_section),
            ("parts.home_page", *home_page != b.home_page),
            (
                "parts.client_interface",
                *client_interface != b.client_interface,
            ),
            (
                "parts.contained_objects",
                *contained_objects != b.contained_objects,
            ),
            ("parts.pictures", *pictures != b.pictures),
            ("parts.help", *help != b.help),
            ("parts.binaries", *binaries != b.binaries),
        ];
        for (path, differ) in checks {
            if differ {
                self.note(at, path, task7(path));
            }
        }
    }

    fn objects(&mut self, object: &str, path: &str, m: &[MdObject], b: &[MdObject]) {
        let key = |l: &[MdObject]| -> Vec<(String, Guid)> {
            l.iter()
                .map(|o| (o.kind.as_str().to_string(), o.id))
                .collect()
        };
        if self.same_ids(object, path, &key(m), &key(b)) {
            for (o, q) in m.iter().zip(b) {
                self.object(o, q);
            }
        }
    }

    fn object(&mut self, o: &MdObject, b: &MdObject) {
        let MdObject {
            kind,
            id,
            name,
            synonym: _,
            comment: _,
            generated_types,
            this_node,
            props,
            children,
            forms,
            templates,
            recalculations,
            subsystems,
            modules,
            parts,
            form,
            template,
            flowchart,
        } = o;
        let full = self.full(*id, name);
        self.rename(&full, name, &b.name);
        if *generated_types != b.generated_types {
            self.note(
                &full,
                "generated_types",
                "structural change: generated types (C9)".into(),
            );
        }
        if *this_node != b.this_node {
            self.note(
                &full,
                "this_node",
                "structural change: ThisNode (C9)".into(),
            );
        }
        self.props(*id, props, &b.props);
        self.children(&full, children, &b.children, true);
        let subs = |l: &[SubObject]| -> Vec<(String, Guid)> {
            l.iter().map(|s| (format!("{:?}", s.kind), s.id)).collect()
        };
        if self.same_ids(&full, "forms", &subs(forms), &subs(&b.forms)) {
            for (s, q) in forms.iter().zip(&b.forms) {
                self.sub(s, q);
            }
        }
        if self.same_ids(&full, "templates", &subs(templates), &subs(&b.templates)) {
            for (s, q) in templates.iter().zip(&b.templates) {
                self.sub(s, q);
            }
        }
        let recalcs = |l: &[Recalculation]| -> Vec<(String, Guid)> {
            l.iter()
                .map(|r| ("Recalculation".to_string(), r.id))
                .collect()
        };
        if self.same_ids(
            &full,
            "recalculations",
            &recalcs(recalculations),
            &recalcs(&b.recalculations),
        ) {
            for (r, q) in recalculations.iter().zip(&b.recalculations) {
                self.recalc(r, q);
            }
        }
        self.objects(&full, "subsystems", subsystems, &b.subsystems);
        self.module_kinds(&full, modules, &b.modules);
        let slots = crate::modules::slot_kinds(self.schema, kind.as_str());
        for module in modules.iter().filter(|m| !slots.contains(&m.kind)) {
            let base = b.modules.iter().find(|q| q.kind == module.kind);
            if base.is_some_and(|q| q != module) {
                self.note(
                    &full,
                    &format!("modules.{:?}", module.kind),
                    unslotted(module.kind),
                );
            }
        }
        self.object_parts(&full, parts, &b.parts);
        if *form != b.form {
            self.note(&full, "form", FORM.into());
        }
        if *template != b.template {
            self.note(&full, "template", task7("the template body"));
        }
        if *flowchart != b.flowchart {
            self.note(&full, "flowchart", task7("the flowchart"));
        }
    }

    fn module_kinds(&mut self, full: &str, m: &[Module], b: &[Module]) {
        for module in m.iter().filter(|x| !b.iter().any(|q| q.kind == x.kind)) {
            self.note(
                full,
                "modules",
                format!("structural change: added module {:?} (C9)", module.kind),
            );
        }
        for module in b.iter().filter(|x| !m.iter().any(|q| q.kind == x.kind)) {
            self.note(
                full,
                "modules",
                format!("structural change: removed module {:?} (C9)", module.kind),
            );
        }
    }

    fn object_parts(&mut self, full: &str, m: &ObjectParts, b: &ObjectParts) {
        let ObjectParts {
            rights,
            predefined,
            command_interface,
            help,
            form_help,
            picture,
            schedule,
            exchange_content,
            style,
            binary_modules,
            ws_definition,
            xdto_package,
        } = m;
        let checks = [
            ("parts.rights", *rights != b.rights),
            ("parts.predefined", *predefined != b.predefined),
            (
                "parts.command_interface",
                *command_interface != b.command_interface,
            ),
            ("parts.help", *help != b.help),
            ("parts.form_help", *form_help != b.form_help),
            ("parts.picture", *picture != b.picture),
            ("parts.schedule", *schedule != b.schedule),
            (
                "parts.exchange_content",
                *exchange_content != b.exchange_content,
            ),
            ("parts.style", *style != b.style),
            ("parts.binary_modules", *binary_modules != b.binary_modules),
            ("parts.ws_definition", *ws_definition != b.ws_definition),
            ("parts.xdto_package", *xdto_package != b.xdto_package),
        ];
        for (path, differ) in checks {
            if differ {
                self.note(full, path, task7(path));
            }
        }
    }

    fn children(&mut self, full: &str, m: &[Child], b: &[Child], direct: bool) {
        let key = |l: &[Child]| -> Vec<(String, Guid)> {
            l.iter().map(|c| (format!("{:?}", c.tag), c.id)).collect()
        };
        if self.same_ids(full, "children", &key(m), &key(b)) {
            for (c, q) in m.iter().zip(b) {
                self.child(c, q, direct);
            }
        }
    }

    fn child(&mut self, c: &Child, b: &Child, direct: bool) {
        let Child {
            tag,
            id,
            name,
            synonym: _,
            comment: _,
            generated_types,
            props,
            children,
            module,
        } = c;
        let full = self.full(*id, name);
        self.rename(&full, name, &b.name);
        if *generated_types != b.generated_types {
            self.note(
                &full,
                "generated_types",
                "structural change: generated types (C9)".into(),
            );
        }
        self.props(*id, props, &b.props);
        self.children(&full, children, &b.children, false);
        let slotted = direct && *tag == crate::model::ChildTag::Command;
        match (module, &b.module) {
            (Some(x), None) => self.note(
                &full,
                "module",
                format!("structural change: added module {:?} (C9)", x.kind),
            ),
            (None, Some(x)) => self.note(
                &full,
                "module",
                format!("structural change: removed module {:?} (C9)", x.kind),
            ),
            (Some(x), Some(y)) if !slotted && x != y => {
                self.note(&full, "module", unslotted(x.kind))
            }
            _ => {}
        }
    }

    fn sub(&mut self, s: &SubObject, b: &SubObject) {
        let SubObject {
            kind: _,
            id,
            name,
            props,
            module,
            form,
            template,
        } = s;
        let full = self.full(*id, name);
        self.rename(&full, name, &b.name);
        self.props(*id, props, &b.props);
        if *module != b.module {
            self.note(&full, "module", "form module: C7 Task 6".into());
        }
        if *form != b.form {
            self.note(&full, "form", FORM.into());
        }
        if *template != b.template {
            self.note(&full, "template", task7("the template body"));
        }
    }

    fn recalc(&mut self, r: &Recalculation, b: &Recalculation) {
        let Recalculation {
            id,
            name,
            synonym: _,
            comment: _,
            generated_types,
            props,
            dimensions,
            module,
        } = r;
        let full = self.full(*id, name);
        self.rename(&full, name, &b.name);
        if *generated_types != b.generated_types {
            self.note(
                &full,
                "generated_types",
                "structural change: generated types (C9)".into(),
            );
        }
        self.props(*id, props, &b.props);
        let dims = |l: &[crate::model::RecalcDimension]| -> Vec<(String, Guid)> {
            l.iter().map(|d| ("Dimension".to_string(), d.id)).collect()
        };
        if self.same_ids(&full, "dimensions", &dims(dimensions), &dims(&b.dimensions)) {
            for (d, q) in dimensions.iter().zip(&b.dimensions) {
                let crate::model::RecalcDimension {
                    id,
                    name,
                    synonym: _,
                    comment: _,
                    register_dimension: _,
                    leading_register_data: _,
                    props,
                } = d;
                let dfull = self.full(*id, name);
                self.rename(&dfull, name, &q.name);
                self.props(*id, props, &q.props);
            }
        }
        match (module, &b.module) {
            (Some(x), None) => self.note(
                &full,
                "module",
                format!("structural change: added module {:?} (C9)", x.kind),
            ),
            (None, Some(x)) => self.note(
                &full,
                "module",
                format!("structural change: removed module {:?} (C9)", x.kind),
            ),
            _ => {}
        }
    }
}

fn unslotted(kind: ModuleKind) -> String {
    if kind == ModuleKind::FormModule {
        "form module: C7 Task 6".into()
    } else {
        format!("module {kind:?} without an entry slot: not written")
    }
}
