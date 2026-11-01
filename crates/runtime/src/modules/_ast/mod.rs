//! The `_ast` module: `ast.AST` and one heap type per ASDL class (built from
//! `rpy_parser::ast::node_classes()`), with CPython 3.15 constructor semantics
//! (defaults for optional/list fields, `_field_types`, `__match_args__`, errors for missing
//! required fields and unknown keywords), `__reduce__`, `__replace__` and the depth-limited
//! `__repr__`.  The classes are process-global: compile() can create and accept AST objects
//! without importing `_ast` (like CPython's interpreter-level AST state).
//!
//! Conversion between Python AST objects and the Rust AST lives in `to_py` (ast2obj) and
//! `from_py` (obj2ast).

pub mod from_py;
pub mod to_py;

use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::modules::fncommon as fc;
use crate::object::*;
use crate::types::typeobject::{self as tobj, type_ref};
use crate::types::{dict, int, list, none, str as pystr, tuple};
use crate::{mdefs, name, raise};
use rpy_parser::ast::{FieldKind, node_classes};
use std::ffi::{c_int, c_void};
use std::sync::{Mutex, OnceLock};

macro_rules! node_kinds {
    ($($v:ident = $s:literal),* $(,)?) => {
        /// Every ASDL class, in `node_classes()` order.
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        #[repr(u8)]
        #[allow(non_camel_case_types)]
        pub enum K { $($v),* }
        pub const KIND_NAMES: &[&str] = &[$($s),*];
    };
}

node_kinds!(
    Mod = "mod", Module = "Module", Interactive = "Interactive", Expression = "Expression", FunctionType = "FunctionType",
    StmtBase = "stmt", FunctionDef = "FunctionDef", AsyncFunctionDef = "AsyncFunctionDef", ClassDef = "ClassDef",
    Return = "Return", Delete = "Delete", Assign = "Assign", TypeAlias = "TypeAlias", AugAssign = "AugAssign",
    AnnAssign = "AnnAssign", For = "For", AsyncFor = "AsyncFor", While = "While", If = "If", With = "With",
    AsyncWith = "AsyncWith", Match = "Match", Raise = "Raise", Try = "Try", TryStar = "TryStar", Assert = "Assert",
    Import = "Import", ImportFrom = "ImportFrom", Global = "Global", Nonlocal = "Nonlocal", ExprStmt = "Expr",
    Pass = "Pass", Break = "Break", Continue = "Continue",
    ExprBase = "expr", BoolOp = "BoolOp", NamedExpr = "NamedExpr", BinOp = "BinOp", UnaryOp = "UnaryOp",
    Lambda = "Lambda", IfExp = "IfExp", Dict = "Dict", Set = "Set", ListComp = "ListComp", SetComp = "SetComp",
    DictComp = "DictComp", GeneratorExp = "GeneratorExp", Await = "Await", Yield = "Yield", YieldFrom = "YieldFrom",
    Compare = "Compare", Call = "Call", FormattedValue = "FormattedValue", Interpolation = "Interpolation",
    JoinedStr = "JoinedStr", TemplateStr = "TemplateStr", Constant = "Constant", Attribute = "Attribute",
    Subscript = "Subscript", Starred = "Starred", Name = "Name", List = "List", Tuple = "Tuple", Slice = "Slice",
    ExprContext = "expr_context", Load = "Load", Store = "Store", Del = "Del",
    BoolOpBase = "boolop", And = "And", Or = "Or",
    OperatorBase = "operator", Add = "Add", Sub = "Sub", Mult = "Mult", MatMult = "MatMult", Div = "Div",
    ModOp = "Mod", Pow = "Pow", LShift = "LShift", RShift = "RShift", BitOr = "BitOr", BitXor = "BitXor",
    BitAnd = "BitAnd", FloorDiv = "FloorDiv",
    UnaryOpBase = "unaryop", Invert = "Invert", Not = "Not", UAdd = "UAdd", USub = "USub",
    CmpOpBase = "cmpop", Eq = "Eq", NotEq = "NotEq", Lt = "Lt", LtE = "LtE", Gt = "Gt", GtE = "GtE", Is = "Is",
    IsNot = "IsNot", In = "In", NotIn = "NotIn",
    Comprehension = "comprehension",
    ExceptHandlerBase = "excepthandler", ExceptHandler = "ExceptHandler",
    Arguments = "arguments", Arg = "arg", Keyword = "keyword", Alias = "alias", WithItem = "withitem",
    MatchCase = "match_case",
    PatternBase = "pattern", MatchValue = "MatchValue", MatchSingleton = "MatchSingleton",
    MatchSequence = "MatchSequence", MatchMapping = "MatchMapping", MatchClass = "MatchClass",
    MatchStar = "MatchStar", MatchAs = "MatchAs", MatchOr = "MatchOr",
    TypeIgnoreBase = "type_ignore", TypeIgnore = "TypeIgnore",
    TypeParamBase = "type_param", TypeVar = "TypeVar", ParamSpec = "ParamSpec", TypeVarTuple = "TypeVarTuple",
);

