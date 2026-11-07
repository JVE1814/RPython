//! Symbol table construction and scope analysis (CPython 3.15 semantics).
//!
//! Pass 1 ([`Builder`]) walks the AST, creating one [`Scope`] per block and recording
//! `DEF_*`/`USE` flags per name. Pass 2 (`analyze`) resolves every name to a [`SymScope`]
//! (local, cell, free, global explicit/implicit), inlines list/set/dict comprehensions into
//! their parent (PEP 709), computes cells, the implicit `__class__`/`__classdict__` cells and
//! `__conditional_annotations__`.
//!
//! The data structure is also what the runtime's `_symtable` module exposes; the flag values
//! are CPython's.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::error::CompileError;

pub const DEF_GLOBAL: u32 = 1;
pub const DEF_LOCAL: u32 = 2;
pub const DEF_PARAM: u32 = 2 << 1;
pub const DEF_NONLOCAL: u32 = 2 << 2;
pub const USE: u32 = 2 << 3;
pub const DEF_FREE: u32 = 2 << 4;
pub const DEF_FREE_CLASS: u32 = 2 << 5;
pub const DEF_IMPORT: u32 = 2 << 6;
pub const DEF_ANNOT: u32 = 2 << 7;
pub const DEF_COMP_ITER: u32 = 2 << 8;
pub const DEF_TYPE_PARAM: u32 = 2 << 9;
pub const DEF_COMP_CELL: u32 = 2 << 10;
pub const DEF_BOUND: u32 = DEF_LOCAL | DEF_PARAM | DEF_IMPORT;
pub const SCOPE_OFFSET: u32 = 12;
pub const SCOPE_MASK: u32 = DEF_GLOBAL | DEF_LOCAL | DEF_PARAM | DEF_NONLOCAL;

/// Resolved scope of a name (values = CPython's LOCAL.. CELL).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SymScope {
    #[default]
    Unknown = 0,
    Local = 1,
    GlobalExplicit = 2,
    GlobalImplicit = 3,
    Free = 4,
    Cell = 5,
}

/// Block types (CPython's `_Py_block_ty`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeKind {
    Function,
    Class,
    Module,
    Annotation,
    TypeAlias,
    TypeParams,
    TypeVariable,
}

impl ScopeKind {
    /// Name as returned by `SymbolTable.get_type()`.
    pub fn type_name(self) -> &'static str {
        match self {
            ScopeKind::Function => "function",
            ScopeKind::Class => "class",
            ScopeKind::Module => "module",
            ScopeKind::Annotation => "annotation",
            ScopeKind::TypeAlias => "type alias",
            ScopeKind::TypeParams => "type parameters",
            ScopeKind::TypeVariable => "type variable",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompKind {
    None,
    List,
    Set,
    Dict,
    Gen,
}

#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: String,
    /// DEF_* / USE bits.
    pub flags: u32,
    pub scope: SymScope,
}

impl Symbol {
    /// Flags as CPython stores them (`flags | scope << SCOPE_OFFSET`).
    pub fn raw_flags(&self) -> u32 {
        self.flags | ((self.scope as u32) << SCOPE_OFFSET)
    }
}

pub type ScopeId = usize;

#[derive(Clone, Debug)]
pub struct Scope {
    pub kind: ScopeKind,
    pub name: String,
    /// Unique, > 0 (`SymbolTable.get_id()`).
    pub id: usize,
    pub lineno: i32,
    pub col_offset: i32,
    pub end_lineno: i32,
    pub end_col_offset: i32,
    pub parent: Option<ScopeId>,
    /// Child blocks (inlined comprehensions are spliced out, their children spliced in).
    pub children: Vec<ScopeId>,
    pub symbols: Vec<Symbol>,
    index: HashMap<String, usize>,
    /// Parameter names in order: posonly, args, kwonly, *args, **kwargs.
    pub varnames: Vec<String>,
    pub nested: bool,
    pub generator: bool,
    pub coroutine: bool,
    pub comprehension: CompKind,
    pub comp_inlined: bool,
    pub varargs: bool,
    pub varkeywords: bool,
    pub returns_value: bool,
    /// Class needs an implicit `__class__` cell.
    pub needs_class_closure: bool,
    /// Class needs an implicit `__classdict__` cell.
    pub needs_classdict: bool,
    pub has_conditional_annotations: bool,
    /// Annotation / type-param scope directly inside a class: may see class names through
    /// `__classdict__`.
    pub can_see_class_scope: bool,
    /// Annotation block (`__annotate__`) of a module/class/function body.
    pub annotation_block: Option<ScopeId>,
    /// For annotation blocks of function parameters: the function's name.
    pub function_name: Option<String>,
    /// Names mangled in a class type-param block.
    pub mangled_names: Option<HashSet<String>>,
    /// Private name (enclosing class name) for name mangling.
    pub private: Option<String>,
    /// Some free variable / child free variable.
    pub free: bool,
    pub child_free: bool,
    /// Directive locations (global / nonlocal / walrus) for error reporting.
    directives: HashMap<String, Loc>,
    pub scope_info: Option<&'static str>,
    in_conditional_block: bool,
    in_unevaluated_annotation: bool,
    comp_iter_target: bool,
    comp_iter_expr: u32,
    in_try: u32,
}

impl Scope {
    fn new(kind: ScopeKind, name: String, id: usize, loc: Loc, parent: Option<ScopeId>) -> Scope {
        Scope {
            kind,
            name,
            id,
            lineno: loc.lineno,
            col_offset: loc.col_offset,
            end_lineno: loc.end_lineno,
            end_col_offset: loc.end_col_offset,
            parent,
            children: Vec::new(),
            symbols: Vec::new(),
            index: HashMap::new(),
            varnames: Vec::new(),
            nested: false,
            generator: false,
            coroutine: false,
            comprehension: CompKind::None,
            comp_inlined: false,
            varargs: false,
            varkeywords: false,
            returns_value: false,
            needs_class_closure: false,
            needs_classdict: false,
            has_conditional_annotations: false,
            can_see_class_scope: false,
            annotation_block: None,
            function_name: None,
            mangled_names: None,
            private: None,
            free: false,
            child_free: false,
            directives: HashMap::new(),
            scope_info: None,
            in_conditional_block: false,
            in_unevaluated_annotation: false,
            comp_iter_target: false,
            comp_iter_expr: 0,
            in_try: 0,
        }
    }
    pub fn lookup(&self, name: &str) -> Option<&Symbol> {
        self.index.get(name).map(|&i| &self.symbols[i])
    }
    fn lookup_mut(&mut self, name: &str) -> Option<&mut Symbol> {
        match self.index.get(name) {
            Some(&i) => Some(&mut self.symbols[i]),
            None => None,
        }
    }
    pub fn flags(&self, name: &str) -> u32 {
        self.lookup(name).map(|s| s.flags).unwrap_or(0)
    }
    pub fn scope_of(&self, name: &str) -> SymScope {
        self.lookup(name).map(|s| s.scope).unwrap_or(SymScope::Unknown)
    }
    fn set_symbol(&mut self, name: &str, flags: u32) {
        if let Some(s) = self.lookup_mut(name) {
            s.flags = flags;
        } else {
            self.index.insert(name.to_string(), self.symbols.len());
            self.symbols.push(Symbol { name: name.to_string(), flags, scope: SymScope::Unknown });
        }
    }
    /// Function-like scopes use fast locals.
    pub fn is_function_like(&self) -> bool {
        matches!(
            self.kind,
            ScopeKind::Function | ScopeKind::Annotation | ScopeKind::TypeAlias | ScopeKind::TypeParams | ScopeKind::TypeVariable
        )
    }
    pub fn is_optimized(&self) -> bool {
        self.is_function_like()
    }
}

/// Result of [`build`]: all scopes; scope 0 is the module.
#[derive(Clone, Debug)]
pub struct SymbolTable {
    pub scopes: Vec<Scope>,
    keys: HashMap<(usize, u8), ScopeId>,
    pub future_flags: u32,
}

pub(crate) const TAG_FUNC: u8 = 0;
pub(crate) const TAG_ANN: u8 = 1;
pub(crate) const TAG_TP: u8 = 2;
pub(crate) const TAG_CLASS: u8 = 3;
pub(crate) const TAG_ALIAS: u8 = 4;
pub(crate) const TAG_COMP: u8 = 5;
pub(crate) const TAG_BOUND: u8 = 6;
pub(crate) const TAG_DEFAULT: u8 = 7;
pub(crate) const TAG_LAMBDA: u8 = 8;

pub(crate) fn key<T>(p: &T, tag: u8) -> (usize, u8) {
    (p as *const T as usize, tag)
}

impl SymbolTable {
    pub fn top(&self) -> &Scope {
        &self.scopes[0]
    }
    pub(crate) fn scope_for(&self, k: (usize, u8)) -> Option<ScopeId> {
        self.keys.get(&k).copied()
    }
}

/// Name mangling for private names (`__spam` in class `Ham` -> `_Ham__spam`).
pub fn mangle(private: Option<&str>, name: &str) -> String {
    match private {
        Some(p) => {
            if !name.starts_with("__") || name.ends_with("__") || name.contains('.') {
                return name.to_string();
            }
            let stripped = p.trim_start_matches('_');
            if stripped.is_empty() {
                return name.to_string();
            }
            let mut s = String::with_capacity(1 + stripped.len() + name.len());
            s.push('_');
            s.push_str(stripped);
            s.push_str(name);
            s
        }
        None => name.to_string(),
    }
}

const FUTURE_ANNOTATIONS: u32 = crate::cf::CO_FUTURE_ANNOTATIONS;
const ALLOW_TOP_LEVEL_AWAIT: u32 = crate::cf::PYCF_ALLOW_TOP_LEVEL_AWAIT;

struct Builder {
    scopes: Vec<Scope>,
    keys: HashMap<(usize, u8), ScopeId>,
    stack: Vec<ScopeId>,
    cur: ScopeId,
    /// Current private name (class name) for mangling.
    private: Option<String>,
    flags: u32,
    depth: u32,
    max_depth: u32,
}

type R = Result<(), CompileError>;

fn err(msg: impl Into<String>, loc: Loc) -> CompileError {
    CompileError::syntax(msg, loc)
}

/// Build and analyse the symbol table for a module.
pub fn build(module: &Mod, flags: u32, max_depth: u32) -> Result<SymbolTable, CompileError> {
    let mut b = Builder {
        scopes: Vec::new(),
        keys: HashMap::new(),
        stack: Vec::new(),
        cur: 0,
        private: None,
        flags,
        depth: 0,
        max_depth,
    };
    b.scopes.push(Scope::new(ScopeKind::Module, "top".to_string(), 1, Loc::new(0, 0, 0, 0), None));
    b.stack.push(0);
    match module {
        Mod::Module { body, .. } | Mod::Interactive { body } => {
            for s in body {
                b.visit_stmt(s)?;
            }
        }
        Mod::Expression { body } => b.visit_expr(body)?,
        Mod::FunctionType { argtypes, returns } => {
            for e in argtypes {
                b.visit_expr(e)?;
            }
            b.visit_expr(returns)?;
        }
    }
    let mut st = SymbolTable { scopes: b.scopes, keys: b.keys, future_flags: flags & crate::cf::PYCF_MASK };
    analyze(&mut st)?;
    Ok(st)
}

impl Builder {
    fn cur(&self) -> &Scope {
        &self.scopes[self.cur]
    }
    fn cur_mut(&mut self) -> &mut Scope {
        &mut self.scopes[self.cur]
    }

