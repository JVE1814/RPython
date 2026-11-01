//! Built-in traceback printing (used when the `traceback` module is unavailable,
//! and by the default sys.excepthook before it is replaced). Follows the layout of
//! traceback.TracebackException.format, including chained exceptions, notes and
//! exception groups (without source carets).
use crate::abstract_ as ab;
use crate::ffi::PyObject;
use crate::object::PyRef;
use crate::types::{exceptions as exc, list, none, str as pystr, tuple};

const MAX_GROUP_WIDTH: usize = 15;
const MAX_GROUP_DEPTH: usize = 10;

struct Ctx {
    out: String,
    depth: usize,
    need_close: bool,
    seen: Vec<usize>,
}

impl Ctx {
    fn indent(&self) -> String {
        " ".repeat(2 * self.depth)
    }
    fn emit(&mut self, text: &str, margin: Option<char>) {
        let mut ind = self.indent();
        if self.depth > 0 {
            ind.push(margin.unwrap_or('|'));
            ind.push(' ');
        }
        for line in text.split_inclusive('\n') {
            self.out.push_str(&ind);
            self.out.push_str(line);
        }
    }
}

fn format_stack(e: &PyObject) -> String {
    let mut out = String::new();
    let mut cur: Option<PyRef> = exc::exc_traceback(e);
    let mut visited: Vec<usize> = Vec::new();
    while let Some(t) = cur {
        if visited.contains(&(t.as_ptr() as usize)) {
            break;
        }
        visited.push(t.as_ptr() as usize);
        let tbo = unsafe { &*(t.as_ptr() as *const crate::types::traceback::PyTracebackObject) };
        let fo = unsafe { &*tbo.tb_frame };
        if let Some(f) = crate::types::frameobject::frame_of(fo) {
            let cd = crate::types::code::code_data(f.code());
            let fname = pystr::to_string_lossy(&cd.filename);
            let name = pystr::to_string_lossy(&cd.co_name);
            out.push_str(&format!("  File \"{}\", line {}, in {}\n", fname, tbo.tb_lineno, name));
            if let Some(line) = crate::modules::sys_full::source_line(&fname, tbo.tb_lineno) {
                let l = line.trim();
                if !l.is_empty() {
                    out.push_str(&format!("    {}\n", l));
                }
            }
        }
        cur = if tbo.tb_next.is_null() { None } else { Some(unsafe { PyRef::from_borrowed(tbo.tb_next) }) };
    }
    out
}

fn safe_str(o: &PyObject) -> String {
    match ab::str(o) {
        Ok(s) => pystr::to_string_lossy(&s),
        Err(_) => {
            crate::errors::clear_exc();
            "<exception str() failed>".into()
        }
    }
}

fn format_exception_only(e: &PyObject) -> String {
    let mut out = String::new();
    let tname = crate::types::typeobject::type_fqname(e.type_ptr());
    if crate::types::typeobject::is_subtype(e.type_ptr(), exc::SyntaxError()) {
        let se = unsafe { &*(e.as_ptr() as *const exc::SyntaxErrorExc) };
        let f = if se.filename.is_null() { "<unknown>".to_string() } else { ab::str_string(unsafe { &*se.filename }).unwrap_or_default() };
        let l = if se.lineno.is_null() { String::new() } else { ab::str_string(unsafe { &*se.lineno }).unwrap_or_default() };
        out.push_str(&format!("  File \"{}\", line {}\n", f, l));
        if !se.text.is_null() && !unsafe { &*se.text }.is(none::none()) {
            out.push_str(&format!("    {}\n", pystr::to_string_lossy(unsafe { &*se.text }).trim()));
        }
        let m = if se.msg.is_null() { String::new() } else { ab::str_string(unsafe { &*se.msg }).unwrap_or_default() };
        out.push_str(&format!("{}: {}\n", tname, m));
    } else {
        let msg = safe_str(e);
        if msg.is_empty() {
            out.push_str(&format!("{}\n", tname));
        } else {
            out.push_str(&format!("{}: {}\n", tname, msg));
        }
    }
    // __notes__
    match ab::getattr_opt(e, crate::name!("__notes__")) {
        Ok(Some(notes)) => {
            if list::is_list(&notes) || tuple::is_tuple(&notes) {
                let items: Vec<PyRef> =
                    if list::is_list(&notes) { list::list_to_vec(&notes) } else { tuple::tuple_to_vec(&notes) };
                for n in items {
                    let s = if pystr::is_str(&n) { pystr::to_string_lossy(&n) } else { safe_str(&n) };
                    out.push_str(&s);
                    out.push('\n');
                }
            } else {
                let r = ab::repr_string(&notes).unwrap_or_else(|_| {
                    crate::errors::clear_exc();
                    "<__notes__ repr() failed>".into()
                });
                out.push_str(&r);
                out.push('\n');
            }
        }
        Ok(None) => {}
        Err(_) => crate::errors::clear_exc(),
    }
    out
}

