use super::{ExecError, ParamValue, ParamValues};
use bsl_prim::fold_name;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

type Charge<'a> = dyn Fn(&str, Option<&ParamValue>, bool) -> Result<(), ExecError> + 'a;

pub struct ParamValuesSnapshot<'a> {
    source: &'a dyn ParamValues,
    values: RefCell<HashMap<String, Option<Arc<ParamValue>>>>,
    charge: Option<&'a Charge<'a>>,
    error: RefCell<Option<ExecError>>,
}

impl<'a> ParamValuesSnapshot<'a> {
    pub fn new(source: &'a dyn ParamValues) -> Self {
        Self {
            source,
            values: RefCell::new(HashMap::new()),
            charge: None,
            error: RefCell::new(None),
        }
    }

    pub fn with_charge(source: &'a dyn ParamValues, charge: &'a Charge<'a>) -> Self {
        Self {
            charge: Some(charge),
            ..Self::new(source)
        }
    }

    pub fn check(&self) -> Result<(), ExecError> {
        match &*self.error.borrow() {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    fn admit(&self, name: &str, value: Option<&ParamValue>, new: bool) -> bool {
        if let Some(charge) = self.charge
            && let Err(error) = charge(name, value, new)
        {
            *self.error.borrow_mut() = Some(error);
            return false;
        }
        true
    }
}

impl ParamValues for ParamValuesSnapshot<'_> {
    fn get(&self, name: &str) -> Option<ParamValue> {
        if self.error.borrow().is_some() {
            return None;
        }
        let key = fold_name(name);
        let cached = self.values.borrow().get(&key).cloned();
        if let Some(value) = cached {
            return self
                .admit(&key, value.as_deref(), false)
                .then(|| value.as_deref().cloned())
                .flatten();
        }
        let value = self.source.get(name);
        if !self.admit(&key, value.as_ref(), true) {
            return None;
        }
        let value = value.map(Arc::new);
        let result = value.as_deref().cloned();
        self.values.borrow_mut().insert(key, value);
        result
    }
}
