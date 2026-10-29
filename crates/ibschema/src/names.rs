macro_rules! std_fields {
    ($($(#[$doc:meta])* $v:ident = $c:ident, $l:literal, $s:literal;)*) => {
        pub mod field {
            $($(#[$doc])* pub const $c: &str = $l;)*
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum StdField {
            $($(#[$doc])* $v),*
        }

        impl StdField {
            pub const ALL: &'static [StdField] = &[$(StdField::$v),*];

            pub fn logical(self) -> &'static str {
                match self { $(StdField::$v => field::$c),* }
            }

            pub fn stor_name(self) -> &'static str {
                match self { $(StdField::$v => $s),* }
            }
        }
    };
}

std_fields! {
    Id = ID, "ID", "Ref";
    Version = VERSION, "Version", "DataVersion";
    Marked = MARKED, "Marked", "DeletionMark";
    PredefinedId = PREDEFINED_ID, "PredefinedID", "PredefinedDataName";
    Code = CODE, "Code", "Code";
    Description = DESCRIPTION, "Description", "Description";
    Name = NAME, "Name", "Description";
    Folder = FOLDER, "Folder", "IsFolder";
    ParentId = PARENT_ID, "ParentID", "Parent";
    OwnerId = OWNER_ID, "OwnerID", "Owner";
    DateTime = DATE_TIME, "Date_Time", "Date";
    Number = NUMBER, "Number", "Number";
    NumberPrefix = NUMBER_PREFIX, "NumberPrefix", "";
    Posted = POSTED, "Posted", "Posted";
    EnumOrder = ENUM_ORDER, "EnumOrder", "Order";
    Type = TYPE, "Type", "ValueType";
    SentNo = SENT_NO, "SentNo", "SentNo";
    ReceivedNo = RECEIVED_NO, "ReceivedNo", "ReceivedNo";
    LastUpdate = LAST_UPDATE, "LastUpdate", "ExchangeDate";
    OrderField = ORDER_FIELD, "OrderField", "Order";
    Kind = KIND, "Kind", "Type";
    OffBalance = OFF_BALANCE, "OffBalance", "OffBalance";
    BaseAsAct = BASE_AS_ACT, "BaseAsAct", "ActionPeriodIsBasic";
    Completed = COMPLETED, "Completed", "Completed";
    Started = STARTED, "Started", "Started";
    HeadTask = HEAD_TASK, "HeadTask", "HeadTask";
    BusinessProcess = BUSINESS_PROCESS, "BusinessProcess", "BusinessProcess";
    Point = POINT, "Point", "RoutePoint";
    Executed = EXECUTED, "Executed", "Executed";
    RoutePointOrder = ROUTE_POINT_ORDER, "RoutePointOrder", "Order";
    Document = DOCUMENT, "Document", "Ref";
    LineNo = LINE_NO, "LineNo", "LineNumber";
    KeyField = KEY_FIELD, "KeyField", "";
    DimKind = DIM_KIND, "DimKind", "ExtDimensionType";
    DimIsMetadata = DIM_IS_METADATA, "DimIsMetadata", "Predefined";
    TurnoverOnly = TURNOVER_ONLY, "TurnoverOnly", "TurnoversOnly";
    BaseCkLineNo = BASE_CK_LINE_NO, "BaseCKLineNo", "LineNumber";
    BaseCkBaseCk = BASE_CK_BASE_CK, "BaseCKBaseCK", "CalculationType";
    PredefinedBaseCkTableLine = PREDEFINED_BASE_CK_TABLE_LINE, "PredefinedBaseCKTableLine", "Predefined";
    LeadingCkLineNo = LEADING_CK_LINE_NO, "LeadingCKLineNo", "LineNumber";
    LeadingCkLeadingCk = LEADING_CK_LEADING_CK, "LeadingCKLeadingCK", "CalculationType";
    PredefinedLeadingCkTableLine = PREDEFINED_LEADING_CK_TABLE_LINE, "PredefinedLeadingCKTableLine", "Predefined";
    DisplacedCkLineNo = DISPLACED_CK_LINE_NO, "DisplacedCKLineNo", "LineNumber";
    DisplacedCkDisplCk = DISPLACED_CK_DISPL_CK, "DisplacedCKDisplCK", "CalculationType";
    PredefinedDisplCkTableLine = PREDEFINED_DISPL_CK_TABLE_LINE, "PredefinedDisplCKTableLine", "Predefined";
    Period = PERIOD, "Period", "Period";
    Recorder = RECORDER, "Recorder", "Recorder";
    Active = ACTIVE, "Active", "Active";
    RecordKind = RECORD_KIND, "RecordKind", "RecordType";
    Correspond = CORRESPOND, "Correspond", "RecordType";
    Account = ACCOUNT, "Account", "Account";
    AccountDt = ACCOUNT_DT, "AccountDt", "AccountDr";
    AccountCt = ACCOUNT_CT, "AccountCt", "AccountCr";
    Value = VALUE, "Value", "Value";
    ValueDt = VALUE_DT, "ValueDt", "";
    ValueCt = VALUE_CT, "ValueCt", "";
    KindDt = KIND_DT, "KindDt", "";
    KindCt = KIND_CT, "KindCt", "";
    Turnover = TURNOVER, "Turnover", "";
    TurnoverDt = TURNOVER_DT, "TurnoverDt", "";
    TurnoverCt = TURNOVER_CT, "TurnoverCt", "";
    CalcKind = CALC_KIND, "CalcKind", "CalculationType";
    ActionPeriod = ACTION_PERIOD, "ActionPeriod", "ActionPeriod";
    ApDateFrom = AP_DATE_FROM, "APDateFrom", "BegOfActionPeriod";
    ApDateTill = AP_DATE_TILL, "APDateTill", "EndOfActionPeriod";
    BpDateFrom = BP_DATE_FROM, "BPDateFrom", "BegOfBasePeriod";
    BpDateTill = BP_DATE_TILL, "BPDateTill", "EndOfBasePeriod";
    Storno = STORNO, "Storno", "ReversingEntry";
    Splitter = SPLITTER, "Splitter", "";
    Node = NODE, "Node", "Node";
    MessageNo = MESSAGE_NO, "MessageNo", "MessageNo";
    ConstId = CONST_ID, "ConstID", "";
    RecordKey = RECORD_KEY, "RecordKey", "";
    PdInitialized = PD_INITIALIZED, "PDInitialized", "";
    CkRef = CK_REF, "CKRef", "";
    DisplNumber = DISPL_NUMBER, "DisplNumber", "";
    SliceUsing = SLICE_USING, "SliceUsing", "";
    RegId = REG_ID, "RegID", "";
    ActualPeriod = ACTUAL_PERIOD, "ActualPeriod", "";
    Periodicity = PERIODICITY, "Periodicity", "";
    RepetitionFactor = REPETITION_FACTOR, "RepetitionFactor", "";
    UseTotals = USE_TOTALS, "UseTotals", "";
    MinPeriod = MIN_PERIOD, "MinPeriod", "";
    UseSplitter = USE_SPLITTER, "UseSplitter", "";
    MinCalculatedPeriod = MIN_CALCULATED_PERIOD, "MinCalculatedPeriod", "";
    Use = USE, "Use", "";
    Level = LEVEL, "Level", "";
    DefaultPeriodic = DEFAULT_PERIODIC, "DefaultPeriodic", "";
    PrdBeg = PRD_BEG, "PrdBeg", "";
    PrdEnd = PRD_END, "PrdEnd", "";
    DimUse = DIM_USE, "DimUse", "";
    Size = SIZE, "Size", "";
    Updated = UPDATED, "Updated", "";
    DefaultPrdBeg = DEFAULT_PRD_BEG, "DefaultPrdBeg", "";
    DefaultPrdEnd = DEFAULT_PRD_END, "DefaultPrdEnd", "";
    Prob = PROB, "Prob", "";
    Data = DATA, "Data", "";
    OffDate = OFF_DATE, "OffDate", "";
    AggMode = AGG_MODE, "AggMode", "";
    EnableUse = ENABLE_USE, "EnableUse", "";
    FillStartDate = FILL_START_DATE, "FillStartDate", "";
    FillEndDate = FILL_END_DATE, "FillEndDate", "";
    DeltaPeriodic = DELTA_PERIODIC, "DeltaPeriodic", "";
    BufferPeriodic = BUFFER_PERIODIC, "BufferPeriodic", "";
    State = STATE, "State", "";
    DimKey = DIM_KEY, "DimKey", "";
    DimsMap = DIMS_MAP, "DimsMap", "";
    UseCnt = USE_CNT, "UseCnt", "";
    DimUuid = DIM_UUID, "DimUUID", "";
    DimCode = DIM_CODE, "DimCode", "";
    MdObjId = MD_OBJ_ID, "MDObjID", "";
    FileName = FILE_NAME, "FileName", "";
    UserId = USER_ID, "UserId", "";
    ExtDataSrcId = EXT_DATA_SRC_ID, "ExtDataSrcId", "";
    ConnectPrms = CONNECT_PRMS, "ConnectPrms", "";
    Content = CONTENT, "Content", "";
    MessageBodyId = MESSAGE_BODY_ID, "MessageBodyId", "";
    MessageBodyPartPosition = MESSAGE_BODY_PART_POSITION, "MessageBodyPartPosition", "";
    MessageBodyPart = MESSAGE_BODY_PART, "MessageBodyPart", "";
    ExpirationDate = EXPIRATION_DATE, "ExpirationDate", "";
    MessageBodyKey = MESSAGE_BODY_KEY, "MessageBodyKey", "";
    MessageId = MESSAGE_ID, "MessageId", "";
    Position = POSITION, "Position", "";
    Processed = PROCESSED, "Processed", "";
    MessageHeader = MESSAGE_HEADER, "MessageHeader", "";
    MessageBody = MESSAGE_BODY, "MessageBody", "";
    JobKey = JOB_KEY, "JobKey", "";
    MetadataId = METADATA_ID, "MetadataID", "";
    PredefinedFlag = PREDEFINED, "Predefined", "";
    Parameters = PARAMETERS, "Parameters", "";
    UserName = USER_NAME, "UserName", "";
    ActivationCondition = ACTIVATION_CONDITION, "ActivationCondition", "";
    RestartCount = RESTART_COUNT, "RestartCount", "";
    RestartPeriod = RESTART_PERIOD, "RestartPeriod", "";
    RestartAttemptNumber = RESTART_ATTEMPT_NUMBER, "RestartAttemptNumber", "";
    StartTime = START_TIME, "StartTime", "";
    FinishTime = FINISH_TIME, "FinishTime", "";
    DataSeparationUse = DATA_SEPARATION_USE, "DataSeparationUse", "";
}

impl StdField {
    pub fn of_name(name: &str) -> Option<StdField> {
        let base = name.trim_end_matches(|c: char| c.is_ascii_digit());
        StdField::ALL.iter().copied().find(|s| s.logical() == base)
    }

    pub fn numbered(self, n: u32) -> String {
        format!("{}{n}", self.logical())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Side {
    Dr,
    Cr,
}

impl Side {
    pub fn stor_suffix(self) -> &'static str {
        match self {
            Side::Dr => "Dt",
            Side::Cr => "Ct",
        }
    }

    pub fn query_suffix(self) -> &'static str {
        match self {
            Side::Dr => "Dr",
            Side::Cr => "Cr",
        }
    }
}

pub mod purpose {
    pub const EXT_DATA_SRC_PRMS: &str = "ExtDataSrcPrms";
    pub const EDBT: &str = "EDBT";
    pub const INTEG_SERVICE_SETTINGS: &str = "IntegServiceSettings";
    pub const INTEG_SERVICE_MSG_BODY: &str = "IntegServiceMsgBody";
    pub const INTEG_SERVICE_EXT_MSG_BODY: &str = "IntegServiceExtMsgBody";
    pub const INTEG_CHANNEL_IN_QUEUE: &str = "IntegChannelInQueue";
    pub const INTEG_CHANNEL_OUT_QUEUE: &str = "IntegChannelOutQueue";
    pub const REFERENCE: &str = "Reference";
    pub const DOCUMENT: &str = "Document";
    pub const ENUM: &str = "Enum";
    pub const CHRC: &str = "Chrc";
    pub const ACC: &str = "Acc";
    pub const CKINDS: &str = "CKinds";
    pub const NODE: &str = "Node";
    pub const BPR: &str = "BPr";
    pub const BPR_POINTS: &str = "BPrPoints";
    pub const TASK: &str = "Task";
    pub const CONST: &str = "Const";
    pub const DOCUMENT_JOURNAL: &str = "DocumentJournal";
    pub const SEQ: &str = "Seq";
    pub const SEQ_B: &str = "SeqB";
    pub const INFO_RG: &str = "InfoRg";
    pub const INFO_RG_SL: &str = "InfoRgSL";
    pub const INFO_RG_SF: &str = "InfoRgSF";
    pub const INFO_RG_OPT: &str = "InfoRgOpt";
    pub const ACCUM_RG: &str = "AccumRg";
    pub const ACCUM_RG_T: &str = "AccumRgT";
    pub const ACCUM_RG_TN: &str = "AccumRgTn";
    pub const ACCUM_RG_OPT: &str = "AccumRgOpt";
    pub const ACCUM_RG_AGG_GRID_K: &str = "AccumRgAggGridK";
    pub const ACCUM_RG_AGG_OPT_K: &str = "AccumRgAggOptK";
    pub const ACCUM_RG_AGG_DICT: &str = "AccumRgAggDict";
    pub const ACCUM_RG_DL_K: &str = "AccumRgDlK";
    pub const ACCUM_RG_BF_K: &str = "AccumRgBfK";
    pub const ACCUM_RG_ST: &str = "AccumRgSt";
    pub const ACC_RG: &str = "AccRg";
    pub const ACC_RG_AT: &str = "AccRgAT";
    pub const ACC_RG_CT: &str = "AccRgCT";
    pub const ACC_RG_ED: &str = "AccRgED";
    pub const ACC_RG_OPT: &str = "AccRgOpt";
    pub const CRG: &str = "CRg";
    pub const CRG_ACT_P: &str = "CRgActP";
    pub const CRG_RECALC: &str = "CRgRecalc";
    pub const CKINDS_DN: &str = "CKindsDN";
    pub const VT: &str = "VT";
    pub const EXT_DIM: &str = "ExtDim";
    pub const BASE_CK: &str = "BaseCK";
    pub const LEADING_CK: &str = "LeadingCK";
    pub const DISPLACED_CK: &str = "DisplacedCK";
    pub const REF_SINF: &str = "RefSInf";
    pub const CHRC_SINF: &str = "ChrcSInf";
    pub const ACC_SINF: &str = "AccSInf";
    pub const CKINDS_SINF: &str = "CKindsSInf";
    pub const REFERENCE_CHNGR: &str = "ReferenceChngR";
    pub const DOCUMENT_CHNGR: &str = "DocumentChngR";
    pub const CHRC_CHNGR: &str = "ChrcChngR";
    pub const ACC_CHNGR: &str = "AccChngR";
    pub const CKINDS_CHNGR: &str = "CKindsChngR";
    pub const BPR_CHNGR: &str = "BPrChngR";
    pub const TASK_CHNGR: &str = "TaskChngR";
    pub const CONST_CHNGR: &str = "ConstChngR";
    pub const INFO_RG_CHNGR: &str = "InfoRgChngR";
    pub const ACCUM_RG_CHNGR: &str = "AccumRgChngR";
    pub const ACC_RG_CHNGR: &str = "AccRgChngR";
    pub const CRG_CHNGR: &str = "CRgChngR";
    pub const CHNGR_SUFFIX: &str = "ChngR";
    pub const FLD: &str = "Fld";
    pub const LINE_NO: &str = "LineNo";
    pub const TURNOVER_DT: &str = "TurnoverDt";
    pub const TURNOVER_CT: &str = "TurnoverCt";
    pub const TURNOVER: &str = "Turnover";
    pub const DATA_SEPARATION_USE: &str = "DataSeparationUse";
    pub const BY_FIELD: &str = "ByField";
    pub const BY_PARENT_FIELD: &str = "ByParentField";
    pub const BY_PROPERTY: &str = "ByProperty";
    pub const BY_RESOURCE: &str = "ByResource";
    pub const BY_DIMS: &str = "ByDims";
    pub const BY_DIM_RECORDER: &str = "ByDimRecorder";
    pub const BY_PROP_RECORDER: &str = "ByPropRecorder";
    pub const STT_SETTINGS: &str = "STTSettings";
    pub const STT_GRAMMAR: &str = "STTGrammar";
    pub const STT_GRAMMAR_CHECKSUM: &str = "STTGrammarChecksum";
    pub const STT_MODELS: &str = "STTModels";
    pub const STT_MODELS_DESC: &str = "STTModelsDesc";
    pub const DESCR: &str = "Descr";
    pub const ACOUSTIC: &str = "Acoustic";
    pub const LANG_MODEL: &str = "LangModel";
    pub const DB_SEGMENTS: &str = "DbSegments";
    pub const DB_SEGMENTS_ITEMS: &str = "DbSegmentsItems";
    pub const WEB_SOCKET_CLIENTS: &str = "WebSocketClients";
    pub const EXTENSIONS_RESTRUCT: &str = "ExtensionsRestruct";
    pub const EXTENSIONS_RESTRUCT_NGS: &str = "ExtensionsRestructNGS";
    pub const EXTENSIONS_INFO: &str = "ExtensionsInfo";
    pub const EXTENSIONS_INFO_NGS: &str = "ExtensionsInfoNGS";
    pub const SYSTEM_SETTINGS: &str = "SystemSettings";
    pub const COMMON_SETTINGS: &str = "CommonSettings";
    pub const REP_SETTINGS: &str = "RepSettings";
    pub const REP_VAR_SETTINGS: &str = "RepVarSettings";
    pub const FRM_DT_SETTINGS: &str = "FrmDtSettings";
    pub const DYN_LIST_SETTINGS: &str = "DynListSettings";
    pub const ERROR_PROCESSING_SETTINGS: &str = "ErrorProcessingSettings";
    pub const URL_EXTERNAL_DATA: &str = "URLExternalData";
    pub const INTERNAL_SETTINGS: &str = "InternalSettings";
    pub const DEFAULT_SYSTEM_SETTINGS: &str = "DefaultSystemSettings";
    pub const DEFAULT_INTERNAL_SETTINGS: &str = "DefaultInternalSettings";
    pub const DB_COPIES_INFO_BASE_USE: &str = "DbCopiesInfoBaseUse";
    pub const DB_COPIES_UPDATE_TABLE_STAT: &str = "DbCopiesUpdateTableStat";
    pub const DB_COPIES_UPDATE_STAT: &str = "DbCopiesUpdateStat";
    pub const DB_COPIES: &str = "DbCopies";
    pub const DB_COPIES_SETTINGS: &str = "DbCopiesSettings";
    pub const DB_COPIES_TR_LOGS: &str = "DbCopiesTrLogs";
    pub const DB_COPIES_TR_TABLES: &str = "DbCopiesTrTables";
    pub const DB_COPIES_UPDATES: &str = "DbCopiesUpdates";
    pub const DB_COPIES_TABLES_STATES: &str = "DbCopiesTablesStates";
    pub const DB_COPIES_INITIAL_LAST: &str = "DbCopiesInitialLast";
    pub const DB_COPIES_TR_CHANGES: &str = "DbCopiesTrChanges";
    pub const DB_COPIES_TR_CH_OBJ: &str = "DbCopiesTrChObj";
    pub const MOBILE_CLIENT_DATA_EXCHANGE: &str = "MobileClientDataExchange";
    pub const BOTS: &str = "Bots";
    pub const ODATA_SETTINGS: &str = "ODataSettings";
    pub const DATA_HISTORY_QUEUE0: &str = "DataHistoryQueue0";
    pub const DATA_HISTORY_VERSIONS: &str = "DataHistoryVersions";
    pub const DATA_HISTORY_LATEST_VERSIONS: &str = "DataHistoryLatestVersions";
    pub const DATA_HISTORY_LATEST_VERSIONS1: &str = "DataHistoryLatestVersions1";
    pub const DATA_HISTORY_METADATA: &str = "DataHistoryMetadata";
    pub const DATA_HISTORY_SETTINGS: &str = "DataHistorySettings";
    pub const DATA_HISTORY_AFTER_WRITE_QUEUE: &str = "DataHistoryAfterWriteQueue";
    pub const REF_OPT: &str = "RefOpt";
    pub const CHRC_OPT: &str = "ChrcOpt";
    pub const ACC_OPT: &str = "AccOpt";
    pub const CKINDS_OPT: &str = "CKindsOpt";
    pub const USERS_WORK_HISTORY: &str = "UsersWorkHistory";
    pub const USERS_DMM: &str = "UsersDmm";
    pub const CONFIG_CHNGR: &str = "ConfigChngR";
    pub const SCHEDULED_JOBS: &str = "ScheduledJobs";
    pub const EXT_PROPS: &str = "ExtProps";
    pub const ACCUM_RG_AGG_DIMS: &str = "AccumRgAggDims";
    pub const CONSTS: &str = "Consts";

    pub fn acc_rg_at(k: u32) -> String {
        format!("{ACC_RG_AT}{k}")
    }

    pub fn accum_rg_agg_dict(k: u32) -> String {
        format!("{ACCUM_RG_AGG_DICT}{k}h")
    }
}

pub mod role {
    pub const MAIN: &str = "Main";
    pub const TABULAR_SECTION: &str = "TabularSection";
    pub const CHANGE_RECORDS: &str = "ChangeRecords";
    pub const CONSTANT: &str = "Constant";
    pub const CONSTANT_CHANGE_RECORDS: &str = "ConstantChangeRecords";
    pub const INITIALIZED_PREDEFINED_DATA_IN_CATALOG: &str = "InitializedPredefinedDataInCatalog";
    pub const INITIALIZED_PREDEFINED_DATA_IN_CHART_OF_CHARACTERISTIC_TYPES: &str =
        "InitializedPredefinedDataInChartOfCharacteristicTypes";
    pub const INITIALIZED_PREDEFINED_DATA_IN_CHART_OF_ACCOUNTS: &str =
        "InitializedPredefinedDataInChartOfAccounts";
    pub const INITIALIZED_PREDEFINED_DATA_IN_CHART_OF_CALCULATION_TYPES: &str =
        "InitializedPredefinedDataInChartOfCalculationTypes";
    pub const ROUTE_POINTS: &str = "RoutePoints";
    pub const SEQUENCE_BOUNDARIES: &str = "SequenceBoundaries";
    pub const INFORMATION_REGISTER_TOTALS_OPTIONS: &str = "InformationRegisterTotalsOptions";
    pub const TOTALS_SLICE_LAST: &str = "TotalsSliceLast";
    pub const TOTALS_SLICE_FIRST: &str = "TotalsSliceFirst";
    pub const TOTALS: &str = "Totals";
    pub const TURNOVERS: &str = "Turnovers";
    pub const ACCUMULATION_REGISTER_TOTALS_OPTIONS: &str = "AccumulationRegisterTotalsOptions";
    pub const ACCUMULATION_REGISTERS_AGGREGATE_LIST: &str = "AccumulationRegistersAggregateList";
    pub const ACCUMULATION_REGISTERS_AGGREGATES_OPTIONS: &str =
        "AccumulationRegistersAggregatesOptions";
    pub const ACCUMULATION_REGISTER_DIMENSION_DICTIONARY: &str =
        "AccumulationRegisterDimensionDictionary";
    pub const NEW_TURNOVERS: &str = "NewTurnovers";
    pub const TURNOVERS_BUFFER: &str = "TurnoversBuffer";
    pub const QUERY_STATISTICS: &str = "QueryStatistics";
    pub const TOTALS_BY_ACCOUNTS: &str = "TotalsByAccounts";
    pub const TOTALS_BETWEEN_ACCOUNTS: &str = "TotalsBetweenAccounts";
    pub const EXT_DIMENSIONS_VALUES: &str = "ExtDimensionsValues";
    pub const ACCOUNT_REGISTER_TOTALS_OPTIONS: &str = "AccountRegisterTotalsOptions";
    pub const DISPLACEMENT_ORDER: &str = "DisplacementOrder";
    pub const ACTION_PERIODS: &str = "ActionPeriods";
    pub const RECALCULATION: &str = "Recalculation";

    pub fn totals_by_accounts(k: u32) -> String {
        if k == 0 {
            TOTALS_BY_ACCOUNTS.to_string()
        } else {
            format!("TotalsByAccountsWithExtDim{k}")
        }
    }
}