    fn enter_block(&mut self, name: &str, kind: ScopeKind, k: Option<(usize, u8)>, loc: Loc) -> ScopeId {
        let id = self.scopes.len();
        let prev = self.cur;
        let mut sc = Scope::new(kind, name.to_string(), id + 1, loc, Some(prev));
        let pk = self.scopes[prev].kind;
        sc.nested = self.scopes[prev].nested || self.scopes[prev].is_function_like();
        if kind == ScopeKind::Module {
            sc.nested = false;
        }
        let _ = pk;
        sc.private = self.private.clone();
        // a block nested in a comprehension iterable inherits the restriction on walrus
        sc.comp_iter_expr = self.scopes[prev].comp_iter_expr;
        // blocks nested in a class type-param block only mangle the type parameter names
        if kind != ScopeKind::Class {
            sc.mangled_names = self.scopes[prev].mangled_names.clone();
        }
        self.scopes.push(sc);
        self.scopes[prev].children.push(id);
        if let Some(k) = k {
            self.keys.insert(k, id);
        }
        self.stack.push(id);
        self.cur = id;
        if matches!(kind, ScopeKind::Annotation | ScopeKind::TypeAlias | ScopeKind::TypeVariable) {
            // the implicit `format` parameter of lazily evaluated scopes
            self.scopes[id].set_symbol(".format", DEF_PARAM);
            self.scopes[id].varnames.push(".format".to_string());
        }
        id
    }

    fn exit_block(&mut self) {
        self.stack.pop();
        self.cur = *self.stack.last().unwrap();
    }

    fn mangle(&self, name: &str) -> String {
        let sc = self.cur();
        if let Some(m) = &sc.mangled_names {
            // type-param block of a class: only mangle names that were defined mangled
            if !m.contains(name) {
                return name.to_string();
            }
        }
        mangle(self.private.as_deref(), name)
    }

    fn add_def(&mut self, name: &str, flag: u32, loc: Loc) -> R {
        let cur = self.cur;
        self.add_def_in(name, flag, cur, loc)
    }

    fn add_def_in(&mut self, name: &str, flag: u32, sid: ScopeId, loc: Loc) -> R {
        if flag & DEF_TYPE_PARAM != 0 {
            if let Some(m) = &mut self.scopes[sid].mangled_names {
                m.insert(name.to_string());
            }
        }
        let mangled = if sid == self.cur { self.mangle(name) } else { mangle(self.private.as_deref(), name) };
        let sc = &mut self.scopes[sid];
        let mut val = match sc.lookup(&mangled) {
            Some(s) => {
                let v = s.flags;
                if flag & DEF_PARAM != 0 && v & DEF_PARAM != 0 {
                    return Err(err(format!("duplicate parameter '{}' in function definition", name), loc));
                }
                if flag & DEF_TYPE_PARAM != 0 && v & DEF_TYPE_PARAM != 0 {
                    return Err(err(format!("duplicate type parameter '{}'", name), loc));
                }
                v | flag
            }
            None => flag,
        };
        if sc.comp_iter_target {
            if val & (DEF_GLOBAL | DEF_NONLOCAL) != 0 {
                return Err(err(
                    format!("comprehension inner loop cannot rebind assignment expression target '{}'", name),
                    loc,
                ));
            }
            val |= DEF_COMP_ITER;
        }
        sc.set_symbol(&mangled, val);
        if flag & DEF_PARAM != 0 {
            sc.varnames.push(mangled.clone());
        } else if flag & DEF_GLOBAL != 0 {
            let g = &mut self.scopes[0];
            let v = g.flags(&mangled) | flag;
            g.set_symbol(&mangled, v);
        }
        Ok(())
    }

    fn record_directive(&mut self, name: &str, loc: Loc) {
        let m = self.mangle(name);
        // errors point at the first global/nonlocal statement naming it
        self.cur_mut().directives.entry(m).or_insert(loc);
    }