fn format_rec(e: &PyObject, ctx: &mut Ctx) {
    // collect the chain (most recent last)
    let mut chain: Vec<(Option<&'static str>, PyRef)> = Vec::new();
    let mut cur = Some(e.to_ref());
    while let Some(c) = cur {
        if ctx.seen.contains(&(c.as_ptr() as usize)) {
            break;
        }
        ctx.seen.push(c.as_ptr() as usize);
        let be = exc::exc(&c);
        let (msg, next) = if !be.cause.is_null() {
            (Some("\nThe above exception was the direct cause of the following exception:\n\n"), Some(unsafe { PyRef::from_borrowed(be.cause) }))
        } else if !be.context.is_null() && be.suppress_context == 0 {
            (Some("\nDuring handling of the above exception, another exception occurred:\n\n"), Some(unsafe { PyRef::from_borrowed(be.context) }))
        } else {
            (None, None)
        };
        let next = next.filter(|n| !ctx.seen.contains(&(n.as_ptr() as usize)));
        chain.push((if next.is_some() { msg } else { None }, c));
        cur = next;
    }
    for (msg, x) in chain.into_iter().rev() {
        if let Some(m) = msg {
            ctx.emit(m, None);
        }
        format_one(&x, ctx);
    }
}

fn format_one(x: &PyObject, ctx: &mut Ctx) {
    let is_group = crate::types::exceptiongroup::is_exception_group(x);
    let stack = format_stack(x);
    if !is_group {
        if !stack.is_empty() {
            ctx.emit("Traceback (most recent call last):\n", None);
            ctx.emit(&stack, None);
        }
        let only = format_exception_only(x);
        ctx.emit(&only, None);
        return;
    }
    if ctx.depth > MAX_GROUP_DEPTH {
        ctx.emit(&format!("... (max_group_depth is {})\n", MAX_GROUP_DEPTH), None);
        return;
    }
    let toplevel = ctx.depth == 0;
    if toplevel {
        ctx.depth += 1;
    }
    if !stack.is_empty() {
        ctx.emit("Exception Group Traceback (most recent call last):\n", if toplevel { Some('+') } else { None });
        ctx.emit(&stack, None);
    }
    let only = format_exception_only(x);
    ctx.emit(&only, None);
    let excs = crate::types::exceptiongroup::eg_exceptions(x);
    let items = tuple::tuple_items(&excs);
    let num = items.len();
    let n = if num <= MAX_GROUP_WIDTH { num } else { MAX_GROUP_WIDTH + 1 };
    ctx.need_close = false;
    for i in 0..n {
        let last = i == n - 1;
        if last {
            ctx.need_close = true;
        }
        let truncated = i >= MAX_GROUP_WIDTH;
        let title = if truncated { "...".to_string() } else { format!("{}", i + 1) };
        let line = format!("{}{}+---------------- {} ----------------\n", ctx.indent(), if i == 0 { "+-" } else { "  " }, title);
        ctx.out.push_str(&line);
        ctx.depth += 1;
        if !truncated {
            format_rec(items[i], ctx);
        } else {
            let remaining = num - MAX_GROUP_WIDTH;
            ctx.emit(&format!("and {} more exception{}\n", remaining, if remaining > 1 { "s" } else { "" }), None);
        }
        if last && ctx.need_close {
            let line = format!("{}+------------------------------------\n", ctx.indent());
            ctx.out.push_str(&line);
            ctx.need_close = false;
        }
        ctx.depth -= 1;
    }
    if toplevel {
        ctx.depth = 0;
    }
}

pub fn format_exception(e: &PyObject) -> String {
    let mut ctx = Ctx { out: String::new(), depth: 0, need_close: false, seen: Vec::new() };
    format_rec(e, &mut ctx);
    ctx.out
}

pub fn print_exception(e: &PyObject) {
    // Like CPython, prefer `traceback._print_exception_bltin` (carets,
    // suggestions, colours); fall back to the built-in printer when the
    // traceback module is unavailable or fails.
    if print_with_traceback_module(e) {
        return;
    }
    let s = format_exception(e);
    crate::modules::sys::write_stderr(&s);
}

thread_local! {
    static IN_PY_PRINT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn print_with_traceback_module(e: &PyObject) -> bool {
    if IN_PY_PRINT.with(|c| c.replace(true)) {
        return false;
    }
    let r = print_with_traceback_module_inner(e);
    IN_PY_PRINT.with(|c| c.set(false));
    r
}

fn print_with_traceback_module_inner(e: &PyObject) -> bool {
    let saved = crate::errors::take_exc();
    let ok = (|| -> crate::errors::PyResult<bool> {
        let tb = match crate::import::import_absolute("traceback") {
            Ok(m) => m,
            Err(_) => {
                crate::errors::clear_exc();
                return Ok(false);
            }
        };
        let f = match ab::getattr_opt(&tb, &pystr::new_str("_print_exception_bltin"))? {
            Some(f) if !f.is(none::none()) => f,
            _ => return Ok(false),
        };
        ab::call(&f, &[e])?;
        Ok(true)
    })();
    let r = match ok {
        Ok(b) => b,
        Err(_) => {
            if let Some(err) = crate::errors::take_exc() {
                crate::errors::set_exc(err);
                crate::modules::sys::write_unraisable_with_obj("Exception ignored in the internal traceback machinery", None);
            }
            crate::errors::clear_exc();
            false
        }
    };
    if let Some(s) = saved {
        crate::errors::set_exc(s);
    }
    r
}
