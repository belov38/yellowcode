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

const LABEL_TABLE: &str = r#"
CREATE TABLE #label (
 MediaName nvarchar(128), MediaSetId uniqueidentifier, FamilyCount int, FamilySequenceNumber int,
 MediaFamilyId uniqueidentifier, MediaSequenceNumber int, MediaLabelPresent tinyint,
 MediaDescription nvarchar(255), SoftwareName nvarchar(128), SoftwareVendorId int,
 MediaDate datetime, MirrorCount int, IsCompressed bit);
"#;

macro_rules! header_columns {
    () => {
        "BackupName nvarchar(128), BackupDescription nvarchar(255), BackupType smallint, ExpirationDate datetime,
 Compressed tinyint, Position smallint, DeviceType tinyint, UserName nvarchar(128), ServerName nvarchar(128),
 DatabaseName nvarchar(128), DatabaseVersion int, DatabaseCreationDate datetime, BackupSize numeric(20,0),
 FirstLSN numeric(25,0), LastLSN numeric(25,0), CheckpointLSN numeric(25,0), DatabaseBackupLSN numeric(25,0),
 BackupStartDate datetime, BackupFinishDate datetime, SortOrder smallint, CodePage smallint,
 UnicodeLocaleId int, UnicodeComparisonStyle int, CompatibilityLevel tinyint, SoftwareVendorId int,
 SoftwareVersionMajor int, SoftwareVersionMinor int, SoftwareVersionBuild int, MachineName nvarchar(128),
 Flags int, BindingID uniqueidentifier, RecoveryForkID uniqueidentifier, Collation nvarchar(128),
 FamilyGUID uniqueidentifier, HasBulkLoggedData bit, IsSnapshot bit, IsReadOnly bit, IsSingleUser bit,
 HasBackupChecksums bit, IsDamaged bit, BeginsLogChain bit, HasIncompleteMetaData bit, IsForceOffline bit,
 IsCopyOnly bit, FirstRecoveryForkID uniqueidentifier, ForkPointLSN numeric(25,0), RecoveryModel nvarchar(60),
 DifferentialBaseLSN numeric(25,0), DifferentialBaseGUID uniqueidentifier, BackupTypeDescription nvarchar(60),
 BackupSetGUID uniqueidentifier, CompressedBackupSize numeric(20,0), Containment tinyint,
 KeyAlgorithm nvarchar(32), EncryptorThumbprint varbinary(20), EncryptorType nvarchar(32)"
    };
}

const HEADER_TABLES: &str = concat!(
    "\nCREATE TABLE #hdr22 (",
    header_columns!(),
    ",\n LastValidRestoreTime datetime, TimeZone smallint, CompressionAlgorithm nvarchar(32));\nCREATE TABLE #hdr19 (",
    header_columns!(),
    ");\nCREATE TABLE #h (Position int, BackupType int, IsSnapshot bit, IsDamaged bit, HasIncompleteMetaData bit,\n KeyAlgorithm nvarchar(32), EncryptorType nvarchar(32), EncryptorThumbprint varbinary(20),\n Compressed int, RecoveryModel nvarchar(60));\n"
);