    fn enter_depth(&mut self) -> R {
        self.depth += 1;
        if self.depth > self.max_depth {
            return Err(CompileError::recursion());
        }
        Ok(())
    }

    // ------------------------------------------------------------------ statements

    fn visit_body(&mut self, body: &[Stmt]) -> R {
        for s in body {
            self.visit_stmt(s)?;
        }
        Ok(())
    }

    fn visit_cond_body(&mut self, body: &[Stmt]) -> R {
        let prev = self.cur().in_conditional_block;
        self.cur_mut().in_conditional_block = true;
        let r = self.visit_body(body);
        self.cur_mut().in_conditional_block = prev;
        r
    }

    fn visit_stmt(&mut self, s: &Stmt) -> R {
        self.enter_depth()?;
        let r = self.visit_stmt_inner(s);
        self.depth -= 1;
        r
    }

    fn visit_stmt_inner(&mut self, s: &Stmt) -> R {
        let loc = s.loc;
        match &s.node {
            StmtKind::FunctionDef(f) | StmtKind::AsyncFunctionDef(f) => {
                let is_async = matches!(s.node, StmtKind::AsyncFunctionDef(_));
                self.add_def(&f.name, DEF_LOCAL, loc)?;
                for d in &f.args.defaults {
                    self.visit_expr(d)?;
                }
                for d in f.args.kw_defaults.iter().flatten() {
                    self.visit_expr(d)?;
                }
                for d in &f.decorator_list {
                    self.visit_expr(d)?;
                }
                let generic = !f.type_params.is_empty();
                if generic {
                    let has_kwd = f.args.kw_defaults.iter().any(|d| d.is_some());
                    self.enter_type_param_block(&f.name, key(s, TAG_TP), !f.args.defaults.is_empty(), has_kwd, false, loc)?;
                    for tp in &f.type_params {
                        self.visit_type_param(tp)?;
                    }
                }
                self.visit_annotations(s, &f.args, f.returns.as_deref(), &f.name)?;
                self.enter_block(&f.name, ScopeKind::Function, Some(key(s, TAG_FUNC)), loc);
                if is_async {
                    self.cur_mut().coroutine = true;
                }
                self.visit_arguments(&f.args)?;
                self.visit_body(&f.body)?;
                self.exit_block();
                if generic {
                    self.exit_block();
                }
            }
            StmtKind::ClassDef { name, bases, keywords, body, decorator_list, type_params } => {
                self.add_def(name, DEF_LOCAL, loc)?;
                for d in decorator_list {
                    self.visit_expr(d)?;
                }
                let tmp = self.private.clone();
                let generic = !type_params.is_empty();
                if generic {
                    self.enter_type_param_block(name, key(s, TAG_TP), false, false, true, loc)?;
                    self.private = Some(name.clone());
                    self.cur_mut().private = Some(name.clone());
                    self.cur_mut().mangled_names = Some(HashSet::new());
                    for tp in type_params {
                        self.visit_type_param(tp)?;
                    }
                }
                for b in bases {
                    self.visit_expr(b)?;
                }
                for k in keywords {
                    self.visit_expr(&k.value)?;
                }
                self.private = Some(name.clone());
                self.enter_block(name, ScopeKind::Class, Some(key(s, TAG_CLASS)), loc);
                if generic {
                    self.add_def("__type_params__", DEF_LOCAL, loc)?;
                    self.add_def(".type_params", USE, loc)?;
                }
                self.visit_body(body)?;
                self.exit_block();
                if generic {
                    self.exit_block();
                }
                self.private = tmp;
            }
            StmtKind::Return { value } => {
                if let Some(v) = value {
                    self.visit_expr(v)?;
                    self.cur_mut().returns_value = true;
                }
            }
            StmtKind::Delete { targets } => {
                for t in targets {
                    self.visit_expr(t)?;
                }
            }
            StmtKind::Assign { targets, value, .. } => {
                for t in targets {
                    self.visit_expr(t)?;
                }
                self.visit_expr(value)?;
            }
            StmtKind::TypeAlias { name, type_params, value } => {
                self.visit_expr(name)?;
                let aname = match &name.node {
                    ExprKind::Name { id, .. } => id.clone(),
                    _ => String::new(),
                };
                let in_class = self.cur().kind == ScopeKind::Class;
                let generic = !type_params.is_empty();
                if generic {
                    self.enter_type_param_block(&aname, key(s, TAG_TP), false, false, false, loc)?;
                    for tp in type_params {
                        self.visit_type_param(tp)?;
                    }
                }
                self.enter_block(&aname, ScopeKind::TypeAlias, Some(key(s, TAG_ALIAS)), loc);
                self.cur_mut().can_see_class_scope = in_class;
                if in_class {
                    self.add_def("__classdict__", USE, value.loc)?;
                }
                self.visit_expr(value)?;
                self.exit_block();
                if generic {
                    self.exit_block();
                }
            }
            StmtKind::AugAssign { target, value, .. } => {
                self.visit_expr(target)?;
                self.visit_expr(value)?;
            }
            StmtKind::AnnAssign { target, annotation, value, simple } => {
                if let ExprKind::Name { id, .. } = &target.node {
                    let m = self.mangle(id);
                    let cur = self.cur().flags(&m);
                    if cur & (DEF_GLOBAL | DEF_NONLOCAL) != 0 && self.cur != 0 && *simple != 0 {
                        let msg = if cur & DEF_GLOBAL != 0 {
                            format!("annotated name '{}' can't be global", id)
                        } else {
                            format!("annotated name '{}' can't be nonlocal", id)
                        };
                        return Err(err(msg, loc));
                    }
                    if *simple != 0 {
                        self.add_def(id, DEF_ANNOT | DEF_LOCAL, target.loc)?;
                    } else if value.is_some() {
                        self.add_def(id, DEF_LOCAL, target.loc)?;
                    }
                } else {
                    self.visit_expr(target)?;
                }
                self.visit_annotation(annotation)?;
                if let Some(v) = value {
                    self.visit_expr(v)?;
                }
            }
            StmtKind::For(f) | StmtKind::AsyncFor(f) => {
                if matches!(s.node, StmtKind::AsyncFor(_)) {
                    self.maybe_set_module_coroutine();
                }
                self.visit_expr(&f.target)?;
                self.visit_expr(&f.iter)?;
                self.visit_cond_body(&f.body)?;
                self.visit_cond_body(&f.orelse)?;
            }
            StmtKind::While { test, body, orelse } => {
                self.visit_expr(test)?;
                self.visit_cond_body(body)?;
                self.visit_cond_body(orelse)?;
            }
            StmtKind::If { test, body, orelse } => {
                self.visit_expr(test)?;
                self.visit_cond_body(body)?;
                self.visit_cond_body(orelse)?;
            }
            StmtKind::With(w) | StmtKind::AsyncWith(w) => {
                if matches!(s.node, StmtKind::AsyncWith(_)) {
                    self.maybe_set_module_coroutine();
                }
                for item in &w.items {
                    self.visit_expr(&item.context_expr)?;
                    if let Some(v) = &item.optional_vars {
                        self.visit_expr(v)?;
                    }
                }
                self.visit_cond_body(&w.body)?;
            }
            StmtKind::Match { subject, cases } => {
                self.visit_expr(subject)?;
                let prev = self.cur().in_conditional_block;
                self.cur_mut().in_conditional_block = true;
                for c in cases {
                    self.visit_pattern(&c.pattern)?;
                    if let Some(g) = &c.guard {
                        self.visit_expr(g)?;
                    }
                    self.visit_body(&c.body)?;
                }
                self.cur_mut().in_conditional_block = prev;
            }
            StmtKind::Raise { exc, cause } => {
                if let Some(e) = exc {
                    self.visit_expr(e)?;
                }
                if let Some(c) = cause {
                    self.visit_expr(c)?;
                }
            }
            StmtKind::Try(t) | StmtKind::TryStar(t) => {
                let prev = self.cur().in_conditional_block;
                self.cur_mut().in_conditional_block = true;
                self.cur_mut().in_try += 1;
                self.visit_body(&t.body)?;
                for h in &t.handlers {
                    if let Some(ty) = &h.type_ {
                        self.visit_expr(ty)?;
                    }
                    if let Some(n) = &h.name {
                        self.add_def(n, DEF_LOCAL, h.loc)?;
                    }
                    self.visit_body(&h.body)?;
                }
                self.cur_mut().in_try -= 1;
                self.visit_body(&t.orelse)?;
                self.visit_body(&t.finalbody)?;
                self.cur_mut().in_conditional_block = prev;
            }
            StmtKind::Assert { test, msg } => {
                self.visit_expr(test)?;
                if let Some(m) = msg {
                    self.visit_expr(m)?;
                }
            }
            StmtKind::Import { names, is_lazy } => {
                if *is_lazy != 0 {
                    self.check_lazy_import(false, loc)?;
                }
                for a in names {
                    self.visit_alias(a)?;
                }
            }
            StmtKind::ImportFrom { module, names, is_lazy, .. } => {
                if *is_lazy != 0 {
                    self.check_lazy_import(true, loc)?;
                    if names.iter().any(|a| a.name == "*") {
                        return Err(err("lazy from ... import * is not allowed", loc));
                    }
                    if module.as_deref() == Some("__future__") {
                        // the error points at the `lazy` keyword
                        let kw = Loc::new(loc.lineno, loc.col_offset, loc.lineno, loc.col_offset + 4);
                        return Err(err("lazy from __future__ import is not allowed", kw));
                    }
                }
                for a in names {
                    self.visit_alias(a)?;
                }
            }
            StmtKind::Global { names } => {
                for name in names {
                    let m = self.mangle(name);
                    let cur = self.cur().flags(&m);
                    if cur & (DEF_PARAM | DEF_LOCAL | USE | DEF_ANNOT) != 0 {
                        let msg = if cur & DEF_PARAM != 0 {
                            format!("name '{}' is parameter and global", name)
                        } else if cur & USE != 0 {
                            format!("name '{}' is used prior to global declaration", name)
                        } else if cur & DEF_ANNOT != 0 {
                            format!("annotated name '{}' can't be global", name)
                        } else {
                            format!("name '{}' is assigned to before global declaration", name)
                        };
                        return Err(err(msg, loc));
                    }
                    self.add_def(name, DEF_GLOBAL, loc)?;
                    self.record_directive(name, loc);
                }
            }
            StmtKind::Nonlocal { names } => {
                for name in names {
                    let m = self.mangle(name);
                    let cur = self.cur().flags(&m);
                    if cur & (DEF_PARAM | DEF_LOCAL | USE | DEF_ANNOT) != 0 {
                        let msg = if cur & DEF_PARAM != 0 {
                            format!("name '{}' is parameter and nonlocal", name)
                        } else if cur & USE != 0 {
                            format!("name '{}' is used prior to nonlocal declaration", name)
                        } else if cur & DEF_ANNOT != 0 {
                            format!("annotated name '{}' can't be nonlocal", name)
                        } else {
                            format!("name '{}' is assigned to before nonlocal declaration", name)
                        };
                        return Err(err(msg, loc));
                    }
                    self.add_def(name, DEF_NONLOCAL, loc)?;
                    self.record_directive(name, loc);
                }
            }
            StmtKind::Expr { value } => self.visit_expr(value)?,
            StmtKind::Pass | StmtKind::Break | StmtKind::Continue => {}
        }
        Ok(())
    }

