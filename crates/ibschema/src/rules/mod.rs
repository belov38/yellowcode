mod accounting;
mod calculation;
mod catalog;
mod common;
mod ctx;
mod document;
mod exchange;
mod external;
mod fields;
mod index;
mod registers;
mod service;
mod types;

use md_model::{Child, ChildTag, Configuration, MdKind, ScriptVariant};

use crate::dbnames::{DbName, DbNames};
use crate::error::{Error, Result};
use crate::logical::{Field, Index, LogicalSchema, Table, TableKind};
use crate::names::{Side, StdField, purpose};
use crate::values::Uuid16;
use ctx::{Ctx, Lookup};
use index::Tok;

#[derive(Debug, Clone, Default)]
pub struct RulesOptions {
    pub skip_unsupported: bool,
}

#[derive(Debug, Clone)]
pub struct Built {
    pub schema: LogicalSchema,
    pub objects: Vec<ObjectTables>,
    pub allocated: Vec<DbName>,
    pub script_variant: ScriptVariant,
    pub separators: Vec<SeparatorSpec>,
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SeparatorSpec {
    pub md: Uuid16,
    pub md_name: String,
    pub field: String,
    pub use_field: Option<String>,
    pub independent: bool,
}

#[derive(Debug, Clone)]
pub struct ObjectTables {
    pub md: Uuid16,
    pub kind: MdKind,
    pub full_name: String,
    pub tables: Vec<TableRole>,
    pub enum_values: Vec<Uuid16>,
}

#[derive(Debug, Clone)]
pub struct TableRole {
    pub logical: String,
    pub purpose: String,
    pub md: Uuid16,
    pub md_name: String,
    pub query_name: String,
    pub fields: Vec<FieldRole>,
}

#[derive(Debug, Clone)]
pub struct FieldRole {
    pub field: String,
    pub md: Option<Uuid16>,
    pub md_name: String,
    pub std: Option<StdField>,
    pub side: Option<Side>,
}

#[doc(hidden)]
pub mod testing {
    pub use super::types::{Class, RefTarget, logical_types};
}

pub(crate) struct Names<'a> {
    db: &'a mut DbNames,
    allocated: Vec<DbName>,
}

impl<'a> Names<'a> {
    pub(crate) fn new(db: &'a mut DbNames) -> Names<'a> {
        Names {
            db,
            allocated: Vec::new(),
        }
    }

    pub(crate) fn get(&self, uuid: Uuid16, purpose: &str) -> Option<u32> {
        self.db.number(uuid, purpose)
    }

    pub(crate) fn number(&mut self, uuid: Uuid16, purpose: &str) -> Result<u32> {
        if let Some(n) = self.db.number(uuid, purpose) {
            return Ok(n);
        }
        let number = self.db.allocate(uuid, purpose)?;
        self.allocated.push(DbName {
            uuid,
            purpose: purpose.to_string(),
            number,
        });
        Ok(number)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn allocated(&self) -> &[DbName] {
        &self.allocated
    }

    fn into_allocated(self) -> Vec<DbName> {
        self.allocated
    }
}

fn names_object(s: &str, at: &str) -> bool {
    s.strip_prefix(at)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(['.', ':']))
}

pub(crate) fn in_object(at: &str, e: Error) -> Error {
    match e {
        Error::Unsupported(m) if !names_object(&m, at) => Error::Unsupported(format!("{at}: {m}")),
        Error::Parse { what, at: a, why } if !names_object(&a, at) => Error::Parse {
            what,
            at: format!("{at}: {a}"),
            why,
        },
        other => other,
    }
}

const IMPLICIT_INDEXES: [&str; 2] = ["ID", "SK"];

pub(crate) struct TableBuilder {
    pub table: Table,
    pub role: TableRole,
}

impl TableBuilder {
    pub(crate) fn new(
        table: Table,
        purpose: &str,
        md: Uuid16,
        md_name: impl Into<String>,
        query_name: impl Into<String>,
    ) -> TableBuilder {
        let logical = if table.parent.is_empty() {
            table.name.clone()
        } else {
            format!("{}.{}", table.parent, table.name)
        };
        TableBuilder {
            table,
            role: TableRole {
                logical,
                purpose: purpose.to_string(),
                md,
                md_name: md_name.into(),
                query_name: query_name.into(),
                fields: Vec::new(),
            },
        }
    }

    pub(crate) fn std(&mut self, s: StdField, f: Field) {
        self.role.fields.push(FieldRole {
            field: f.name.clone(),
            md: None,
            md_name: String::new(),
            std: Some(s),
            side: None,
        });
        self.table.fields.push(f);
    }

