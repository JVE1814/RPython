//! _thread support hooks (stub).
use crate::errors::*;
use crate::threads::ThreadState;
pub fn raise_async_exc(ts: &ThreadState) -> PyResult<()> {
    let _ = ts;
    Ok(())
}
