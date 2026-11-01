//! The `_ast` module: AST node classes (generated from the parser crate's node
//! table), conversion of the Rust AST (rpy_parser::ast) to Python objects and
//! back (for `compile(ast_object, ...)`), and AST validation.
//!
//! The node classes themselves are ordinary Python classes created at module
//! initialisation from `rpy_parser::ast::node_classes()`; the behaviour of the
//! `AST` base class (constructor rules, `__reduce__`, `__replace__`,
//! `__repr__`) is defined by a small Python prelude run in the module.
use crate::abstract_ as ab;
use crate::errors::*;
use crate::ffi::*;
use crate::object::*;
use crate::raise;
use crate::types::{bytes, dict, float, int, list, none, str as pystr, tuple};
use rpy_parser::ast::*;
use std::collections::HashMap;
use std::sync::OnceLock;

const PRELUDE: &str = r#"
def _tpname(cls):
    if cls.__module__ == 'ast':
        return 'ast.' + cls.__name__
    return cls.__name__

def _plural(n):
    return '' if n == 1 else 's'

def _join_names(names):
    names = [repr(n) for n in names]
    if len(names) == 1:
        return names[0]
    if len(names) == 2:
        return names[0] + ' and ' + names[1]
    return ', '.join(names[:-1]) + ', and ' + names[-1]

def _is_list_type(t):
    return getattr(t, '__origin__', None) is list

def _is_optional_type(t):
    if _is_list_type(t):
        return False
    try:
        args = t.__args__
    except AttributeError:
        return False
    try:
        return type(None) in args
    except TypeError:
        return False

class AST:
    __module__ = 'ast'
    _fields = ()
    _attributes = ()
    __match_args__ = ()

    def __init__(self, *args, **kwargs):
        cls = type(self)
        if _is_abstract(cls):
            import warnings
            warnings.warn(
                f"Instantiating abstract AST node class {_tpname(cls)} is "
                f"deprecated. This will become an error in Python 3.20",
                DeprecationWarning, stacklevel=2)
        fields = cls._fields
        name = _tpname(cls)
        if fields is None:
            fields = ()
        nfields = len(fields)
        if len(args) > nfields:
            raise TypeError(f"{name} constructor takes at most "
                            f"{nfields} positional argument{_plural(nfields)}")
        for f, a in zip(fields, args):
            setattr(self, f, a)
        remaining = list(fields[len(args):])
        if kwargs:
            attributes = getattr(cls, '_attributes', ()) or ()
            for key, value in kwargs.items():
                if key in fields:
                    if list(fields).index(key) < len(args):
                        raise TypeError(f"{name} got multiple values for "
                                        f"argument {key!r}")
                elif key not in attributes:
                    raise TypeError(f"{name}.__init__ got an unexpected "
                                    f"keyword argument {key!r}")
                setattr(self, key, value)
                if key in remaining:
                    remaining.remove(key)
        if not remaining:
            return
        try:
            field_types = cls._field_types
        except AttributeError:
            return
        missing = []
        for f in remaining:
            try:
                t = field_types[f]
            except (KeyError, TypeError):
                raise TypeError(f"Field {f!r} is missing from "
                                f"{cls.__qualname__}._field_types") from None
            if _is_list_type(t):
                setattr(self, f, [])
            elif t is expr_context:
                setattr(self, f, Load())
            elif _is_optional_type(t):
                setattr(self, f, None)
            else:
                missing.append(f)
        if missing:
            raise TypeError(f"{name}.__init__ missing {len(missing)} required "
                            f"positional argument{_plural(len(missing))}: "
                            f"{_join_names(missing)}")

    def __reduce__(self):
        d = getattr(self, '__dict__', None)
        cls = type(self)
        if not d:
            return cls, ()
        d = dict(d)
        args = []
        for f in getattr(cls, '_fields', ()) or ():
            if f not in d:
                break
            args.append(d.pop(f))
        if d:
            return cls, tuple(args), d
        return cls, tuple(args)

    def __replace__(self, **kwargs):
        cls = type(self)
        name = _tpname(cls)
        fields = tuple(getattr(cls, '_fields', ()) or ())
        attributes = tuple(getattr(cls, '_attributes', ()) or ())
        for key in kwargs:
            if key not in fields and key not in attributes:
                raise TypeError(f"{name}.__replace__ got an unexpected "
                                f"keyword argument {key!r}.")
        payload = {}
        missing = []
        for f in fields + attributes:
            if f in kwargs:
                payload[f] = kwargs[f]
            else:
                try:
                    payload[f] = getattr(self, f)
                except AttributeError:
                    if f in fields:
                        missing.append(f)
        if missing:
            field_types = getattr(cls, '_field_types', {})
            really = [f for f in missing
                      if f in field_types
                      and not _is_list_type(field_types[f])
                      and field_types[f] is not expr_context
                      and not _is_optional_type(field_types[f])]
            if really:
                raise TypeError(f"{name}.__replace__ missing {len(really)} "
                                f"keyword argument{_plural(len(really))}: "
                                f"{_join_names(really)}.")
        return cls(**payload)

    def __repr__(self):
        return _repr(self, 0)

def _repr_value(value, depth):
    if isinstance(value, AST):
        return _repr(value, depth)
    if isinstance(value, list):
        if not value:
            return '[]'
        if len(value) > 2:
            return ('[' + _repr_value(value[0], depth) + ', ..., '
                    + _repr_value(value[-1], depth) + ']')
        return '[' + ', '.join(_repr_value(v, depth) for v in value) + ']'
    return repr(value)

def _repr(node, depth):
    cls = type(node)
    if depth >= 3:
        return f'{cls.__name__}(...)'
    parts = []
    for f in getattr(cls, '_fields', ()) or ():
        try:
            value = getattr(node, f)
        except AttributeError:
            continue
        parts.append(f'{f}={_repr_value(value, depth + 1)}')
    return f'{cls.__name__}({", ".join(parts)})'

_ABSTRACT = set()

def _is_abstract(cls):
    return cls is AST or cls in _ABSTRACT

_BUILTIN_TYPES = {'identifier': str, 'string': str, 'int': int, 'constant': object}

def _build(table, ns):
    # table: list of (name, base, is_abstract, fields, attributes, doc)
    #   fields: list of (name, kind, type); kind 0=single 1=opt 2=seq 3=optseq
    #   attributes: list of (name, optional)
    created = {}
    for name, base, abstract, fields, attributes, doc in table:
        bcls = AST if base == 'AST' else created[base]
        body = {
            '__module__': 'ast',
            '__doc__': doc,
            '_fields': tuple(f[0] for f in fields),
            '__match_args__': tuple(f[0] for f in fields),
        }
        if attributes or abstract:
            body['_attributes'] = tuple(a[0] for a in attributes)
        for aname, aopt in attributes:
            if aopt:
                body[aname] = None
        cls = type(name, (bcls,), body)
        created[name] = cls
        ns[name] = cls
        if abstract:
            _ABSTRACT.add(cls)
    # field types need all classes
    for name, base, abstract, fields, attributes, doc in table:
        cls = created[name]
        ftypes = {}
        for fname, kind, ftype in fields:
            t = _BUILTIN_TYPES.get(ftype) or created[ftype]
            if kind == 0:
                ft = t
            elif kind == 1:
                ft = t | None
                if fname not in cls.__dict__:
                    setattr(cls, fname, None)
            elif kind == 2:
                ft = list[t]
            else:
                ft = list[t | None]
            ftypes[fname] = ft
        cls._field_types = ftypes
        cls.__annotations__ = dict(ftypes)
    return created
"#;

struct State {
    /// name -> class object (owned, never freed)
    classes: HashMap<&'static str, usize>,
    /// singleton instances of operator / context classes
    singletons: HashMap<&'static str, usize>,
    module: usize,
}

unsafe impl Send for State {}
unsafe impl Sync for State {}

static STATE: OnceLock<State> = OnceLock::new();

fn state() -> PyResult<&'static State> {
    if let Some(s) = STATE.get() {
        return Ok(s);
    }
    // Importing _ast initialises the state.
    crate::import::import_absolute("_ast")?;
    match STATE.get() {
        Some(s) => Ok(s),
        None => raise!(SystemError, "_ast module state not initialised"),
    }
}

fn cls(name: &str) -> PyResult<&'static PyObject> {
    let st = state()?;
    match st.classes.get(name) {
        Some(&p) => Ok(unsafe { &*(p as *const PyObject) }),
        None => raise!(SystemError, "unknown AST class {}", name),
    }
}

fn singleton(name: &str) -> PyResult<PyRef> {
    let st = state()?;
    match st.singletons.get(name) {
        Some(&p) => Ok(unsafe { PyRef::from_borrowed(p as *mut PyObject) }),
        None => raise!(SystemError, "unknown AST singleton {}", name),
    }
}

pub fn init_module() -> PyResult {
    let m = crate::types::module::new_module("_ast")?;
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "__builtins__", &crate::builtins::builtins_module())?;
    let c = crate::compile::compile_str(PRELUDE, "<frozen _ast>", crate::compile::Mode::Exec, 0, 0)?;
    crate::interp::eval_code(&c.code, &d, Some(&d))?;
    // Build the class table.
    let mut rows = Vec::new();
    for nc in node_classes() {
        let fields: Vec<PyRef> = nc
            .fields
            .iter()
            .map(|f| {
                let kind = match f.kind {
                    FieldKind::Single => 0,
                    FieldKind::Optional => 1,
                    FieldKind::Sequence => 2,
                    FieldKind::OptionalSequence => 3,
                };
                tuple::new_tuple(vec![pystr::new_str(f.name), int::new_int(kind), pystr::new_str(f.ty)])
            })
            .collect();
        let attrs: Vec<PyRef> = nc
            .attributes
            .iter()
            .map(|a| tuple::new_tuple(vec![pystr::new_str(a.name), crate::types::bool_::new_bool(a.kind == FieldKind::Optional)]))
            .collect();
        rows.push(tuple::new_tuple(vec![
            pystr::new_str(nc.name),
            pystr::new_str(nc.base),
            crate::types::bool_::new_bool(nc.is_abstract),
            list::new_list(fields),
            list::new_list(attrs),
            pystr::new_str(&nc.doc),
        ]));
    }
    let build = dict::get_item_rs(&d, "_build").unwrap();
    let created = ab::call(&build, &[&list::new_list(rows), &d])?;
    let mut classes = HashMap::new();
    let mut singletons = HashMap::new();
    if let Some(ast) = dict::get_item_rs(&d, "AST") {
        classes.insert("AST", ast.into_raw() as usize);
    }
    for nc in node_classes() {
        let c = dict::get_item_rs(&created, nc.name).unwrap();
        let is_singleton = matches!(nc.base, "expr_context" | "boolop" | "operator" | "unaryop" | "cmpop");
        if is_singleton {
            let inst = ab::call(&c, &[])?;
            singletons.insert(nc.name, inst.into_raw() as usize);
        }
        classes.insert(nc.name, c.into_raw() as usize);
    }
    for (k, v) in [
        ("PyCF_ONLY_AST", rpy_compiler::cf::PYCF_ONLY_AST),
        ("PyCF_TYPE_COMMENTS", rpy_compiler::cf::PYCF_TYPE_COMMENTS),
        ("PyCF_ALLOW_TOP_LEVEL_AWAIT", rpy_compiler::cf::PYCF_ALLOW_TOP_LEVEL_AWAIT),
        ("PyCF_OPTIMIZED_AST", rpy_compiler::cf::PYCF_OPTIMIZED_AST),
    ] {
        dict::set_item_str(&d, k, &int::new_int(v as i64))?;
    }
    for k in ["_build", "_tpname", "_plural", "_BUILTIN_TYPES"] {
        let _ = dict::del_item(&d, &pystr::new_str(k));
    }
    let _ = STATE.set(State { classes, singletons, module: m.clone().into_raw() as usize });
    let _ = STATE.get().map(|s| s.module);
    Ok(m)
}