    pub(crate) fn std_of(&mut self, s: StdField, f: Field, md_name: impl Into<String>) {
        self.std(s, f);
        if let Some(r) = self.role.fields.last_mut() {
            r.md_name = md_name.into();
        }
    }

    pub(crate) fn item(&mut self, f: Field, md: Uuid16, md_name: impl Into<String>) {
        self.role.fields.push(FieldRole {
            field: f.name.clone(),
            md: Some(md),
            md_name: md_name.into(),
            std: None,
            side: None,
        });
        self.table.fields.push(f);
    }

    pub(crate) fn item_sided(
        &mut self,
        f: Field,
        md: Uuid16,
        md_name: impl Into<String>,
        side: Side,
    ) {
        self.item(f, md, md_name);
        if let Some(r) = self.role.fields.last_mut() {
            r.side = Some(side);
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn index(&mut self, ix: Index) -> Result<()> {
        if IMPLICIT_INDEXES.contains(&ix.name.as_str()) {
            return Err(Error::Unsupported(format!(
                "index {} of {} is implicit and not written to DBSCHEMA",
                ix.name, self.table.name
            )));
        }
        self.table.indexes.push(ix);
        Ok(())
    }

    pub(crate) fn rule(
        &mut self,
        cx: &Ctx,
        names: &mut Names,
        template: &str,
        unique: bool,
        toks: &[Tok],
        item: Option<&Child>,
    ) -> Result<()> {
        let name = index::name(template, item, names)?;
        if IMPLICIT_INDEXES.contains(&name.as_str()) {
            return Err(Error::Unsupported(format!(
                "index {name} of {} is implicit and not written to DBSCHEMA",
                self.table.name
            )));
        }
        let fields = index::fields(toks, &self.table, item, cx, names)?;
        let flags = index::flags(index::table_purpose(&self.table.name), &name, cx);
        self.table
            .indexes
            .push(index::index(name, unique, fields, flags));
        Ok(())
    }

    pub(crate) fn rule_fields(
        &mut self,
        cx: &Ctx,
        name: String,
        unique: bool,
        fields: Vec<String>,
    ) -> Result<()> {
        if IMPLICIT_INDEXES.contains(&name.as_str()) {
            return Err(Error::Unsupported(format!(
                "index {name} of {} is implicit and not written to DBSCHEMA",
                self.table.name
            )));
        }
        let flags = index::flags(index::table_purpose(&self.table.name), &name, cx);
        self.table
            .indexes
            .push(index::index(name, unique, fields, flags));
        Ok(())
    }

    pub(crate) fn rule_distinct(
        &mut self,
        cx: &Ctx,
        names: &mut Names,
        template: &str,
        unique: bool,
        toks: &[Tok],
        item: Option<&Child>,
    ) -> Result<()> {
        let fields = index::fields(toks, &self.table, item, cx, names)?;
        if self.table.indexes.iter().any(|i| i.fields == fields) {
            return Ok(());
        }
        self.rule(cx, names, template, unique, toks, item)
    }

    pub(crate) fn finish(self) -> (Table, TableRole) {
        (self.table, self.role)
    }
}

pub(crate) type KindFn = fn(&Ctx, &mut Names) -> Result<Vec<(Table, TableRole)>>;

enum Rules {
    NoTables,
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "reserved for future metadata kinds without rules")
    )]
    Unsupported,
    Kind(KindFn),
}

fn rules_of(kind: MdKind) -> Rules {
    use MdKind::*;
    match kind {
        Catalog => Rules::Kind(catalog::catalog),
        ChartOfCharacteristicTypes => Rules::Kind(catalog::chrc),
        Enum => Rules::Kind(catalog::enumeration),
        Document => Rules::Kind(document::document),
        InformationRegister => Rules::Kind(registers::information),
        AccumulationRegister => Rules::Kind(registers::accumulation),
        ChartOfAccounts => Rules::Kind(accounting::chart_of_accounts),
        AccountingRegister => Rules::Kind(accounting::register),
        Constant => Rules::Kind(catalog::constant),
        ExchangePlan => Rules::Kind(exchange::exchange_plan),
        ChartOfCalculationTypes => Rules::Kind(calculation::chart_of_calculation_types),
        CalculationRegister => Rules::Kind(calculation::register),
        DocumentJournal => Rules::Kind(document::journal),
        Sequence => Rules::Kind(document::sequence),
        BusinessProcess => Rules::Kind(document::business_process),
        Task => Rules::Kind(document::task),
        ExternalDataSource => Rules::Kind(external::external_data_source),
        IntegrationService => Rules::Kind(external::integration_service),
        Configuration
        | Subsystem
        | CommonModule
        | SessionParameter
        | Role
        | CommonAttribute
        | FilterCriterion
        | EventSubscription
        | ScheduledJob
        | FunctionalOption
        | FunctionalOptionsParameter
        | DefinedType
        | SettingsStorage
        | CommonForm
        | CommonCommand
        | CommandGroup
        | CommonTemplate
        | CommonPicture
        | Interface
        | Style
        | StyleItem
        | Language
        | XDTOPackage
        | WebService
        | HTTPService
        | WSReference
        | Report
        | DataProcessor
        | DocumentNumerator
        | WebSocketClient
        | PaletteColor
        | Bot => Rules::NoTables,
    }
}

