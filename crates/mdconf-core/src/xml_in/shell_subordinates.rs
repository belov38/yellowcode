use super::*;

impl Loader<'_> {
    pub(super) fn subobject(
        &mut self,
        tag: &str,
        name: &str,
        rel: &Path,
        full: &str,
        object: Guid,
    ) -> Result<SubObject> {
        let spec = overlay::kind(self.s, tag).ok_or_else(|| err(rel, "missing subobject table"))?;
        let bytes = self.read(rel)?;
        let path = self.real(rel)?;
        let budget = self.budget.clone();
        let arena = parse(&bytes, &path, self.opts, &budget, &mut self.text_work)?;
        let (n, fmt) = description(&arena, tag, &path, &mut self.diag)?;
        self.version(fmt, &path)?;
        structure(n, &path)?;
        check_name(n, name, &path)?;
        let id = self.ids.identity(n, full, &path)?;
        self.ix.names.insert(id, full.into());
        let props = self.properties(n, &spec.props, tag, &format!("{tag}Properties"), &path)?;
        if block(n, MD, "ChildObjects", &path)?.is_some_and(|c| c.children().next().is_some())
            || block(n, MD, "InternalInfo", &path)?.is_some_and(|c| c.children().next().is_some())
        {
            return Err(err(&path, "unsupported subobject structure"));
        }
        let kind = if tag == "Form" {
            SubKind::Form
        } else {
            SubKind::Template
        };
        self.ix.subs.entry(object).or_default().push((kind, id));
        Ok(SubObject {
            kind,
            id,
            name: name.into(),
            props,
            module: None,
            form: None,
            template: None,
        })
    }
    pub(super) fn recalculation(
        &mut self,
        name: &str,
        rel: &Path,
        full: &str,
        object: Guid,
        objname: &str,
        depth: usize,
    ) -> Result<Recalculation> {
        if depth > self.opts.max_metadata_depth {
            return Err(err(rel, "metadata depth limit"));
        }
        let bytes = self.read(rel)?;
        let path = self.real(rel)?;
        let budget = self.budget.clone();
        let arena = parse(&bytes, &path, self.opts, &budget, &mut self.text_work)?;
        let (n, fmt) = description(&arena, "Recalculation", &path, &mut self.diag)?;
        self.version(fmt, &path)?;
        structure(n, &path)?;
        check_name(n, name, &path)?;
        let id = self.ids.identity(n, full, &path)?;
        self.ix.names.insert(id, full.into());
        self.ix.recalcs.entry(object).or_default().push(id);
        let expected: Vec<_> = crate::objects::recalc::GENERATED
            .iter()
            .map(|(cat, t, v)| mdschema::InternalInfoSpec {
                category: (*cat).into(),
                type_id: t.to_vec(),
                value_id: v.to_vec(),
            })
            .collect();
        let generated_types = self.ids.generated(
            n,
            &expected,
            "Recalculation",
            &format!("{objname}.{name}"),
            owner(object, None, Some(id)),
            self.s,
            &mut self.ix,
            &mut self.diag,
            &path,
        )?;
        let props = self.properties(n, &[], "Recalculation", "RecalculationProperties", &path)?;
        let mut dimensions = Vec::new();
        if let Some(ch) = block(n, MD, "ChildObjects", &path)? {
            for d in ch.children() {
                if d.ns() != Some(MD) || d.local() != "Dimension" {
                    return Err(err(&path, "unknown recalculation child"));
                }
                structure(d, &path)?;
                let name = object_name(d, &path)?;
                let full = self.ids.full_name(&[full, ".Dimension.", &name], &path)?;
                let id = self.ids.identity(d, &full, &path)?;
                self.ix.names.insert(id, full);
                let dimension_props = self.properties(
                    d,
                    &[],
                    "Dimension",
                    "RecalculationDimensionProperties",
                    &path,
                )?;
                if block(d, MD, "InternalInfo", &path)?
                    .is_some_and(|n| n.children().next().is_some())
                    || block(d, MD, "ChildObjects", &path)?
                        .is_some_and(|n| n.children().next().is_some())
                {
                    return Err(err(&path, "unsupported recalculation dimension structure"));
                }
                let (mut synonym, mut comment) = Default::default();
                objects::headers(&dimension_props, &mut synonym, &mut comment);
                let register_dimension = dimension_props
                    .iter()
                    .find_map(|p| match (&*p.name, &p.value) {
                        ("RegisterDimension", Value::Ref(r)) => Some(r.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let leading_register_data = dimension_props
                    .iter()
                    .find_map(|p| match (&*p.name, &p.value) {
                        ("LeadingRegisterData", Value::RefList(r)) => Some(r.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                dimensions.push(RecalcDimension {
                    id,
                    name,
                    synonym,
                    comment,
                    register_dimension,
                    leading_register_data,
                    props: dimension_props,
                });
            }
        }
        let (mut synonym, mut comment) = Default::default();
        objects::headers(&props, &mut synonym, &mut comment);
        Ok(Recalculation {
            id,
            name: name.into(),
            synonym,
            comment,
            generated_types,
            props,
            dimensions,
            module: None,
        })
    }
    pub(super) fn unsupported_description(
        &mut self,
        tag: &str,
        name: &str,
        rel: &Path,
        full: &str,
        depth: usize,
    ) -> Result<()> {
        if depth > self.opts.max_metadata_depth {
            return Err(err(rel, "metadata depth limit"));
        }
        let bytes = self.read(rel)?;
        let path = self.real(rel)?;
        let budget = self.budget.clone();
        let arena = parse(&bytes, &path, self.opts, &budget, &mut self.text_work)?;
        let (n, fmt) = description(&arena, tag, &path, &mut self.diag)?;
        self.version(fmt, &path)?;
        check_name(n, name, &path)?;
        self.unsupported_node(n, tag, rel, full, depth)
    }
    fn unsupported_node(
        &mut self,
        n: N<'_, '_>,
        tag: &str,
        rel: &Path,
        full: &str,
        depth: usize,
    ) -> Result<()> {
        if depth > self.opts.max_metadata_depth {
            return Err(err(rel, "metadata depth limit"));
        }
        let path = self.real(rel)?;
        structure(n, &path)?;
        let id = self.ids.identity(n, full, &path)?;
        self.report.unsupported.push(UnsupportedShell {
            id,
            name: full.into(),
            path: rel.into(),
        });
        let spec = overlay::kind(self.s, tag);
        schema::properties(
            self.s,
            n,
            spec.map_or(&[], |s| s.props.as_slice()),
            tag,
            &format!("{tag}Properties"),
            &path,
            &mut self.report,
            &mut self.diag,
        )?;
        if block(n, MD, "InternalInfo", &path)?.is_some_and(|ii| ii.children().next().is_some()) {
            self.diag.unsupported.push(format!(
                "{}: unsupported InternalInfo of {tag}",
                rel.display()
            ));
        }
        if let Some(ch) = block(n, MD, "ChildObjects", &path)? {
            for c in ch.children() {
                if c.ns() != Some(MD) {
                    return Err(err(&path, "unknown unsupported child namespace"));
                }
                if let Some(spec) = spec
                    && spec.subobjects.iter().any(|(t, _)| t == c.local())
                {
                    let name = component(&path, leaf(c, &path)?)?;
                    let folder = self
                        .s
                        .folder(c.local())
                        .map(String::from)
                        .unwrap_or_else(|| format!("{}s", c.local()));
                    let sr = rel
                        .with_extension("")
                        .join(folder)
                        .join(format!("{name}.xml"));
                    let child_full = self
                        .ids
                        .full_name(&[full, ".", c.local(), ".", &name], &path)?;
                    self.unsupported_description(c.local(), &name, &sr, &child_full, depth + 1)?;
                } else if spec.is_some_and(|s| s.children.iter().any(|(_, cs)| cs.tag == c.local()))
                    || self
                        .s
                        .xdto_props(&format!("{tag}ChildObjects"))
                        .iter()
                        .any(|p| p.name == c.local())
                {
                    let name = object_name(c, &path)?;
                    let child_full = self
                        .ids
                        .full_name(&[full, ".", c.local(), ".", &name], &path)?;
                    self.unsupported_node(c, c.local(), rel, &child_full, depth + 1)?;
                } else {
                    return Err(err(
                        &path,
                        format!("unknown unsupported child {}", c.local()),
                    ));
                }
            }
        }
        Ok(())
    }
}
