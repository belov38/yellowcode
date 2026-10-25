use crate::{BudgetError, LimitReason, MemoryLimit, Reservation};

#[derive(Debug)]
pub struct Charged<T> {
    pub(crate) value: T,
    pub(crate) reservation: Reservation,
}
impl<T> Charged<T> {
    pub fn map<U>(self, convert: impl FnOnce(T) -> U) -> Charged<U> {
        let Self { value, reservation } = self;
        Charged {
            value: convert(value),
            reservation,
        }
    }
    pub fn try_map<U, E>(
        self,
        convert: impl FnOnce(T) -> Result<U, E>,
    ) -> Result<Charged<U>, Charged<E>> {
        let Self { value, reservation } = self;
        match convert(value) {
            Ok(value) => Ok(Charged { value, reservation }),
            Err(value) => Err(Charged { value, reservation }),
        }
    }
    pub fn try_zip<U>(self, other: Charged<U>) -> Result<Charged<(T, U)>, BudgetError> {
        if !self.reservation.same_owner_category(&other.reservation) {
            return Err(BudgetError::Limit(MemoryLimit {
                category: self.reservation.category(),
                requested: 0,
                reason: LimitReason::InvalidSize,
            }));
        }
        let Self {
            value: first,
            mut reservation,
        } = self;
        let Charged {
            value: second,
            reservation: other_reservation,
        } = other;
        reservation.merge_same(other_reservation);
        Ok(Charged {
            value: (first, second),
            reservation,
        })
    }
    pub fn try_into_shared(self) -> Result<crate::Shared<T>, BudgetError> {
        let Self { value, reservation } = self;
        crate::Shared::try_new(value, reservation)
    }
    pub fn reservation(&self) -> &Reservation {
        &self.reservation
    }
}
impl<T> std::ops::Deref for Charged<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}
impl<T> AsRef<T> for Charged<T> {
    fn as_ref(&self) -> &T {
        &self.value
    }
}