fn name_number(name: &str) -> Option<u32> {
    let digits = name.len() - name.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    name.get(name.len() - digits..)?.parse().ok()
}

fn assemble(tables: Vec<Table>) -> Result<Vec<Table>> {
    let (mut top, mut nested): (Vec<Table>, Vec<Table>) =
        tables.into_iter().partition(|t| t.kind == TableKind::Main);
    top.sort_by_key(|t| t.number);
    nested.sort_by_key(|t| name_number(&t.name));
    for n in nested {
        let owner = top.iter_mut().find(|t| t.name == n.parent).ok_or_else(|| {
            Error::Unsupported(format!(
                "nested table {} without its owner {}",
                n.name, n.parent
            ))
        })?;
        owner.nested.push(n);
    }
    Ok(top)
}

pub fn build(
    cfg: &Configuration,
    names: &mut DbNames,
    base: &LogicalSchema,
    opts: &RulesOptions,
) -> Result<Built> {
    build_with(cfg, names, base, rules_of, opts.skip_unsupported)
}

fn build_with(
    cfg: &Configuration,
    names: &mut DbNames,
    base: &LogicalSchema,
    rules: fn(MdKind) -> Rules,
    skip_unsupported: bool,
) -> Result<Built> {
    let mut schema = service::base_tables(base, names)?;
    let md = Lookup::new(cfg);
    let mut nm = Names::new(names);
    let mut objects = Vec::new();
    let mut tables = Vec::new();
    let mut skipped = Vec::new();
    for obj in &cfg.objects {
        let f = match rules(obj.kind) {
            Rules::NoTables => continue,
            Rules::Unsupported if skip_unsupported => {
                skipped.push(ctx::full_name(obj));
                continue;
            }
            Rules::Unsupported => return Err(Error::UnsupportedObject(ctx::full_name(obj))),
            Rules::Kind(f) => f,
        };
        let cx = Ctx::new(&md, obj)?;
        let checkpoint = nm.db.allocation_checkpoint();
        let allocated = nm.allocated.len();
        let made = match f(&cx, &mut nm) {
            Ok(made) => made,
            Err(Error::Unsupported(_)) if skip_unsupported => {
                nm.db.rollback_allocations(checkpoint);
                nm.allocated.truncate(allocated);
                skipped.push(cx.full_name);
                continue;
            }
            Err(e) => return Err(in_object(&cx.full_name, e)),
        };
        let mut roles = Vec::new();
        for (t, role) in made {
            if t.name == purpose::EXT_DATA_SRC_PRMS
                && let Some(existing) = schema.iter().find(|old| old.name == t.name)
            {
                if existing != &t {
                    return Err(Error::Unsupported(format!(
                        "{}: base shared parameters differ from the supported layout",
                        cx.full_name
                    )));
                }
            } else {
                tables.push(t);
            }
            roles.push(role);
        }
        let enum_values = obj
            .children
            .iter()
            .filter(|c| c.tag == ChildTag::EnumValue)
            .map(|c| c.id)
            .collect();
        objects.push(ObjectTables {
            md: obj.id,
            kind: obj.kind,
            full_name: cx.full_name,
            tables: roles,
            enum_values,
        });
    }
    schema.extend(service::config_dependent(cfg, &mut nm)?);
    let objects_schema = assemble(tables)?;
    let seps = common::config_separators(&md, &mut nm)?;
    service::apply_separation(&mut schema, &seps, &mut nm)?;
    service::latest_versions(&mut schema, &objects_schema, &seps, &mut nm)?;
    schema.extend(objects_schema);
    let separators = seps
        .iter()
        .map(|s| SeparatorSpec {
            md: s.id,
            md_name: s.md_name.clone(),
            field: s.fld.clone(),
            use_field: nm
                .get(s.id, purpose::DATA_SEPARATION_USE)
                .map(|n| format!("{}{n}", purpose::DATA_SEPARATION_USE)),
            independent: s.independent,
        })
        .collect();
    Ok(Built {
        schema: LogicalSchema { tables: schema },
        objects,
        allocated: nm.into_allocated(),
        script_variant: cfg.script_variant(),
        separators,
        skipped,
    })
}
