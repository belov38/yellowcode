use std::{io::Write, path::PathBuf};
use yellowcode_core::users;

#[derive(Clone, Copy, clap::ValueEnum)]
enum Format {
    Text,
    Json,
}

pub(crate) fn write_notice(err: &mut dyn Write) {
    let _ = writeln!(
        err,
        "Изменения записываются напрямую в файл базы. Сначала закройте базу в 1С и сделайте резервную копию: при сбое записи или питания база может быть повреждена. Автоматической резервной копии и восстановления нет.\nChanges are written directly to the infobase file. Close the infobase in 1C and make a backup first: a write or power failure can damage the infobase. There is no automatic backup or recovery."
    );
    let _ = err.flush();
}

pub(crate) const EXAMPLES: &str = "\
Примеры:
  # Список пользователей
  yellowcode users list base

  # Список в JSON-файл
  yellowcode users list base --format json > users.json

  # Карточка пользователя по имени (регистр не важен)
  yellowcode users show base Администратор

  # Карточка по UUID, если имена повторяются в разных областях данных
  yellowcode users show base 11111111-1111-1111-1111-111111111111

  # Создать пользователя с пустым паролем и явно указанной ролью
  yellowcode users create base --name Оператор --roles 11111111-1111-1111-1111-111111111111

  # Изменить полное имя, сохранив пароль
  yellowcode users edit base Оператор --full-name \"Сергей Иванов\"

  # Удалить выбранного пользователя с подтверждением
  yellowcode users delete base Оператор --confirm

  # Очистить пароль выбранного пользователя с подтверждением
  yellowcode users clear-password base Администратор --confirm";

const MUTATION_HELP: &str = "\
Изменения записываются напрямую в файл базы. Сначала закройте базу в 1С и сделайте резервную копию: при сбое записи или питания база может быть повреждена. Автоматической резервной копии и восстановления нет.

Примеры:
  # Создать пользователя с пустым паролем и явно указанной ролью
  yellowcode users create base --name Оператор --roles 11111111-1111-1111-1111-111111111111

  # Изменить полное имя, сохранив пароль
  yellowcode users edit base Оператор --full-name \"Сергей Иванов\"

  # Удалить выбранного пользователя с подтверждением
  yellowcode users delete base Оператор --confirm

  # Очистить пароль выбранного пользователя с подтверждением
  yellowcode users clear-password base Администратор --confirm";

#[derive(clap::Args)]
pub(crate) struct Base {
    #[arg(help = "Каталог информационной базы или её файл 1Cv8.1CD")]
    base: PathBuf,
    #[arg(
        long,
        value_enum,
        help = "Формат вывода; по умолчанию текст в терминале и JSON при перенаправлении"
    )]
    format: Option<Format>,
}

#[derive(clap::Subcommand)]
pub(crate) enum Command {
    #[command(
        about = "Список пользователей базы без служебной строки и секретов",
        after_help = EXAMPLES
    )]
    List(Base),
    #[command(
        about = "Карточка пользователя по имени или UUID; UUID нужен, если имена повторяются в разных областях данных",
        after_help = EXAMPLES
    )]
    Show {
        #[command(flatten)]
        base: Base,
        #[arg(help = "Имя или UUID пользователя")]
        name: String,
    },
    /// Create a user with an empty password and an explicit role set.
    #[command(
        about = "Создать пользователя с пустым паролем и явно заданным набором ролей",
        after_help = MUTATION_HELP
    )]
    Create {
        #[command(flatten)]
        base: Base,
        /// New user name.
        #[arg(long, help = "Имя нового пользователя")]
        name: String,
        /// Full name; defaults to empty.
        #[arg(long, default_value = "", help = "Полное имя; по умолчанию пустое")]
        full_name: String,
        /// Role UUIDs from the current configuration, separated by commas; may be repeated.
        #[arg(
            long,
            value_delimiter = ',',
            required_unless_present = "clear_roles",
            conflicts_with = "clear_roles",
            help = "UUID ролей текущей конфигурации через запятую; параметр можно повторять"
        )]
        roles: Option<Vec<String>>,
        /// Explicitly assign an empty role set.
        #[arg(long, help = "Явно назначить пустой набор ролей")]
        clear_roles: bool,
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set, help = "Показывать пользователя в списке выбора при входе (true или false)")]
        show_in_list: bool,
    },
    /// Edit only supplied public fields, preserving the existing password.
    #[command(
        about = "Изменить только указанные поля пользователя, сохранив существующий пароль",
        after_help = MUTATION_HELP
    )]
    Edit {
        #[command(flatten)]
        base: Base,
        /// User name or UUID; use UUID when names are ambiguous.
        #[arg(help = "Имя или UUID пользователя; при совпадении имён укажите UUID")]
        identity: String,
        /// Replace the user name.
        #[arg(long, help = "Новое имя пользователя")]
        name: Option<String>,
        /// Replace the full name; an empty value clears it.
        #[arg(long, help = "Новое полное имя; пустое значение очищает его")]
        full_name: Option<String>,
        /// Replace roles with these configuration UUIDs, separated by commas; may be repeated.
        #[arg(
            long,
            value_delimiter = ',',
            conflicts_with = "clear_roles",
            help = "Заменить роли указанными UUID конфигурации через запятую; параметр можно повторять"
        )]
        roles: Option<Vec<String>>,
        /// Replace roles with an empty set; omission preserves roles.
        #[arg(
            long,
            help = "Заменить роли пустым набором; без параметра роли сохраняются"
        )]
        clear_roles: bool,
        /// Set true or false; omission preserves the current value.
        #[arg(long, action = clap::ArgAction::Set, help = "Задать true или false; без параметра текущее значение сохраняется")]
        show_in_list: Option<bool>,
    },
    /// Delete the selected user with explicit confirmation.
    #[command(
        about = "Удалить выбранного пользователя с явным подтверждением",
        after_help = MUTATION_HELP
    )]
    Delete {
        #[command(flatten)]
        base: Base,
        /// User name or UUID; use UUID when names are ambiguous.
        #[arg(help = "Имя или UUID пользователя; при совпадении имён укажите UUID")]
        identity: String,
        /// Confirm deletion of the selected user in the current snapshot.
        #[arg(
            long,
            required = true,
            help = "Подтвердить удаление выбранного пользователя в текущем состоянии базы"
        )]
        confirm: bool,
        /// Additional sole-user intent: a userless base opens with full rights. Requires native qualification.
        #[arg(
            long,
            help = "Дополнительно подтвердить удаление последнего пользователя: база откроется с полными правами. Эта операция пока не поддерживается"
        )]
        allow_last_user: bool,
    },
    /// Clear the selected user's password with explicit confirmation.
    #[command(
        about = "Очистить пароль выбранного пользователя с явным подтверждением",
        after_help = MUTATION_HELP
    )]
    ClearPassword {
        #[command(flatten)]
        base: Base,
        /// User name or UUID; use UUID when names are ambiguous.
        #[arg(help = "Имя или UUID пользователя; при совпадении имён укажите UUID")]
        identity: String,
        /// Confirm the explicit password operation for the selected user.
        #[arg(
            long,
            required = true,
            help = "Подтвердить установку пустого пароля выбранному пользователю"
        )]
        confirm: bool,
    },
}