    fn check_lazy_import(&self, from: bool, loc: Loc) -> R {
        let what = if from { "lazy from ... import" } else { "lazy import" };
        let kind = self.cur().kind;
        if kind == ScopeKind::Class {
            return Err(err(format!("{} not allowed inside classes", what), loc));
        }
        if kind != ScopeKind::Module {
            return Err(err(format!("{} not allowed inside functions", what), loc));
        }
        if self.cur().in_try > 0 {
            return Err(err(format!("{} not allowed inside try/except blocks", what), loc));
        }
        Ok(())
    }

    fn maybe_set_module_coroutine(&mut self) {
        if self.flags & ALLOW_TOP_LEVEL_AWAIT != 0 && self.cur().kind == ScopeKind::Module {
            self.cur_mut().coroutine = true;
        }
    }

    fn allows_top_level_await(&self) -> bool {
        self.flags & ALLOW_TOP_LEVEL_AWAIT != 0 && self.cur().kind == ScopeKind::Module
    }

    fn is_async_def(&self) -> bool {
        self.cur().kind == ScopeKind::Function && self.cur().coroutine
    }

    fn visit_alias(&mut self, a: &Alias) -> R {
        let name = a.asname.as_ref().unwrap_or(&a.name);
        if name == "*" {
            if self.cur().kind != ScopeKind::Module {
                return Err(err("import * only allowed at module level", a.loc));
            }
            return Ok(());
        }
        let store = match name.find('.') {
            Some(i) => &name[..i],
            None => name.as_str(),
        };
        self.add_def(store, DEF_IMPORT, a.loc)
    }

    fn enter_type_param_block(
        &mut self,
        name: &str,
        k: (usize, u8),
        has_defaults: bool,
        has_kwdefaults: bool,
        is_class: bool,
        loc: Loc,
    ) -> R {
        let cur_kind = self.cur().kind;
        self.enter_block(name, ScopeKind::TypeParams, Some(k), loc);
        if cur_kind == ScopeKind::Class {
            self.cur_mut().can_see_class_scope = true;
            self.add_def("__classdict__", USE, loc)?;
        }
        if is_class {
            self.add_def(".type_params", DEF_LOCAL, loc)?;
            self.add_def(".type_params", USE, loc)?;
            self.add_def(".generic_base", DEF_LOCAL, loc)?;
            self.add_def(".generic_base", USE, loc)?;
        }
        if has_defaults {
            self.add_def(".defaults", DEF_PARAM, loc)?;
        }
        if has_kwdefaults {
            self.add_def(".kwdefaults", DEF_PARAM, loc)?;
        }
        Ok(())
    }

    fn visit_type_param(&mut self, tp: &TypeParam) -> R {
        let loc = tp.loc;
        match &tp.node {
            TypeParamKind::TypeVar { name, bound, default_value } => {
                self.add_def(name, DEF_TYPE_PARAM | DEF_LOCAL, loc)?;
                let info = bound.as_ref().map(|b| {
                    if matches!(b.node, ExprKind::Tuple { .. }) {
                        "a TypeVar constraint"
                    } else {
                        "a TypeVar bound"
                    }
                });
                self.visit_bound_or_default(bound.as_deref(), name, key(tp, TAG_BOUND), info.unwrap_or(""), loc)?;
                self.visit_bound_or_default(default_value.as_deref(), name, key(tp, TAG_DEFAULT), "a TypeVar default", loc)?;
            }
            TypeParamKind::TypeVarTuple { name, default_value } => {
                self.add_def(name, DEF_TYPE_PARAM | DEF_LOCAL, loc)?;
                self.visit_bound_or_default(default_value.as_deref(), name, key(tp, TAG_DEFAULT), "a TypeVarTuple default", loc)?;
            }
            TypeParamKind::ParamSpec { name, default_value } => {
                self.add_def(name, DEF_TYPE_PARAM | DEF_LOCAL, loc)?;
                self.visit_bound_or_default(default_value.as_deref(), name, key(tp, TAG_DEFAULT), "a ParamSpec default", loc)?;
            }
        }
        Ok(())
    }

