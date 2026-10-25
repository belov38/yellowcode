use md_model::{Configuration, MdKind};

use super::common::ServiceSep;
use super::ctx::{Props, is_balance};
use super::exchange;
use super::{Names, fields, index};
use crate::dbnames::DbNames;
use crate::error::{Error, Result};
use crate::logical::{KeyKind, LogicalSchema, Table, TableKind, VAR};
use crate::names::{field as f, purpose};
use crate::values::Uuid16;

pub(crate) fn base_tables(base: &LogicalSchema, names: &DbNames) -> Result<Vec<Table>> {
    base.tables
        .iter()
        .map(|t| {
            let number = names
                .number(Uuid16::NIL, &t.name)
                .ok_or_else(|| Error::Parse {
                    what: "DBNames",
                    at: t.name.clone(),
                    why: "no entry for the service table".into(),
                })?;
            Ok(Table {
                number,
                ..t.clone()
            })
        })
        .collect()
}

pub(crate) fn config_dependent(cfg: &Configuration, names: &mut Names) -> Result<Vec<Table>> {
    let mut out = Vec::new();
    let turnovers = cfg
        .objects
        .iter()
        .any(|o| o.kind == MdKind::AccumulationRegister && !is_balance(o));
    if turnovers {
        let number = names.number(Uuid16::NIL, purpose::ACCUM_RG_AGG_DIMS)?;
        let mut t = Table::new(purpose::ACCUM_RG_AGG_DIMS, TableKind::Main, number, "");
        t.key = KeyKind::Plain;
        t.fields = vec![
            fields::binary(f::REG_ID, 16),
            fields::binary(f::DIM_UUID, 16),
            fields::numeric(f::DIM_CODE, 2, 0),
        ];
        t.indexes = vec![index::index(
            "ByID",
            true,
            vec![f::REG_ID.to_string(), f::DIM_UUID.to_string()],
            (true, false),
        )];
        out.push(t);
    }
    if cfg
        .objects
        .iter()
        .any(|o| o.kind == MdKind::ExchangePlan && o.flag("DistributedInfoBase"))
    {
        out.push(exchange::config_change_records(names)?);
    }
    for job in cfg
        .objects
        .iter()
        .filter(|o| o.kind == MdKind::ScheduledJob)
    {
        out.push(scheduled_job(job.id, names)?);
    }
    Ok(out)
}

fn scheduled_job(id: Uuid16, names: &mut Names) -> Result<Table> {
    let number = names.number(id, purpose::SCHEDULED_JOBS)?;
    let mut t = Table::new(
        format!("{}{number}", purpose::SCHEDULED_JOBS),
        TableKind::Main,
        number,
        "",
    );
    t.fields = vec![
        fields::binary(f::ID, 16),
        fields::string(f::DESCRIPTION, 128),
        fields::string(f::JOB_KEY, 128),
        fields::binary(f::METADATA_ID, 16),
        fields::boolean(f::PREDEFINED),
        fields::binary(f::PARAMETERS, VAR),
        fields::string(f::USER_NAME, 255),
        fields::binary(f::ACTIVATION_CONDITION, VAR),
        fields::boolean(f::USE),
        fields::numeric(f::RESTART_COUNT, 9, 0),
        fields::numeric(f::RESTART_PERIOD, 9, 0),
        fields::numeric(f::RESTART_ATTEMPT_NUMBER, 9, 0),
        fields::numeric(f::STATE, 9, 0),
        fields::datetime(f::START_TIME),
        fields::datetime(f::FINISH_TIME),
        fields::numeric(f::VERSION, 9, 0),
    ];
    t.indexes = vec![index::index(
        "ByID",
        true,
        vec![f::ID.to_string()],
        (true, false),
    )];
    Ok(t)
}

#[derive(Clone, Copy)]
enum Sel {
    All,
    Independent,
}

struct NewIndex {
    name: &'static str,
    with_use: bool,
    only_use: bool,
    clustered: bool,
    first: bool,
}

struct SepRule {
    table: &'static str,
    sel: Sel,
    use_fields: bool,
    nullable: bool,
    new_index: Option<NewIndex>,
    suffix_pairs: bool,
    prefix: &'static [&'static str],
    splitters: bool,
}

const fn settings(table: &'static str) -> SepRule {
    SepRule {
        table,
        sel: Sel::All,
        use_fields: true,
        nullable: false,
        new_index: Some(NewIndex {
            name: "BySepUse",
            with_use: false,
            only_use: true,
            clustered: false,
            first: true,
        }),
        suffix_pairs: false,
        prefix: &[],
        splitters: true,
    }
}

const fn stt(table: &'static str) -> SepRule {
    SepRule {
        table,
        sel: Sel::Independent,
        use_fields: true,
        nullable: true,
        new_index: Some(NewIndex {
            name: "BySeparatorUsage",
            with_use: true,
            only_use: false,
            clustered: false,
            first: true,
        }),
        suffix_pairs: true,
        prefix: &[],
        splitters: false,
    }
}

const fn history(
    table: &'static str,
    sel: Sel,
    prefix: &'static [&'static str],
    new_index: Option<NewIndex>,
) -> SepRule {
    SepRule {
        table,
        sel,
        use_fields: false,
        nullable: true,
        new_index,
        suffix_pairs: false,
        prefix,
        splitters: false,
    }
}