#[repr(C)]
pub struct AstObject {
    ob_base: PyObject,
    dict: *mut PyObject,
}

pub static mut AST_TYPE: PyTypeObject = PyTypeObject::ZERO;

pub fn ast_type() -> *mut PyTypeObject {
    &raw mut AST_TYPE
}

pub struct State {
    /// Class objects indexed by `K as usize`.
    classes: Vec<PyRef>,
    abstract_: Vec<bool>,
    /// Shared instances of fieldless classes (Load, Add, ...), created on first use.
    singletons: Vec<OnceLock<PyRef>>,
    /// Exact type pointer -> kind (for the fast path of isinstance checks).
    by_ptr: std::collections::HashMap<usize, K>,
}

static STATE: OnceLock<State> = OnceLock::new();
static INIT: Mutex<()> = Mutex::new(());

impl State {
    #[inline]
    pub fn cls(&self, k: K) -> &PyObject {
        &self.classes[k as usize]
    }
    #[inline]
    pub fn tp(&self, k: K) -> *mut PyTypeObject {
        self.classes[k as usize].as_ptr() as *mut PyTypeObject
    }
    pub fn kind_of_exact(&self, o: &PyObject) -> Option<K> {
        self.by_ptr.get(&(o.type_ptr() as usize)).copied()
    }
    /// `isinstance(o, cls(k))`.
    pub fn isinstance(&self, o: &PyObject, k: K) -> PyResult<bool> {
        let tp = self.tp(k);
        if o.type_ptr() == tp {
            return Ok(true);
        }
        if tobj::is_subtype(o.type_ptr(), tp) {
            return Ok(true);
        }
        // honour __instancecheck__ / __class__ overrides like PyObject_IsInstance
        if o.type_ptr() == ast_type() || self.by_ptr.contains_key(&(o.type_ptr() as usize)) {
            return Ok(false);
        }
        ab::isinstance(o, self.cls(k))
    }
    /// Shared instance of a fieldless class.
    pub fn singleton(&self, k: K) -> PyResult<PyRef> {
        let cell = &self.singletons[k as usize];
        if let Some(v) = cell.get() {
            return Ok(v.clone());
        }
        let v = new_node(self.tp(k))?;
        let _ = cell.set(v);
        Ok(cell.get().unwrap().clone())
    }
}

/// The global AST state (classes are created on first use).
pub fn state() -> PyResult<&'static State> {
    if let Some(s) = STATE.get() {
        return Ok(s);
    }
    let _g = INIT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = STATE.get() {
        return Ok(s);
    }
    let s = build_state()?;
    let _ = STATE.set(s);
    Ok(STATE.get().unwrap())
}

pub fn state_if_ready() -> Option<&'static State> {
    STATE.get()
}

/// Is `o` an instance of `ast.AST` (no import, no class creation)?
pub fn is_ast_node(o: &PyObject) -> bool {
    unsafe {
        if AST_TYPE.tp_flags & Py_TPFLAGS_READY == 0 {
            return false;
        }
    }
    tobj::is_subtype(o.type_ptr(), ast_type())
}