    fn visit_bound_or_default(&mut self, e: Option<&Expr>, name: &str, k: (usize, u8), info: &'static str, tploc: Loc) -> R {
        if name == "__classdict__" {
            return Err(err(format!("reserved name '{}' cannot be used for type parameter", name), tploc));
        }
        if let Some(e) = e {
            let in_class = self.cur().can_see_class_scope;
            self.enter_block(name, ScopeKind::TypeVariable, Some(k), e.loc);
            self.cur_mut().can_see_class_scope = in_class;
            if in_class {
                self.add_def("__classdict__", USE, e.loc)?;
            }
            self.cur_mut().scope_info = Some(info);
            self.visit_expr(e)?;
            self.exit_block();
        }
        Ok(())
    }

    /// Parameter annotations of a function: always creates the `__annotate__` block.
    fn visit_annotations(&mut self, s: &Stmt, a: &Arguments, returns: Option<&Expr>, fname: &str) -> R {
        let cur_kind = self.cur().kind;
        // a generic method's annotations live inside its type-parameter block, which
        // itself sees the class namespace
        let in_class = cur_kind == ScopeKind::Class
            || (cur_kind == ScopeKind::TypeParams && self.cur().can_see_class_scope);
        let future = self.flags & FUTURE_ANNOTATIONS != 0;
        self.enter_block("__annotate__", ScopeKind::Annotation, Some(key(s, TAG_ANN)), s.loc);
        self.cur_mut().function_name = Some(fname.to_string());
        if in_class && !future {
            self.cur_mut().can_see_class_scope = true;
            self.add_def("__classdict__", USE, s.loc)?;
        }
        if !future {
            for arg in a.posonlyargs.iter().chain(a.args.iter()) {
                if let Some(ann) = &arg.annotation {
                    self.visit_expr(ann)?;
                }
            }
            if let Some(v) = &a.vararg {
                if let Some(ann) = &v.annotation {
                    self.visit_expr(ann)?;
                }
            }
            if let Some(v) = &a.kwarg {
                if let Some(ann) = &v.annotation {
                    self.visit_expr(ann)?;
                }
            }
            for arg in &a.kwonlyargs {
                if let Some(ann) = &arg.annotation {
                    self.visit_expr(ann)?;
                }
            }
            if let Some(r) = returns {
                self.visit_expr(r)?;
            }
        } else {
            // string annotations: only check for forbidden expressions
            self.cur_mut().in_unevaluated_annotation = true;
            let mut anns: Vec<&Expr> = Vec::new();
            for arg in a.posonlyargs.iter().chain(a.args.iter()).chain(a.kwonlyargs.iter()) {
                if let Some(ann) = &arg.annotation {
                    anns.push(ann);
                }
            }
            for v in a.vararg.iter().chain(a.kwarg.iter()) {
                if let Some(ann) = &v.annotation {
                    anns.push(ann);
                }
            }
            if let Some(r) = returns {
                anns.push(r);
            }
            for ann in anns {
                self.visit_expr(ann)?;
            }
            self.cur_mut().in_unevaluated_annotation = false;
        }
        self.exit_block();
        Ok(())
    }

    /// Annotation of an annotated assignment statement.
    fn visit_annotation(&mut self, ann: &Expr) -> R {
        let sc = self.cur();
        let future = self.flags & FUTURE_ANNOTATIONS != 0;
        let is_unevaluated = sc.kind == ScopeKind::Function || future;
        if !future
            && ((sc.kind == ScopeKind::Class && sc.in_conditional_block) || sc.kind == ScopeKind::Module)
            && !sc.has_conditional_annotations
        {
            self.cur_mut().has_conditional_annotations = true;
            self.add_def("__conditional_annotations__", USE, ann.loc)?;
        }
        let parent = self.cur;
        match self.scopes[parent].annotation_block {
            None => {
                let ptype = self.scopes[parent].kind;
                let id = self.enter_block("__annotate__", ScopeKind::Annotation, None, ann.loc);
                self.scopes[parent].annotation_block = Some(id);
                if ptype == ScopeKind::Class && !future {
                    self.cur_mut().can_see_class_scope = true;
                    self.add_def("__classdict__", USE, ann.loc)?;
                }
            }
            Some(id) => {
                self.stack.push(id);
                self.cur = id;
            }
        }
        if self.scopes[parent].kind == ScopeKind::Class && self.scopes[parent].has_conditional_annotations && !future {
            // the class __annotate__ reads the set through a closure cell
            self.add_def("__conditional_annotations__", USE, ann.loc)?;
        }
        if is_unevaluated {
            self.cur_mut().in_unevaluated_annotation = true;
        }
        let r = self.visit_expr(ann);
        if is_unevaluated {
            self.cur_mut().in_unevaluated_annotation = false;
        }
        self.exit_block();
        r
    }

    fn visit_arguments(&mut self, a: &Arguments) -> R {
        for arg in a.posonlyargs.iter().chain(a.args.iter()).chain(a.kwonlyargs.iter()) {
            self.add_def(&arg.arg, DEF_PARAM, arg.loc)?;
        }
        if let Some(v) = &a.vararg {
            self.add_def(&v.arg, DEF_PARAM, v.loc)?;
            self.cur_mut().varargs = true;
        }
        if let Some(v) = &a.kwarg {
            self.add_def(&v.arg, DEF_PARAM, v.loc)?;
            self.cur_mut().varkeywords = true;
        }
        Ok(())
    }

    // ------------------------------------------------------------------ expressions

    fn raise_if_annotation_block(&self, what: &str, loc: Loc) -> R {
        let sc = self.cur();
        let msg = match sc.kind {
            ScopeKind::Annotation => format!("{} cannot be used within an annotation", what),
            ScopeKind::TypeVariable => format!("{} cannot be used within {}", what, sc.scope_info.unwrap_or("a TypeVar bound")),
            ScopeKind::TypeAlias => format!("{} cannot be used within a type alias", what),
            ScopeKind::TypeParams => format!("{} cannot be used within the definition of a generic", what),
            _ => return Ok(()),
        };
        Err(err(msg, loc))
    }

    fn raise_if_comprehension(&self, loc: Loc) -> R {
        let msg = match self.cur().comprehension {
            CompKind::List => "'yield' inside list comprehension",
            CompKind::Set => "'yield' inside set comprehension",
            CompKind::Dict => "'yield' inside dict comprehension",
            _ => "'yield' inside generator expression",
        };
        Err(err(msg, loc))
    }

    fn visit_expr(&mut self, e: &Expr) -> R {
        self.enter_depth()?;
        let r = self.visit_expr_inner(e);
        self.depth -= 1;
        r
    }

