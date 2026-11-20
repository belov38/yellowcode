use super::builder::{Builder, count, list};
use super::values::base64;
use super::{FormBuildError, Result, invalid, xml};
use crate::forms::fe::E;
use brace::{Guid, Node};
use md_graphics::templates::dcs_build::{
    self, Asset, EmbeddedChild, EmbeddedNamespaces, EmbeddedRoot, InverseLookup,
};
use mdconf_core::xml::view::NodeRef;
use memory_budget::Charged;

pub(super) const SETTINGS: &str = "http://v8.1c.ru/8.1/data-composition-system/settings";
pub(super) const SCHEMA: &str = "http://v8.1c.ru/8.1/data-composition-system/schema";
struct Lookup<'a>(&'a dyn super::FormBuildResolution);
impl InverseLookup for Lookup<'_> {
    fn type_id(&self, name: &str) -> Option<Guid> {
        self.0.type_id(name)
    }
    fn object_id(&self, name: &str) -> Option<Guid> {
        self.0.metadata_id(name)
    }
}
fn error(e: dcs_build::BuildError) -> FormBuildError {
    match e {
        dcs_build::BuildError::Cancelled => FormBuildError::Cancelled,
        dcs_build::BuildError::Memory(e) => FormBuildError::Budget(e),
        dcs_build::BuildError::Invalid(what) => invalid("embedded XML", what),
    }
}
impl<'a> Builder<'a> {
    fn appearance_fields<'s>(
        &self,
        e: &'s E,
        seen: &mut std::collections::HashSet<&'s str>,
        names: &mut Vec<&'s str>,
    ) -> Result<()> {
        self.check()?;
        if e.tag == "dcsset:item" && xml::attr(e, "xsi:type") == Some("dcsset:FilterItemComparison")
        {
            for field in &e.kids {
                self.check()?;
                if matches!(field.tag.as_str(), "dcsset:left" | "dcsset:right")
                    && xml::attr(field, "xsi:type") == Some("dcscor:Field")
                    && field.kids.is_empty()
                {
                    let name = xml::text(field);
                    if !seen.contains(name) {
                        self.retain_asset_allocation(
                            name.len()
                                .checked_add(128)
                                .ok_or(FormBuildError::Limit("conditional appearance names"))?,
                        )?;
                        seen.insert(name);
                        names.push(name);
                    }
                }
            }
        }
        for child in &e.kids {
            self.appearance_fields(child, seen, names)?;
        }
        Ok(())
    }
    fn appearance_path_bytes(&self, node: &Node) -> Result<usize> {
        self.check()?;
        let bytes = match node {
            Node::Atom(text) | Node::Str(text) => text.len(),
            Node::List(values) => values.iter().try_fold(0usize, |bytes, value| {
                bytes
                    .checked_add(self.appearance_path_bytes(value)?)
                    .ok_or(FormBuildError::Limit("conditional appearance paths"))
            })?,
        };
        bytes
            .checked_add(std::mem::size_of::<Node>() + 64)
            .ok_or(FormBuildError::Limit("conditional appearance paths"))
    }
    fn admit_embedded_assets(&self, n: NodeRef<'_, '_>) -> Result<()> {
        self.check()?;
        if n.local() == "Abs"
            && let Some(path) = n.text()
        {
            for (index, (asset, bytes)) in self.input.files.iter().enumerate() {
                if asset.strip_prefix("Template/") == Some(path) {
                    self.retain_asset_allocation(bytes.len())?;
                    self.asset_uses.borrow_mut().insert(index);
                }
            }
        }
        for child in n.children() {
            self.admit_embedded_assets(child)?;
        }
        Ok(())
    }
    pub fn source_of(&self, e: &E) -> Result<NodeRef<'a, 'a>> {
        if std::ptr::eq(e, self.root) {
            return Ok(self.source);
        }
        self.sources
            .get(&(std::ptr::from_ref(e) as usize))
            .copied()
            .ok_or_else(|| invalid("embedded XML", "source element not retained"))
    }
    pub fn embedded(
        &self,
        source: Option<&E>,
        local: &str,
        namespace: &str,
        profile: EmbeddedNamespaces,
        wrap: Option<(&str, &str)>,
    ) -> Result<Charged<Vec<u8>>> {
        self.check()?;
        let source = source.map(|e| self.source_of(e)).transpose()?;
        if let Some(source) = source {
            self.admit_embedded_assets(source)?;
        }
        let assets: Vec<_> = self
            .input
            .files
            .iter()
            .map(|(path, bytes)| Asset { path, bytes })
            .collect();
        dcs_build::write_embedded_checked(
            source
                .into_iter()
                .flat_map(|n| n.children())
                .map(|source| EmbeddedChild {
                    source,
                    rename: None,
                    fallback_type: None,
                }),
            EmbeddedRoot {
                namespace,
                local,
                wrap,
                namespaces: profile,
            },
            &assets,
            &Lookup(self.resolution),
            self.memory,
            self.cancelled,
        )
        .map_err(error)
    }
    pub fn server_state_xml(&self, source: &E) -> Result<Charged<Vec<u8>>> {
        let source = self.source_of(source)?;
        self.admit_embedded_assets(source)?;
        let assets: Vec<_> = self
            .input
            .files
            .iter()
            .map(|(path, bytes)| Asset { path, bytes })
            .collect();
        let children = source
            .children()
            .filter(|c| !super::dynamic::consumes_setting(c.ns(), c.local()))
            .map(|source| {
                let (local, fallback) = match source.local() {
                    "Parameter" => ("Parameter", Some((SCHEMA, "Parameter"))),
                    "CalculatedField" => ("ExpressionField", Some((SCHEMA, "CalculatedField"))),
                    local => (local, None),
                };
                EmbeddedChild {
                    source,
                    rename: Some(("", local)),
                    fallback_type: fallback,
                }
            });
        dcs_build::write_embedded_checked(
            children,
            EmbeddedRoot {
                namespace: "",
                local: "UniversalListServerOnlyState",
                wrap: None,
                namespaces: EmbeddedNamespaces::Plain,
            },
            &assets,
            &Lookup(self.resolution),
            self.memory,
            self.cancelled,
        )
        .map_err(error)
    }
    pub fn appearance_tail(&self) -> Result<Vec<Node>> {
        if self.path_model.is_none() {
            return Ok(vec![
                Node::int(0),
                Node::int(0),
                list(vec![base64(&self.embedded(
                    None,
                    "Settings",
                    SETTINGS,
                    EmbeddedNamespaces::Settings,
                    None,
                )?)]),
            ]);
        }
        let appearance = (self.path_stage >= 2)
            .then(|| {
                xml::child(self.root, "Attributes")
                    .and_then(|e| xml::child(e, "ConditionalAppearance"))
            })
            .flatten();
        let mut fields = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut names = Vec::new();
        if let Some(e) = appearance {
            self.appearance_fields(e, &mut seen, &mut names)?;
        }
        for name in names {
            self.check()?;
            let path = self
                .with_form_info(|info| info.unresolve(name))?
                .flatten()
                .unwrap_or_else(|| list(vec![Node::int(0)]));
            let mut collection = list(vec![Node::int(0)]);
            self.with_form_info(|info| -> Result<()> {
                let segments = path
                    .as_list()
                    .ok_or_else(|| invalid("ConditionalAppearance", "invalid data path"))?;
                for len in (1..segments.len().saturating_sub(1)).rev() {
                    self.check()?;
                    let prefix = segments
                        .get(1..=len)
                        .ok_or_else(|| invalid("ConditionalAppearance", "invalid path prefix"))?;
                    let bytes = prefix.iter().try_fold(128usize, |bytes, value| {
                        bytes
                            .checked_add(self.appearance_path_bytes(value)?)
                            .ok_or(FormBuildError::Limit("conditional appearance paths"))
                    })?;
                    self.retain_asset_allocation(bytes)?;
                    let mut sub = vec![count(len)];
                    sub.extend_from_slice(prefix);
                    let sub = list(sub);
                    if let Some((_, Some(node))) = info.resolve(&sub) {
                        let dynamic = node
                            .attr
                            .and_then(|i| info.attrs.get(i))
                            .is_some_and(|a| !a.dl_fields.is_empty());
                        if node.kind == crate::forms::ctx::NodeKind::Ts
                            || node.cols.is_some()
                            || dynamic
                        {
                            collection = sub;
                            break;
                        }
                    }
                }
                Ok(())
            })?
            .transpose()?;
            fields.extend([Node::str(name), Node::str(name), path, collection]);
        }
        let document = self.embedded(
            appearance,
            "Settings",
            SETTINGS,
            EmbeddedNamespaces::Settings,
            appearance.map(|_| (SETTINGS, "conditionalAppearance")),
        )?;
        let groups = self.additional_columns()?;
        let mut tail = vec![count(groups.len())];
        tail.extend(groups);
        tail.push(count(fields.len() / 4));
        tail.extend(fields);
        tail.push(list(vec![base64(&document)]));
        Ok(tail)
    }
}