const SEP_RULES: [SepRule; 22] = [
    settings("SystemSettings"),
    settings("CommonSettings"),
    settings("RepSettings"),
    settings("RepVarSettings"),
    settings("FrmDtSettings"),
    settings("DynListSettings"),
    settings("ErrorProcessingSettings"),
    settings("URLExternalData"),
    settings("InternalSettings"),
    settings("DefaultSystemSettings"),
    settings("DefaultInternalSettings"),
    settings("UsersWorkHistory"),
    stt("STTSettings"),
    stt("STTGrammar"),
    stt("STTGrammarChecksum"),
    stt("STTModels"),
    stt("STTModelsDesc"),
    SepRule {
        table: "ODataSettings",
        sel: Sel::All,
        use_fields: true,
        nullable: true,
        new_index: Some(NewIndex {
            name: "BySeparatorUsage",
            with_use: true,
            only_use: false,
            clustered: true,
            first: true,
        }),
        suffix_pairs: true,
        prefix: &[],
        splitters: false,
    },
    history(
        "DataHistorySettings",
        Sel::Independent,
        &["SeparatorsMetadataId"],
        None,
    ),
    history(
        "DataHistoryMetadata",
        Sel::Independent,
        &["SeparatorsIdIsSetIsActNumber"],
        None,
    ),
    history(
        "DataHistoryQueue0",
        Sel::All,
        &[],
        Some(NewIndex {
            name: "Separators",
            with_use: false,
            only_use: false,
            clustered: false,
            first: false,
        }),
    ),
    history(
        "DataHistoryAfterWriteQueue",
        Sel::All,
        &["SeparatorsMetadataIdHistoryDataIdVersionNumber"],
        None,
    ),
];

pub(crate) fn apply_separation(
    schema: &mut [Table],
    seps: &[ServiceSep],
    names: &mut Names,
) -> Result<()> {
    if seps.is_empty()
        || !schema
            .iter()
            .any(|t| SEP_RULES.iter().any(|r| r.table == t.name))
    {
        return Ok(());
    }
    let mut uses = Vec::with_capacity(seps.len());
    for s in seps {
        let n = names.number(s.id, purpose::DATA_SEPARATION_USE)?;
        uses.push(format!("{}{n}", purpose::DATA_SEPARATION_USE));
    }
    for t in schema.iter_mut() {
        let Some(rule) = SEP_RULES.iter().find(|r| r.table == t.name) else {
            continue;
        };
        let picked: Vec<usize> = (0..seps.len())
            .filter(|&i| match rule.sel {
                Sel::All => true,
                Sel::Independent => seps.get(i).is_some_and(|s| s.independent),
            })
            .collect();
        if picked.is_empty() {
            continue;
        }
        let pick = |v: &[String]| -> Vec<String> {
            picked.iter().filter_map(|&i| v.get(i).cloned()).collect()
        };
        let use_names = pick(&uses);
        let flds: Vec<String> = picked
            .iter()
            .filter_map(|&i| seps.get(i).map(|s| s.fld.clone()))
            .collect();
        if rule.use_fields {
            t.fields
                .extend(use_names.iter().map(|n| fields::boolean(n)));
        }
        for &i in &picked {
            if let Some(s) = seps.get(i) {
                t.fields.push(fields::field(
                    s.fld.as_str(),
                    rule.nullable,
                    s.types.clone(),
                ));
            }
        }
        if rule.suffix_pairs {
            let pairs: Vec<String> = use_names
                .iter()
                .zip(&flds)
                .flat_map(|(u, f)| [u.clone(), f.clone()])
                .collect();
            for ix in &mut t.indexes {
                ix.fields.extend(pairs.iter().cloned());
            }
        }
        for ix in &mut t.indexes {
            if rule.prefix.contains(&ix.name.as_str()) {
                ix.fields.splice(0..0, flds.iter().cloned());
            }
        }
        if let Some(n) = &rule.new_index {
            let mut f = Vec::new();
            if n.with_use || n.only_use {
                f.extend(use_names.iter().cloned());
            }
            if !n.only_use {
                f.extend(flds.iter().cloned());
            }
            let ix = index::index(n.name, false, f, (n.clustered, false));
            if n.first {
                t.indexes.insert(0, ix);
            } else {
                t.indexes.push(ix);
            }
        }
        if rule.splitters {
            t.splitters[0] = vec![flds.clone()];
        }
    }
    Ok(())
}

pub(crate) fn latest_versions(
    schema: &mut Vec<Table>,
    objects: &[Table],
    seps: &[ServiceSep],
    names: &mut Names,
) -> Result<()> {
    let Some(base) = schema
        .iter()
        .find(|t| t.name == purpose::DATA_HISTORY_LATEST_VERSIONS)
        .cloned()
    else {
        return Ok(());
    };
    let mut seen: Vec<&[Vec<Vec<String>>; 2]> = Vec::new();
    for t in objects {
        if t.splitters[0].is_empty() || seen.contains(&&t.splitters) {
            continue;
        }
        seen.push(&t.splitters);
        let name = format!("{}{}", purpose::DATA_HISTORY_LATEST_VERSIONS, seen.len());
        let number = names.number(Uuid16::NIL, &name)?;
        let mut n = Table {
            name,
            number,
            splitters: t.splitters.clone(),
            ..base.clone()
        };
        for f in t.splitters[0].iter().flatten() {
            let s = seps
                .iter()
                .find(|s| &s.fld == f)
                .ok_or_else(|| Error::Parse {
                    what: "rules",
                    at: n.name.clone(),
                    why: format!("separator {f} is not a common attribute"),
                })?;
            n.fields
                .push(fields::field(f.as_str(), false, s.types.clone()));
        }
        schema.push(n);
    }
    Ok(())
}
