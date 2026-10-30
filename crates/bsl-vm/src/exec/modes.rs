use crate::{CallResult, Execution, HostError};

impl Execution {
    pub fn set_safe_mode(&mut self, enabled: bool) -> CallResult<()> {
        self.change_mode(enabled, true)
    }

    pub fn set_privileged_mode(&mut self, enabled: bool) -> CallResult<()> {
        self.change_mode(enabled, false)
    }

    fn change_mode(&mut self, enabled: bool, safe: bool) -> CallResult<()> {
        let frame = self
            .exec
            .frames
            .last_mut()
            .ok_or_else(|| super::internal("mode setter outside a BSL frame"))?;
        let depth = if safe {
            &mut frame.safe_depth
        } else {
            &mut frame.privileged_depth
        };
        if enabled {
            *depth = depth.saturating_add(1);
        } else {
            *depth = depth.checked_sub(1).ok_or_else(|| {
                HostError::key(if safe {
                    "backend:IDS_ERR_SAFENOTACTIVE"
                } else {
                    "backend:IDS_ERR_PRIVILEGEDNOTACTIVE"
                })
            })?;
        }
        Ok(())
    }
}
