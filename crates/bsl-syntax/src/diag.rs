use crate::source::Pos;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct MsgId(pub &'static str);

pub mod msg {
    use super::MsgId;

    pub const EXP_EXPR: MsgId = MsgId("IDS_STX_EXP_EXPR");
    pub const EXPR_ERROR: MsgId = MsgId("IDS_STX_EXPR_ERROR");
    pub const EXP_SEMICOLON: MsgId = MsgId("IDS_STX_EXP_SEMICOLON");
    pub const EXP_RPAR: MsgId = MsgId("IDS_STX_EXP_RPAR");
    pub const EXP_LPAR: MsgId = MsgId("IDS_STX_EXP_LPAR");
    pub const EXP_COMMA: MsgId = MsgId("IDS_STX_EXP_COMMA");
    pub const EXP_RBRACK: MsgId = MsgId("IDS_STX_EXP_RBRACK");
    pub const EXP_EQ: MsgId = MsgId("IDS_STX_EXP_EQ");
    pub const EXP_THEN: MsgId = MsgId("IDS_STX_EXP_THEN");
    pub const EXP_DO: MsgId = MsgId("IDS_STX_EXP_DO");
    pub const EXP_TO: MsgId = MsgId("IDS_STX_EXP_TO");
    pub const EXP_IN: MsgId = MsgId("IDS_STX_EXP_IN");
    pub const EXP_ENDIF: MsgId = MsgId("IDS_STX_EXP_ENDIF");
    pub const EXP_ENDDO: MsgId = MsgId("IDS_STX_EXP_ENDDO");
    pub const EXP_ENDPROCEDURE: MsgId = MsgId("IDS_STX_EXP_ENDPROCEDURE");
    pub const EXP_ENDFUNCTION: MsgId = MsgId("IDS_STX_EXP_ENDFUNCTION");
    pub const EXP_ENDTRY: MsgId = MsgId("IDS_STX_EXP_ENDTRY");
    pub const EXP_EXCEPT: MsgId = MsgId("IDS_STX_EXP_EXCEPT");
    pub const EXP_STMTS: MsgId = MsgId("IDS_STX_EXP_STMTS");
    pub const UNRECOGN_STMT: MsgId = MsgId("IDS_STX_UNRECOGN_STMT");
    pub const EXP_IDENT: MsgId = MsgId("IDS_STX_EXP_IDENT");
    pub const EXP_VARNAME: MsgId = MsgId("IDS_STX_EXP_VARNAME");
    pub const EXP_PROCNAME: MsgId = MsgId("IDS_STX_EXP_PROCNAME");
    pub const EXP_FUNCNAME: MsgId = MsgId("IDS_STX_EXP_FUNCNAME");
    pub const EXP_FORMPARAMS: MsgId = MsgId("IDS_STX_EXP_FORMPARAMS");
    pub const EXP_FORMPARAM: MsgId = MsgId("IDS_STX_EXP_FORMPARAM");
    pub const EXP_FORMPARAMNAME: MsgId = MsgId("IDS_STX_EXP_FORMPARAMNAME");
    pub const EXP_CONST: MsgId = MsgId("IDS_STX_EXP_CONST");
    pub const EXP_LABEL: MsgId = MsgId("IDS_STX_EXP_LABEL");
    pub const EXP_COLON: MsgId = MsgId("IDS_STX_EXP_COLON");
    pub const EXP_EOT: MsgId = MsgId("IDS_STX_EXP_EOT");
    pub const BAD_NUMCONST: MsgId = MsgId("IDS_STX_BAD_NUMCONST");
    pub const BAD_DATECONST: MsgId = MsgId("IDS_STX_BAD_DATECONST");
    pub const MIS_DOUBLEQUOT: MsgId = MsgId("IDS_STX_MIS_DOUBLEQUOT");
    pub const MIS_SINGLEQUOT: MsgId = MsgId("IDS_STX_MIS_SINGLEQUOT");
    pub const BLTNFUNC_BAD_USE: MsgId = MsgId("IDS_STX_BLTNFUNC_BAD_USE");
    pub const BREAK_INVALID: MsgId = MsgId("IDS_STX_BREAK_INVALID");
    pub const CONTINUE_INVALID: MsgId = MsgId("IDS_STX_CONTINUE_INVALID");
    pub const RETURN_INVALID: MsgId = MsgId("IDS_STX_RETURN_INVALID");
    pub const RAISE_INVALID: MsgId = MsgId("IDS_STX_RAISE_INVALID");
    pub const AWAIT_INVALID: MsgId = MsgId("IDS_STX_AWAIT_INVALID");
    pub const PROC_RETVAL: MsgId = MsgId("IDS_STX_PROC_RETVAL");
    pub const LATE_DECLARE: MsgId = MsgId("IDS_STX_LATE_DECLARE");
    pub const LATE_PROCFUNC: MsgId = MsgId("IDS_STX_LATE_PROCFUNC");
    pub const EXP_VARPROCFUNC: MsgId = MsgId("IDS_STX_EXP_VARPROCFUNC");
    pub const LIBRARY_MODULE: MsgId = MsgId("IDS_STX_LIBRARY_MODULE");
    pub const EXPORT_LOCAL: MsgId = MsgId("IDS_STX_EXPORT_LOCAL");
    pub const VAR_DEFINED: MsgId = MsgId("IDS_STX_VAR_DEFINED");
    pub const FORMPARAM_DEFINED: MsgId = MsgId("IDS_STX_FORMPARAM_DEFINED");
    pub const LABEL_DEFINED: MsgId = MsgId("IDS_STX_LABEL_DEFINED");
    pub const LABEL_NOTDEFINED: MsgId = MsgId("IDS_STX_LABEL_NOTDEFINED");
    pub const LABEL_WRONG_GOTO: MsgId = MsgId("IDS_STX_LABEL_WRONG_GOTO");
    pub const UNKNOWN_ANNOT: MsgId = MsgId("IDS_STX_UNKNOWN_ANNOT");
    pub const EXP_PREPROCESSOREXPR: MsgId = MsgId("IDS_STX_EXP_PREPROCESSOREXPR");
    pub const EXP_PREPROCESSORIDENT: MsgId = MsgId("IDS_STX_EXP_PREPROCESSORIDENT");
    pub const EXP_PREPROCESSORSTMT: MsgId = MsgId("IDS_STX_EXP_PREPROCESSORSTMT");
    pub const EXP_PREPROCESSORENDIF: MsgId = MsgId("IDS_STX_EXPPREPROCESSORENDIF");
    pub const EXP_PREPROCESSORENDREGION: MsgId = MsgId("IDS_STX_EXPPREPROCESSORENDREGION");
    pub const EXP_PREPROCESSORENDINSERT: MsgId = MsgId("IDS_STX_EXPPREPROCESSORENDINSERT");
    pub const EXP_PREPROCESSORENDDELETE: MsgId = MsgId("IDS_STX_EXPPREPROCESSORENDDELETE");
    pub const EXP_PREPROCESSORINSERT: MsgId = MsgId("IDS_STX_EXPPREPROCESSORINSERT");
    pub const EXP_PREPROCESSORDELETE: MsgId = MsgId("IDS_STX_EXPPREPROCESSORDELETE");
    pub const MIS_PREPROCESSORIF: MsgId = MsgId("IDS_STX_MIS_PREPROCESSORIF");
    pub const MIS_PREPROCESSORREGION: MsgId = MsgId("IDS_STX_MIS_PREPROCESSORREGION");
    pub const PREPROCESSORERROR: MsgId = MsgId("IDS_STX_PREPROCESSORERROR");
    pub const UNKNOWN: MsgId = MsgId("IDS_STX_UNKNOWN");
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxError {
    pub msg: MsgId,
    pub arg: Option<String>,
    pub pos: Pos,
}