impl Command {
    pub(crate) fn is_mutation(&self) -> bool {
        !matches!(self, Self::List(_) | Self::Show { .. })
    }
}

pub(crate) fn committed_output_error() -> yellowcode_core::Error {
    std::io::Error::other("changes committed but output failed; do not retry automatically").into()
}

pub(crate) fn run(command: Command, out: &mut dyn Write, tty: bool) -> yellowcode_core::Result<()> {
    match command {
        Command::List(base) => read(base, None, out, tty),
        Command::Show { base, name } => read(base, Some(name), out, tty),
        Command::Create {
            base,
            name,
            full_name,
            roles,
            clear_roles: _,
            show_in_list,
        } => write(
            base,
            users::UserAction::Create {
                draft: users::UserDraft {
                    name,
                    full_name,
                    roles: roles.unwrap_or_default(),
                    show_in_list,
                },
            },
            false,
            false,
            out,
            tty,
        ),
        Command::Edit {
            base,
            identity,
            name,
            full_name,
            roles,
            clear_roles,
            show_in_list,
        } => write(
            base,
            users::UserAction::Update {
                identity,
                patch: users::UserPatch {
                    name,
                    full_name,
                    roles: if clear_roles { Some(Vec::new()) } else { roles },
                    show_in_list,
                },
            },
            false,
            false,
            out,
            tty,
        ),
        Command::Delete {
            base,
            identity,
            confirm,
            allow_last_user,
        } => write(
            base,
            users::UserAction::Delete { identity },
            confirm,
            allow_last_user,
            out,
            tty,
        ),
        Command::ClearPassword {
            base,
            identity,
            confirm,
        } => write(
            base,
            users::UserAction::ClearPassword { identity },
            confirm,
            false,
            out,
            tty,
        ),
    }
}

impl Base {
    fn json(&self, tty: bool) -> bool {
        matches!(
            self.format
                .unwrap_or(if tty { Format::Text } else { Format::Json }),
            Format::Json
        )
    }
}

fn write(
    base: Base,
    action: users::UserAction,
    confirmed: bool,
    allow_last_user: bool,
    out: &mut dyn Write,
    tty: bool,
) -> yellowcode_core::Result<()> {
    let snapshot = users::write_snapshot(&base.base)?;
    let outcome = users::apply(
        &base.base,
        &users::UserWriteRequest {
            revision: snapshot.revision,
            confirmed,
            allow_last_user,
            action,
        },
    )?;
    publish_outcome(&outcome, base.json(tty), out)
}

fn publish_outcome(
    outcome: &users::UserWriteOutcome,
    json: bool,
    out: &mut dyn Write,
) -> yellowcode_core::Result<()> {
    let result = (|| -> yellowcode_core::Result<()> {
        if json {
            users::write_json(out, outcome)?;
        } else {
            if let Some(card) = &outcome.user {
                users::write_card(out, card)?;
            }
            if let Some(id) = &outcome.deleted_id {
                writeln!(out, "Deleted UUID: {id}")?;
            }
            writeln!(out, "Users remaining: {}", outcome.users_remaining)?;
            writeln!(out, "Revision: {}", outcome.revision)?;
        }
        Ok(())
    })();
    result.map_err(|_| committed_output_error())
}

fn read(
    base: Base,
    name: Option<String>,
    out: &mut dyn Write,
    tty: bool,
) -> yellowcode_core::Result<()> {
    let cards = users::list(&base.base)?;
    let card = name
        .as_deref()
        .map(|name| users::find(&cards, name))
        .transpose()?;
    if base.json(tty) {
        if let Some(card) = card {
            users::write_json(out, card)?;
        } else {
            users::write_json(out, &cards)?;
        }
    } else if let Some(card) = card {
        users::write_card(out, card)?;
    } else {
        users::write_list(out, &cards)?;
    }
    Ok(())
}
