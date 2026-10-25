use md_model::Configuration;
use query_builder::ops::{self, Op, OpError};
use query_builder::{Ctx, OpenError, Schema, Shape};
use query_ir::{NoTempTables, QuerySchema};
use query_syntax::{Lang, MsgId, ParseMode, QueryError};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};

pub const MAX_UNDO: usize = 50;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorOut {
    pub chain: Vec<String>,
    pub at: Option<(u32, u32)>,
}

impl ErrorOut {
    fn plain(text: impl Into<String>) -> ErrorOut {
        ErrorOut {
            chain: vec![text.into()],
            at: None,
        }
    }

    fn from_query(e: &QueryError) -> ErrorOut {
        let mut chain = Vec::new();
        let mut at = None;
        let mut cur = Some(e);
        while let Some(x) = cur {
            if at.is_none() {
                at = x.at.map(|a| (a.pos.line, a.pos.col));
            }
            chain.push(x.to_string());
            cur = x.cause.as_deref();
        }
        ErrorOut { chain, at }
    }
}

impl From<&OpenError> for ErrorOut {
    fn from(e: &OpenError) -> ErrorOut {
        match e {
            OpenError::Syntax(q) | OpenError::Semantic(q) => ErrorOut::from_query(q),
            OpenError::Shape { stmt, what } => {
                ErrorOut::plain(format!("Оператор {}: {}", stmt + 1, shape_text(*what)))
            }
        }
    }
}

impl From<&OpError> for ErrorOut {
    fn from(e: &OpError) -> ErrorOut {
        match e {
            OpError::Expr(q) | OpError::Semantic(q) => ErrorOut::from_query(q),
            OpError::Shape(s) => ErrorOut::plain(shape_text(*s)),
            OpError::NoStmt(i) => ErrorOut::plain(format!("Нет запроса пакета с номером {i}")),
            OpError::NotSelect(i) => {
                ErrorOut::plain(format!("Элемент пакета {i} не запрос выбора"))
            }
            OpError::NoOperator(i) => ErrorOut::plain(format!("Нет оператора с номером {i}")),
            OpError::LastOperator => ErrorOut::plain("Нельзя удалить последний оператор запроса"),
            OpError::NoSource(a) => ErrorOut::plain(format!("Источник не найден: {a}")),
            OpError::AliasTaken(a) => ErrorOut::plain(format!(
                "{} \"{a}\"",
                ru(MsgId::SELECT_ERROR_DUPLICATEDALIAS)
            )),
            OpError::NotRoot(a) => ErrorOut::plain(format!("Источник не корневой: {a}")),
            OpError::Cycle(a) => ErrorOut::plain(format!("Циклическое соединение: {a}")),
            OpError::NoItem { what, index } => {
                ErrorOut::plain(format!("Нет элемента {index} ({what})"))
            }
            OpError::Unsupported(w) => ErrorOut::plain(format!("Только в тексте: {w}")),
        }
    }
}

fn ru(id: MsgId) -> String {
    id.text(Lang::Ru).to_string()
}

fn shape_text(s: Shape) -> String {
    match s {
        Shape::IntoNotFirst => {
            ru(MsgId::SELECT_ERROR_INTOCANBEONLYINMAINSELECT).replace("%1", "ПОМЕСТИТЬ")
        }
        Shape::IndexNotLast => format!("{} \"ОБЪЕДИНИТЬ\"", ru(MsgId::SELECT_ERROR_SYNTAXERROR)),
        Shape::UnionWidth => ru(MsgId::SELECT_ERROR_DIFFERENTFIELDSCOUNTINUNIONS),
        Shape::SubqueryAlias => ru(MsgId::SELECT_ERROR_ALIASEXPECTED),
        Shape::DcsOrder => "УПОРЯДОЧИТЬ ПО в произвольных выражениях не поддерживается".into(),
    }
}

pub struct Session {
    pub(crate) qs: Arc<QuerySchema>,
    schema: Schema,
    undo: Vec<Schema>,
    pub(crate) catalog: OnceLock<crate::view::Catalog>,
}

impl Session {
    pub fn new(cfg: &Configuration) -> Result<Session, ErrorOut> {
        let qs = QuerySchema::build(cfg, platform_catalog::catalog()).map_err(|e| {
            ErrorOut::plain(format!(
                "Не удалось построить схему запросов конфигурации: {e}"
            ))
        })?;
        Ok(Session::with_schema(qs))
    }

    pub fn with_schema(qs: Arc<QuerySchema>) -> Session {
        Session::empty(qs, ParseMode::Query)
    }

    pub fn empty(qs: Arc<QuerySchema>, mode: ParseMode) -> Session {
        Session {
            qs,
            schema: Schema::new(mode),
            undo: Vec::new(),
            catalog: OnceLock::new(),
        }
    }

    pub fn query_schema(&self) -> &Arc<QuerySchema> {
        &self.qs
    }

    pub fn open(&mut self, text: &str, mode: ParseMode) -> Result<(), ErrorOut> {
        let ctx = Ctx {
            schema: &self.qs,
            temp: &NoTempTables,
        };
        let opened = query_builder::open(text, mode, &ctx).map_err(|e| ErrorOut::from(&e))?;
        self.schema = opened.schema;
        self.undo.clear();
        Ok(())
    }

    pub fn apply(&mut self, op: &Op) -> Result<(), ErrorOut> {
        let ctx = Ctx {
            schema: &self.qs,
            temp: &NoTempTables,
        };
        let before = self.schema.deep_clone();
        ops::apply(&mut self.schema, op, &ctx).map_err(|e| ErrorOut::from(&e))?;
        self.undo.push(before);
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        match self.undo.pop() {
            Some(s) => {
                self.schema = s;
                true
            }
            None => false,
        }
    }

    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    pub fn text(&self) -> String {
        self.schema.to_text()
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    pub fn mode(&self) -> ParseMode {
        self.schema.mode
    }
}
