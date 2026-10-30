#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NativeOwner {
    pub role: i32,
    pub site: i32,
}