const FILES_DECISION: &str = r#" SET @refuse=CASE
  WHEN EXISTS(SELECT 1 FROM #files WHERE TDEThumbprint IS NOT NULL) THEN 'tde'
  WHEN EXISTS(SELECT 1 FROM #files WHERE SnapshotURL IS NOT NULL) THEN 'snapshot_url'
  WHEN (SELECT COUNT(*) FROM #files)<>2 OR (SELECT COUNT(*) FROM #files WHERE [Type]='D')<>1 OR (SELECT COUNT(*) FROM #files WHERE [Type]='L')<>1 OR EXISTS(SELECT 1 FROM #files WHERE IsPresent<>1) THEN 'layout'
  END;
"#;

const CLEANUP_WAIT_SECONDS: u32 = 30;

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

const HEX_UNITS_PER_LINE: usize = 500;

fn backup_expression(path: &str) -> String {
    if path.is_ascii() {
        return format!("N'{}'", path.replace('\'', "''"));
    }
    let units: Vec<u16> = path.encode_utf16().collect();
    let lines: Vec<String> = units
        .chunks(HEX_UNITS_PER_LINE)
        .map(|line| {
            let hex: String = line
                .iter()
                .flat_map(|unit| unit.to_le_bytes())
                .map(|byte| format!("{byte:02X}"))
                .collect();
            format!("0x{hex}")
        })
        .collect();
    format!("CONVERT(nvarchar(4000),{})", lines.join("\n+"))
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

    fn header_decision(&self) -> String {
        let n = self.backup_set;
        format!(
            r#"SET @refuse=CASE
 WHEN @shape=0 THEN 'header_shape'
 WHEN EXISTS(SELECT 1 FROM #h WHERE BackupType NOT IN (1,2,4,5,6,7,8) OR Compressed NOT IN (0,1) OR RecoveryModel COLLATE Latin1_General_100_CI_AS NOT IN (N'FULL',N'BULK-LOGGED',N'BULK_LOGGED',N'SIMPLE')) THEN 'header_shape'
 WHEN EXISTS(SELECT 1 FROM #label WHERE FamilyCount>1) THEN 'striped'
 WHEN NOT EXISTS(SELECT 1 FROM #h WHERE Position={n}) THEN 'set_not_found:'+CONVERT(varchar(9),(SELECT COUNT(*) FROM #h))
 WHEN EXISTS(SELECT 1 FROM #h WHERE Position={n} AND ((KeyAlgorithm IS NOT NULL AND KeyAlgorithm COLLATE Latin1_General_100_CI_AS<>N'NO_Encryption') OR EncryptorType IS NOT NULL OR EncryptorThumbprint IS NOT NULL)) THEN 'encrypted'
 WHEN EXISTS(SELECT 1 FROM #h WHERE Position={n} AND BackupType<>1) THEN 'not_full'
 WHEN EXISTS(SELECT 1 FROM #h WHERE Position={n} AND IsSnapshot=1) THEN 'snapshot'
 WHEN EXISTS(SELECT 1 FROM #h WHERE Position={n} AND IsDamaged=1) THEN 'damaged'
 WHEN EXISTS(SELECT 1 FROM #h WHERE Position={n} AND HasIncompleteMetaData=1) THEN 'incomplete'
 END;
"#
        )
    }

    fn inspection_tables(&self) -> String {
        format!(
            "{}DECLARE @refuse varchar(64)=NULL, @shape int=0;\n{}{}{}",
            self.declarations(),
            FILES_TABLE,
            LABEL_TABLE,
            HEADER_TABLES
        )
    }

    #[doc(hidden)]
    pub fn decision_probe_script(&self, fabricated_rows: &str) -> String {
        format!(
            "{}SET @shape=22;\n{fabricated_rows}\n{}IF @refuse IS NULL BEGIN\n{FILES_DECISION}END;\nSELECT COALESCE(@refuse,'ok');\nGO\n",
            self.inspection_tables(),
            self.header_decision()
        )
    }

    pub fn restore_script(&self) -> String {
        let backup = backup_expression(&self.backup);
        let backup_set = self.backup_set;
        format!(
            r#"SET NOCOUNT ON;
IF ISNULL(IS_SRVROLEMEMBER(N'sysadmin'),0)<>1 BEGIN SELECT 'YC_BAK_REFUSED:not_sysadmin'; RETURN; END;
{tables}SELECT 'YC_BAK_INSPECTING';
DECLARE @backup nvarchar(4000)={backup};
IF DB_ID(@db) IS NOT NULL THROW 51000,'temporary database already exists',1;
DECLARE @query nvarchar(max), @major int=CONVERT(int,SERVERPROPERTY('ProductMajorVersion'));
SET @query=N'RESTORE LABELONLY FROM DISK=@backup';
INSERT #label EXEC sys.sp_executesql @query,N'@backup nvarchar(4000)',@backup=@backup;
SET @query=N'RESTORE HEADERONLY FROM DISK=@backup';
IF @major>=16 BEGIN
 INSERT #hdr22 EXEC sys.sp_executesql @query,N'@backup nvarchar(4000)',@backup=@backup; SET @shape=22;
END ELSE IF @major BETWEEN 13 AND 15 BEGIN
 INSERT #hdr19 EXEC sys.sp_executesql @query,N'@backup nvarchar(4000)',@backup=@backup; SET @shape=19;
END;
INSERT #h SELECT Position,BackupType,IsSnapshot,IsDamaged,HasIncompleteMetaData,KeyAlgorithm,EncryptorType,EncryptorThumbprint,Compressed,RecoveryModel FROM #hdr22;
INSERT #h SELECT Position,BackupType,IsSnapshot,IsDamaged,HasIncompleteMetaData,KeyAlgorithm,EncryptorType,EncryptorThumbprint,Compressed,RecoveryModel FROM #hdr19;
{header_decision}IF @refuse IS NULL BEGIN
 SET @query=N'RESTORE FILELISTONLY FROM DISK=@backup WITH FILE={backup_set}';
 INSERT #files EXEC sys.sp_executesql @query,N'@backup nvarchar(4000)',@backup=@backup;
{FILES_DECISION}END;
IF @refuse IS NOT NULL BEGIN SELECT 'YC_BAK_REFUSED:'+@refuse; RETURN; END;
DECLARE @data nvarchar(128)=(SELECT LogicalName FROM #files WHERE [Type]='D'), @log nvarchar(128)=(SELECT LogicalName FROM #files WHERE [Type]='L');
DECLARE @dd nvarchar(4000)=CONVERT(nvarchar(4000),SERVERPROPERTY('InstanceDefaultDataPath')), @ld nvarchar(4000)=CONVERT(nvarchar(4000),SERVERPROPERTY('InstanceDefaultLogPath'));
IF @dd IS NULL OR @ld IS NULL OR RIGHT(@dd,1) NOT IN (N'\',N'/') OR RIGHT(@ld,1) NOT IN (N'\',N'/') THROW 51000,'unsupported server data path',1;
DECLARE @df nvarchar(4000)=@dd+@db+N'.mdf', @lf nvarchar(4000)=@ld+@db+N'.ldf';
SELECT 'YC_BAK_RESTORING';
RESTORE DATABASE @db FROM DISK=@backup WITH FILE={backup_set}, MOVE @data TO @df, MOVE @log TO @lf, RECOVERY;
IF DATABASEPROPERTYEX(@db,'Status')<>'ONLINE' THROW 51000,'restored database is not online',1;
IF DATABASEPROPERTYEX(@db,'Updateability')='READ_ONLY' BEGIN SET @query=N'ALTER DATABASE '+QUOTENAME(@db)+N' SET READ_WRITE'; EXEC(@query); END;
SET @query=N'USE '+QUOTENAME(@db)+N'; IF EXISTS(SELECT 1 FROM sys.extended_properties WHERE class=0 AND major_id=0 AND minor_id=0 AND name=N''yellowcode.bak.token'') EXEC sys.sp_updateextendedproperty @name=N''yellowcode.bak.token'',@value=@token ELSE EXEC sys.sp_addextendedproperty @name=N''yellowcode.bak.token'',@value=@token';
EXEC sys.sp_executesql @query,N'@token nvarchar(32)',@token=@token;
SET @query=N'ALTER DATABASE '+QUOTENAME(@db)+N' SET READ_ONLY'; EXEC(@query);
SELECT 'YC_BAK_READY';
GO
"#,
            tables = self.inspection_tables(),
            header_decision = self.header_decision()
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

    pub fn abandoned_restore_script(&self, wait_seconds: u32) -> String {
        format!(
            r#"{declarations}DECLARE @wait int={wait_seconds}, @waited int=0, @id int, @state nvarchar(60), @session int, @verdict varchar(30);
CREATE TABLE #files (physical_name nvarchar(260) NOT NULL, size_mb bigint NOT NULL);
WHILE 1=1 BEGIN
 SET @id=NULL; SET @state=NULL; SET @session=NULL;
 SELECT @id=d.database_id, @state=d.state_desc FROM sys.databases AS d WHERE d.name COLLATE Latin1_General_100_BIN2=@db COLLATE Latin1_General_100_BIN2 AND DATALENGTH(d.name)=DATALENGTH(@db);
 IF @id IS NULL BEGIN SELECT 'YC_BAK_GONE'; RETURN; END;
 IF @state COLLATE Latin1_General_100_BIN2<>N'RESTORING' BEGIN SELECT 'YC_BAK_KEPT:'+@state; RETURN; END;
 BEGIN TRY
  SELECT TOP (1) @session=l.request_session_id FROM sys.dm_tran_locks AS l WHERE l.resource_type=N'DATABASE' AND l.resource_database_id=@id AND l.request_session_id<>@@SPID ORDER BY l.request_session_id;
 END TRY
 BEGIN CATCH
  SET @session=NULL;
 END CATCH;
 IF @session IS NULL BREAK;
 IF @waited>=@wait BEGIN SELECT 'YC_BAK_BUSY:'+CONVERT(varchar(11),@session); RETURN; END;
 WAITFOR DELAY '00:00:01';
 SET @waited+=1;
END;
SET @id=NULL; SET @state=NULL;
SELECT @id=d.database_id, @state=d.state_desc FROM sys.databases AS d WHERE d.name COLLATE Latin1_General_100_BIN2=@db COLLATE Latin1_General_100_BIN2 AND DATALENGTH(d.name)=DATALENGTH(@db);
IF @id IS NULL BEGIN SELECT 'YC_BAK_GONE'; RETURN; END;
IF @state COLLATE Latin1_General_100_BIN2<>N'RESTORING' BEGIN SELECT 'YC_BAK_KEPT:'+@state; RETURN; END;
INSERT #files SELECT f.physical_name, CONVERT(bigint,f.size)*8/1024 FROM sys.master_files AS f WHERE f.database_id=@id;
IF IS_SRVROLEMEMBER(N'sysadmin')<>1 BEGIN SET @verdict='YC_BAK_NEEDS_SYSADMIN'; END
ELSE BEGIN
 DECLARE @drop nvarchar(300)=N'DROP DATABASE '+QUOTENAME(@db);
 BEGIN TRY
  EXEC(@drop);
 END TRY
 BEGIN CATCH
  SELECT 'YC_BAK_NOT_DROPPED:'+CONVERT(varchar(11),ERROR_NUMBER())+':'+REPLACE(REPLACE(CONVERT(nvarchar(400),ERROR_MESSAGE()),CHAR(13),N' '),CHAR(10),N' ');
  RETURN;
 END CATCH;
 SET @verdict='YC_BAK_DROPPED';
END;
BEGIN TRY
 EXEC sys.sp_executesql N'SELECT ''YC_BAK_FILE|''+CONVERT(varchar(1),x.file_exists)+''|''+CONVERT(varchar(20),f.size_mb)+''|''+f.physical_name FROM #files AS f CROSS APPLY sys.dm_os_file_exists(f.physical_name) AS x ORDER BY f.physical_name';
END TRY
BEGIN CATCH
 SELECT 'YC_BAK_FILE|?|'+CONVERT(varchar(20),f.size_mb)+'|'+f.physical_name FROM #files AS f ORDER BY f.physical_name;
END CATCH;
SELECT @verdict;
GO
"#,
            declarations = self.declarations()
        )
    }

    pub fn cleanup_script(&self) -> String {
        self.cleanup_script_with_wait(CLEANUP_WAIT_SECONDS)
    }

    pub fn cleanup_script_with_wait(&self, wait_seconds: u32) -> String {
        format!(
            r#"{declarations}DECLARE @wait int={wait_seconds}, @waited int=0, @id int, @state nvarchar(60), @step varchar(5), @error int, @text nvarchar(400);
SELECT @id=d.database_id, @state=d.state_desc FROM sys.databases AS d WHERE d.name COLLATE Latin1_General_100_BIN2=@db COLLATE Latin1_General_100_BIN2 AND DATALENGTH(d.name)=DATALENGTH(@db);
IF @id IS NULL BEGIN SELECT 'YC_BAK_GONE'; RETURN; END;
{ownership}
DECLARE @alter nvarchar(300)=N'ALTER DATABASE '+QUOTENAME(@db)+N' SET SINGLE_USER WITH ROLLBACK IMMEDIATE', @drop nvarchar(300)=N'DROP DATABASE '+QUOTENAME(@db);
WHILE 1=1 BEGIN
 SET @step=NULL; SET @error=NULL;
 BEGIN TRY
  IF @state COLLATE Latin1_General_100_BIN2=N'ONLINE' BEGIN SET @step='alter'; EXEC(@alter); END;
  SET @step='drop';
  EXEC(@drop);
  BREAK;
 END TRY
 BEGIN CATCH
  SET @error=ERROR_NUMBER();
  SET @text=REPLACE(REPLACE(CONVERT(nvarchar(400),ERROR_MESSAGE()),CHAR(13),N' '),CHAR(10),N' ');
 END CATCH;
 IF @waited>=@wait OR @error NOT IN (924,1205,3101,3702,5061,5069) BEGIN
  SELECT 'YC_BAK_NOT_REMOVED:'+ISNULL(@step,'alter')+':'+CONVERT(varchar(11),@error)+':'+CONVERT(varchar(20),(SELECT ISNULL(SUM(CONVERT(bigint,f.size)),0)*8/1024 FROM sys.master_files AS f WHERE f.database_id=@id))+':'+@text;
  RETURN;
 END;
 WAITFOR DELAY '00:00:01';
 SET @waited+=1;
END;
IF EXISTS(SELECT 1 FROM sys.databases AS d WHERE d.name COLLATE Latin1_General_100_BIN2=@db COLLATE Latin1_General_100_BIN2 AND DATALENGTH(d.name)=DATALENGTH(@db)) BEGIN
 SELECT 'YC_BAK_NOT_REMOVED:drop:0:'+CONVERT(varchar(20),(SELECT ISNULL(SUM(CONVERT(bigint,f.size)),0)*8/1024 FROM sys.master_files AS f WHERE f.database_id=@id))+':the database is still listed after the DROP';
 RETURN;
END;
SELECT 'YC_BAK_GONE';
GO
"#,
            declarations = self.declarations(),
            ownership = CHECK_OWNERSHIP,
        )
    }
}