// ======================================================================
// Rust AST -> Python objects
// ======================================================================

fn new_node(name: &str) -> PyResult {
    let c = cls(name)?;
    let tp = c.as_ptr() as *mut PyTypeObject;
    let op = unsafe { crate::types::typeobject::generic_alloc(tp, 0) };
    if op.is_null() {
        return Err(PyErr);
    }
    Ok(unsafe { PyRef::from_raw(op) })
}

struct NodeBuilder {
    obj: PyRef,
    dict: PyRef,
}

impl NodeBuilder {
    fn new(name: &str) -> PyResult<NodeBuilder> {
        let obj = new_node(name)?;
        let dict = match crate::types::typeobject::instance_dict(&obj) {
            Some(d) => d,
            None => raise!(SystemError, "AST node without __dict__"),
        };
        Ok(NodeBuilder { obj, dict })
    }
    fn set(&self, k: &str, v: PyRef) -> PyResult<()> {
        dict::set_item(&self.dict, &pystr::intern_str(k), &v)
    }
    fn loc(&self, l: &Loc) -> PyResult<()> {
        self.set("lineno", int::new_int(l.lineno as i64))?;
        self.set("col_offset", int::new_int(l.col_offset as i64))?;
        self.set("end_lineno", int::new_int(l.end_lineno as i64))?;
        self.set("end_col_offset", int::new_int(l.end_col_offset as i64))
    }
    fn done(self) -> PyRef {
        self.obj
    }
}

fn s(x: &str) -> PyRef {
    pystr::new_str(x)
}

fn ident(x: &str) -> PyRef {
    pystr::intern_str(x)
}

fn opt_ident(x: &Option<String>) -> PyRef {
    match x {
        Some(v) => ident(v),
        None => none::none_ref(),
    }
}

fn opt_str(x: &Option<String>) -> PyRef {
    match x {
        Some(v) => s(v),
        None => none::none_ref(),
    }
}

fn pystr_obj(p: &PyStr) -> PyRef {
    match p {
        PyStr::Utf8(x) => s(x),
        PyStr::CodePoints(v) => pystr::from_codepoints(v),
    }
}

fn int_obj(i: &Int) -> PyResult {
    match i {
        Int::Small(v) => Ok(int::new_int(*v)),
        Int::Big(d) => {
            let (neg, digits) = match d.strip_prefix('-') {
                Some(r) => (true, r),
                None => (false, d.as_str()),
            };
            crate::types::int_methods::from_decimal_str(neg, digits)
        }
    }
}

fn const_obj(c: &Constant) -> PyResult {
    Ok(match c {
        Constant::None => none::none_ref(),
        Constant::Bool(b) => crate::types::bool_::new_bool(*b),
        Constant::Ellipsis => none::ellipsis().to_ref(),
        Constant::Int(i) => int_obj(i)?,
        Constant::Float(f) => float::new_float(*f),
        Constant::Complex { real, imag } => crate::types::complex::new_complex(*real, *imag),
        Constant::Str(p) => pystr_obj(p),
        Constant::Bytes(b) => bytes::new_bytes(b),
        Constant::Tuple(items) => {
            let mut v = Vec::with_capacity(items.len());
            for it in items {
                v.push(const_obj(it)?);
            }
            tuple::new_tuple(v)
        }
        Constant::FrozenSet(items) => {
            let mut v = Vec::with_capacity(items.len());
            for it in items {
                v.push(const_obj(it)?);
            }
            crate::types::set::new_frozenset_from_vec(v)?
        }
    })
}

fn seq<T>(items: &[T], f: impl Fn(&T) -> PyResult) -> PyResult {
    let mut v = Vec::with_capacity(items.len());
    for it in items {
        v.push(f(it)?);
    }
    Ok(list::new_list(v))
}

fn opt<T>(x: &Option<T>, f: impl Fn(&T) -> PyResult) -> PyResult {
    match x {
        Some(v) => f(v),
        None => Ok(none::none_ref()),
    }
}

pub fn mod_to_py(m: &Mod) -> PyResult {
    match m {
        Mod::Module { body, type_ignores } => {
            let n = NodeBuilder::new("Module")?;
            n.set("body", seq(body, stmt_to_py)?)?;
            n.set("type_ignores", seq(type_ignores, type_ignore_to_py)?)?;
            Ok(n.done())
        }
        Mod::Interactive { body } => {
            let n = NodeBuilder::new("Interactive")?;
            n.set("body", seq(body, stmt_to_py)?)?;
            Ok(n.done())
        }
        Mod::Expression { body } => {
            let n = NodeBuilder::new("Expression")?;
            n.set("body", expr_to_py(body)?)?;
            Ok(n.done())
        }
        Mod::FunctionType { argtypes, returns } => {
            let n = NodeBuilder::new("FunctionType")?;
            n.set("argtypes", seq(argtypes, expr_to_py)?)?;
            n.set("returns", expr_to_py(returns)?)?;
            Ok(n.done())
        }
    }
}

fn type_ignore_to_py(t: &TypeIgnore) -> PyResult {
    let n = NodeBuilder::new("TypeIgnore")?;
    n.set("lineno", int::new_int(t.lineno as i64))?;
    n.set("tag", s(&t.tag))?;
    Ok(n.done())
}

fn boxed_expr(e: &Box<Expr>) -> PyResult {
    expr_to_py(e)
}

fn type_params_to_py(tps: &[TypeParam]) -> PyResult {
    seq(tps, |tp| {
        let (name, n) = match &tp.node {
            TypeParamKind::TypeVar { name, bound, default_value } => {
                let n = NodeBuilder::new("TypeVar")?;
                n.set("bound", opt(bound, boxed_expr)?)?;
                n.set("default_value", opt(default_value, boxed_expr)?)?;
                (name, n)
            }
            TypeParamKind::ParamSpec { name, default_value } => {
                let n = NodeBuilder::new("ParamSpec")?;
                n.set("default_value", opt(default_value, boxed_expr)?)?;
                (name, n)
            }
            TypeParamKind::TypeVarTuple { name, default_value } => {
                let n = NodeBuilder::new("TypeVarTuple")?;
                n.set("default_value", opt(default_value, boxed_expr)?)?;
                (name, n)
            }
        };
        n.set("name", ident(name))?;
        n.loc(&tp.loc)?;
        Ok(n.done())
    })
}

fn arg_to_py(a: &Arg) -> PyResult {
    let n = NodeBuilder::new("arg")?;
    n.set("arg", ident(&a.arg))?;
    n.set("annotation", opt(&a.annotation, boxed_expr)?)?;
    n.set("type_comment", opt_str(&a.type_comment))?;
    n.loc(&a.loc)?;
    Ok(n.done())
}

fn arguments_to_py(a: &Arguments) -> PyResult {
    let n = NodeBuilder::new("arguments")?;
    n.set("posonlyargs", seq(&a.posonlyargs, arg_to_py)?)?;
    n.set("args", seq(&a.args, arg_to_py)?)?;
    n.set("vararg", opt(&a.vararg, |x| arg_to_py(x))?)?;
    n.set("kwonlyargs", seq(&a.kwonlyargs, arg_to_py)?)?;
    n.set("kw_defaults", seq(&a.kw_defaults, |x| opt(x, expr_to_py))?)?;
    n.set("kwarg", opt(&a.kwarg, |x| arg_to_py(x))?)?;
    n.set("defaults", seq(&a.defaults, expr_to_py)?)?;
    Ok(n.done())
}

fn keyword_to_py(k: &Keyword) -> PyResult {
    let n = NodeBuilder::new("keyword")?;
    n.set("arg", opt_ident(&k.arg))?;
    n.set("value", expr_to_py(&k.value)?)?;
    n.loc(&k.loc)?;
    Ok(n.done())
}

fn alias_to_py(a: &Alias) -> PyResult {
    let n = NodeBuilder::new("alias")?;
    n.set("name", ident(&a.name))?;
    n.set("asname", opt_ident(&a.asname))?;
    n.loc(&a.loc)?;
    Ok(n.done())
}

fn withitem_to_py(w: &WithItem) -> PyResult {
    let n = NodeBuilder::new("withitem")?;
    n.set("context_expr", expr_to_py(&w.context_expr)?)?;
    n.set("optional_vars", opt(&w.optional_vars, boxed_expr)?)?;
    Ok(n.done())
}

fn handler_to_py(h: &ExceptHandler) -> PyResult {
    let n = NodeBuilder::new("ExceptHandler")?;
    n.set("type", opt(&h.type_, boxed_expr)?)?;
    n.set("name", opt_ident(&h.name))?;
    n.set("body", seq(&h.body, stmt_to_py)?)?;
    n.loc(&h.loc)?;
    Ok(n.done())
}

fn comprehension_to_py(c: &Comprehension) -> PyResult {
    let n = NodeBuilder::new("comprehension")?;
    n.set("target", expr_to_py(&c.target)?)?;
    n.set("iter", expr_to_py(&c.iter)?)?;
    n.set("ifs", seq(&c.ifs, expr_to_py)?)?;
    n.set("is_async", int::new_int(c.is_async as i64))?;
    Ok(n.done())
}

fn match_case_to_py(c: &MatchCase) -> PyResult {
    let n = NodeBuilder::new("match_case")?;
    n.set("pattern", pattern_to_py(&c.pattern)?)?;
    n.set("guard", opt(&c.guard, boxed_expr)?)?;
    n.set("body", seq(&c.body, stmt_to_py)?)?;
    Ok(n.done())
}

fn pattern_to_py(p: &Pattern) -> PyResult {
    let n = match &p.node {
        PatternKind::MatchValue { value } => {
            let n = NodeBuilder::new("MatchValue")?;
            n.set("value", expr_to_py(value)?)?;
            n
        }
        PatternKind::MatchSingleton { value } => {
            let n = NodeBuilder::new("MatchSingleton")?;
            n.set("value", const_obj(value)?)?;
            n
        }
        PatternKind::MatchSequence { patterns } => {
            let n = NodeBuilder::new("MatchSequence")?;
            n.set("patterns", seq(patterns, pattern_to_py)?)?;
            n
        }
        PatternKind::MatchMapping { keys, patterns, rest } => {
            let n = NodeBuilder::new("MatchMapping")?;
            n.set("keys", seq(keys, expr_to_py)?)?;
            n.set("patterns", seq(patterns, pattern_to_py)?)?;
            n.set("rest", opt_ident(rest))?;
            n
        }
        PatternKind::MatchClass { cls, patterns, kwd_attrs, kwd_patterns } => {
            let n = NodeBuilder::new("MatchClass")?;
            n.set("cls", expr_to_py(cls)?)?;
            n.set("patterns", seq(patterns, pattern_to_py)?)?;
            n.set("kwd_attrs", seq(kwd_attrs, |x| Ok(ident(x)))?)?;
            n.set("kwd_patterns", seq(kwd_patterns, pattern_to_py)?)?;
            n
        }
        PatternKind::MatchStar { name } => {
            let n = NodeBuilder::new("MatchStar")?;
            n.set("name", opt_ident(name))?;
            n
        }
        PatternKind::MatchAs { pattern, name } => {
            let n = NodeBuilder::new("MatchAs")?;
            n.set("pattern", opt(pattern, |x| pattern_to_py(x))?)?;
            n.set("name", opt_ident(name))?;
            n
        }
        PatternKind::MatchOr { patterns } => {
            let n = NodeBuilder::new("MatchOr")?;
            n.set("patterns", seq(patterns, pattern_to_py)?)?;
            n
        }
    };
    n.loc(&p.loc)?;
    Ok(n.done())
}

