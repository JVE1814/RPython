//! complex power (used for negative float ** fractional).
use crate::errors::*;
pub fn complex_pow(a: (f64, f64), b: (f64, f64)) -> PyResult<(f64, f64)> {
    if b.0 == 0.0 && b.1 == 0.0 {
        return Ok((1.0, 0.0));
    }
    if a.0 == 0.0 && a.1 == 0.0 {
        if b.1 != 0.0 || b.0 < 0.0 {
            crate::raise!(ZeroDivisionError, "zero to a negative or complex power");
        }
        return Ok((0.0, 0.0));
    }
    let vabs = a.0.hypot(a.1);
    let mut len = vabs.powf(b.0);
    let at = a.1.atan2(a.0);
    let mut phase = at * b.0;
    if b.1 != 0.0 {
        len /= (at * b.1).exp();
        phase += b.1 * vabs.ln();
    }
    Ok((len * phase.cos(), len * phase.sin()))
}