    fn visit_expr_inner(&mut self, e: &Expr) -> R {
        let loc = e.loc;
        match &e.node {
            ExprKind::BoolOp { values, .. } => {
                for v in values {
                    self.visit_expr(v)?;
                }
            }
            ExprKind::NamedExpr { target, value } => {
                self.raise_if_annotation_block("named expression", loc)?;
                self.handle_namedexpr(target, value, loc)?;
            }
            ExprKind::BinOp { left, right, .. } => {
                // iterate down left-nested chains to keep recursion shallow
                self.visit_expr(left)?;
                self.visit_expr(right)?;
            }
            ExprKind::UnaryOp { operand, .. } => self.visit_expr(operand)?,
            ExprKind::Lambda { args, body } => {
                for d in &args.defaults {
                    self.visit_expr(d)?;
                }
                for d in args.kw_defaults.iter().flatten() {
                    self.visit_expr(d)?;
                }
                self.enter_block("<lambda>", ScopeKind::Function, Some(key(e, TAG_LAMBDA)), loc);
                self.visit_arguments(args)?;
                self.visit_expr(body)?;
                self.exit_block();
            }
            ExprKind::IfExp { test, body, orelse } => {
                self.visit_expr(test)?;
                self.visit_expr(body)?;
                self.visit_expr(orelse)?;
            }
            ExprKind::Dict { keys, values } => {
                for k in keys.iter().flatten() {
                    self.visit_expr(k)?;
                }
                for v in values {
                    self.visit_expr(v)?;
                }
            }
            ExprKind::Set { elts } => {
                for v in elts {
                    self.visit_expr(v)?;
                }
            }
            ExprKind::GeneratorExp { elt, generators } => {
                self.handle_comprehension(e, "<genexpr>", CompKind::Gen, generators, elt, None)?;
            }
            ExprKind::ListComp { elt, generators } => {
                self.handle_comprehension(e, "<listcomp>", CompKind::List, generators, elt, None)?;
            }
            ExprKind::SetComp { elt, generators } => {
                self.handle_comprehension(e, "<setcomp>", CompKind::Set, generators, elt, None)?;
            }
            ExprKind::DictComp { key: k, value, generators } => {
                self.handle_comprehension(e, "<dictcomp>", CompKind::Dict, generators, k, value.as_deref())?;
            }
            ExprKind::Yield { value } => {
                self.raise_if_annotation_block("yield expression", loc)?;
                if let Some(v) = value {
                    self.visit_expr(v)?;
                }
                self.cur_mut().generator = true;
                if self.cur().comprehension != CompKind::None {
                    return self.raise_if_comprehension(loc);
                }
            }
            ExprKind::YieldFrom { value } => {
                self.raise_if_annotation_block("yield expression", loc)?;
                self.visit_expr(value)?;
                self.cur_mut().generator = true;
                if self.cur().comprehension != CompKind::None {
                    return self.raise_if_comprehension(loc);
                }
            }
            ExprKind::Await { value } => {
                self.raise_if_annotation_block("await expression", loc)?;
                if !self.allows_top_level_await() {
                    if !self.cur().is_function_like() {
                        return Err(err("'await' outside function", loc));
                    }
                    if !self.is_async_def() && self.cur().comprehension == CompKind::None {
                        return Err(err("'await' outside async function", loc));
                    }
                }
                self.visit_expr(value)?;
                self.cur_mut().coroutine = true;
            }
            ExprKind::Compare { left, comparators, .. } => {
                self.visit_expr(left)?;
                for c in comparators {
                    self.visit_expr(c)?;
                }
            }
            ExprKind::Call { func, args, keywords } => {
                self.visit_expr(func)?;
                for a in args {
                    self.visit_expr(a)?;
                }
                for k in keywords {
                    self.visit_expr(&k.value)?;
                }
            }
            ExprKind::FormattedValue { value, format_spec, .. } | ExprKind::Interpolation { value, format_spec, .. } => {
                self.visit_expr(value)?;
                if let Some(f) = format_spec {
                    self.visit_expr(f)?;
                }
            }
            ExprKind::JoinedStr { values } | ExprKind::TemplateStr { values } => {
                for v in values {
                    self.visit_expr(v)?;
                }
            }
            ExprKind::Constant { .. } => {}
            ExprKind::Attribute { value, .. } => self.visit_expr(value)?,
            ExprKind::Subscript { value, slice, .. } => {
                self.visit_expr(value)?;
                self.visit_expr(slice)?;
            }
            ExprKind::Starred { value, .. } => self.visit_expr(value)?,
            ExprKind::Slice { lower, upper, step } => {
                for x in [lower, upper, step].into_iter().flatten() {
                    self.visit_expr(x)?;
                }
            }
            ExprKind::Name { id, ctx } => {
                if !self.cur().in_unevaluated_annotation {
                    self.add_def(id, if *ctx == ExprContext::Load { USE } else { DEF_LOCAL }, loc)?;
                }
                if *ctx == ExprContext::Load && self.cur().is_function_like() && id == "super" {
                    self.add_def("__class__", USE, loc)?;
                }
            }
            ExprKind::List { elts, .. } | ExprKind::Tuple { elts, .. } => {
                for v in elts {
                    self.visit_expr(v)?;
                }
            }
        }
        Ok(())
    }

    fn handle_namedexpr(&mut self, target: &Expr, value: &Expr, loc: Loc) -> R {
        if self.cur().comp_iter_expr > 0 {
            return Err(err("assignment expression cannot be used in a comprehension iterable expression", loc));
        }
        if self.cur().comprehension != CompKind::None {
            self.extend_namedexpr_scope(target)?;
        }
        self.visit_expr(value)?;
        self.visit_expr(target)?;
        Ok(())
    }

    fn extend_namedexpr_scope(&mut self, target: &Expr) -> R {
        let name = match &target.node {
            ExprKind::Name { id, .. } => id.clone(),
            _ => return Ok(()),
        };
        let loc = target.loc;
        for i in (0..self.stack.len()).rev() {
            let sid = self.stack[i];
            let sc = &self.scopes[sid];
            if sc.comprehension != CompKind::None {
                let f = sc.flags(&mangle(self.private.as_deref(), &name));
                if f & DEF_COMP_ITER != 0 && f & DEF_LOCAL != 0 {
                    return Err(err(
                        format!("assignment expression cannot rebind comprehension iteration variable '{}'", name),
                        loc,
                    ));
                }
                continue;
            }
            match sc.kind {
                ScopeKind::Function => {
                    let f = sc.flags(&mangle(self.private.as_deref(), &name));
                    if f & DEF_GLOBAL != 0 {
                        self.add_def(&name, DEF_GLOBAL, loc)?;
                    } else {
                        self.add_def(&name, DEF_NONLOCAL, loc)?;
                    }
                    self.record_directive(&name, loc);
                    return self.add_def_in(&name, DEF_LOCAL, sid, loc);
                }
                ScopeKind::Module => {
                    self.add_def(&name, DEF_GLOBAL, loc)?;
                    self.record_directive(&name, loc);
                    return self.add_def_in(&name, DEF_GLOBAL, sid, loc);
                }
                ScopeKind::Class => {
                    return Err(err("assignment expression within a comprehension cannot be used in a class body", loc))
                }
                ScopeKind::TypeParams => {
                    return Err(err(
                        "assignment expression within a comprehension cannot be used within the definition of a generic",
                        loc,
                    ))
                }
                ScopeKind::TypeAlias => {
                    return Err(err("assignment expression within a comprehension cannot be used in a type alias", loc))
                }
                ScopeKind::TypeVariable => {
                    return Err(err("assignment expression within a comprehension cannot be used in a TypeVar bound", loc))
                }
                ScopeKind::Annotation => return Err(err("named expression cannot be used within an annotation", loc)),
            }
        }
        Ok(())
    }