fn funcdef_to_py(name: &str, f: &FunctionDef) -> PyResult<NodeBuilder> {
    let n = NodeBuilder::new(name)?;
    n.set("name", ident(&f.name))?;
    n.set("args", arguments_to_py(&f.args)?)?;
    n.set("body", seq(&f.body, stmt_to_py)?)?;
    n.set("decorator_list", seq(&f.decorator_list, expr_to_py)?)?;
    n.set("returns", opt(&f.returns, boxed_expr)?)?;
    n.set("type_comment", opt_str(&f.type_comment))?;
    n.set("type_params", type_params_to_py(&f.type_params)?)?;
    Ok(n)
}

fn for_to_py(name: &str, f: &For) -> PyResult<NodeBuilder> {
    let n = NodeBuilder::new(name)?;
    n.set("target", expr_to_py(&f.target)?)?;
    n.set("iter", expr_to_py(&f.iter)?)?;
    n.set("body", seq(&f.body, stmt_to_py)?)?;
    n.set("orelse", seq(&f.orelse, stmt_to_py)?)?;
    n.set("type_comment", opt_str(&f.type_comment))?;
    Ok(n)
}

fn with_to_py(name: &str, w: &With) -> PyResult<NodeBuilder> {
    let n = NodeBuilder::new(name)?;
    n.set("items", seq(&w.items, withitem_to_py)?)?;
    n.set("body", seq(&w.body, stmt_to_py)?)?;
    n.set("type_comment", opt_str(&w.type_comment))?;
    Ok(n)
}

fn try_to_py(name: &str, t: &Try) -> PyResult<NodeBuilder> {
    let n = NodeBuilder::new(name)?;
    n.set("body", seq(&t.body, stmt_to_py)?)?;
    n.set("handlers", seq(&t.handlers, handler_to_py)?)?;
    n.set("orelse", seq(&t.orelse, stmt_to_py)?)?;
    n.set("finalbody", seq(&t.finalbody, stmt_to_py)?)?;
    Ok(n)
}

pub fn stmt_to_py(st: &Stmt) -> PyResult {
    use StmtKind as K;
    let n = match &st.node {
        K::FunctionDef(f) => funcdef_to_py("FunctionDef", f)?,
        K::AsyncFunctionDef(f) => funcdef_to_py("AsyncFunctionDef", f)?,
        K::ClassDef { name, bases, keywords, body, decorator_list, type_params } => {
            let n = NodeBuilder::new("ClassDef")?;
            n.set("name", ident(name))?;
            n.set("bases", seq(bases, expr_to_py)?)?;
            n.set("keywords", seq(keywords, keyword_to_py)?)?;
            n.set("body", seq(body, stmt_to_py)?)?;
            n.set("decorator_list", seq(decorator_list, expr_to_py)?)?;
            n.set("type_params", type_params_to_py(type_params)?)?;
            n
        }
        K::Return { value } => {
            let n = NodeBuilder::new("Return")?;
            n.set("value", opt(value, boxed_expr)?)?;
            n
        }
        K::Delete { targets } => {
            let n = NodeBuilder::new("Delete")?;
            n.set("targets", seq(targets, expr_to_py)?)?;
            n
        }
        K::Assign { targets, value, type_comment } => {
            let n = NodeBuilder::new("Assign")?;
            n.set("targets", seq(targets, expr_to_py)?)?;
            n.set("value", expr_to_py(value)?)?;
            n.set("type_comment", opt_str(type_comment))?;
            n
        }
        K::TypeAlias { name, type_params, value } => {
            let n = NodeBuilder::new("TypeAlias")?;
            n.set("name", expr_to_py(name)?)?;
            n.set("type_params", type_params_to_py(type_params)?)?;
            n.set("value", expr_to_py(value)?)?;
            n
        }
        K::AugAssign { target, op, value } => {
            let n = NodeBuilder::new("AugAssign")?;
            n.set("target", expr_to_py(target)?)?;
            n.set("op", singleton(op.name())?)?;
            n.set("value", expr_to_py(value)?)?;
            n
        }
        K::AnnAssign { target, annotation, value, simple } => {
            let n = NodeBuilder::new("AnnAssign")?;
            n.set("target", expr_to_py(target)?)?;
            n.set("annotation", expr_to_py(annotation)?)?;
            n.set("value", opt(value, boxed_expr)?)?;
            n.set("simple", int::new_int(*simple as i64))?;
            n
        }
        K::For(f) => for_to_py("For", f)?,
        K::AsyncFor(f) => for_to_py("AsyncFor", f)?,
        K::While { test, body, orelse } => {
            let n = NodeBuilder::new("While")?;
            n.set("test", expr_to_py(test)?)?;
            n.set("body", seq(body, stmt_to_py)?)?;
            n.set("orelse", seq(orelse, stmt_to_py)?)?;
            n
        }
        K::If { test, body, orelse } => {
            let n = NodeBuilder::new("If")?;
            n.set("test", expr_to_py(test)?)?;
            n.set("body", seq(body, stmt_to_py)?)?;
            n.set("orelse", seq(orelse, stmt_to_py)?)?;
            n
        }
        K::With(w) => with_to_py("With", w)?,
        K::AsyncWith(w) => with_to_py("AsyncWith", w)?,
        K::Match { subject, cases } => {
            let n = NodeBuilder::new("Match")?;
            n.set("subject", expr_to_py(subject)?)?;
            n.set("cases", seq(cases, match_case_to_py)?)?;
            n
        }
        K::Raise { exc, cause } => {
            let n = NodeBuilder::new("Raise")?;
            n.set("exc", opt(exc, boxed_expr)?)?;
            n.set("cause", opt(cause, boxed_expr)?)?;
            n
        }
        K::Try(t) => try_to_py("Try", t)?,
        K::TryStar(t) => try_to_py("TryStar", t)?,
        K::Assert { test, msg } => {
            let n = NodeBuilder::new("Assert")?;
            n.set("test", expr_to_py(test)?)?;
            n.set("msg", opt(msg, boxed_expr)?)?;
            n
        }
        K::Import { names, is_lazy } => {
            let n = NodeBuilder::new("Import")?;
            n.set("names", seq(names, alias_to_py)?)?;
            n.set("is_lazy", int::new_int(*is_lazy as i64))?;
            n
        }
        K::ImportFrom { module, names, level, is_lazy } => {
            let n = NodeBuilder::new("ImportFrom")?;
            n.set("module", opt_ident(module))?;
            n.set("names", seq(names, alias_to_py)?)?;
            n.set("level", int::new_int(*level as i64))?;
            n.set("is_lazy", int::new_int(*is_lazy as i64))?;
            n
        }
        K::Global { names } => {
            let n = NodeBuilder::new("Global")?;
            n.set("names", seq(names, |x| Ok(ident(x)))?)?;
            n
        }
        K::Nonlocal { names } => {
            let n = NodeBuilder::new("Nonlocal")?;
            n.set("names", seq(names, |x| Ok(ident(x)))?)?;
            n
        }
        K::Expr { value } => {
            let n = NodeBuilder::new("Expr")?;
            n.set("value", expr_to_py(value)?)?;
            n
        }
        K::Pass => NodeBuilder::new("Pass")?,
        K::Break => NodeBuilder::new("Break")?,
        K::Continue => NodeBuilder::new("Continue")?,
    };
    n.loc(&st.loc)?;
    Ok(n.done())
}

