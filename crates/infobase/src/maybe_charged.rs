use memory_budget::{Charged, Reservation};

#[derive(Debug)]
pub enum MaybeCharged<T> {
    Legacy(T),
    Charged(Charged<T>),
}

impl<T> MaybeCharged<T> {
    pub fn reservation(&self) -> Option<&Reservation> {
        match self {
            Self::Legacy(_) => None,
            Self::Charged(c) => Some(c.reservation()),
        }
    }

    pub fn is_charged(&self) -> bool {
        matches!(self, Self::Charged(_))
    }
}

impl<T> std::ops::Deref for MaybeCharged<T> {
    type Target = T;
    fn deref(&self) -> &T {
        match self {
            Self::Legacy(v) => v,
            Self::Charged(c) => c,
        }
    }
}

impl<T> AsRef<T> for MaybeCharged<T> {
    fn as_ref(&self) -> &T {
        self
    }
}

impl<T> From<T> for MaybeCharged<T> {
    fn from(value: T) -> Self {
        Self::Legacy(value)
    }
}

impl<T> From<Charged<T>> for MaybeCharged<T> {
    fn from(value: Charged<T>) -> Self {
        Self::Charged(value)
    }
}