    fn handle_comprehension(
        &mut self,
        e: &Expr,
        scope_name: &str,
        kind: CompKind,
        generators: &[Comprehension],
        elt: &Expr,
        value: Option<&Expr>,
    ) -> R {
        let is_generator = kind == CompKind::Gen;
        let outermost = match generators.first() {
            Some(g) => g,
            None => return Err(CompileError::other(crate::error::ErrorKind::ValueError, "comprehension with no generators")),
        };
        self.cur_mut().comp_iter_expr += 1;
        let r = self.visit_expr(&outermost.iter);
        self.cur_mut().comp_iter_expr -= 1;
        r?;
        self.enter_block(scope_name, ScopeKind::Function, Some(key(e, TAG_COMP)), e.loc);
        self.cur_mut().comprehension = kind;
        if outermost.is_async != 0 {
            self.cur_mut().coroutine = true;
        }
        self.add_def(".0", DEF_PARAM, e.loc)?;
        self.cur_mut().comp_iter_target = true;
        self.visit_expr(&outermost.target)?;
        self.cur_mut().comp_iter_target = false;
        for i in &outermost.ifs {
            self.visit_expr(i)?;
        }
        for g in &generators[1..] {
            self.cur_mut().comp_iter_target = true;
            self.visit_expr(&g.target)?;
            self.cur_mut().comp_iter_target = false;
            self.cur_mut().comp_iter_expr += 1;
            self.visit_expr(&g.iter)?;
            self.cur_mut().comp_iter_expr -= 1;
            for i in &g.ifs {
                self.visit_expr(i)?;
            }
            if g.is_async != 0 {
                self.cur_mut().coroutine = true;
            }
        }
        if let Some(v) = value {
            self.visit_expr(v)?;
        }
        self.visit_expr(elt)?;
        self.cur_mut().generator = is_generator;
        let is_async = self.cur().coroutine && !is_generator;
        self.exit_block();
        if is_async && !self.is_async_def() && self.cur().comprehension == CompKind::None && !self.allows_top_level_await() {
            return Err(err("asynchronous comprehension outside of an asynchronous function", e.loc));
        }
        if is_async {
            self.cur_mut().coroutine = true;
        }
        Ok(())
    }

    fn visit_pattern(&mut self, p: &Pattern) -> R {
        self.enter_depth()?;
        let r = self.visit_pattern_inner(p);
        self.depth -= 1;
        r
    }

    fn visit_pattern_inner(&mut self, p: &Pattern) -> R {
        match &p.node {
            PatternKind::MatchValue { value } => self.visit_expr(value)?,
            PatternKind::MatchSingleton { .. } => {}
            PatternKind::MatchSequence { patterns } => {
                for q in patterns {
                    self.visit_pattern(q)?;
                }
            }
            PatternKind::MatchStar { name } => {
                if let Some(n) = name {
                    self.add_def(n, DEF_LOCAL, p.loc)?;
                }
            }
            PatternKind::MatchMapping { keys, patterns, rest } => {
                for k in keys {
                    self.visit_expr(k)?;
                }
                for q in patterns {
                    self.visit_pattern(q)?;
                }
                if let Some(r) = rest {
                    self.add_def(r, DEF_LOCAL, p.loc)?;
                }
            }
            PatternKind::MatchClass { cls, patterns, kwd_patterns, .. } => {
                self.visit_expr(cls)?;
                for q in patterns.iter().chain(kwd_patterns.iter()) {
                    self.visit_pattern(q)?;
                }
            }
            PatternKind::MatchAs { pattern, name } => {
                if let Some(q) = pattern {
                    self.visit_pattern(q)?;
                }
                if let Some(n) = name {
                    self.add_def(n, DEF_LOCAL, p.loc)?;
                }
            }
            PatternKind::MatchOr { patterns } => {
                for q in patterns {
                    self.visit_pattern(q)?;
                }
            }
        }
        Ok(())
    }
}

// ====================================================================== analysis

type NameSet = HashSet<String>;

struct Analyzer<'a> {
    scopes: &'a mut Vec<Scope>,
}

fn analyze(st: &mut SymbolTable) -> R {
    let mut free = NameSet::new();
    let mut global = NameSet::new();
    let mut tp = NameSet::new();
    let mut a = Analyzer { scopes: &mut st.scopes };
    a.analyze_block(0, None, &mut free, &mut global, &mut tp, None)
}

impl<'a> Analyzer<'a> {
    #[allow(clippy::too_many_arguments)]
    fn analyze_name(
        &mut self,
        sid: ScopeId,
        scopes: &mut HashMap<String, SymScope>,
        name: &str,
        flags: u32,
        bound: Option<&NameSet>,
        local: &mut NameSet,
        free: &mut NameSet,
        global: &mut NameSet,
        type_params: &mut NameSet,
        class_entry: Option<ScopeId>,
    ) -> R {
        let directive_err = |s: &Scope, msg: String| -> CompileError {
            let loc = s.directives.get(name).copied().unwrap_or(Loc::new(s.lineno, s.col_offset, s.end_lineno, s.end_col_offset));
            err(msg, loc)
        };
        if flags & DEF_GLOBAL != 0 {
            if flags & DEF_NONLOCAL != 0 {
                return Err(directive_err(&self.scopes[sid], format!("name '{}' is nonlocal and global", name)));
            }
            scopes.insert(name.to_string(), SymScope::GlobalExplicit);
            global.insert(name.to_string());
            return Ok(());
        }
        if flags & DEF_NONLOCAL != 0 {
            let bound = match bound {
                None => {
                    return Err(directive_err(&self.scopes[sid], "nonlocal declaration not allowed at module level".to_string()))
                }
                Some(b) => b,
            };
            if !bound.contains(name) {
                return Err(directive_err(&self.scopes[sid], format!("no binding for nonlocal '{}' found", name)));
            }
            if type_params.contains(name) {
                return Err(directive_err(
                    &self.scopes[sid],
                    format!("nonlocal binding not allowed for type parameter '{}'", name),
                ));
            }
            scopes.insert(name.to_string(), SymScope::Free);
            self.scopes[sid].free = true;
            free.insert(name.to_string());
            return Ok(());
        }
        if flags & DEF_BOUND != 0 {
            scopes.insert(name.to_string(), SymScope::Local);
            local.insert(name.to_string());
            global.remove(name);
            if flags & DEF_TYPE_PARAM != 0 {
                type_params.insert(name.to_string());
            } else {
                type_params.remove(name);
            }
            return Ok(());
        }
        // (the implicit class cells are always reached through the closure, even when the
        // class body also binds the name, e.g. `__classdict__: int`)
        let implicit_cell = matches!(name, "__classdict__" | "__class__" | "__conditional_annotations__");
        if let Some(ce) = class_entry.filter(|_| !implicit_cell) {
            let cf = self.scopes[ce].flags(name);
            if cf & DEF_GLOBAL != 0 {
                scopes.insert(name.to_string(), SymScope::GlobalExplicit);
                return Ok(());
            } else if cf & DEF_BOUND != 0 && cf & DEF_NONLOCAL == 0 {
                scopes.insert(name.to_string(), SymScope::GlobalImplicit);
                return Ok(());
            }
        }
        if let Some(b) = bound {
            if b.contains(name) {
                scopes.insert(name.to_string(), SymScope::Free);
                self.scopes[sid].free = true;
                free.insert(name.to_string());
                return Ok(());
            }
        }
        scopes.insert(name.to_string(), SymScope::GlobalImplicit);
        Ok(())
    }