pub fn expr_to_py(e: &Expr) -> PyResult {
    use ExprKind as K;
    let n = match &e.node {
        K::BoolOp { op, values } => {
            let n = NodeBuilder::new("BoolOp")?;
            n.set("op", singleton(op.name())?)?;
            n.set("values", seq(values, expr_to_py)?)?;
            n
        }
        K::NamedExpr { target, value } => {
            let n = NodeBuilder::new("NamedExpr")?;
            n.set("target", expr_to_py(target)?)?;
            n.set("value", expr_to_py(value)?)?;
            n
        }
        K::BinOp { left, op, right } => {
            let n = NodeBuilder::new("BinOp")?;
            n.set("left", expr_to_py(left)?)?;
            n.set("op", singleton(op.name())?)?;
            n.set("right", expr_to_py(right)?)?;
            n
        }
        K::UnaryOp { op, operand } => {
            let n = NodeBuilder::new("UnaryOp")?;
            n.set("op", singleton(op.name())?)?;
            n.set("operand", expr_to_py(operand)?)?;
            n
        }
        K::Lambda { args, body } => {
            let n = NodeBuilder::new("Lambda")?;
            n.set("args", arguments_to_py(args)?)?;
            n.set("body", expr_to_py(body)?)?;
            n
        }
        K::IfExp { test, body, orelse } => {
            let n = NodeBuilder::new("IfExp")?;
            n.set("test", expr_to_py(test)?)?;
            n.set("body", expr_to_py(body)?)?;
            n.set("orelse", expr_to_py(orelse)?)?;
            n
        }
        K::Dict { keys, values } => {
            let n = NodeBuilder::new("Dict")?;
            n.set("keys", seq(keys, |k| opt(k, expr_to_py))?)?;
            n.set("values", seq(values, expr_to_py)?)?;
            n
        }
        K::Set { elts } => {
            let n = NodeBuilder::new("Set")?;
            n.set("elts", seq(elts, expr_to_py)?)?;
            n
        }
        K::ListComp { elt, generators } | K::SetComp { elt, generators } | K::GeneratorExp { elt, generators } => {
            let n = NodeBuilder::new(e.class_name())?;
            n.set("elt", expr_to_py(elt)?)?;
            n.set("generators", seq(generators, comprehension_to_py)?)?;
            n
        }
        K::DictComp { key, value, generators } => {
            let n = NodeBuilder::new("DictComp")?;
            n.set("key", expr_to_py(key)?)?;
            n.set("value", opt(value, boxed_expr)?)?;
            n.set("generators", seq(generators, comprehension_to_py)?)?;
            n
        }
        K::Await { value } | K::YieldFrom { value } => {
            let n = NodeBuilder::new(e.class_name())?;
            n.set("value", expr_to_py(value)?)?;
            n
        }
        K::Yield { value } => {
            let n = NodeBuilder::new("Yield")?;
            n.set("value", opt(value, boxed_expr)?)?;
            n
        }
        K::Compare { left, ops, comparators } => {
            let n = NodeBuilder::new("Compare")?;
            n.set("left", expr_to_py(left)?)?;
            n.set("ops", seq(ops, |o| singleton(o.name()))?)?;
            n.set("comparators", seq(comparators, expr_to_py)?)?;
            n
        }
        K::Call { func, args, keywords } => {
            let n = NodeBuilder::new("Call")?;
            n.set("func", expr_to_py(func)?)?;
            n.set("args", seq(args, expr_to_py)?)?;
            n.set("keywords", seq(keywords, keyword_to_py)?)?;
            n
        }
        K::FormattedValue { value, conversion, format_spec } => {
            let n = NodeBuilder::new("FormattedValue")?;
            n.set("value", expr_to_py(value)?)?;
            n.set("conversion", int::new_int(*conversion as i64))?;
            n.set("format_spec", opt(format_spec, boxed_expr)?)?;
            n
        }
        K::Interpolation { value, str, conversion, format_spec } => {
            let n = NodeBuilder::new("Interpolation")?;
            n.set("value", expr_to_py(value)?)?;
            n.set("str", pystr_obj(str))?;
            n.set("conversion", int::new_int(*conversion as i64))?;
            n.set("format_spec", opt(format_spec, boxed_expr)?)?;
            n
        }
        K::JoinedStr { values } | K::TemplateStr { values } => {
            let n = NodeBuilder::new(e.class_name())?;
            n.set("values", seq(values, expr_to_py)?)?;
            n
        }
        K::Constant { value, kind } => {
            let n = NodeBuilder::new("Constant")?;
            n.set("value", const_obj(value)?)?;
            n.set("kind", opt_str(kind))?;
            n
        }
        K::Attribute { value, attr, ctx } => {
            let n = NodeBuilder::new("Attribute")?;
            n.set("value", expr_to_py(value)?)?;
            n.set("attr", ident(attr))?;
            n.set("ctx", singleton(ctx.name())?)?;
            n
        }
        K::Subscript { value, slice, ctx } => {
            let n = NodeBuilder::new("Subscript")?;
            n.set("value", expr_to_py(value)?)?;
            n.set("slice", expr_to_py(slice)?)?;
            n.set("ctx", singleton(ctx.name())?)?;
            n
        }
        K::Starred { value, ctx } => {
            let n = NodeBuilder::new("Starred")?;
            n.set("value", expr_to_py(value)?)?;
            n.set("ctx", singleton(ctx.name())?)?;
            n
        }
        K::Name { id, ctx } => {
            let n = NodeBuilder::new("Name")?;
            n.set("id", ident(id))?;
            n.set("ctx", singleton(ctx.name())?)?;
            n
        }
        K::List { elts, ctx } | K::Tuple { elts, ctx } => {
            let n = NodeBuilder::new(e.class_name())?;
            n.set("elts", seq(elts, expr_to_py)?)?;
            n.set("ctx", singleton(ctx.name())?)?;
            n
        }
        K::Slice { lower, upper, step } => {
            let n = NodeBuilder::new("Slice")?;
            n.set("lower", opt(lower, boxed_expr)?)?;
            n.set("upper", opt(upper, boxed_expr)?)?;
            n.set("step", opt(step, boxed_expr)?)?;
            n
        }
    };
    n.loc(&e.loc)?;
    Ok(n.done())
}

// ======================================================================
// Python objects -> Rust AST
// ======================================================================

struct Conv {
    /// recursion guard
    depth: u32,
}

const MAX_DEPTH: u32 = 3000;

fn type_name_of(o: &PyObject) -> String {
    crate::types::typeobject::tname(o)
}

fn isinstance_cls(o: &PyObject, name: &str) -> PyResult<bool> {
    ab::isinstance(o, cls(name)?)
}

