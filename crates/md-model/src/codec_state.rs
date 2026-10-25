use std::any::Any;
use std::fmt::Debug;

#[doc(hidden)]
pub trait CodecState: Any + Debug + Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn same(&self, other: &dyn CodecState) -> bool;
}

impl PartialEq for dyn CodecState {
    fn eq(&self, other: &Self) -> bool {
        self.same(other)
    }
}