/// Allocate a node of class `tp` with an empty instance dict (no __init__).
pub fn new_node(tp: *mut PyTypeObject) -> PyResult {
    unsafe {
        let a = (*tp).tp_alloc.unwrap_or(tobj::generic_alloc);
        let op = a(tp, 0);
        if op.is_null() {
            return Err(PyErr);
        }
        let r = PyRef::from_raw(op);
        Ok(r)
    }
}

fn node_dict(o: &PyObject) -> PyRef {
    match tobj::instance_dict(o) {
        Some(d) => d,
        None => dict::new_dict(),
    }
}

// ----------------------------------------------------------------------------------------
// ast.AST type
// ----------------------------------------------------------------------------------------

unsafe extern "C" fn ast_new(tp: *mut PyTypeObject, _a: *mut PyObject, _k: *mut PyObject) -> *mut PyObject {
    into_c(new_node(tp))
}

unsafe extern "C" fn ast_dealloc(o: *mut PyObject) {
    unsafe {
        crate::gc::untrack(o);
        fc::clear_field(&raw mut (*(o as *mut AstObject)).dict);
        fc::free(o);
    }
}
unsafe extern "C" fn ast_traverse(o: *mut PyObject, v: visitproc, arg: *mut c_void) -> c_int {
    unsafe { fc::visit((*(o as *mut AstObject)).dict, v, arg) }
}
unsafe extern "C" fn ast_clear(o: *mut PyObject) -> c_int {
    unsafe { fc::clear_field(&raw mut (*(o as *mut AstObject)).dict) };
    0
}

fn type_name_c(tp: *mut PyTypeObject) -> String {
    unsafe { std::ffi::CStr::from_ptr((*tp).tp_name).to_string_lossy().into_owned() }
}

/// Is this class (exactly) `ast.AST` or one of the abstract ASDL sum-type bases?
fn is_abstract_type(tp: *mut PyTypeObject) -> bool {
    if tp == ast_type() {
        return false;
    }
    match state_if_ready() {
        Some(s) => s.by_ptr.get(&(tp as usize)).is_some_and(|k| s.abstract_[*k as usize]),
        None => false,
    }
}

fn format_missing(names: &[String]) -> String {
    let q: Vec<String> = names.iter().map(|n| format!("'{}'", n)).collect();
    match q.len() {
        1 => q[0].clone(),
        2 => format!("{} and {}", q[0], q[1]),
        _ => format!("{}, and {}", q[..q.len() - 1].join(", "), q[q.len() - 1]),
    }
}

fn seq_items(o: &PyObject) -> PyResult<Vec<PyRef>> {
    if tuple::is_tuple(o) {
        return Ok(tuple::tuple_to_vec(o));
    }
    if list::is_list(o) {
        return Ok(list::list_to_vec(o));
    }
    let n = ab::length(o)?;
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        v.push(ab::getitem(o, &int::new_int(i as i64))?);
    }
    Ok(v)
}

