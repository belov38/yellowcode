use crate::{Command, bak, config, ext, ib, remote};
use std::ffi::OsStr;

pub(crate) const ENV: &str = "YELLOWCODE_READONLY";

pub(crate) const REFUSED: i32 = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Effect {
    Read,
    Change,
}

impl Command {
    pub(crate) fn effect(&self) -> Effect {
        match self {
            Command::Open(_) | Command::Convert(_) => Effect::Read,
            Command::Config(command) => match command {
                config::Command::Metadata(command) => match command {
                    config::metadata::Command::Tree { .. }
                    | config::metadata::Command::Show { .. } => Effect::Read,
                },
                config::Command::Module(command) => match command {
                    config::module::Command::List { .. }
                    | config::module::Command::Show { .. }
                    | config::module::Command::Outline { .. } => Effect::Read,
                },
                config::Command::Template(command) => match command {
                    config::template::Command::List { .. }
                    | config::template::Command::Show { .. } => Effect::Read,
                },
                config::Command::Grep(_) | config::Command::Export(_) => Effect::Read,
            },
            Command::Ext(command) => match command {
                ext::Command::Info { .. }
                | ext::Command::Objects { .. }
                | ext::Command::Modules { .. }
                | ext::Command::Intercepts { .. } => Effect::Read,
            },
            Command::Ib(command) => match command {
                ib::Command::Query(_) | ib::Command::Table(_) | ib::Command::Serve(_) => {
                    Effect::Read
                }
                ib::Command::Users(command) => command.effect(),
            },
            Command::Bak(command) => match command {
                bak::Command::Inspect { .. } => Effect::Read,
                bak::Command::Convert { server, .. } => match server {
                    Some(_) => Effect::Change,
                    None => Effect::Read,
                },
            },
            Command::Remote(command) => match command {
                remote::Command::Info { .. }
                | remote::Command::Metadata { .. }
                | remote::Command::Modules { .. }
                | remote::Command::Object { .. }
                | remote::Command::List { .. } => Effect::Read,
                remote::Command::Call { .. } => Effect::Change,
                remote::Command::Report {
                    login: _,
                    form: _,
                    report: _,
                    call,
                    params: _,
                    params_json: _,
                    init_call,
                    init_params: _,
                    attrs: _,
                    before,
                    poll: _,
                    poll_while: _,
                    timeout: _,
                    generate: _,
                    variant,
                    sets: _,
                    filters: _,
                    offs: _,
                    details: _,
                    attribute: _,
                    format: _,
                } => change_if(
                    call.is_some()
                        || init_call.is_some()
                        || !before.is_empty()
                        || variant.is_some(),
                ),
                remote::Command::Print {
                    login: _,
                    form: _,
                    params: _,
                    params_json: _,
                    refs: _,
                    init_call,
                    init_params: _,
                    attribute: _,
                    format: _,
                } => change_if(init_call.is_some()),
                remote::Command::Create {
                    login: _,
                    set: _,
                    delete_rows: _,
                    clear_tables: _,
                    data: _,
                    post: _,
                    unpost: _,
                    dry_run,
                    json: _,
                    object: _,
                } => change_if(!*dry_run),
                remote::Command::Write {
                    login: _,
                    set: _,
                    delete_rows: _,
                    clear_tables: _,
                    data: _,
                    post: _,
                    unpost: _,
                    dry_run,
                    json: _,
                    reference: _,
                    mark_deletion: _,
                    unmark_deletion: _,
                } => change_if(!*dry_run),
                remote::Command::Form {
                    login: _,
                    form: _,
                    reference: _,
                    set: _,
                    command,
                    call,
                    params: _,
                    params_json: _,
                    list: _,
                    json: _,
                } => change_if(command.is_some() || call.is_some()),
            },
        }
    }
}

fn change_if(changes: bool) -> Effect {
    if changes {
        Effect::Change
    } else {
        Effect::Read
    }
}

impl ib::users::Command {
    pub(crate) fn effect(&self) -> Effect {
        match self {
            ib::users::Command::List { .. } | ib::users::Command::Show { .. } => Effect::Read,
            ib::users::Command::Create { .. }
            | ib::users::Command::Edit { .. }
            | ib::users::Command::Delete { .. }
            | ib::users::Command::ClearPassword { .. } => Effect::Change,
        }
    }
}

pub(crate) fn readonly_from_env(value: Option<&OsStr>) -> Result<bool, String> {
    let Some(value) = value else {
        return Ok(false);
    };
    match value.to_str().map(str::to_ascii_lowercase).as_deref() {
        Some("1" | "true") => Ok(true),
        Some("0" | "false" | "") => Ok(false),
        _ => Err(format!(
            "{ENV}: неизвестное значение «{}»; допустимо 1, true, 0, false",
            value.to_string_lossy()
        )),
    }
}

pub(crate) fn command_path(matches: &clap::ArgMatches) -> String {
    let mut names = Vec::new();
    let mut current = matches;
    while let Some((name, sub)) = current.subcommand() {
        names.push(name);
        current = sub;
    }
    names.join(" ")
}

pub(crate) fn refusal(path: &str, value: &OsStr) -> String {
    format!(
        "{path} меняет данные, а {ENV}={} разрешает только чтение",
        value.to_string_lossy()
    )
}