    fn analyze_block(
        &mut self,
        sid: ScopeId,
        bound: Option<&NameSet>,
        free: &mut NameSet,
        global: &mut NameSet,
        type_params: &mut NameSet,
        class_entry: Option<ScopeId>,
    ) -> R {
        let mut local = NameSet::new();
        let mut scopes: HashMap<String, SymScope> = HashMap::new();
        let mut newglobal = NameSet::new();
        let mut newfree = NameSet::new();
        let mut newbound = NameSet::new();
        let mut inlined_cells = NameSet::new();
        let kind = self.scopes[sid].kind;

        if kind == ScopeKind::Class {
            newglobal.extend(global.iter().cloned());
            if let Some(b) = bound {
                newbound.extend(b.iter().cloned());
            }
        }
        let syms: Vec<(String, u32)> = self.scopes[sid].symbols.iter().map(|s| (s.name.clone(), s.flags)).collect();
        for (name, flags) in &syms {
            self.analyze_name(sid, &mut scopes, name, *flags, bound, &mut local, free, global, type_params, class_entry)?;
        }
        if kind != ScopeKind::Class {
            if self.scopes[sid].is_function_like() {
                newbound.extend(local.iter().cloned());
            }
            if let Some(b) = bound {
                newbound.extend(b.iter().cloned());
            }
            // a `global` declaration hides enclosing bindings of the name from
            // nested scopes (they see the global)
            for (name, flags) in &syms {
                if flags & DEF_GLOBAL != 0 {
                    newbound.remove(name);
                }
            }
            newglobal.extend(global.iter().cloned());
        } else {
            newbound.insert("__class__".to_string());
            newbound.insert("__classdict__".to_string());
            newbound.insert("__conditional_annotations__".to_string());
        }

        let children = self.scopes[sid].children.clone();
        let can_see_class = self.scopes[sid].can_see_class_scope;
        for &child in &children {
            let mut child_free = NameSet::new();
            let new_class_entry = if self.scopes[child].can_see_class_scope {
                if kind == ScopeKind::Class {
                    Some(sid)
                } else {
                    class_entry
                }
            } else {
                None
            };
            let inline_comp = self.scopes[child].comprehension != CompKind::None
                && !self.scopes[child].generator
                && !can_see_class;
            {
                let mut tp2 = type_params.clone();
                let mut g2 = newglobal.clone();
                self.analyze_block(child, Some(&newbound), &mut child_free, &mut g2, &mut tp2, new_class_entry)?;
            }
            if inline_comp {
                self.inline_comprehension(sid, child, &mut scopes, &mut child_free, &mut inlined_cells);
                self.scopes[child].comp_inlined = true;
            }
            newfree.extend(child_free);
        }
        // splice children of inlined comprehensions
        let mut new_children = Vec::with_capacity(children.len());
        for &c in &children {
            if self.scopes[c].comp_inlined {
                splice_children(self.scopes, c, &mut new_children);
            } else {
                new_children.push(c);
            }
        }
        self.scopes[sid].children = new_children;

        if self.scopes[sid].is_function_like() {
            // analyze_cells
            for (name, sc) in scopes.iter_mut() {
                if *sc != SymScope::Local {
                    continue;
                }
                if newfree.contains(name) || inlined_cells.contains(name) {
                    *sc = SymScope::Cell;
                    newfree.remove(name);
                }
            }
        } else if kind == ScopeKind::Class {
            if newfree.remove("__class__") {
                self.scopes[sid].needs_class_closure = true;
            }
            if newfree.remove("__classdict__") {
                self.scopes[sid].needs_classdict = true;
            }
            if newfree.remove("__conditional_annotations__") {
                self.scopes[sid].has_conditional_annotations = true;
            }
        }
        // update_symbols
        let classflag = kind == ScopeKind::Class || can_see_class;
        {
            let sc = &mut self.scopes[sid];
            for s in sc.symbols.iter_mut() {
                if inlined_cells.contains(&s.name) {
                    s.flags |= DEF_COMP_CELL;
                }
                s.scope = scopes.get(&s.name).copied().unwrap_or(SymScope::Unknown);
            }
            let mut fv: Vec<&String> = newfree.iter().collect();
            fv.sort();
            for name in fv {
                if let Some(s) = sc.lookup_mut(name) {
                    if classflag {
                        s.flags |= DEF_FREE_CLASS;
                    }
                    continue;
                }
                if let Some(b) = bound {
                    if !b.contains(name.as_str()) {
                        continue;
                    }
                }
                sc.set_symbol(name, 0);
                sc.lookup_mut(name).unwrap().scope = SymScope::Free;
            }
            if !newfree.is_empty() {
                sc.child_free = true;
            }
        }
        free.extend(newfree);
        Ok(())
    }

    fn inline_comprehension(
        &mut self,
        sid: ScopeId,
        comp: ScopeId,
        scopes: &mut HashMap<String, SymScope>,
        comp_free: &mut NameSet,
        inlined_cells: &mut NameSet,
    ) {
        let parent_kind = self.scopes[sid].kind;
        let csyms: Vec<Symbol> = self.scopes[comp].symbols.clone();
        let mut remove = Vec::new();
        for s in &csyms {
            if s.flags & DEF_PARAM != 0 {
                continue; // .0
            }
            let mut scope = s.scope;
            let only_flags = s.flags;
            if scope == SymScope::Cell || only_flags & DEF_COMP_CELL != 0 {
                inlined_cells.insert(s.name.clone());
            }
            if scope == SymScope::Free
                && parent_kind == ScopeKind::Class
                && matches!(s.name.as_str(), "__class__" | "__classdict__" | "__conditional_annotations__")
                && !self.is_free_in_any_child(comp, &s.name)
            {
                scope = SymScope::GlobalImplicit;
                comp_free.remove(&s.name);
                remove.push(s.name.clone());
                // the comprehension body must resolve the name as a global
                if let Some(cs) = self.scopes[comp].lookup_mut(&s.name) {
                    cs.scope = SymScope::GlobalImplicit;
                }
            }
            let existing = self.scopes[sid].lookup(&s.name).map(|x| x.flags);
            let implicit_class_cell = parent_kind == ScopeKind::Class
                && scope == SymScope::Free
                && matches!(s.name.as_str(), "__class__" | "__classdict__" | "__conditional_annotations__");
            match existing {
                None if implicit_class_cell => {}
                None => {
                    self.scopes[sid].set_symbol(&s.name, only_flags);
                    scopes.insert(s.name.clone(), scope);
                }
                Some(flags) => {
                    if flags & DEF_BOUND != 0 && parent_kind != ScopeKind::Class && !self.is_free_in_any_child(comp, &s.name) {
                        comp_free.remove(&s.name);
                    }
                    // a load in the comprehension of the parent's variable counts as a use
                    if parent_kind != ScopeKind::Class && scope == SymScope::Free && only_flags & USE != 0 {
                        self.scopes[sid].set_symbol(&s.name, flags | USE);
                    }
                }
            }
        }
        let _ = remove;
    }

    fn is_free_in_any_child(&self, sid: ScopeId, name: &str) -> bool {
        for &c in &self.scopes[sid].children {
            if self.scopes[c].lookup(name).map(|s| s.scope == SymScope::Free).unwrap_or(false) {
                return true;
            }
        }
        false
    }
}

fn splice_children(scopes: &[Scope], c: ScopeId, out: &mut Vec<ScopeId>) {
    for &cc in &scopes[c].children {
        out.push(cc);
    }
}

// ====================================================================== queries for codegen

impl SymbolTable {
    /// Cell variable names of a scope, sorted (CPython order), plus implicit class cells.
    pub fn cellvars(&self, sid: ScopeId) -> Vec<String> {
        let sc = &self.scopes[sid];
        let mut v: Vec<String> = Vec::new();
        if sc.kind == ScopeKind::Class {
            if sc.needs_class_closure {
                v.push("__class__".to_string());
            }
            if sc.needs_classdict {
                v.push("__classdict__".to_string());
            }
            if sc.has_conditional_annotations {
                v.push("__conditional_annotations__".to_string());
            }
        } else if sc.is_function_like() {
            for s in &sc.symbols {
                if s.scope == SymScope::Cell {
                    v.push(s.name.clone());
                }
            }
        }
        v.sort();
        v
    }

    /// Free variable names of a scope, sorted (CPython: scope FREE, or DEF_FREE_CLASS for names
    /// passed through a class / class-visible annotation scope).
    pub fn freevars(&self, sid: ScopeId) -> Vec<String> {
        let sc = &self.scopes[sid];
        let mut v: Vec<String> = Vec::new();
        for s in &sc.symbols {
            if s.scope == SymScope::Free || (s.flags & DEF_FREE_CLASS != 0 && s.scope != SymScope::Cell) {
                v.push(s.name.clone());
            }
        }
        v.sort();
        v
    }
}
