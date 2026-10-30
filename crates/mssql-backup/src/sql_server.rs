#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid SQL backup path, task token or backup set position")]
    InvalidInput,
    #[error("unsupported SQL data table: {0}")]
    InvalidTable(&'static str),
    #[error(transparent)]
    Catalog(#[from] crate::sql_catalog::Error),
}

pub struct SqlPlan {
    backup: String,
    backup_set: u32,
    token: String,
    database: String,
}

pub fn validate_backup_set(backup_set: u32) -> Result<(), Error> {
    if backup_set == 0 || i32::try_from(backup_set).is_err() {
        return Err(Error::InvalidInput);
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProjectionMode {
    Data,
    FramedResearch,
    OrderedFolder,
    LegacyResearch,
    LegacyFramedResearch,
}

const FILES_TABLE: &str = r#"
CREATE TABLE #files (
 LogicalName nvarchar(128), PhysicalName nvarchar(260), [Type] char(1),
 FileGroupName nvarchar(128), Size numeric(20,0), MaxSize numeric(20,0),
 FileID bigint, CreateLSN numeric(25,0), DropLSN numeric(25,0), UniqueID uniqueidentifier,
 ReadOnlyLSN numeric(25,0), ReadWriteLSN numeric(25,0), BackupSizeInBytes bigint,
 SourceBlockSize int, FileGroupID int, LogGroupGUID uniqueidentifier,
 DifferentialBaseLSN numeric(25,0), DifferentialBaseGUID uniqueidentifier,
 IsReadOnly bit, IsPresent bit, TDEThumbprint varbinary(32), SnapshotURL nvarchar(360));
"#;

const CHECK_OWNERSHIP: &str = r#"
DECLARE @owned bit=0, @check nvarchar(max);
SET @check=N'USE '+QUOTENAME(@db)+N'; SELECT @owned=CASE WHEN EXISTS (SELECT 1 FROM sys.extended_properties WHERE class=0 AND major_id=0 AND minor_id=0 AND name=N''yellowcode.bak.token'' AND DATALENGTH(CONVERT(nvarchar(128),value))=DATALENGTH(@token) AND CONVERT(varbinary(256),CONVERT(nvarchar(128),value))=CONVERT(varbinary(64),@token)) THEN 1 ELSE 0 END';
EXEC sys.sp_executesql @check,N'@token nvarchar(32), @owned bit OUTPUT',@token=@token,@owned=@owned OUTPUT;
IF @owned<>1 THROW 51000,'temporary database ownership mismatch',1;
"#;

fn valid_table_name(table: &str) -> bool {
    !table.is_empty()
        && table.len() <= 128
        && table.is_ascii()
        && !table.chars().any(char::is_control)
}

impl SqlPlan {
    pub fn legacy_schema_script(&self) -> String {
        format!(
            r#"{}IF DB_ID(@db) IS NULL THROW 51000,'temporary database missing',1;
{}
IF DATABASEPROPERTYEX(@db,'Updateability')<>'READ_ONLY' THROW 51000,'temporary database is not read-only',1;
DECLARE @query nvarchar(max)=N'USE '+QUOTENAME(@db)+N'; SELECT ''0x''+CONVERT(varchar(max),CONVERT(varbinary(max),[SerializedData]),2) AS [schema], (SELECT [Offset] FROM dbo._YearOffset) AS [year_offset] FROM dbo.DBSCHEMA FOR JSON PATH, INCLUDE_NULL_VALUES';
EXEC(@query);
GO
"#,
            self.declarations(),
            CHECK_OWNERSHIP
        )
    }
    pub fn legacy_support_script(&self) -> String {
        format!(
            r#"{}IF DB_ID(@db) IS NULL THROW 51000,'temporary database missing',1;
{}
IF DATABASEPROPERTYEX(@db,'Updateability')<>'READ_ONLY' THROW 51000,'temporary database is not read-only',1;
DECLARE @query nvarchar(max)=N'USE '+QUOTENAME(@db)+N';
IF EXISTS(SELECT 1 FROM sys.tables t JOIN sys.schemas s ON s.schema_id=t.schema_id WHERE s.name=N''dbo'' AND t.name COLLATE Latin1_General_100_CI_AS IN (N''BinaryData'',N''BinaryDataStorageContent'',N''BinaryDataStorageVersion'',N''ExternalBinDataStrgsBList'',N''ExternalBinDataStrgsList'',N''V8CMSDPWDS'',N''V8USERPWDPLCS'',N''V8USERSMATKEYS'')) THROW 51000,''legacy profile has unexpected modern system table'',1;
SELECT
CASE WHEN EXISTS(SELECT 1 FROM dbo.V8USERS) THEN 1 ELSE 0 END AS [users],
CASE WHEN EXISTS(SELECT 1 FROM dbo.Params WHERE FileName COLLATE Latin1_General_100_CI_AS=N''users.usr'') THEN 1 ELSE 0 END AS [legacy_users],
0 AS [policies], 0 AS [compromised], 0 AS [auth_keys], 0 AS [storage_content], 0 AS [storage_version], 0 AS [external_stores], 0 AS [external_blobs],
CASE WHEN EXISTS(SELECT 1 FROM dbo.ConfigCAS) THEN 1 ELSE 0 END AS [config_cas],
CASE WHEN EXISTS(SELECT 1 FROM dbo.ConfigCASSave) THEN 1 ELSE 0 END AS [config_cas_save],
CASE WHEN (SELECT COUNT_BIG(*) FROM dbo.SchemaStorage)=1 AND (SELECT COUNT_BIG(*) FROM dbo.DBSchema)=1 AND EXISTS(SELECT 1 FROM dbo.SchemaStorage s CROSS JOIN dbo.DBSchema d WHERE s.SchemaID=0 AND s.Status=100 AND DATALENGTH(s.CurrentSchema)=DATALENGTH(d.SerializedData) AND s.CurrentSchema=CONVERT(varbinary(max),d.SerializedData) AND DATALENGTH(s.NewGenCreated)=14 AND s.NewGenCreated=0xEFBBBF7B302C0D0A7B307D0D0A7D AND DATALENGTH(s.NewGenDropped)=14 AND s.NewGenDropped=0xEFBBBF7B302C0D0A7B307D0D0A7D) THEN 0 ELSE 1 END AS [schema_unstable],
CASE WHEN (SELECT COUNT_BIG(*) FROM dbo.IBVersion)=1 AND EXISTS(SELECT 1 FROM dbo.IBVersion WHERE IBVersion=2 AND PlatformVersionReq=80306) THEN 0 ELSE 1 END AS [version_unverified]
FOR JSON PATH, WITHOUT_ARRAY_WRAPPER, INCLUDE_NULL_VALUES';
EXEC(@query);
GO
"#,
            self.declarations(),
            CHECK_OWNERSHIP
        )
    }
    pub fn extra_table_presence_script(&self, table: &str) -> Result<String, Error> {
        if !valid_table_name(table) {
            return Err(Error::InvalidTable("invalid table name"));
        }
        let table = format!("[{}]", table.replace(']', "]]")).replace('\'', "''");
        Ok(format!(
            r#"{}IF DB_ID(@db) IS NULL THROW 51000,'temporary database missing',1;
{}
IF DATABASEPROPERTYEX(@db,'Updateability')<>'READ_ONLY' THROW 51000,'temporary database is not read-only',1;
DECLARE @query nvarchar(max)=N'USE '+QUOTENAME(@db)+N'; SELECT CASE WHEN EXISTS(SELECT 1 FROM dbo.{}) THEN 1 ELSE 0 END AS [nonempty] FOR JSON PATH, WITHOUT_ARRAY_WRAPPER';
EXEC(@query);
GO
"#,
            self.declarations(),
            CHECK_OWNERSHIP,
            table
        ))
    }
    pub fn new(backup: &str, token: &str) -> Result<Self, Error> {
        Self::new_with_backup_set(backup, token, 1)
    }

    pub fn new_with_backup_set(backup: &str, token: &str, backup_set: u32) -> Result<Self, Error> {
        validate_backup_set(backup_set)?;
        if backup.is_empty()
            || backup.encode_utf16().count() > 4000
            || backup
                .chars()
                .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
            || token.len() != 32
            || !token
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(Error::InvalidInput);
        }
        Ok(Self {
            backup: backup.to_owned(),
            backup_set,
            token: token.to_owned(),
            database: format!("yc_bak_{token}"),
        })
    }

    pub fn database_name(&self) -> &str {
        &self.database
    }

    fn declarations(&self) -> String {
        format!(
            "SET NOCOUNT ON;\nDECLARE @db sysname=N'{}', @token nvarchar(32)=N'{}';\n",
            self.database, self.token
        )
    }

    pub fn restore_script(&self) -> String {
        let backup = self.backup.replace('\'', "''");
        let backup_set = self.backup_set;
        format!(
            r#"{}DECLARE @backup nvarchar(4000)=N'{}';
IF DB_ID(@db) IS NOT NULL THROW 51000,'temporary database already exists',1;
{}
DECLARE @query nvarchar(max)=N'RESTORE FILELISTONLY FROM DISK=@backup WITH FILE={backup_set}';
INSERT #files EXEC sys.sp_executesql @query,N'@backup nvarchar(4000)',@backup=@backup;
IF (SELECT COUNT(*) FROM #files)<>2 OR (SELECT COUNT(*) FROM #files WHERE [Type]='D')<>1 OR (SELECT COUNT(*) FROM #files WHERE [Type]='L')<>1 OR EXISTS(SELECT 1 FROM #files WHERE IsPresent<>1 OR TDEThumbprint IS NOT NULL OR SnapshotURL IS NOT NULL) THROW 51000,'unsupported backup file layout',1;
DECLARE @data nvarchar(128)=(SELECT LogicalName FROM #files WHERE [Type]='D'), @log nvarchar(128)=(SELECT LogicalName FROM #files WHERE [Type]='L');
DECLARE @dd nvarchar(4000)=CONVERT(nvarchar(4000),SERVERPROPERTY('InstanceDefaultDataPath')), @ld nvarchar(4000)=CONVERT(nvarchar(4000),SERVERPROPERTY('InstanceDefaultLogPath'));
IF @dd IS NULL OR @ld IS NULL OR RIGHT(@dd,1) NOT IN (N'\',N'/') OR RIGHT(@ld,1) NOT IN (N'\',N'/') THROW 51000,'unsupported server data path',1;
DECLARE @df nvarchar(4000)=@dd+@db+N'.mdf', @lf nvarchar(4000)=@ld+@db+N'.ldf';
RESTORE DATABASE @db FROM DISK=@backup WITH FILE={backup_set}, MOVE @data TO @df, MOVE @log TO @lf, RECOVERY;
IF DATABASEPROPERTYEX(@db,'Status')<>'ONLINE' THROW 51000,'restored database is not online',1;
IF DATABASEPROPERTYEX(@db,'Updateability')='READ_ONLY' BEGIN SET @query=N'ALTER DATABASE '+QUOTENAME(@db)+N' SET READ_WRITE'; EXEC(@query); END;
SET @query=N'USE '+QUOTENAME(@db)+N'; IF EXISTS(SELECT 1 FROM sys.extended_properties WHERE class=0 AND major_id=0 AND minor_id=0 AND name=N''yellowcode.bak.token'') EXEC sys.sp_updateextendedproperty @name=N''yellowcode.bak.token'',@value=@token ELSE EXEC sys.sp_addextendedproperty @name=N''yellowcode.bak.token'',@value=@token';
EXEC sys.sp_executesql @query,N'@token nvarchar(32)',@token=@token;
SET @query=N'ALTER DATABASE '+QUOTENAME(@db)+N' SET READ_ONLY'; EXEC(@query);
SELECT 'YC_BAK_READY';
GO
"#,
            self.declarations(),
            backup,
            FILES_TABLE
        )
    }

    pub fn year_offset_script(&self) -> String {
        format!(
            r#"{}IF DB_ID(@db) IS NULL THROW 51000,'temporary database missing',1;
{}
IF DATABASEPROPERTYEX(@db,'Updateability')<>'READ_ONLY' THROW 51000,'temporary database is not read-only',1;
DECLARE @query nvarchar(max)=N'USE '+QUOTENAME(@db)+N'; SELECT COUNT_BIG(*) AS [rows], MIN(CONVERT(int,[Offset])) AS [year_offset] FROM dbo._YearOffset FOR JSON PATH, WITHOUT_ARRAY_WRAPPER, INCLUDE_NULL_VALUES';
EXEC(@query);
GO
"#,
            self.declarations(),
            CHECK_OWNERSHIP
        )
    }

    pub fn framed_config_script(&self) -> String {
        format!(
            r#"{}IF DB_ID(@db) IS NULL THROW 51000,'temporary database missing',1;
{}
IF DATABASEPROPERTYEX(@db,'Updateability')<>'READ_ONLY' THROW 51000,'temporary database is not read-only',1;
DECLARE @query nvarchar(max)=N'USE '+QUOTENAME(@db)+N'; SELECT N''YC_ROWS_BEGIN''; SELECT N''R''+(SELECT N''0x''+CONVERT(varchar(max),CONVERT(varbinary(max),CONVERT(nvarchar(max),FileName)),2) AS [name], CONVERT(varchar(33),Creation,126) AS [creation], CONVERT(varchar(33),Modified,126) AS [modified], CONVERT(bigint,Attributes) AS [attributes], CONVERT(bigint,PartNo) AS [part_no], CONVERT(bigint,DataSize) AS [data_size], CONVERT(bigint,DATALENGTH(BinaryData)) AS [stored_size], CONVERT(varbinary(max),BinaryData) AS [data] FOR JSON PATH, WITHOUT_ARRAY_WRAPPER, INCLUDE_NULL_VALUES) FROM dbo.CONFIG ORDER BY FileName COLLATE Latin1_General_100_BIN2,PartNo; SELECT N''YC_ROWS_END:''+CONVERT(nvarchar(20),COUNT_BIG(*)) FROM dbo.CONFIG';
EXEC(@query);
GO
"#,
            self.declarations(),
            CHECK_OWNERSHIP
        )
    }

    pub fn export_script(&self) -> String {
        format!(
            r#"{}IF DB_ID(@db) IS NULL THROW 51000,'temporary database missing',1;
{}
IF DATABASEPROPERTYEX(@db,'Updateability')<>'READ_ONLY' THROW 51000,'temporary database is not read-only',1;
DECLARE @query nvarchar(max)=N'USE '+QUOTENAME(@db)+N'; SELECT (SELECT [Offset] FROM dbo._YearOffset) AS [year_offset], JSON_QUERY((SELECT FileName AS [name], CONVERT(varchar(33),Creation,126) AS [creation], CONVERT(varchar(33),Modified,126) AS [modified], CONVERT(bigint,Attributes) AS [attributes], CONVERT(bigint,PartNo) AS [part_no], CONVERT(bigint,DataSize) AS [data_size], CONVERT(bigint,DATALENGTH(BinaryData)) AS [stored_size], CONVERT(varbinary(max),BinaryData) AS [data] FROM dbo.CONFIG FOR JSON PATH, INCLUDE_NULL_VALUES)) AS [rows] FOR JSON PATH, WITHOUT_ARRAY_WRAPPER, INCLUDE_NULL_VALUES';
EXEC(@query);
GO
"#,
            self.declarations(),
            CHECK_OWNERSHIP
        )
    }

    pub fn schema_script(&self) -> String {
        format!(
            r#"{}IF DB_ID(@db) IS NULL THROW 51000,'temporary database missing',1;
{}
IF DATABASEPROPERTYEX(@db,'Updateability')<>'READ_ONLY' THROW 51000,'temporary database is not read-only',1;
DECLARE @query nvarchar(max)=N'USE '+QUOTENAME(@db)+N'; SELECT ''0x''+CONVERT(varchar(max),[SerializedData],2) AS [schema], (SELECT [Offset] FROM dbo._YearOffset) AS [year_offset] FROM dbo.DBSCHEMA FOR JSON PATH, INCLUDE_NULL_VALUES';
EXEC(@query);
GO
"#,
            self.declarations(),
            CHECK_OWNERSHIP
        )
    }

    pub fn inventory_script(&self) -> String {
        format!(
            r#"{}IF DB_ID(@db) IS NULL THROW 51000,'temporary database missing',1;
{}
IF DATABASEPROPERTYEX(@db,'Updateability')<>'READ_ONLY' THROW 51000,'temporary database is not read-only',1;
DECLARE @query nvarchar(max)=N'USE '+QUOTENAME(@db)+N'; SELECT s.name AS [schema],t.name AS [name] FROM sys.tables t JOIN sys.schemas s ON s.schema_id=t.schema_id ORDER BY s.name,t.name FOR JSON PATH, INCLUDE_NULL_VALUES';
EXEC(@query);
GO
"#,
            self.declarations(),
            CHECK_OWNERSHIP
        )
    }

    pub fn support_script(&self) -> String {
        format!(
            r#"{}IF DB_ID(@db) IS NULL THROW 51000,'temporary database missing',1;
{}
IF DATABASEPROPERTYEX(@db,'Updateability')<>'READ_ONLY' THROW 51000,'temporary database is not read-only',1;
DECLARE @query nvarchar(max)=N'USE '+QUOTENAME(@db)+N'; SELECT
CASE WHEN EXISTS(SELECT 1 FROM dbo.V8USERS) THEN 1 ELSE 0 END AS [users],
CASE WHEN EXISTS(SELECT 1 FROM dbo.Params WHERE FileName COLLATE Latin1_General_100_CI_AS=N''users.usr'') THEN 1 ELSE 0 END AS [legacy_users],
CASE WHEN EXISTS(SELECT 1 FROM dbo.V8USERPWDPLCS) THEN 1 ELSE 0 END AS [policies],
CASE WHEN EXISTS(SELECT 1 FROM dbo.V8CMSDPWDS) THEN 1 ELSE 0 END AS [compromised],
CASE WHEN EXISTS(SELECT 1 FROM dbo.V8USERSMATKEYS) THEN 1 ELSE 0 END AS [auth_keys],
CASE WHEN EXISTS(SELECT 1 FROM dbo.BinaryDataStorageContent) THEN 1 ELSE 0 END AS [storage_content],
CASE WHEN EXISTS(SELECT 1 FROM dbo.BinaryDataStorageVersion) THEN 1 ELSE 0 END AS [storage_version],
CASE WHEN EXISTS(SELECT 1 FROM dbo.ExternalBinDataStrgsList) THEN 1 ELSE 0 END AS [external_stores],
CASE WHEN EXISTS(SELECT 1 FROM dbo.ExternalBinDataStrgsBList) THEN 1 ELSE 0 END AS [external_blobs],
CASE WHEN EXISTS(SELECT 1 FROM dbo.ConfigCAS) THEN 1 ELSE 0 END AS [config_cas],
CASE WHEN EXISTS(SELECT 1 FROM dbo.ConfigCASSave) THEN 1 ELSE 0 END AS [config_cas_save],
CASE WHEN (SELECT COUNT_BIG(*) FROM dbo.SchemaStorage)=1 AND (SELECT COUNT_BIG(*) FROM dbo.DBSchema)=1 AND EXISTS(SELECT 1 FROM dbo.SchemaStorage s CROSS JOIN dbo.DBSchema d WHERE s.SchemaID=0 AND s.Status=100 AND DATALENGTH(s.CurrentSchema)=DATALENGTH(d.SerializedData) AND s.CurrentSchema=d.SerializedData AND DATALENGTH(s.NewGenCreated)=14 AND s.NewGenCreated=0xEFBBBF7B302C0D0A7B307D0D0A7D AND DATALENGTH(s.NewGenDropped)=14 AND s.NewGenDropped=0xEFBBBF7B302C0D0A7B307D0D0A7D) THEN 0 ELSE 1 END AS [schema_unstable],
CASE WHEN (SELECT COUNT_BIG(*) FROM dbo.IBVersion)=1 AND EXISTS(SELECT 1 FROM dbo.IBVersion WHERE IBVersion=7 AND PlatformVersionReq=80313) THEN 0 ELSE 1 END AS [version_unverified]
FOR JSON PATH, WITHOUT_ARRAY_WRAPPER, INCLUDE_NULL_VALUES';
EXEC(@query);
GO
"#,
            self.declarations(),
            CHECK_OWNERSHIP
        )
    }

    pub fn catalog_script(&self, table: &str) -> Result<String, Error> {
        if !valid_table_name(table) {
            return Err(Error::InvalidTable("invalid table name"));
        }
        let table = table.replace('\'', "''");
        Ok(format!(
            r#"{}DECLARE @table nvarchar(128)=N'{}';
IF DB_ID(@db) IS NULL THROW 51000,'temporary database missing',1;
{}
IF DATABASEPROPERTYEX(@db,'Updateability')<>'READ_ONLY' THROW 51000,'temporary database is not read-only',1;
DECLARE @query nvarchar(max)=N'USE '+QUOTENAME(@db)+N'; SELECT c.name AS [column_name], ty.name AS [sql_type], c.max_length, c.precision, c.scale, c.is_nullable FROM sys.tables t JOIN sys.schemas s ON s.schema_id=t.schema_id JOIN sys.columns c ON c.object_id=t.object_id JOIN sys.types ty ON ty.user_type_id=c.user_type_id WHERE s.name=N''dbo'' AND t.name=@table ORDER BY c.column_id FOR JSON PATH, INCLUDE_NULL_VALUES';
EXEC sys.sp_executesql @query,N'@table nvarchar(128)',@table=@table;
GO
"#,
            self.declarations(),
            table,
            CHECK_OWNERSHIP
        ))
    }

    pub fn blob_count_script(&self) -> String {
        format!(
            r#"{}IF DB_ID(@db) IS NULL THROW 51000,'temporary database missing',1;
{}
IF DATABASEPROPERTYEX(@db,'Updateability')<>'READ_ONLY' THROW 51000,'temporary database is not read-only',1;
DECLARE @query nvarchar(max)=N'USE '+QUOTENAME(@db)+N'; SELECT COUNT_BIG(*) AS [rows] FROM dbo.BINARYDATA FOR JSON PATH, WITHOUT_ARRAY_WRAPPER';
EXEC(@query);
GO
"#,
            self.declarations(),
            CHECK_OWNERSHIP
        )
    }

    pub fn users_email_script(&self) -> String {
        format!(
            r#"{}IF DB_ID(@db) IS NULL THROW 51000,'temporary database missing',1;
{}
IF DATABASEPROPERTYEX(@db,'Updateability')<>'READ_ONLY' THROW 51000,'temporary database is not read-only',1;
DECLARE @query nvarchar(max)=N'USE '+QUOTENAME(@db)+N'; SELECT CASE WHEN EXISTS(SELECT 1 FROM dbo.V8USERS WHERE Email IS NOT NULL) THEN 1 ELSE 0 END AS [present] FOR JSON PATH, WITHOUT_ARRAY_WRAPPER';
EXEC(@query);
GO
"#,
            self.declarations(),
            CHECK_OWNERSHIP
        )
    }

    pub fn blob_script(
        &self,
        reference: &crate::sql_blob::StorageRef,
        actual: &[crate::sql_catalog::SqlColumn],
    ) -> Result<String, Error> {
        use ibschema::columns::{ColRole, ColType, Column, Origin, Part};
        use std::fmt::Write as _;
        if reference.size > 64 * 1024 * 1024 || actual.len() != 4 {
            return Err(Error::InvalidTable(
                "unsupported binary storage catalog or size",
            ));
        }
        let column = |name: &str, ty, role| Column {
            name: name.into(),
            ty,
            role,
            nullable: false,
            origin: Origin::Field(0),
            part: Part::Single,
        };
        let expected = [
            column("f_key", ColType::B(16), ColRole::Binary),
            column("f_off", ColType::N(18, 0), ColRole::Number),
            column("f_num", ColType::N(18, 0), ColRole::Number),
            column("f_data", ColType::I, ColRole::Binary),
        ];
        crate::sql_catalog::validate_columns(&expected, actual)?;
        let physical = |name: &str| -> Result<String, Error> {
            let item = actual
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(name))
                .ok_or(Error::InvalidTable("validated storage column missing"))?;
            Ok(format!("[{}]", item.name.replace(']', "]]")))
        };
        let key = physical("f_key")?;
        let offset = physical("f_off")?;
        let number = physical("f_num")?;
        let data = physical("f_data")?;
        let mut literal = String::from("0x");
        for byte in reference.key {
            write!(&mut literal, "{byte:02X}")
                .map_err(|_| Error::InvalidTable("storage key formatting"))?;
        }
        let query = format!(
            "IF EXISTS (SELECT 1 FROM dbo.[BINARYDATA] WHERE {key}={literal}) SELECT '0x'+CONVERT(varchar(max),{key},2) AS [key],CONVERT(varchar(100),{offset}) AS [offset],CONVERT(varchar(100),{number}) AS [number],'0x'+CONVERT(varchar(max),{data},2) AS [data] FROM dbo.[BINARYDATA] WHERE {key}={literal} FOR JSON PATH, INCLUDE_NULL_VALUES ELSE SELECT N'[]'"
        );
        let query = query.replace('\'', "''");
        Ok(format!(
            r#"{}IF DB_ID(@db) IS NULL THROW 51000,'temporary database missing',1;
{}
IF DATABASEPROPERTYEX(@db,'Updateability')<>'READ_ONLY' THROW 51000,'temporary database is not read-only',1;
DECLARE @query nvarchar(max)=N'USE '+QUOTENAME(@db)+N'; {}';
EXEC(@query);
GO
"#,
            self.declarations(),
            CHECK_OWNERSHIP,
            query
        ))
    }

    pub fn folder_table_script(
        &self,
        head: &dtfile::TableHead,
        actual: &[crate::sql_catalog::SqlColumn],
    ) -> Result<String, Error> {
        let columns = crate::sql_folder::source_columns(head)
            .map_err(|_| Error::InvalidTable("unsupported folder layout"))?;
        if actual.len() != columns.len() {
            return Err(Error::InvalidTable("unexpected folder catalog columns"));
        }
        self.data_table_script_inner(
            &head.phys_name,
            &columns,
            actual,
            ProjectionMode::OrderedFolder,
        )
    }

    pub fn data_table_script(
        &self,
        table: &str,
        columns: &[ibschema::columns::Column],
        actual: &[crate::sql_catalog::SqlColumn],
    ) -> Result<String, Error> {
        self.data_table_script_inner(table, columns, actual, ProjectionMode::Data)
    }

    pub fn framed_data_table_script(
        &self,
        table: &str,
        columns: &[ibschema::columns::Column],
        actual: &[crate::sql_catalog::SqlColumn],
    ) -> Result<String, Error> {
        self.data_table_script_inner(table, columns, actual, ProjectionMode::FramedResearch)
    }

    pub fn legacy_data_table_script(
        &self,
        table: &str,
        columns: &[ibschema::columns::Column],
        actual: &[crate::sql_catalog::SqlColumn],
    ) -> Result<String, Error> {
        self.data_table_script_inner(table, columns, actual, ProjectionMode::LegacyResearch)
    }

    pub fn legacy_framed_data_table_script(
        &self,
        table: &str,
        columns: &[ibschema::columns::Column],
        actual: &[crate::sql_catalog::SqlColumn],
    ) -> Result<String, Error> {
        self.data_table_script_inner(table, columns, actual, ProjectionMode::LegacyFramedResearch)
    }

    fn data_table_script_inner(
        &self,
        table: &str,
        columns: &[ibschema::columns::Column],
        actual: &[crate::sql_catalog::SqlColumn],
        mode: ProjectionMode,
    ) -> Result<String, Error> {
        use ibschema::columns::ColType;
        if !valid_table_name(table) || columns.is_empty() {
            return Err(Error::InvalidTable("invalid table name or empty schema"));
        }
        if matches!(
            mode,
            ProjectionMode::LegacyResearch | ProjectionMode::LegacyFramedResearch
        ) {
            crate::sql_catalog::validate_legacy_columns(columns, actual)?;
        } else {
            crate::sql_catalog::validate_columns(columns, actual)?;
        }
        let quote = |name: &str| format!("[{}]", name.replace(']', "]]"));
        let mut projections = Vec::with_capacity(columns.len());
        for column in columns {
            let physical = actual
                .iter()
                .find(|item| item.name.eq_ignore_ascii_case(&column.name))
                .ok_or(Error::InvalidTable("validated SQL column missing"))?;
            let field = quote(&physical.name);
            let alias = quote(&column.name);
            let value = match column.ty {
                ColType::I if physical.sql_type.eq_ignore_ascii_case("image") => {
                    format!("'0x'+CONVERT(varchar(max),CONVERT(varbinary(max),{field}),2)")
                }
                ColType::NT if physical.sql_type.eq_ignore_ascii_case("ntext") => {
                    format!(
                        "'0x'+CONVERT(varchar(max),CONVERT(varbinary(max),CONVERT(nvarchar(max),{field})),2)"
                    )
                }
                ColType::L | ColType::B(_) | ColType::VB(_) | ColType::I => {
                    format!("'0x'+CONVERT(varchar(max),{field},2)")
                }
                ColType::NC(_) | ColType::NVC(_) | ColType::NT => {
                    format!("'0x'+CONVERT(varchar(max),CONVERT(varbinary(max),{field}),2)")
                }
                ColType::N(_, _) => format!("CONVERT(varchar(100),{field})"),
                ColType::DT => format!("CONVERT(varchar(33),{field},126)"),
                ColType::RV => return Err(Error::InvalidTable("rowversion cannot be projected")),
            };
            projections.push(format!("{value} AS {alias}"));
        }
        let order = if mode == ProjectionMode::OrderedFolder {
            let physical = |name: &str| {
                actual
                    .iter()
                    .find(|column| column.name.eq_ignore_ascii_case(name))
                    .map(|column| quote(&column.name))
                    .ok_or(Error::InvalidTable("folder ordering column missing"))
            };
            format!(
                " ORDER BY {} COLLATE Latin1_General_100_BIN2,{}",
                physical("FILENAME")?,
                physical("PARTNO")?
            )
        } else {
            String::new()
        };
        let query = if matches!(
            mode,
            ProjectionMode::FramedResearch | ProjectionMode::LegacyFramedResearch
        ) {
            format!(
                "SELECT N'YC_ROWS_BEGIN'; SELECT N'R'+(SELECT {columns} FOR JSON PATH, WITHOUT_ARRAY_WRAPPER, INCLUDE_NULL_VALUES) FROM dbo.{table}; SELECT N'YC_ROWS_END:'+CONVERT(nvarchar(20),COUNT_BIG(*)) FROM dbo.{table}",
                columns = projections.join(","),
                table = quote(table),
            )
        } else {
            format!(
                "IF EXISTS (SELECT 1 FROM dbo.{table}) SELECT {columns} FROM dbo.{table}{order} FOR JSON PATH, INCLUDE_NULL_VALUES ELSE SELECT N'[]'",
                columns = projections.join(","),
                table = quote(table),
            )
        };
        let query = query.replace('\'', "''");
        Ok(format!(
            r#"{}IF DB_ID(@db) IS NULL THROW 51000,'temporary database missing',1;
{}
IF DATABASEPROPERTYEX(@db,'Updateability')<>'READ_ONLY' THROW 51000,'temporary database is not read-only',1;
DECLARE @query nvarchar(max)=N'USE '+QUOTENAME(@db)+N'; {}';
EXEC(@query);
GO
"#,
            self.declarations(),
            CHECK_OWNERSHIP,
            query
        ))
    }

    pub fn cleanup_script(&self) -> String {
        format!(
            r#"{}IF DB_ID(@db) IS NULL BEGIN SELECT 'YC_BAK_GONE'; RETURN; END;
{}
DECLARE @query nvarchar(max)=N'DROP DATABASE '+QUOTENAME(@db); EXEC(@query);
SELECT 'YC_BAK_GONE';
GO
"#,
            self.declarations(),
            CHECK_OWNERSHIP
        )
    }
}
