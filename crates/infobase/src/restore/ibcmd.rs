use super::*;

pub(super) fn matches(schema: &LogicalSchema) -> Result<bool> {
    Ok(*schema == LogicalSchema::from_text(include_bytes!("ibcmd-empty-schema.txt"))?)
}

pub(super) fn extra_tables() -> Result<Vec<TableDescr>> {
    [
        include_str!("ibcmd/binarydata.txt"),
        include_str!("ibcmd/binarydatastorageversion.txt"),
        include_str!("ibcmd/externalbindatastrgslist.txt"),
        include_str!("ibcmd/externalbindatastrgsblist.txt"),
    ]
    .into_iter()
    .map(|text| TableDescr::parse(text).map_err(Error::from))
    .collect()
}

pub(super) fn check_metadata(ib: &Infobase) -> Result<()> {
    if ib.raw().dbnames_text()? != include_bytes!("ibcmd-dbnames.txt") {
        return Err(bad("unqualified no-config object mappings"));
    }
    for folder in [
        "CONFIG",
        "CONFIGSAVE",
        "DEPOTFILES",
        "CONFIGCAS",
        "CONFIGCASSAVE",
    ] {
        if !ib.raw().folder_index(folder)?.entries.is_empty() {
            return Err(bad("unqualified no-config application metadata"));
        }
    }
    let params = ib.raw().folder_index("PARAMS")?;
    let names = [
        "DBNames",
        "DBNamesVersion-DBNames",
        "evlogparams.inf",
        "ibparams.inf",
        "locale.inf",
        "log.inf",
    ];
    if params.entries.len() != names.len()
        || params
            .entries
            .iter()
            .any(|entry| !names.contains(&entry.name.as_str()))
    {
        return Err(bad("unqualified no-config Params metadata"));
    }
    Ok(())
}
