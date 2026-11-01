//! Signal handling hooks (stub).
use crate::errors::*;
use crate::threads::ThreadState;
pub fn run_pending_handlers(ts: &ThreadState) -> PyResult<()> {
    let _ = ts;
    Ok(())
}