fn contains_eq(items: &[PyRef], key: &PyObject) -> PyResult<bool> {
    for it in items {
        if it.is(key) || ab::rich_compare_bool(it, key, Py_EQ)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Default value for a missing field from its `_field_types` entry: Some(value) if it has a
/// default (None for `X | None`, [] for `list[X]`, Load() for `expr_context`).
fn field_default(ftype: &PyObject) -> PyResult<Option<PyRef>> {
    if crate::types::union::is_union(ftype) {
        let args = crate::types::union::union_args(ftype);
        let nonetype = crate::types::typeobject::type_obj(none::none().type_ptr());
        if tuple::tuple_items(&args).iter().any(|a| a.is(nonetype) || a.is(none::none())) {
            return Ok(Some(none::none_ref()));
        }
        return Ok(None);
    }
    if crate::types::genericalias::is_generic_alias(ftype) {
        let origin = crate::types::genericalias::ga_origin(ftype);
        if origin.is(crate::types::typeobject::type_obj(list::list_type())) {
            return Ok(Some(list::new_list_empty()));
        }
        return Ok(None);
    }
    if let Some(s) = state_if_ready() {
        if ftype.is(s.cls(K::ExprContext)) {
            return Ok(Some(ab::call0(s.cls(K::Load))?));
        }
    }
    Ok(None)
}

fn ast_init_impl(o: &PyObject, args: &[&PyObject], kw: Option<&PyObject>) -> PyResult<()> {
    let tp = o.type_ptr();
    let tpo = tobj::type_obj(tp);
    if tp == ast_type() || is_abstract_type(tp) {
        crate::modules::warnings::warn(
            crate::types::exceptions::DeprecationWarning(),
            &format!(
                "Instantiating abstract AST node class {} is deprecated. This will become an error in Python 3.20",
                tobj::type_name(unsafe { &*tp })
            ),
            1,
        )?;
    }
    let fields_obj = ab::getattr(tpo, name!("_fields"))?;
    let fields = seq_items(&fields_obj)?;
    let tpname = type_name_c(tp);
    if args.len() > fields.len() {
        raise!(
            TypeError,
            "{} constructor takes at most {} positional argument{}",
            tpname,
            fields.len(),
            if fields.len() == 1 { "" } else { "s" }
        );
    }
    for (i, a) in args.iter().enumerate() {
        ab::setattr(o, &fields[i], a)?;
    }
    let mut remaining: Vec<PyRef> = fields[args.len()..].to_vec();
    if let Some(kw) = kw {
        if dict::dict_len(kw) > 0 {
            let attributes = match ab::getattr_opt(tpo, name!("_attributes"))? {
                Some(a) => seq_items(&a)?,
                None => Vec::new(),
            };
            for (key, value) in dict::items_vec(kw) {
                if contains_eq(&fields[..args.len()], &key)? {
                    raise!(TypeError, "{} got multiple values for argument {}", tpname, ab::repr_string(&key)?);
                }
                let mut pos = None;
                for (i, f) in remaining.iter().enumerate() {
                    if f.is(&key) || ab::rich_compare_bool(f, &key, Py_EQ)? {
                        pos = Some(i);
                        break;
                    }
                }
                if let Some(i) = pos {
                    remaining.remove(i);
                } else if !contains_eq(&attributes, &key)? {
                    raise!(TypeError, "{}.__init__ got an unexpected keyword argument {}", tpname, ab::repr_string(&key)?);
                }
                ab::setattr(o, &key, &value)?;
            }
        }
    }
    if remaining.is_empty() {
        return Ok(());
    }
    let field_types = match ab::getattr_opt(tpo, name!("_field_types"))? {
        Some(ft) => ft,
        None => return Ok(()),
    };
    let mut missing: Vec<String> = Vec::new();
    for name in &remaining {
        let ftype = match ab::getitem(&field_types, name) {
            Ok(t) => t,
            Err(e) => {
                if exc_matches(crate::types::exceptions::KeyError()) {
                    clear_exc();
                    raise!(
                        TypeError,
                        "Field {} is missing from {}._field_types. This will become an error in Python 3.15.",
                        ab::repr_string(name)?,
                        tobj::type_fqname(tp)
                    );
                }
                return Err(e);
            }
        };
        match field_default(&ftype)? {
            Some(v) => ab::setattr(o, name, &v)?,
            None => missing.push(if pystr::is_str(name) { pystr::to_string_lossy(name) } else { ab::repr_string(name)? }),
        }
    }
    if !missing.is_empty() {
        raise!(
            TypeError,
            "{}.__init__ missing {} required positional argument{}: {}",
            tpname,
            missing.len(),
            if missing.len() == 1 { "" } else { "s" },
            format_missing(&missing)
        );
    }
    Ok(())
}

unsafe extern "C" fn ast_init(o: *mut PyObject, args: *mut PyObject, kw: *mut PyObject) -> c_int {
    unsafe {
        let a = fc::targs(args);
        int_into_c(ast_init_impl(&*o, a, fc::opt(kw)))
    }
}

fn ast_reduce(o: &PyObject) -> PyResult {
    let tp = type_ref(o.type_ptr());
    let d = match ab::getattr_opt(o, name!("__dict__"))? {
        Some(d) => d,
        None => return Ok(tuple::new_tuple(vec![tp, tuple::empty_tuple()])),
    };
    let fields = match ab::getattr_opt(&tp, name!("_fields"))? {
        Some(f) => f,
        None => return Ok(tuple::new_tuple(vec![tp, tuple::empty_tuple(), d])),
    };
    let mut pos = Vec::new();
    for f in seq_items(&fields)? {
        if !ab::contains(&d, &f)? {
            break;
        }
        pos.push(none::none_ref());
    }
    Ok(tuple::new_tuple(vec![tp, tuple::new_tuple(pos), d]))
}

fn ast_replace(o: &PyObject, args: &[&PyObject], kwnames: Option<&PyObject>) -> PyResult {
    let nkw = kwnames.map_or(0, tuple::tuple_len);
    if args.len() > nkw {
        raise!(TypeError, "__replace__() takes no positional arguments");
    }
    let tp = type_ref(o.type_ptr());
    let payload = dict::new_dict();
    for which in [name!("_fields"), name!("_attributes")] {
        if let Some(names) = ab::getattr_opt(&tp, which)? {
            for n in seq_items(&names)? {
                if let Some(v) = ab::getattr_opt(o, &n)? {
                    dict::set_item(&payload, &n, &v)?;
                }
            }
        }
    }
    if let Some(k) = kwnames {
        for (i, nm) in tuple::tuple_items(k).iter().enumerate() {
            dict::set_item(&payload, nm, args[args.len() - nkw + i])?;
        }
    }
    ab::call_with_kwargs_dict(&tp, &[], Some(&payload))
}

const REPR_DEPTH: i32 = 3;

fn repr_value(v: &PyObject, depth: i32) -> PyResult<String> {
    if is_ast_node(v) {
        return repr_node(v, depth);
    }
    if list::is_list(v) {
        let items = list::list_to_vec(v);
        if items.is_empty() {
            return Ok("[]".into());
        }
        let mut parts = Vec::new();
        let n = items.len();
        for (i, it) in items.iter().enumerate() {
            if n > 2 && i > 0 && i < n - 1 {
                if i == 1 {
                    parts.push("...".to_string());
                }
                continue;
            }
            parts.push(repr_value(it, depth)?);
        }
        return Ok(format!("[{}]", parts.join(", ")));
    }
    ab::repr_string(v)
}

fn repr_node(o: &PyObject, depth: i32) -> PyResult<String> {
    let name = tobj::type_name(o.ty());
    if depth <= 0 {
        return Ok(format!("{}(...)", name));
    }
    let _g = match fc::ReprGuard::enter(o) {
        Some(g) => g,
        None => return Ok(format!("{}(...)", name)),
    };
    let fields = match ab::getattr_opt(tobj::type_obj(o.type_ptr()), name!("_fields"))? {
        Some(f) => seq_items(&f)?,
        None => Vec::new(),
    };
    let mut parts = Vec::new();
    for f in &fields {
        let v = match ab::getattr_opt(o, f)? {
            Some(v) => v,
            None => continue,
        };
        let fname = if pystr::is_str(f) { pystr::to_string_lossy(f) } else { ab::repr_string(f)? };
        parts.push(format!("{}={}", fname, repr_value(&v, depth - 1)?));
    }
    Ok(format!("{}({})", name, parts.join(", ")))
}

unsafe extern "C" fn ast_repr(o: *mut PyObject) -> *mut PyObject {
    unsafe { into_c(repr_node(&*o, REPR_DEPTH).map(|s| pystr::new_str(&s))) }
}

fn init_ast_type() -> PyResult<()> {
    unsafe {
        let t = &mut *ast_type();
        if t.tp_flags & Py_TPFLAGS_READY != 0 {
            return Ok(());
        }
        t.tp_name = c"ast.AST".as_ptr();
        t.tp_basicsize = std::mem::size_of::<AstObject>() as isize;
        t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC;
        t.tp_getattro = Some(tobj::object_generic_getattro);
        t.tp_setattro = Some(tobj::object_generic_setattro);
        t.tp_alloc = Some(tobj::generic_alloc);
        t.tp_free = Some(tobj::generic_free);
        t.tp_new = Some(ast_new);
        t.tp_init = Some(ast_init);
        t.tp_repr = Some(ast_repr);
        t.tp_dealloc = Some(ast_dealloc);
        t.tp_traverse = Some(ast_traverse);
        t.tp_clear = Some(ast_clear);
        t.tp_dictoffset = std::mem::offset_of!(AstObject, dict) as isize;
        t.tp_methods = mdefs![
            ("__reduce__", NOARGS, ast_reduce, "Helper for pickle."),
            ("__replace__", FASTKW, ast_replace, "__replace__($self, /, **fields)\n--\n\nReturn a copy of the AST node with new values for the specified fields."),
        ];
        t.tp_getset = crate::getset_table![("__dict__", fc::get_dict_attr, fc::set_dict_attr)];
        tobj::type_ready(ast_type())?;
        let d = tobj::type_dict(ast_type());
        dict::set_item(d, name!("_fields"), &tuple::empty_tuple())?;
        dict::set_item(d, name!("_attributes"), &tuple::empty_tuple())?;
        dict::set_item(d, name!("__match_args__"), &tuple::empty_tuple())?;
        dict::set_item(d, name!("__module__"), &pystr::intern_str("ast"))?;
    }
    Ok(())
}

// ----------------------------------------------------------------------------------------
// Node classes
// ----------------------------------------------------------------------------------------

fn asdl_doc(doc: &str) -> String {
    doc.replace("?*", "*")
}

fn build_state() -> PyResult<State> {
    init_ast_type()?;
    let ncs = node_classes();
    assert_eq!(ncs.len(), KIND_NAMES.len(), "_ast: class table mismatch");
    let mut classes: Vec<PyRef> = Vec::with_capacity(ncs.len());
    let mut abstract_ = Vec::with_capacity(ncs.len());
    let mut by_name: std::collections::HashMap<&str, PyRef> = std::collections::HashMap::new();
    let ast_cls = type_ref(ast_type());
    let module_name = pystr::intern_str("ast");
    let type_obj = |tp: *mut PyTypeObject| tobj::type_obj(tp).to_ref();
    for (i, nc) in ncs.iter().enumerate() {
        assert_eq!(nc.name, KIND_NAMES[i], "_ast: class order mismatch");
        let base = if nc.base == "AST" { ast_cls.clone() } else { by_name[nc.base].clone() };
        let ns = dict::new_dict();
        dict::set_item(&ns, name!("__module__"), &module_name)?;
        dict::set_item(&ns, name!("__doc__"), &pystr::new_str(&asdl_doc(&nc.doc)))?;
        let fields: Vec<PyRef> = nc.fields.iter().map(|f| pystr::intern_str(f.name)).collect();
        let fields_t = tuple::new_tuple(fields);
        dict::set_item(&ns, name!("_fields"), &fields_t)?;
        dict::set_item(&ns, name!("__match_args__"), &fields_t)?;
        let attrs: Vec<PyRef> = nc.attributes.iter().map(|f| pystr::intern_str(f.name)).collect();
        dict::set_item(&ns, name!("_attributes"), &tuple::new_tuple(attrs))?;
        for f in &nc.fields {
            if f.kind == FieldKind::Optional {
                dict::set_item(&ns, &pystr::intern_str(f.name), none::none())?;
            }
        }
        // optional attributes (`int? end_lineno`) default to None on the class that declares them
        if nc.is_abstract || nc.base == "AST" {
            for a in &nc.attributes {
                if a.kind == FieldKind::Optional {
                    dict::set_item(&ns, &pystr::intern_str(a.name), none::none())?;
                }
            }
        }
        let args = tuple::new_tuple(vec![pystr::new_str(nc.name), tuple::new_tuple(vec![base]), ns]);
        let cls = crate::types::object_type::type_new(tobj::type_type(), &args, None)?;
        // tp_name "ast.<Name>" (error messages use it, like CPython)
        unsafe {
            let tp = cls.as_ptr() as *mut PyTypeObject;
            let full = format!("ast.{}\0", nc.name);
            let leaked: &'static [u8] = Box::leak(full.into_bytes().into_boxed_slice());
            (*tp).tp_name = leaked.as_ptr() as *const _;
            (*crate::types::typeobject::heap(tp))._ht_tpname = leaked.as_ptr() as *mut _;
            make_immortal(cls.as_ptr());
        }
        by_name.insert(nc.name, cls.clone());
        classes.push(cls);
        abstract_.push(nc.is_abstract);
    }
    // second pass: `_field_types` / `__annotations__` (may refer to classes defined later)
    for (i, nc) in ncs.iter().enumerate() {
        if nc.is_abstract {
            continue;
        }
        let ft = dict::new_dict();
        for f in &nc.fields {
            let base_t: PyRef = match f.ty {
                "identifier" | "string" => type_obj(pystr::str_type()),
                "int" => type_obj(int::int_type()),
                "constant" => type_obj(tobj::object_type()),
                other => by_name.get(other).cloned().unwrap_or_else(|| type_obj(tobj::object_type())),
            };
            let t = match f.kind {
                FieldKind::Single => base_t,
                FieldKind::Optional => crate::types::union::union_or(&base_t, none::none())?,
                FieldKind::Sequence | FieldKind::OptionalSequence => {
                    crate::types::genericalias::new_generic_alias(&type_obj(list::list_type()), &base_t)?
                }
            };
            dict::set_item(&ft, &pystr::intern_str(f.name), &t)?;
        }
        let tp = classes[i].as_ptr() as *mut PyTypeObject;
        let d = tobj::type_dict(tp);
        dict::set_item(d, name!("__annotations__"), &*dict::copy(&ft)?)?;
        dict::set_item(d, name!("_field_types"), &ft)?;
        crate::types::slots::type_modified(tp);
    }
    let mut by_ptr = std::collections::HashMap::new();
    for (i, c) in classes.iter().enumerate() {
        let k: K = unsafe { std::mem::transmute::<u8, K>(i as u8) };
        by_ptr.insert(c.as_ptr() as usize, k);
    }
    let singletons = (0..classes.len()).map(|_| OnceLock::new()).collect();
    Ok(State { classes, abstract_, singletons, by_ptr })
}

// ----------------------------------------------------------------------------------------
// The _ast module
// ----------------------------------------------------------------------------------------

fn m_is_abstract(_m: &PyObject, cls: &PyObject) -> PyResult {
    if !tobj::is_type(cls) {
        return Ok(crate::types::bool_::new_bool(false));
    }
    state()?;
    Ok(crate::types::bool_::new_bool(is_abstract_type(tobj::as_type(cls))))
}

pub const PYCF_ONLY_AST: i64 = 0x400;
pub const PYCF_TYPE_COMMENTS: i64 = 0x1000;
pub const PYCF_ALLOW_TOP_LEVEL_AWAIT: i64 = 0x2000;
pub const PYCF_OPTIMIZED_AST: i64 = 0x8000 | 0x400;

pub fn init_module() -> PyResult {
    let st = state()?;
    let methods = mdefs![("_is_abstract", O, m_is_abstract, "Return True if the AST class is abstract.")];
    let (m, d) = fc::new_module("_ast", methods, None)?;
    dict::set_item_str(&d, "AST", tobj::type_obj(ast_type()))?;
    for (i, nm) in KIND_NAMES.iter().enumerate() {
        dict::set_item_str(&d, nm, &st.classes[i])?;
    }
    dict::set_item_str(&d, "PyCF_ONLY_AST", &int::new_int(PYCF_ONLY_AST))?;
    dict::set_item_str(&d, "PyCF_TYPE_COMMENTS", &int::new_int(PYCF_TYPE_COMMENTS))?;
    dict::set_item_str(&d, "PyCF_ALLOW_TOP_LEVEL_AWAIT", &int::new_int(PYCF_ALLOW_TOP_LEVEL_AWAIT))?;
    dict::set_item_str(&d, "PyCF_OPTIMIZED_AST", &int::new_int(PYCF_OPTIMIZED_AST))?;
    Ok(m)
}

#[allow(dead_code)]
fn _unused(o: &PyObject) -> PyRef {
    node_dict(o)
}