fn node_kind_name(o: &PyObject, candidates: &[&'static str]) -> PyResult<Option<&'static str>> {
    for c in candidates {
        if isinstance_cls(o, c)? {
            return Ok(Some(c));
        }
    }
    Ok(None)
}

fn get_field(o: &PyObject, field: &str) -> PyResult<Option<PyRef>> {
    ab::getattr_opt(o, &pystr::intern_str(field))
}

fn required(o: &PyObject, field: &str, owner: &str) -> PyResult {
    match get_field(o, field)? {
        Some(v) if !v.is(none::none()) => Ok(v),
        _ => raise!(TypeError, "required field \"{}\" missing from {}", field, owner),
    }
}

fn optional(o: &PyObject, field: &str) -> PyResult<Option<PyRef>> {
    match get_field(o, field)? {
        Some(v) if !v.is(none::none()) => Ok(Some(v)),
        _ => Ok(None),
    }
}

fn list_field(o: &PyObject, field: &str, owner: &str) -> PyResult<Vec<PyRef>> {
    match get_field(o, field)? {
        None => raise!(TypeError, "required field \"{}\" missing from {}", field, owner),
        Some(v) => {
            if !list::is_list(&v) {
                raise!(TypeError, "{} field \"{}\" must be a list, not a {}", owner, field, type_name_of(&v));
            }
            Ok(list::list_to_vec(&v))
        }
    }
}

fn to_ident(v: &PyObject) -> PyResult<String> {
    if !pystr::is_str(v) {
        raise!(TypeError, "AST identifier must be of type str");
    }
    Ok(pystr::to_string_lossy(v))
}

fn to_string(v: &PyObject) -> PyResult<String> {
    if !pystr::is_str(v) {
        raise!(TypeError, "AST string must be of type str");
    }
    Ok(pystr::to_string_lossy(v))
}

fn to_i32(v: &PyObject) -> PyResult<i32> {
    if !int::is_int(v) {
        raise!(ValueError, "invalid integer value: {}", ab::repr_string(v)?);
    }
    let x = ab::as_i64(v)?;
    if x < i32::MIN as i64 || x > i32::MAX as i64 {
        raise!(OverflowError, "Python int too large to convert to C int");
    }
    Ok(x as i32)
}

fn req_ident(o: &PyObject, field: &str, owner: &str) -> PyResult<String> {
    to_ident(&*required(o, field, owner)?)
}

fn opt_ident_f(o: &PyObject, field: &str) -> PyResult<Option<String>> {
    match optional(o, field)? {
        Some(v) => Ok(Some(to_ident(&v)?)),
        None => Ok(None),
    }
}

fn opt_string_f(o: &PyObject, field: &str) -> PyResult<Option<String>> {
    match optional(o, field)? {
        Some(v) => Ok(Some(to_string(&v)?)),
        None => Ok(None),
    }
}

fn req_int(o: &PyObject, field: &str, owner: &str) -> PyResult<i32> {
    to_i32(&*required(o, field, owner)?)
}

fn opt_int_f(o: &PyObject, field: &str, default: i32) -> PyResult<i32> {
    match optional(o, field)? {
        Some(v) => to_i32(&v),
        None => Ok(default),
    }
}

fn loc_of(o: &PyObject, owner: &str, end_optional: bool) -> PyResult<Loc> {
    let lineno = req_int(o, "lineno", owner)?;
    let col = req_int(o, "col_offset", owner)?;
    let end_lineno = if end_optional { opt_int_f(o, "end_lineno", lineno)? } else { req_int(o, "end_lineno", owner)? };
    let end_col = if end_optional { opt_int_f(o, "end_col_offset", col)? } else { req_int(o, "end_col_offset", owner)? };
    Ok(Loc { lineno, col_offset: col, end_lineno, end_col_offset: end_col })
}

fn obj_to_pystr(v: &PyObject) -> PyStr {
    match pystr::to_string_strict(v) {
        Some(s) => PyStr::Utf8(s),
        None => {
            let cps: Vec<u32> = match pystr::str_data(v) {
                d => (0..pystr::str_len(v)).map(|i| d.get(i)).collect(),
            };
            PyStr::CodePoints(cps)
        }
    }
}

fn obj_to_constant(v: &PyObject) -> PyResult<Constant> {
    if v.is(none::none()) {
        return Ok(Constant::None);
    }
    if v.is(none::ellipsis()) {
        return Ok(Constant::Ellipsis);
    }
    if crate::types::bool_::is_bool(v) {
        return Ok(Constant::Bool(ab::is_true(v)?));
    }
    if int::is_int(v) {
        if let Some(x) = int::as_i64(v) {
            return Ok(Constant::Int(Int::Small(x)));
        }
        let neg = int::sign(v) < 0;
        let mag = int::int_methods_mag_string(v, 10);
        let mag = mag.trim_start_matches('-').to_string();
        return Ok(Constant::Int(Int::Big(if neg { format!("-{}", mag) } else { mag })));
    }
    if float::is_float(v) {
        return Ok(Constant::Float(float::float_value(v)));
    }
    if crate::types::complex::is_complex(v) {
        let c = crate::types::complex::complex_value(v);
        return Ok(Constant::Complex { real: c.real, imag: c.imag });
    }
    if pystr::is_str(v) {
        return Ok(Constant::Str(obj_to_pystr(v)));
    }
    if bytes::is_bytes(v) {
        return Ok(Constant::Bytes(bytes::bytes_data(v).to_vec()));
    }
    if tuple::is_tuple(v) {
        let mut items = Vec::new();
        for i in 0..tuple::tuple_len(v) {
            items.push(obj_to_constant(tuple::tuple_get(v, i))?);
        }
        return Ok(Constant::Tuple(items));
    }
    if crate::types::set::is_frozenset(v) {
        let mut items = Vec::new();
        for it in crate::types::set::set_items(v) {
            items.push(obj_to_constant(&it)?);
        }
        return Ok(Constant::FrozenSet(items));
    }
    raise!(TypeError, "got an invalid type in Constant: {}", type_name_of(v));
}

const EXPR_KINDS: &[&str] = &[
    "BoolOp", "NamedExpr", "BinOp", "UnaryOp", "Lambda", "IfExp", "Dict", "Set", "ListComp", "SetComp", "DictComp",
    "GeneratorExp", "Await", "Yield", "YieldFrom", "Compare", "Call", "FormattedValue", "Interpolation", "JoinedStr",
    "TemplateStr", "Constant", "Attribute", "Subscript", "Starred", "Name", "List", "Tuple", "Slice",
];

const STMT_KINDS: &[&str] = &[
    "FunctionDef", "AsyncFunctionDef", "ClassDef", "Return", "Delete", "Assign", "TypeAlias", "AugAssign", "AnnAssign",
    "For", "AsyncFor", "While", "If", "With", "AsyncWith", "Match", "Raise", "Try", "TryStar", "Assert", "Import",
    "ImportFrom", "Global", "Nonlocal", "Expr", "Pass", "Break", "Continue",
];

const PATTERN_KINDS: &[&str] =
    &["MatchValue", "MatchSingleton", "MatchSequence", "MatchMapping", "MatchClass", "MatchStar", "MatchAs", "MatchOr"];

fn expected(sort: &str, o: &PyObject) -> PyErr {
    let r = ab::repr_string(o).unwrap_or_else(|_| {
        clear_exc();
        "<repr failed>".into()
    });
    err!(TypeError, "expected some sort of {}, but got {}", sort, r)
}

fn enum_of(o: &PyObject, sort: &str, names: &[&'static str]) -> PyResult<&'static str> {
    match node_kind_name(o, names)? {
        Some(n) => Ok(n),
        None => Err(expected(sort, o)),
    }
}

fn ctx_of(o: &PyObject, owner: &str) -> PyResult<ExprContext> {
    let v = required(o, "ctx", owner)?;
    Ok(match enum_of(&v, "expr_context", &["Load", "Store", "Del"])? {
        "Load" => ExprContext::Load,
        "Store" => ExprContext::Store,
        _ => ExprContext::Del,
    })
}

fn operator_of(v: &PyObject) -> PyResult<Operator> {
    use Operator::*;
    let n = enum_of(
        v,
        "operator",
        &["Add", "Sub", "Mult", "MatMult", "Div", "Mod", "Pow", "LShift", "RShift", "BitOr", "BitXor", "BitAnd", "FloorDiv"],
    )?;
    Ok(match n {
        "Add" => Add,
        "Sub" => Sub,
        "Mult" => Mult,
        "MatMult" => MatMult,
        "Div" => Div,
        "Mod" => Mod,
        "Pow" => Pow,
        "LShift" => LShift,
        "RShift" => RShift,
        "BitOr" => BitOr,
        "BitXor" => BitXor,
        "BitAnd" => BitAnd,
        _ => FloorDiv,
    })
}

fn cmpop_of(v: &PyObject) -> PyResult<CmpOp> {
    use CmpOp::*;
    let n = enum_of(v, "cmpop", &["Eq", "NotEq", "Lt", "LtE", "Gt", "GtE", "Is", "IsNot", "In", "NotIn"])?;
    Ok(match n {
        "Eq" => Eq,
        "NotEq" => NotEq,
        "Lt" => Lt,
        "LtE" => LtE,
        "Gt" => Gt,
        "GtE" => GtE,
        "Is" => Is,
        "IsNot" => IsNot,
        "In" => In,
        _ => NotIn,
    })
}

impl Conv {
    fn enter(&mut self) -> PyResult<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            raise!(RecursionError, "maximum recursion depth exceeded during ast construction");
        }
        Ok(())
    }

    fn exprs(&mut self, o: &PyObject, field: &str, owner: &str) -> PyResult<Vec<Expr>> {
        let items = list_field(o, field, owner)?;
        let mut out = Vec::with_capacity(items.len());
        for it in items {
            out.push(self.expr(&it)?);
        }
        Ok(out)
    }

    fn req_expr(&mut self, o: &PyObject, field: &str, owner: &str) -> PyResult<Box<Expr>> {
        let v = required(o, field, owner)?;
        Ok(Box::new(self.expr(&v)?))
    }

    fn opt_expr(&mut self, o: &PyObject, field: &str) -> PyResult<Option<Box<Expr>>> {
        match optional(o, field)? {
            Some(v) => Ok(Some(Box::new(self.expr(&v)?))),
            None => Ok(None),
        }
    }

    fn stmts(&mut self, o: &PyObject, field: &str, owner: &str) -> PyResult<Vec<Stmt>> {
        let items = list_field(o, field, owner)?;
        let mut out = Vec::with_capacity(items.len());
        for it in items {
            out.push(self.stmt(&it)?);
        }
        Ok(out)
    }

    fn idents(&mut self, o: &PyObject, field: &str, owner: &str) -> PyResult<Vec<String>> {
        let items = list_field(o, field, owner)?;
        items.iter().map(|x| to_ident(x)).collect()
    }

    fn type_params(&mut self, o: &PyObject, owner: &str) -> PyResult<Vec<TypeParam>> {
        // type_params may be missing on nodes created by older code
        let items = match get_field(o, "type_params")? {
            None => return Ok(Vec::new()),
            Some(v) if v.is(none::none()) => return Ok(Vec::new()),
            Some(v) => {
                if !list::is_list(&v) {
                    raise!(TypeError, "{} field \"type_params\" must be a list, not a {}", owner, type_name_of(&v));
                }
                list::list_to_vec(&v)
            }
        };
        let mut out = Vec::new();
        for it in items {
            let kind = enum_of(&it, "type_param", &["TypeVar", "ParamSpec", "TypeVarTuple"])?;
            let name = req_ident(&it, "name", kind)?;
            let default_value = self.opt_expr(&it, "default_value")?;
            let node = match kind {
                "TypeVar" => TypeParamKind::TypeVar { name, bound: self.opt_expr(&it, "bound")?, default_value },
                "ParamSpec" => TypeParamKind::ParamSpec { name, default_value },
                _ => TypeParamKind::TypeVarTuple { name, default_value },
            };
            out.push(TypeParam { node, loc: loc_of(&it, kind, false)? });
        }
        Ok(out)
    }

    fn arg(&mut self, o: &PyObject) -> PyResult<Arg> {
        if !isinstance_cls(o, "arg")? {
            return Err(expected("arg", o));
        }
        Ok(Arg {
            arg: req_ident(o, "arg", "arg")?,
            annotation: self.opt_expr(o, "annotation")?,
            type_comment: opt_string_f(o, "type_comment")?,
            loc: loc_of(o, "arg", true)?,
        })
    }

    fn args_list(&mut self, o: &PyObject, field: &str) -> PyResult<Vec<Arg>> {
        let items = list_field(o, field, "arguments")?;
        let mut out = Vec::new();
        for it in items {
            out.push(self.arg(&it)?);
        }
        Ok(out)
    }

    fn arguments(&mut self, o: &PyObject) -> PyResult<Box<Arguments>> {
        if !isinstance_cls(o, "arguments")? {
            return Err(expected("arguments", o));
        }
        let posonlyargs = self.args_list(o, "posonlyargs")?;
        let args = self.args_list(o, "args")?;
        let vararg = match optional(o, "vararg")? {
            Some(v) => Some(Box::new(self.arg(&v)?)),
            None => None,
        };
        let kwonlyargs = self.args_list(o, "kwonlyargs")?;
        let kwd = list_field(o, "kw_defaults", "arguments")?;
        let mut kw_defaults = Vec::new();
        for it in kwd {
            kw_defaults.push(if it.is(none::none()) { None } else { Some(self.expr(&it)?) });
        }
        let kwarg = match optional(o, "kwarg")? {
            Some(v) => Some(Box::new(self.arg(&v)?)),
            None => None,
        };
        let defaults = self.exprs(o, "defaults", "arguments")?;
        Ok(Box::new(Arguments { posonlyargs, args, vararg, kwonlyargs, kw_defaults, kwarg, defaults }))
    }

    fn keywords(&mut self, o: &PyObject, field: &str, owner: &str) -> PyResult<Vec<Keyword>> {
        let items = list_field(o, field, owner)?;
        let mut out = Vec::new();
        for it in items {
            if !isinstance_cls(&it, "keyword")? {
                return Err(expected("keyword", &it));
            }
            out.push(Keyword {
                arg: opt_ident_f(&it, "arg")?,
                value: *self.req_expr(&it, "value", "keyword")?,
                loc: loc_of(&it, "keyword", true)?,
            });
        }
        Ok(out)
    }

    fn comprehensions(&mut self, o: &PyObject, owner: &str) -> PyResult<Vec<Comprehension>> {
        let items = list_field(o, "generators", owner)?;
        let mut out = Vec::new();
        for it in items {
            if !isinstance_cls(&it, "comprehension")? {
                return Err(expected("comprehension", &it));
            }
            out.push(Comprehension {
                target: *self.req_expr(&it, "target", "comprehension")?,
                iter: *self.req_expr(&it, "iter", "comprehension")?,
                ifs: self.exprs(&it, "ifs", "comprehension")?,
                is_async: req_int(&it, "is_async", "comprehension")?,
            });
        }
        Ok(out)
    }

    fn conversion(&mut self, o: &PyObject, owner: &str) -> PyResult<i32> {
        req_int(o, "conversion", owner)
    }

    fn expr(&mut self, o: &PyObject) -> PyResult<Expr> {
        self.enter()?;
        let r = self.expr_inner(o);
        self.depth -= 1;
        r
    }

    fn expr_inner(&mut self, o: &PyObject) -> PyResult<Expr> {
        use ExprKind as K;
        let kind = match node_kind_name(o, EXPR_KINDS)? {
            Some(k) => k,
            None => return Err(expected("expr", o)),
        };
        let node = match kind {
            "BoolOp" => {
                let opv = required(o, "op", kind)?;
                let op = match enum_of(&opv, "boolop", &["And", "Or"])? {
                    "And" => BoolOp::And,
                    _ => BoolOp::Or,
                };
                K::BoolOp { op, values: self.exprs(o, "values", kind)? }
            }
            "NamedExpr" => K::NamedExpr { target: self.req_expr(o, "target", kind)?, value: self.req_expr(o, "value", kind)? },
            "BinOp" => K::BinOp {
                left: self.req_expr(o, "left", kind)?,
                op: operator_of(&*required(o, "op", kind)?)?,
                right: self.req_expr(o, "right", kind)?,
            },
            "UnaryOp" => {
                let opv = required(o, "op", kind)?;
                let op = match enum_of(&opv, "unaryop", &["Invert", "Not", "UAdd", "USub"])? {
                    "Invert" => UnaryOp::Invert,
                    "Not" => UnaryOp::Not,
                    "UAdd" => UnaryOp::UAdd,
                    _ => UnaryOp::USub,
                };
                K::UnaryOp { op, operand: self.req_expr(o, "operand", kind)? }
            }
            "Lambda" => {
                let a = required(o, "args", kind)?;
                K::Lambda { args: self.arguments(&a)?, body: self.req_expr(o, "body", kind)? }
            }
            "IfExp" => K::IfExp {
                test: self.req_expr(o, "test", kind)?,
                body: self.req_expr(o, "body", kind)?,
                orelse: self.req_expr(o, "orelse", kind)?,
            },
            "Dict" => {
                let ks = list_field(o, "keys", kind)?;
                let mut keys = Vec::new();
                for k in ks {
                    keys.push(if k.is(none::none()) { None } else { Some(self.expr(&k)?) });
                }
                K::Dict { keys, values: self.exprs(o, "values", kind)? }
            }
            "Set" => K::Set { elts: self.exprs(o, "elts", kind)? },
            "ListComp" => K::ListComp { elt: self.req_expr(o, "elt", kind)?, generators: self.comprehensions(o, kind)? },
            "SetComp" => K::SetComp { elt: self.req_expr(o, "elt", kind)?, generators: self.comprehensions(o, kind)? },
            "GeneratorExp" => K::GeneratorExp { elt: self.req_expr(o, "elt", kind)?, generators: self.comprehensions(o, kind)? },
            "DictComp" => K::DictComp {
                key: self.req_expr(o, "key", kind)?,
                value: self.opt_expr(o, "value")?,
                generators: self.comprehensions(o, kind)?,
            },
            "Await" => K::Await { value: self.req_expr(o, "value", kind)? },
            "Yield" => K::Yield { value: self.opt_expr(o, "value")? },
            "YieldFrom" => K::YieldFrom { value: self.req_expr(o, "value", kind)? },
            "Compare" => {
                let left = self.req_expr(o, "left", kind)?;
                let ops_l = list_field(o, "ops", kind)?;
                let mut ops = Vec::new();
                for op in ops_l {
                    ops.push(cmpop_of(&op)?);
                }
                K::Compare { left, ops, comparators: self.exprs(o, "comparators", kind)? }
            }
            "Call" => K::Call {
                func: self.req_expr(o, "func", kind)?,
                args: self.exprs(o, "args", kind)?,
                keywords: self.keywords(o, "keywords", kind)?,
            },
            "FormattedValue" => K::FormattedValue {
                value: self.req_expr(o, "value", kind)?,
                conversion: self.conversion(o, kind)?,
                format_spec: self.opt_expr(o, "format_spec")?,
            },
            "Interpolation" => {
                let sv = required(o, "str", kind)?;
                let sstr = if pystr::is_str(&sv) { obj_to_pystr(&sv) } else { PyStr::Utf8(ab::str_string(&sv)?) };
                K::Interpolation {
                    value: self.req_expr(o, "value", kind)?,
                    str: sstr,
                    conversion: self.conversion(o, kind)?,
                    format_spec: self.opt_expr(o, "format_spec")?,
                }
            }
            "JoinedStr" => K::JoinedStr { values: self.exprs(o, "values", kind)? },
            "TemplateStr" => K::TemplateStr { values: self.exprs(o, "values", kind)? },
            "Constant" => {
                let v = match get_field(o, "value")? {
                    Some(v) => v,
                    None => raise!(TypeError, "required field \"value\" missing from Constant"),
                };
                K::Constant { value: obj_to_constant(&v)?, kind: opt_string_f(o, "kind")? }
            }
            "Attribute" => K::Attribute {
                value: self.req_expr(o, "value", kind)?,
                attr: req_ident(o, "attr", kind)?,
                ctx: ctx_of(o, kind)?,
            },
            "Subscript" => K::Subscript {
                value: self.req_expr(o, "value", kind)?,
                slice: self.req_expr(o, "slice", kind)?,
                ctx: ctx_of(o, kind)?,
            },
            "Starred" => K::Starred { value: self.req_expr(o, "value", kind)?, ctx: ctx_of(o, kind)? },
            "Name" => K::Name { id: req_ident(o, "id", kind)?, ctx: ctx_of(o, kind)? },
            "List" => K::List { elts: self.exprs(o, "elts", kind)?, ctx: ctx_of(o, kind)? },
            "Tuple" => K::Tuple { elts: self.exprs(o, "elts", kind)?, ctx: ctx_of(o, kind)? },
            _ => K::Slice {
                lower: self.opt_expr(o, "lower")?,
                upper: self.opt_expr(o, "upper")?,
                step: self.opt_expr(o, "step")?,
            },
        };
        Ok(Expr { node, loc: loc_of(o, "expr", true)? })
    }

    fn pattern(&mut self, o: &PyObject) -> PyResult<Pattern> {
        self.enter()?;
        let r = self.pattern_inner(o);
        self.depth -= 1;
        r
    }

    fn patterns(&mut self, o: &PyObject, field: &str, owner: &str) -> PyResult<Vec<Pattern>> {
        let items = list_field(o, field, owner)?;
        let mut out = Vec::new();
        for it in items {
            out.push(self.pattern(&it)?);
        }
        Ok(out)
    }

    fn pattern_inner(&mut self, o: &PyObject) -> PyResult<Pattern> {
        use PatternKind as K;
        let kind = match node_kind_name(o, PATTERN_KINDS)? {
            Some(k) => k,
            None => return Err(expected("pattern", o)),
        };
        let node = match kind {
            "MatchValue" => K::MatchValue { value: self.req_expr(o, "value", kind)? },
            "MatchSingleton" => {
                let v = match get_field(o, "value")? {
                    Some(v) => v,
                    None => raise!(TypeError, "required field \"value\" missing from MatchSingleton"),
                };
                K::MatchSingleton { value: obj_to_constant(&v)? }
            }
            "MatchSequence" => K::MatchSequence { patterns: self.patterns(o, "patterns", kind)? },
            "MatchMapping" => K::MatchMapping {
                keys: self.exprs(o, "keys", kind)?,
                patterns: self.patterns(o, "patterns", kind)?,
                rest: opt_ident_f(o, "rest")?,
            },
            "MatchClass" => K::MatchClass {
                cls: self.req_expr(o, "cls", kind)?,
                patterns: self.patterns(o, "patterns", kind)?,
                kwd_attrs: self.idents(o, "kwd_attrs", kind)?,
                kwd_patterns: self.patterns(o, "kwd_patterns", kind)?,
            },
            "MatchStar" => K::MatchStar { name: opt_ident_f(o, "name")? },
            "MatchAs" => K::MatchAs {
                pattern: match optional(o, "pattern")? {
                    Some(p) => Some(Box::new(self.pattern(&p)?)),
                    None => None,
                },
                name: opt_ident_f(o, "name")?,
            },
            _ => K::MatchOr { patterns: self.patterns(o, "patterns", kind)? },
        };
        Ok(Pattern { node, loc: loc_of(o, "pattern", false)? })
    }

    fn funcdef(&mut self, o: &PyObject, kind: &str) -> PyResult<FunctionDef> {
        let a = required(o, "args", kind)?;
        Ok(FunctionDef {
            name: req_ident(o, "name", kind)?,
            args: self.arguments(&a)?,
            body: self.stmts(o, "body", kind)?,
            decorator_list: self.exprs(o, "decorator_list", kind)?,
            returns: self.opt_expr(o, "returns")?,
            type_comment: opt_string_f(o, "type_comment")?,
            type_params: self.type_params(o, kind)?,
        })
    }

    fn for_(&mut self, o: &PyObject, kind: &str) -> PyResult<For> {
        Ok(For {
            target: self.req_expr(o, "target", kind)?,
            iter: self.req_expr(o, "iter", kind)?,
            body: self.stmts(o, "body", kind)?,
            orelse: self.stmts(o, "orelse", kind)?,
            type_comment: opt_string_f(o, "type_comment")?,
        })
    }

    fn with_(&mut self, o: &PyObject, kind: &str) -> PyResult<With> {
        let items_l = list_field(o, "items", kind)?;
        let mut items = Vec::new();
        for it in items_l {
            if !isinstance_cls(&it, "withitem")? {
                return Err(expected("withitem", &it));
            }
            items.push(WithItem {
                context_expr: *self.req_expr(&it, "context_expr", "withitem")?,
                optional_vars: self.opt_expr(&it, "optional_vars")?,
            });
        }
        Ok(With { items, body: self.stmts(o, "body", kind)?, type_comment: opt_string_f(o, "type_comment")? })
    }

    fn try_(&mut self, o: &PyObject, kind: &str) -> PyResult<Try> {
        let hl = list_field(o, "handlers", kind)?;
        let mut handlers = Vec::new();
        for h in hl {
            if !isinstance_cls(&h, "ExceptHandler")? {
                return Err(expected("excepthandler", &h));
            }
            handlers.push(ExceptHandler {
                type_: self.opt_expr(&h, "type")?,
                name: opt_ident_f(&h, "name")?,
                body: self.stmts(&h, "body", "ExceptHandler")?,
                loc: loc_of(&h, "excepthandler", true)?,
            });
        }
        Ok(Try {
            body: self.stmts(o, "body", kind)?,
            handlers,
            orelse: self.stmts(o, "orelse", kind)?,
            finalbody: self.stmts(o, "finalbody", kind)?,
        })
    }

    fn aliases(&mut self, o: &PyObject, owner: &str) -> PyResult<Vec<Alias>> {
        let l = list_field(o, "names", owner)?;
        let mut out = Vec::new();
        for it in l {
            if !isinstance_cls(&it, "alias")? {
                return Err(expected("alias", &it));
            }
            out.push(Alias { name: req_ident(&it, "name", "alias")?, asname: opt_ident_f(&it, "asname")?, loc: loc_of(&it, "alias", true)? });
        }
        Ok(out)
    }

    fn stmt(&mut self, o: &PyObject) -> PyResult<Stmt> {
        self.enter()?;
        let r = self.stmt_inner(o);
        self.depth -= 1;
        r
    }

    fn stmt_inner(&mut self, o: &PyObject) -> PyResult<Stmt> {
        use StmtKind as K;
        let kind = match node_kind_name(o, STMT_KINDS)? {
            Some(k) => k,
            None => return Err(expected("stmt", o)),
        };
        let node = match kind {
            "FunctionDef" => K::FunctionDef(self.funcdef(o, kind)?),
            "AsyncFunctionDef" => K::AsyncFunctionDef(self.funcdef(o, kind)?),
            "ClassDef" => K::ClassDef {
                name: req_ident(o, "name", kind)?,
                bases: self.exprs(o, "bases", kind)?,
                keywords: self.keywords(o, "keywords", kind)?,
                body: self.stmts(o, "body", kind)?,
                decorator_list: self.exprs(o, "decorator_list", kind)?,
                type_params: self.type_params(o, kind)?,
            },
            "Return" => K::Return { value: self.opt_expr(o, "value")? },
            "Delete" => K::Delete { targets: self.exprs(o, "targets", kind)? },
            "Assign" => K::Assign {
                targets: self.exprs(o, "targets", kind)?,
                value: self.req_expr(o, "value", kind)?,
                type_comment: opt_string_f(o, "type_comment")?,
            },
            "TypeAlias" => K::TypeAlias {
                name: self.req_expr(o, "name", kind)?,
                type_params: self.type_params(o, kind)?,
                value: self.req_expr(o, "value", kind)?,
            },
            "AugAssign" => K::AugAssign {
                target: self.req_expr(o, "target", kind)?,
                op: operator_of(&*required(o, "op", kind)?)?,
                value: self.req_expr(o, "value", kind)?,
            },
            "AnnAssign" => K::AnnAssign {
                target: self.req_expr(o, "target", kind)?,
                annotation: self.req_expr(o, "annotation", kind)?,
                value: self.opt_expr(o, "value")?,
                simple: req_int(o, "simple", kind)?,
            },
            "For" => K::For(self.for_(o, kind)?),
            "AsyncFor" => K::AsyncFor(self.for_(o, kind)?),
            "While" => K::While {
                test: self.req_expr(o, "test", kind)?,
                body: self.stmts(o, "body", kind)?,
                orelse: self.stmts(o, "orelse", kind)?,
            },
            "If" => K::If {
                test: self.req_expr(o, "test", kind)?,
                body: self.stmts(o, "body", kind)?,
                orelse: self.stmts(o, "orelse", kind)?,
            },
            "With" => K::With(self.with_(o, kind)?),
            "AsyncWith" => K::AsyncWith(self.with_(o, kind)?),
            "Match" => {
                let subject = self.req_expr(o, "subject", kind)?;
                let cl = list_field(o, "cases", kind)?;
                let mut cases = Vec::new();
                for c in cl {
                    if !isinstance_cls(&c, "match_case")? {
                        return Err(expected("match_case", &c));
                    }
                    let p = required(&c, "pattern", "match_case")?;
                    cases.push(MatchCase {
                        pattern: self.pattern(&p)?,
                        guard: self.opt_expr(&c, "guard")?,
                        body: self.stmts(&c, "body", "match_case")?,
                    });
                }
                K::Match { subject, cases }
            }
            "Raise" => K::Raise { exc: self.opt_expr(o, "exc")?, cause: self.opt_expr(o, "cause")? },
            "Try" => K::Try(self.try_(o, kind)?),
            "TryStar" => K::TryStar(self.try_(o, kind)?),
            "Assert" => K::Assert { test: self.req_expr(o, "test", kind)?, msg: self.opt_expr(o, "msg")? },
            "Import" => K::Import { names: self.aliases(o, kind)?, is_lazy: opt_int_f(o, "is_lazy", 0)? },
            "ImportFrom" => K::ImportFrom {
                module: opt_ident_f(o, "module")?,
                names: self.aliases(o, kind)?,
                level: opt_int_f(o, "level", 0)?,
                is_lazy: opt_int_f(o, "is_lazy", 0)?,
            },
            "Global" => K::Global { names: self.idents(o, "names", kind)? },
            "Nonlocal" => K::Nonlocal { names: self.idents(o, "names", kind)? },
            "Expr" => K::Expr { value: self.req_expr(o, "value", kind)? },
            "Pass" => K::Pass,
            "Break" => K::Break,
            _ => K::Continue,
        };
        Ok(Stmt { node, loc: loc_of(o, "stmt", true)? })
    }

    fn module(&mut self, o: &PyObject, mode: crate::compile::Mode) -> PyResult<Mod> {
        use crate::compile::Mode as M;
        let (expected_cls, mode_name) = match mode {
            M::Exec => ("Module", "exec"),
            M::Eval => ("Expression", "eval"),
            M::Single => ("Interactive", "single"),
            M::FuncType => ("FunctionType", "func_type"),
        };
        if !isinstance_cls(o, expected_cls)? {
            let got = ab::getattr_str(&*ab::getattr_str(o, "__class__")?, "__name__")?;
            raise!(TypeError, "expected {} node, got {}", expected_cls, ab::str_string(&got)?);
        }
        let _ = mode_name;
        Ok(match mode {
            M::Exec => {
                let body = self.stmts(o, "body", "Module")?;
                let til = list_field(o, "type_ignores", "Module")?;
                let mut type_ignores = Vec::new();
                for t in til {
                    if !isinstance_cls(&t, "TypeIgnore")? {
                        return Err(expected("type_ignore", &t));
                    }
                    type_ignores.push(TypeIgnore { lineno: req_int(&t, "lineno", "TypeIgnore")?, tag: to_string(&*required(&t, "tag", "TypeIgnore")?)? });
                }
                Mod::Module { body, type_ignores }
            }
            M::Eval => Mod::Expression { body: self.req_expr(o, "body", "Expression")? },
            M::Single => Mod::Interactive { body: self.stmts(o, "body", "Interactive")? },
            M::FuncType => Mod::FunctionType {
                argtypes: self.exprs(o, "argtypes", "FunctionType")?,
                returns: self.req_expr(o, "returns", "FunctionType")?,
            },
        })
    }
}

// ======================================================================
// Validation (PyAST_Validate)
// ======================================================================

mod validate {
    use super::*;

    type R = PyResult<()>;

    fn verr(msg: String) -> PyErr {
        err!(ValueError, "{}", msg)
    }

    fn ctx_name(c: ExprContext) -> &'static str {
        c.name()
    }

    fn check_ctx(actual: ExprContext, expected: ExprContext) -> R {
        if actual != expected {
            return Err(verr(format!(
                "expression must have {} context but has {} instead",
                ctx_name(expected),
                ctx_name(actual)
            )));
        }
        Ok(())
    }

    fn nonempty_seq<T>(v: &[T], what: &str, owner: &str) -> R {
        if v.is_empty() {
            return Err(verr(format!("empty {} on {}", what, owner)));
        }
        Ok(())
    }

    fn name_ok(n: &str) -> R {
        if n == "None" || n == "True" || n == "False" {
            return Err(verr(format!("identifier field can't represent '{}' constant", n)));
        }
        Ok(())
    }

    pub fn module(m: &Mod) -> R {
        match m {
            Mod::Module { body, .. } => stmts(body),
            Mod::Interactive { body } => stmts(body),
            Mod::Expression { body } => expr(body, ExprContext::Load),
            Mod::FunctionType { argtypes, returns } => {
                exprs(argtypes, ExprContext::Load)?;
                expr(returns, ExprContext::Load)
            }
        }
    }

    fn stmts(v: &[Stmt]) -> R {
        for s in v {
            stmt(s)?;
        }
        Ok(())
    }

    fn body(v: &[Stmt], owner: &str) -> R {
        nonempty_seq(v, "body", owner)?;
        stmts(v)
    }

    fn exprs(v: &[Expr], ctx: ExprContext) -> R {
        for e in v {
            expr(e, ctx)?;
        }
        Ok(())
    }

    fn opt_expr(v: &Option<Box<Expr>>, ctx: ExprContext) -> R {
        if let Some(e) = v {
            expr(e, ctx)?;
        }
        Ok(())
    }

    fn arguments(a: &Arguments) -> R {
        for x in a.posonlyargs.iter().chain(a.args.iter()).chain(a.kwonlyargs.iter()) {
            if let Some(ann) = &x.annotation {
                expr(ann, ExprContext::Load)?;
            }
        }
        if let Some(v) = &a.vararg {
            if let Some(ann) = &v.annotation {
                expr(ann, ExprContext::Load)?;
            }
        }
        if let Some(v) = &a.kwarg {
            if let Some(ann) = &v.annotation {
                expr(ann, ExprContext::Load)?;
            }
        }
        if a.defaults.len() > a.posonlyargs.len() + a.args.len() {
            return Err(verr("more positional defaults than args on arguments".into()));
        }
        if a.kw_defaults.len() != a.kwonlyargs.len() {
            return Err(verr("length of kwonlyargs is not the same as kw_defaults on arguments".into()));
        }
        exprs(&a.defaults, ExprContext::Load)?;
        for d in a.kw_defaults.iter().flatten() {
            expr(d, ExprContext::Load)?;
        }
        Ok(())
    }

    fn type_params(tps: &[TypeParam]) -> R {
        for tp in tps {
            match &tp.node {
                TypeParamKind::TypeVar { name, bound, default_value } => {
                    name_ok(name)?;
                    opt_expr(bound, ExprContext::Load)?;
                    opt_expr(default_value, ExprContext::Load)?;
                }
                TypeParamKind::ParamSpec { name, default_value } | TypeParamKind::TypeVarTuple { name, default_value } => {
                    name_ok(name)?;
                    opt_expr(default_value, ExprContext::Load)?;
                }
            }
        }
        Ok(())
    }

    fn stmt(s: &Stmt) -> R {
        use StmtKind as K;
        match &s.node {
            K::FunctionDef(f) | K::AsyncFunctionDef(f) => {
                body(&f.body, s.class_name())?;
                type_params(&f.type_params)?;
                arguments(&f.args)?;
                exprs(&f.decorator_list, ExprContext::Load)?;
                opt_expr(&f.returns, ExprContext::Load)
            }
            K::ClassDef { bases, keywords, body: b, decorator_list, type_params: tps, .. } => {
                body(b, "ClassDef")?;
                type_params(tps)?;
                exprs(bases, ExprContext::Load)?;
                for k in keywords {
                    expr(&k.value, ExprContext::Load)?;
                }
                exprs(decorator_list, ExprContext::Load)
            }
            K::Return { value } => opt_expr(value, ExprContext::Load),
            K::Delete { targets } => {
                nonempty_seq(targets, "targets", "Delete")?;
                exprs(targets, ExprContext::Del)
            }
            K::Assign { targets, value, .. } => {
                nonempty_seq(targets, "targets", "Assign")?;
                exprs(targets, ExprContext::Store)?;
                expr(value, ExprContext::Load)
            }
            K::TypeAlias { name, type_params: tps, value } => {
                if !matches!(name.node, ExprKind::Name { .. }) {
                    return Err(err!(TypeError, "TypeAlias with non-Name name"));
                }
                expr(name, ExprContext::Store)?;
                type_params(tps)?;
                expr(value, ExprContext::Load)
            }
            K::AugAssign { target, value, .. } => {
                expr(target, ExprContext::Store)?;
                expr(value, ExprContext::Load)
            }
            K::AnnAssign { target, annotation, value, simple } => {
                if *simple != 0 && !matches!(target.node, ExprKind::Name { .. }) {
                    return Err(err!(TypeError, "AnnAssign with simple non-Name target"));
                }
                expr(target, ExprContext::Store)?;
                opt_expr(value, ExprContext::Load)?;
                expr(annotation, ExprContext::Load)
            }
            K::For(f) | K::AsyncFor(f) => {
                expr(&f.target, ExprContext::Store)?;
                expr(&f.iter, ExprContext::Load)?;
                body(&f.body, s.class_name())?;
                stmts(&f.orelse)
            }
            K::While { test, body: b, orelse } => {
                expr(test, ExprContext::Load)?;
                body(b, "While")?;
                stmts(orelse)
            }
            K::If { test, body: b, orelse } => {
                expr(test, ExprContext::Load)?;
                body(b, "If")?;
                stmts(orelse)
            }
            K::With(w) | K::AsyncWith(w) => {
                nonempty_seq(&w.items, "items", s.class_name())?;
                for it in &w.items {
                    expr(&it.context_expr, ExprContext::Load)?;
                    opt_expr(&it.optional_vars, ExprContext::Store)?;
                }
                body(&w.body, s.class_name())
            }
            K::Match { subject, cases } => {
                expr(subject, ExprContext::Load)?;
                nonempty_seq(cases, "cases", "Match")?;
                for c in cases {
                    pattern(&c.pattern, false)?;
                    opt_expr(&c.guard, ExprContext::Load)?;
                    body(&c.body, "match_case")?;
                }
                Ok(())
            }
            K::Raise { exc, cause } => {
                if exc.is_none() && cause.is_some() {
                    return Err(verr("Raise with cause but no exception".into()));
                }
                opt_expr(exc, ExprContext::Load)?;
                opt_expr(cause, ExprContext::Load)
            }
            K::Try(t) | K::TryStar(t) => {
                let owner = s.class_name();
                body(&t.body, owner)?;
                if t.handlers.is_empty() && t.finalbody.is_empty() {
                    return Err(verr(format!("{} has neither except handlers nor finalbody", owner)));
                }
                if t.handlers.is_empty() && !t.orelse.is_empty() {
                    return Err(verr(format!("{} has orelse but no except handlers", owner)));
                }
                for h in &t.handlers {
                    opt_expr(&h.type_, ExprContext::Load)?;
                    body(&h.body, "ExceptHandler")?;
                }
                stmts(&t.finalbody)?;
                stmts(&t.orelse)
            }
            K::Assert { test, msg } => {
                expr(test, ExprContext::Load)?;
                opt_expr(msg, ExprContext::Load)
            }
            K::Import { names, .. } => nonempty_seq(names, "names", "Import"),
            K::ImportFrom { names, level, .. } => {
                if *level < 0 {
                    return Err(verr("Negative ImportFrom level".into()));
                }
                nonempty_seq(names, "names", "ImportFrom")
            }
            K::Global { names } => nonempty_seq(names, "names", "Global"),
            K::Nonlocal { names } => nonempty_seq(names, "names", "Nonlocal"),
            K::Expr { value } => expr(value, ExprContext::Load),
            K::Pass | K::Break | K::Continue => Ok(()),
        }
    }

    fn comprehension(gens: &[Comprehension], owner: &str) -> R {
        if gens.is_empty() {
            return Err(verr(format!("comprehension with no generators")));
        }
        let _ = owner;
        for g in gens {
            expr(&g.target, ExprContext::Store)?;
            expr(&g.iter, ExprContext::Load)?;
            exprs(&g.ifs, ExprContext::Load)?;
        }
        Ok(())
    }

    fn constant_ok(c: &Constant) -> bool {
        match c {
            Constant::Tuple(items) | Constant::FrozenSet(items) => items.iter().all(constant_ok),
            _ => true,
        }
    }

    pub fn expr(e: &Expr, ctx: ExprContext) -> R {
        use ExprKind as K;
        let mut actual_ctx = None;
        match &e.node {
            K::Attribute { ctx: c, .. } | K::Subscript { ctx: c, .. } | K::Starred { ctx: c, .. } | K::List { ctx: c, .. } | K::Tuple { ctx: c, .. } => {
                actual_ctx = Some(*c)
            }
            K::Name { id, ctx: c } => {
                name_ok(id)?;
                actual_ctx = Some(*c);
            }
            _ => {
                if ctx != ExprContext::Load {
                    return Err(verr(format!("expression which can't be assigned to in {} context", ctx_name(ctx))));
                }
            }
        }
        if let Some(c) = actual_ctx {
            check_ctx(c, ctx)?;
        }
        match &e.node {
            K::BoolOp { values, .. } => {
                if values.len() < 2 {
                    return Err(verr("BoolOp with less than 2 values".into()));
                }
                exprs(values, ExprContext::Load)
            }
            K::NamedExpr { target, value } => {
                if !matches!(target.node, K::Name { .. }) {
                    return Err(err!(TypeError, "NamedExpr target must be a Name"));
                }
                expr(value, ExprContext::Load)
            }
            K::BinOp { left, right, .. } => {
                expr(left, ExprContext::Load)?;
                expr(right, ExprContext::Load)
            }
            K::UnaryOp { operand, .. } => expr(operand, ExprContext::Load),
            K::Lambda { args, body } => {
                arguments(args)?;
                expr(body, ExprContext::Load)
            }
            K::IfExp { test, body, orelse } => {
                expr(test, ExprContext::Load)?;
                expr(body, ExprContext::Load)?;
                expr(orelse, ExprContext::Load)
            }
            K::Dict { keys, values } => {
                if keys.len() != values.len() {
                    return Err(verr("Dict doesn't have the same number of keys as values".into()));
                }
                for k in keys.iter().flatten() {
                    expr(k, ExprContext::Load)?;
                }
                exprs(values, ExprContext::Load)
            }
            K::Set { elts } => exprs(elts, ExprContext::Load),
            K::ListComp { elt, generators } | K::SetComp { elt, generators } | K::GeneratorExp { elt, generators } => {
                comprehension(generators, e.class_name())?;
                expr(elt, ExprContext::Load)
            }
            K::DictComp { key, value, generators } => {
                comprehension(generators, "DictComp")?;
                expr(key, ExprContext::Load)?;
                opt_expr(value, ExprContext::Load)
            }
            K::Await { value } | K::YieldFrom { value } => expr(value, ExprContext::Load),
            K::Yield { value } => opt_expr(value, ExprContext::Load),
            K::Compare { left, ops, comparators } => {
                if comparators.is_empty() {
                    return Err(verr("Compare with no comparators".into()));
                }
                if comparators.len() != ops.len() {
                    return Err(verr("Compare has a different number of comparators and operands".into()));
                }
                exprs(comparators, ExprContext::Load)?;
                expr(left, ExprContext::Load)
            }
            K::Call { func, args, keywords } => {
                expr(func, ExprContext::Load)?;
                exprs(args, ExprContext::Load)?;
                for k in keywords {
                    expr(&k.value, ExprContext::Load)?;
                }
                Ok(())
            }
            K::FormattedValue { value, format_spec, .. } | K::Interpolation { value, format_spec, .. } => {
                expr(value, ExprContext::Load)?;
                opt_expr(format_spec, ExprContext::Load)
            }
            K::JoinedStr { values } | K::TemplateStr { values } => exprs(values, ExprContext::Load),
            K::Constant { value, .. } => {
                if !constant_ok(value) {
                    return Err(err!(TypeError, "got an invalid type in Constant"));
                }
                Ok(())
            }
            K::Attribute { value, .. } => expr(value, ExprContext::Load),
            K::Subscript { value, slice, .. } => {
                expr(slice, ExprContext::Load)?;
                expr(value, ExprContext::Load)
            }
            K::Starred { value, .. } => expr(value, ctx),
            K::Slice { lower, upper, step } => {
                opt_expr(lower, ExprContext::Load)?;
                opt_expr(upper, ExprContext::Load)?;
                opt_expr(step, ExprContext::Load)
            }
            K::List { elts, .. } | K::Tuple { elts, .. } => exprs(elts, ctx),
            K::Name { .. } => Ok(()),
        }
    }

    fn pattern(p: &Pattern, star_ok: bool) -> R {
        use PatternKind as K;
        match &p.node {
            K::MatchValue { value } => {
                expr(value, ExprContext::Load)?;
                match &value.node {
                    ExprKind::Constant { value: c, .. } => {
                        if matches!(c, Constant::None | Constant::Bool(_)) {
                            return Err(verr("patterns may only match literals and attribute lookups".into()));
                        }
                        Ok(())
                    }
                    ExprKind::Attribute { .. } | ExprKind::UnaryOp { .. } | ExprKind::BinOp { .. } => Ok(()),
                    ExprKind::JoinedStr { .. } => Err(verr("patterns may only match literals and attribute lookups".into())),
                    _ => Err(verr("patterns may only match literals and attribute lookups".into())),
                }
            }
            K::MatchSingleton { value } => {
                if !matches!(value, Constant::None | Constant::Bool(_)) {
                    return Err(verr("MatchSingleton can only contain True, False and None".into()));
                }
                Ok(())
            }
            K::MatchSequence { patterns } => {
                for q in patterns {
                    pattern(q, true)?;
                }
                Ok(())
            }
            K::MatchMapping { keys, patterns, rest } => {
                if keys.len() != patterns.len() {
                    return Err(verr("MatchMapping doesn't have the same number of keys as patterns".into()));
                }
                if let Some(r) = rest {
                    name_ok(r)?;
                }
                for k in keys {
                    expr(k, ExprContext::Load)?;
                }
                for q in patterns {
                    pattern(q, false)?;
                }
                Ok(())
            }
            K::MatchClass { cls, patterns, kwd_attrs, kwd_patterns } => {
                expr(cls, ExprContext::Load)?;
                let mut c = &**cls;
                loop {
                    match &c.node {
                        ExprKind::Name { .. } => break,
                        ExprKind::Attribute { value, .. } => c = value,
                        _ => return Err(verr("MatchClass cls field can only contain Name or Attribute nodes.".into())),
                    }
                }
                if kwd_attrs.len() != kwd_patterns.len() {
                    return Err(verr("MatchClass doesn't have the same number of keyword attributes as patterns".into()));
                }
                for a in kwd_attrs {
                    name_ok(a)?;
                }
                for q in patterns.iter().chain(kwd_patterns.iter()) {
                    pattern(q, false)?;
                }
                Ok(())
            }
            K::MatchStar { name } => {
                if !star_ok {
                    return Err(verr("can't use MatchStar here".into()));
                }
                if let Some(n) = name {
                    name_ok(n)?;
                }
                Ok(())
            }
            K::MatchAs { pattern: q, name } => {
                if let Some(n) = name {
                    name_ok(n)?;
                }
                match q {
                    Some(q) => {
                        if name.is_none() {
                            return Err(verr("MatchAs must specify a target name if a pattern is given".into()));
                        }
                        pattern(q, false)
                    }
                    None => Ok(()),
                }
            }
            K::MatchOr { patterns } => {
                if patterns.len() < 2 {
                    return Err(verr("MatchOr requires at least 2 patterns".into()));
                }
                for q in patterns {
                    pattern(q, false)?;
                }
                Ok(())
            }
        }
    }
}

// ======================================================================
// Bridge used by builtins (compile) and the _ast/ast modules
// ======================================================================

pub fn is_ast_node(o: &PyObject) -> bool {
    if STATE.get().is_none() {
        // No AST object can exist before _ast is imported.
        return false;
    }
    match cls("AST") {
        Ok(c) => ab::isinstance(o, c).unwrap_or_else(|_| {
            clear_exc();
            false
        }),
        Err(_) => {
            clear_exc();
            false
        }
    }
}

/// Convert a Python AST object into the Rust AST (with validation).
pub fn py_to_mod(o: &PyObject, mode: crate::compile::Mode) -> PyResult<Mod> {
    let mut c = Conv { depth: 0 };
    let m = c.module(o, mode)?;
    validate::module(&m)?;
    Ok(m)
}

/// compile() entry point for PyCF_ONLY_AST / AST input.
pub fn compile_with_ast(src: &PyObject, fname: &str, mode: crate::compile::Mode, flags: u32, optimize: i32, feature_version: Option<u32>) -> PyResult {
    let only_ast = flags & rpy_compiler::cf::PYCF_ONLY_AST != 0;
    if is_ast_node(src) {
        let mut m = py_to_mod(src, mode)?;
        if only_ast {
            if flags & rpy_compiler::cf::PYCF_OPTIMIZED_AST == rpy_compiler::cf::PYCF_OPTIMIZED_AST {
                let opt = if optimize < 0 { crate::modules::sys::optimize_flag() } else { optimize };
                if rpy_compiler::optimize_ast(&mut m, opt, flags).is_err() {
                    raise!(SystemError, "AST optimization failed");
                }
                return mod_to_py(&m);
            }
            // compile(ast, ..., PyCF_ONLY_AST) returns the input unchanged
            return Ok(src.to_ref());
        }
        let c = crate::compile::compile_ast(&m, fname, mode, flags, optimize, None)?;
        return Ok(c.code);
    }
    // source -> AST
    state()?;
    let b = if pystr::is_str(src) {
        let s = pystr::as_utf8(src)?;
        if s.contains('\0') {
            raise!(SyntaxError, "source code string cannot contain null bytes");
        }
        s.as_bytes().to_vec()
    } else {
        match bytes::bytes_like_data(src) {
            Some(b) => {
                if b.contains(&0) {
                    raise!(SyntaxError, "source code string cannot contain null bytes");
                }
                b
            }
            None => raise!(TypeError, "compile() arg 1 must be a string, bytes or AST object"),
        }
    };
    let mut m = crate::compile::parse_to_ast(&b, fname, mode, flags, feature_version)?;
    if flags & rpy_compiler::cf::PYCF_OPTIMIZED_AST == rpy_compiler::cf::PYCF_OPTIMIZED_AST {
        let opt = if optimize < 0 { crate::modules::sys::optimize_flag() } else { optimize };
        if let Err(e) = rpy_compiler::optimize_ast(&mut m, opt, flags) {
            let _ = e;
            raise!(SystemError, "AST optimization failed");
        }
    }
    mod_to_py(&m)
}
