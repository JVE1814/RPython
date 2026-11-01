//! sys.settrace / sys.setprofile / sys.monitoring dispatch (stub).
use crate::errors::*;
use crate::ffi::PyObject;
use crate::interp::frame::Frame;
use crate::interp::Exit;
use crate::threads::ThreadState;
pub fn trace_call(_ts: &ThreadState, _f: &mut Frame) -> PyResult<()> {
    Ok(())
}
pub fn trace_instr(_ts: &ThreadState, _f: &mut Frame) -> PyResult<()> {
    Ok(())
}
pub fn trace_exit(_ts: &ThreadState, _f: &mut Frame, _e: &Exit) -> PyResult<()> {
    Ok(())
}
pub fn trace_exception(_ts: &ThreadState, _f: &mut Frame, _e: &PyObject) -> PyResult<()> {
    Ok(())
}
pub fn trace_unwind(_ts: &ThreadState, _f: &mut Frame) -> PyResult<()> {
    Ok(())
}
