use std::{collections::HashSet, sync::OnceLock};

const WORDS: &str = concat!(
    "a about above across after again against all almost along already also although always am among an and \
     another any anyone anything are around as at away back be became because become been before began behind \
     being below beside besides between beyond both but by can cannot could did do does doing done down during \
     each either else enough even ever every except few first for from further get gets give given gives go \
     goes going gone got had has have having he her here hers him his how however i if in indeed inside \
     instead into is it its itself just keep kept last later least less let like likely made make makes many \
     may me might more most much must my near neither never new next no nor not nothing now of off often on \
     once one only onto or other others otherwise our out outside over own per perhaps please rather really \
     same several shall she should since so some something sometimes soon still such than that the their them \
     then there therefore these they this those though through throughout thus to together too toward towards \
     under unless until up upon us use used uses using usually very via was we well were what whatever when \
     whenever where whereas whether which while who whom whose why will with within without would yes yet you \
     your ",
    "t s d ll re ve m doesn isn aren wasn weren hasn haven hadn didn don couldn wouldn shouldn mustn needn ",
    "abnormal abnormally abort aborted absent absolute access accessed accessible account accounts achieved \
     action actions active activity actual add added adding additional address addresses administrator \
     advanced affected agent algorithm alias allocate allocated allocation allow allowed allows alter altered \
     alternate ambiguous amount analysis answer api application applied applies apply approach archive area \
     argument arguments arithmetic array ascii assembly assign assigned associated attached attempt attempted \
     attempting attempts attention attribute attributed attributes authentication authority authorization \
     automatic automatically available average avoid ",
    "backed backup backups bad bak balance base based basic batch begin behavior best binary binding bit bits \
     bitmap block blocked blocking blocks boolean boot bound bounded boundary bounds broken budget buffer \
     buffers build builds built bulk busy button byte bytes ",
    "cache cached calculate calculated call called calling calls cancel canceled cancelled cancellation \
     capacity case cases catalog category cause caused causes certificate chain change changed changes \
     changing channel char character characters charset check checked checking checkpoint checks checksum \
     checksums children chosen chunk class classes clean cleaned cleanup clear client clients clock clone \
     close closed closing cluster code codepage codes collate collation collations collect collected column \
     columns combination comma command commands comment commit committed common communication compare \
     compared comparison compatibility compatible compile complete completed completely completes completion \
     component components compress compressed compression compromised computer concurrent condition \
     conditions config configuration configurations configure configured confirm confirmed conflict connect \
     connected connecting connection connections consecutive consistency consistent constraint consumer \
     contain contained container containing contains containment content contents context continue control \
     controller conversion convert converted converting copy copyright core corrupt corrupted corruption \
     correct correctly count counted counter counters counts cpu crash create created creates creating \
     creation credential credentials critical cross current currently cursor custom cyclic ",
    "damaged data database databases datasize datatype date datetime day days deadlock deadline debug decimal \
     decision declared decode decompress default defaults deferred define defined definition definitions \
     delete deleted deleting delimiter denied depend dependent deprecated derived describe description \
     descriptor design destination detach detail details detect detected detection developer device devices \
     diagnostic diagnostics differ different differential differs digit digits direct direction directory \
     dirty disable disabled disappeared disconnect disconnected discovered disk dismounted display \
     distributed divide domain  double  download drain drive driver drivers drop dropped duplicate \
     duration dynamic ",
    "earlier early edition effective element elements email empty enable enabled enclosing encoded encoding \
     encountered encrypt encrypted encryption end ended ends endpoint engine ensure enterprise entire entries \
     entry environment equal equals error errors escape escaped established establishing event events exact \
     exactly example exceed exceeded exceeds exception excessive exchange excluded exclusive execute executed \
     executing execution exist existing exists exit exited expected expects experimental expiration expire \
     expired explicit export express expression extended extension extensions extent extents external extra ",
    "fail failed failing fails failure failures false family fast fatal fault feature features fewer field \
     fields file filegroup filegroups filelistonly filemark files filestream fill filter final finally find \
     finish finished firewall fix fixed flag flags float flush folder folders follow followed following \
     footer force forced foreign form format formats formatting formed forward found fraction fractional \
     fragment frame free full function functions future ",
    "gap garbage gateway general generate generated generic global grant granted greater group groups guid \
     handle handled handler hardware hash  head header headeronly heap held help hex hexadecimal \
     hierarchy high higher hint history hit hold holds host hostname hosts hour hours http https hyphen \
     identical identifier identified identity idle ignore ignored image immediate implicit import important \
     improper inactive include included includes including incomplete inconsistent incompatible incorrect \
     incorrectly increase increment index indexes indexing indicate indicated information informational \
     inherited init initial initialization initialize initialized initializing inner input insert inserted \
     inspect inspecting inspection instance instances insufficient integer integrity interface internal \
     interrupt interrupted interval inventory invalid invoke io ip issue issued item items ",
    "job join journal json key keys kill killed kind kinds known label labelonly lack language large larger \
     largest  latch latency latin layer layout lead leading  left legacy length  level levels \
     library license life lifecycle limit limited limits line lines link linked list listed listen listener \
     load loaded loading local location lock locked locking locks log logged logging logic logical login \
     logins logon logs long longer lookup lost low lower machine main maintenance major manage managed \
     management manager manually  map mapped mark marked marker master match matched matches maximum \
     media member members memory merge message messages metadata method methods microseconds microsoft \
     migration minimum minor minute minutes mirror mismatch mismatched missing mixed mode model modified \
     modify module modules monitor month mount mounted move moved msdb multi multipart multiple ",
    "name named namely names namespace native need needed negative nested network  newer newest  \
     node nodes non none nonnegative normal note notice notification notified nul null number numbers numeric \
     object objects observed observing occur occurred occurrence occurs odbc odd offline offset offsets old \
     older omitted online open opened opening operating operation operations operator option optional \
     options order ordered ordering ordinal ordinary original os output  overflow overlapping \
     overlaps overwrite overwritten owned owner ownership ",
    "pack package packet packets padding page pages parallel parameter parameters parent parse parser \
     parsing part partial partially partition parts pass passed password passwords past path paths payload \
     peak pending percent perform performance performed period permanent permission permissions persist \
     phase physical pipe pipes plain plan platform point pointer policies policy pool port portion position \
     positive possible post practice precision predicate prefix prepare prepared presence present preserve \
     previous previously primary print priority private privilege problem problems procedure procedures \
     process processed processes processing product profile program progress projected property protect \
     protected protection protocol provide provided provider provides public purpose ",
    "query queue quota quoted raise random range rate raw read readable reading readonly ready real reap \
     reaping reason reasons rebuild receive received recent recompile reconnect record records recover \
     recovered recovery recursive redirect redacted reduce redundancy reference references refresh refuse \
     refused refusal region regions register registered registry regard related relation relational \
     relative release released reliable remain remaining remote remove removed rename repair repeat repeats \
     replace replaced replica replicate replication report reports represent request requested requests \
     require required requirement requirements requires reserve reserved reset resolve resolved resource \
     resources respond response responses restart restore restored restoring restrict restricted result \
     results resume retain retained retention retrieve retry return returned returns reuse reverse revert \
     review right rights role roles rollback root row rows rowversion rule rules run running runtime ",
    "safe sample save saved scan schedule scheduler schema scope script scripts search second seconds \
     section sections secure security seek segment select selected selection send sending sent separate \
     separated separation sequence serial serializable server servers service services session sessions set \
     sets setting settings setup  severe severity shape shared shell short shrink shut shutdown side \
     signal signature signed simple single site size sizes skip slot slow small smallint snapshot socket \
     software solution  sort source sources space spaces spare special specific specification specified \
     specify split spool sql sqlcmd sqlserver ssl stack stage staged stages staging standard standby start \
     started starting starts startup state statement statements static statistics status  stop stopped \
     storage store stored stores stream streams strict string strings stripe striped structure structures \
     style subcommand subject success successful successfully suffix suggest supervisor supply support \
     supported supports suspend suspended switch symbol sync synchronous syntax sysadmin system systems ",
    "table tables tail take taken tape target task tasks tcp temp temporary terminate terminated \
     terminating termination terminator test tested text thread threads threshold time timed timeout \
     timeouts times timestamp tinyint tls token tokens tool tools top total trace track trailing transaction \
     transactions transfer transient transport trigger triggers true truncate truncated trust trusted try \
     tuple type types unable unaccounted unavailable uncertain unchanged unclean unconfirmed undefined \
     underlying undo unexpected unicode unique unit units unknown unlimited unlock unnamed unproven \
     unreachable unreadable unrecognized unresolved unsafe unsigned unsupported unused unverified update \
     updated updates upgrade upgraded upgrading usage user username users utc utf uuid ",
    "valid validate validated validation value values variable variables verification verified verify \
     verifying version versions view views violates violation virtual volume volumes wait waiting waits \
     wake warning warnings wide width window windows wire work worker workers workload write writer writes \
     writing written wrong xml year years yellowcode zero zone ",
    "msg see books mb kb gb tb sec ms rtm id ids filename filenames expand principal able matching evaluating \
     handshake handshakes accept limitation actively unneeded autogrowth immediately corrected threatens factors \
     dbcc checkdb share shares assertion assertions len subtract slice panic panicked entered borrowed poisoned unwrap ",
    "sigint sigkill sigterm sighup ",
    "bulk_logged read_only read_write single_user multi_user restricted_user no_encryption"
);

fn set() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| WORDS.split_whitespace().collect())
}

pub(crate) fn contains(word: &str) -> bool {
    word.len() <= 24 && set().contains(word.to_ascii_lowercase().as_str())
}
