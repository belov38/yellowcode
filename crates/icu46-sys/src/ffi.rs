use std::ffi::{c_char, c_void};

pub type UErrorCode = i32;

pub const U_ZERO_ERROR: UErrorCode = 0;
pub const U_ILLEGAL_ARGUMENT_ERROR: UErrorCode = 1;
pub const U_MEMORY_ALLOCATION_ERROR: UErrorCode = 7;
#[cfg(feature = "managed")]
pub const U_INVALID_STATE_ERROR: UErrorCode = 27;
#[cfg(all(feature = "managed", feature = "regex"))]
pub const U_REGEX_STOPPED_BY_CALLER: UErrorCode = 0x10313;

pub const UDATA_NO_FILES: i32 = 3;

#[repr(C)]
pub struct UCollator {
    _private: [u8; 0],
}

unsafe extern "C" {
    pub fn ucol_open_46(loc: *const c_char, status: *mut UErrorCode) -> *mut UCollator;
    pub fn ucol_close_46(coll: *mut UCollator);
    pub fn ucol_setStrength_46(coll: *mut UCollator, strength: i32);
    pub fn ucol_getSortKey_46(
        coll: *const UCollator,
        source: *const u16,
        source_length: i32,
        result: *mut u8,
        result_length: i32,
    ) -> i32;
    pub fn udata_setCommonData_46(data: *const c_void, status: *mut UErrorCode);
    pub fn udata_setFileAccess_46(access: i32, status: *mut UErrorCode);
    pub fn u_setDataDirectory_46(directory: *const c_char);
    pub fn uloc_setDefault_46(locale_id: *const c_char, status: *mut UErrorCode);
    pub fn u_errorName_46(code: UErrorCode) -> *const c_char;
    pub fn u_getVersion_46(version: *mut u8);
}

#[cfg(feature = "regex")]
#[repr(C)]
pub struct URegularExpression {
    _private: [u8; 0],
}

#[cfg(feature = "regex")]
unsafe extern "C" {
    pub fn uregex_open_46(
        pattern: *const u16,
        len: i32,
        flags: u32,
        parse_error: *mut c_void,
        status: *mut UErrorCode,
    ) -> *mut URegularExpression;
    pub fn uregex_close_46(regex: *mut URegularExpression);
    pub fn uregex_clone_46(
        regex: *const URegularExpression,
        status: *mut UErrorCode,
    ) -> *mut URegularExpression;
    pub fn uregex_setText_46(
        regex: *mut URegularExpression,
        text: *const u16,
        len: i32,
        status: *mut UErrorCode,
    );
    pub fn uregex_find_46(
        regex: *mut URegularExpression,
        start: i32,
        status: *mut UErrorCode,
    ) -> i8;
    pub fn uregex_findNext_46(regex: *mut URegularExpression, status: *mut UErrorCode) -> i8;
    pub fn uregex_lookingAt_46(
        regex: *mut URegularExpression,
        start: i32,
        status: *mut UErrorCode,
    ) -> i8;
    pub fn uregex_matches_46(
        regex: *mut URegularExpression,
        start: i32,
        status: *mut UErrorCode,
    ) -> i8;
    pub fn uregex_start_46(
        regex: *mut URegularExpression,
        group: i32,
        status: *mut UErrorCode,
    ) -> i32;
    pub fn uregex_end_46(
        regex: *mut URegularExpression,
        group: i32,
        status: *mut UErrorCode,
    ) -> i32;
    pub fn uregex_groupCount_46(regex: *mut URegularExpression, status: *mut UErrorCode) -> i32;
    pub fn uregex_replaceAll_46(
        regex: *mut URegularExpression,
        replacement: *const u16,
        len: i32,
        dest: *mut u16,
        capacity: i32,
        status: *mut UErrorCode,
    ) -> i32;
    pub fn uregex_setTimeLimit_46(
        regex: *mut URegularExpression,
        limit: i32,
        status: *mut UErrorCode,
    );
    pub fn uregex_setStackLimit_46(
        regex: *mut URegularExpression,
        limit: i32,
        status: *mut UErrorCode,
    );
}
