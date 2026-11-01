//! Code generation: AST (+ symbol table) -> IR -> [`CodeUnit`].

mod expr;
pub(crate) use expr::const_truth;
mod pattern;
mod stmt;

use std::collections::{BTreeSet, HashMap};

use crate::assemble::{self, Instr, IOp, Ir, Label, UnitInfo, CELL_TAG, FREE_TAG, NO_LABEL};
use crate::ast::{self, Expr, ExprKind, Loc, Mod, Stmt, StmtKind};
use crate::bytecode::*;
use crate::cf;
use crate::error::{CompileError, ErrorKind, Warning};
use crate::symtable::{self, mangle, key, ScopeId, ScopeKind, SymScope, SymbolTable};
use crate::{CompileOptions, CompileOutput, Mode};

pub(crate) type R<T = ()> = Result<T, CompileError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnitKind {
    Module,
    Class,
    Function,
    AsyncFunction,
    Lambda,
    Comprehension,
    Annotations,
    TypeParams,
}

/// Frame-block kinds for break/continue/return unwinding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FbKind {
    WhileLoop,
    ForLoop,
    TryExcept,
    FinallyTry,
    FinallyEnd,
    With,
    AsyncWith,
    HandlerCleanup,
    PopValue,
    ExceptionHandler,
    ExceptionGroupHandler,
    AsyncComprehensionGenerator,
    StopIteration,
}

#[derive(Clone)]
pub(crate) struct FBlock<'a> {
    pub kind: FbKind,
    pub block: Label,
    pub exit: Label,
    pub loc: Location,
    /// FinallyTry: the finally body
    pub finally: Option<&'a [Stmt]>,
    /// HandlerCleanup: bound exception name
    pub name: Option<&'a str>,
}

/// Slot of a name inside an inlined comprehension.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CompSlot {
    /// fast index, or CELL_TAG/FREE_TAG tagged slot
    pub slot: u32,
    pub is_cell: bool,
}

pub(crate) struct CompCtx {
    pub scope: ScopeId,
    pub slots: HashMap<String, CompSlot>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum CKey {
    None,
    Bool(bool),
    Ellipsis,
    Int(i64),
    Big(bool, String),
    Float(u64),
    Complex(u64, u64),
    Str(Vec<u32>),
    Bytes(Vec<u8>),
    Tuple(Vec<CKey>),
    FrozenSet(Vec<CKey>),
    Code(usize),
}

fn ckey(c: &Constant, code_ctr: &mut usize) -> CKey {
    match c {
        Constant::None => CKey::None,
        Constant::Bool(b) => CKey::Bool(*b),
        Constant::Ellipsis => CKey::Ellipsis,
        Constant::Int(i) => CKey::Int(*i),
        Constant::BigInt { neg, decimal } => CKey::Big(*neg, decimal.clone()),
        Constant::Float(f) => CKey::Float(f.to_bits()),
        Constant::Complex { real, imag } => CKey::Complex(real.to_bits(), imag.to_bits()),
        Constant::Str(s) => CKey::Str(s.clone()),
        Constant::Bytes(b) => CKey::Bytes(b.clone()),
        Constant::Tuple(v) => CKey::Tuple(v.iter().map(|x| ckey(x, code_ctr)).collect()),
        Constant::FrozenSet(v) => CKey::FrozenSet(v.iter().map(|x| ckey(x, code_ctr)).collect()),
        Constant::Code(_) => {
            *code_ctr += 1;
            CKey::Code(*code_ctr)
        }
    }
}

pub(crate) struct Unit<'a> {
    pub ir: Ir,
    pub scope: ScopeId,
    pub kind: UnitKind,
    pub name: String,
    pub qualname: String,
    pub firstlineno: u32,
    pub argcount: u32,
    pub posonlyargcount: u32,
    pub kwonlyargcount: u32,
    pub flags: u32,
    pub consts: Vec<Constant>,
    const_map: HashMap<CKey, u32>,
    code_ctr: usize,
    pub names: Vec<String>,
    name_map: HashMap<String, u32>,
    pub varnames: Vec<String>,
    var_map: HashMap<String, u32>,
    pub varkinds: Vec<u8>,
    pub cellvars: Vec<String>,
    cell_map: HashMap<String, u32>,
    pub cellkinds: Vec<u8>,
    pub cell_arg: Vec<Option<u32>>,
    pub freevars: Vec<String>,
    free_map: HashMap<String, u32>,
    pub fblocks: Vec<FBlock<'a>>,
    pub comps: Vec<CompCtx>,
    pub private: Option<String>,
    pub static_attrs: Option<BTreeSet<String>>,
    /// Deferred (PEP 649) annotations: (statement, conditional index)
    pub deferred: Vec<(&'a Stmt, Option<u32>)>,
    pub next_cond_index: u32,
    pub loc: Location,
    pub nparams: u32,
    pub function_like: bool,
    pub prologue: Label,
}

impl<'a> Unit<'a> {
    pub fn add_const(&mut self, c: Constant) -> u32 {
        let k = ckey(&c, &mut self.code_ctr);
        if let Some(&i) = self.const_map.get(&k) {
            return i;
        }
        let i = self.consts.len() as u32;
        self.consts.push(c);
        self.const_map.insert(k, i);
        i
    }
    pub fn add_name(&mut self, n: &str) -> u32 {
        if let Some(&i) = self.name_map.get(n) {
            return i;
        }
        let i = self.names.len() as u32;
        self.names.push(n.to_string());
        self.name_map.insert(n.to_string(), i);
        i
    }
    pub fn var_index(&mut self, n: &str) -> u32 {
        if let Some(&i) = self.var_map.get(n) {
            return i;
        }
        let i = self.varnames.len() as u32;
        self.varnames.push(n.to_string());
        self.varkinds.push(fast::LOCAL);
        self.var_map.insert(n.to_string(), i);
        i
    }
    fn hidden_var(&mut self, n: &str) -> u32 {
        if let Some(&i) = self.var_map.get(n) {
            return i;
        }
        let i = self.var_index(n);
        self.varkinds[i as usize] |= fast::HIDDEN;
        i
    }
    fn hidden_cell(&mut self, n: &str) -> u32 {
        if let Some(&i) = self.cell_map.get(n) {
            return i | CELL_TAG;
        }
        let i = self.cellvars.len() as u32;
        self.cellvars.push(n.to_string());
        self.cellkinds.push(fast::CELL | fast::HIDDEN);
        self.cell_arg.push(None);
        self.cell_map.insert(n.to_string(), i);
        i | CELL_TAG
    }
    pub fn cell_slot(&self, n: &str) -> Option<u32> {
        self.cell_map.get(n).map(|&i| i | CELL_TAG)
    }
    pub fn free_slot(&self, n: &str) -> Option<u32> {
        self.free_map.get(n).map(|&i| i | FREE_TAG)
    }
}

pub(crate) struct Compiler<'a> {
    pub st: SymbolTable,
    pub opts: &'a CompileOptions,
    pub future: u32,
    pub optimize: i32,
    pub units: Vec<Unit<'a>>,
    pub warnings: Vec<Warning>,
    pub depth: u32,
    pub interactive: bool,
}

pub(crate) fn loc_of(l: Loc) -> Location {
    if l.lineno <= 0 {
        return Location { lineno: 0, end_lineno: 0, col: u32::MAX, end_col: u32::MAX };
    }
    let end_lineno = if l.end_lineno >= l.lineno { l.end_lineno as u32 } else { l.lineno as u32 };
    Location {
        lineno: l.lineno as u32,
        end_lineno,
        col: if l.col_offset >= 0 { l.col_offset as u32 } else { u32::MAX },
        end_col: if l.end_col_offset >= 0 { l.end_col_offset as u32 } else { u32::MAX },
    }
}

pub(crate) const NO_LOC: Location = Location { lineno: 0, end_lineno: 0, col: u32::MAX, end_col: u32::MAX };

pub(crate) fn conv_const(c: &ast::Constant) -> Constant {
    match c {
        ast::Constant::None => Constant::None,
        ast::Constant::Bool(b) => Constant::Bool(*b),
        ast::Constant::Ellipsis => Constant::Ellipsis,
        ast::Constant::Int(ast::Int::Small(i)) => Constant::Int(*i),
        ast::Constant::Int(ast::Int::Big(s)) => {
            let (neg, d) = match s.strip_prefix('-') {
                Some(r) => (true, r),
                None => (false, s.as_str()),
            };
            Constant::BigInt { neg, decimal: d.to_string() }
        }
        ast::Constant::Float(f) => Constant::Float(*f),
        ast::Constant::Complex { real, imag } => Constant::Complex { real: *real, imag: *imag },
        ast::Constant::Str(s) => Constant::Str(s.code_points().collect()),
        ast::Constant::Bytes(b) => Constant::Bytes(b.clone()),
        ast::Constant::Tuple(v) => Constant::Tuple(v.iter().map(conv_const).collect()),
        ast::Constant::FrozenSet(v) => Constant::FrozenSet(v.iter().map(conv_const).collect()),
    }
}

pub(crate) fn str_const(s: &str) -> Constant {
    Constant::Str(s.chars().map(|c| c as u32).collect())
}

/// `from __future__` processing: returns the CO_FUTURE_* flags in effect.
pub(crate) fn check_future(m: &Mod, inherited: u32) -> R<u32> {
    let mut flags = inherited & cf::PYCF_MASK;
    let body = match m {
        Mod::Module { body, .. } | Mod::Interactive { body } => body,
        _ => return Ok(flags),
    };
    let mut done = false;
    for (i, s) in body.iter().enumerate() {
        match &s.node {
            StmtKind::ImportFrom { module: Some(m), names, .. } if m == "__future__" => {
                if done {
                    return Err(CompileError::syntax("from __future__ imports must occur at the beginning of the file", s.loc));
                }
                for a in names {
                    match a.name.as_str() {
                        "nested_scopes" | "generators" | "with_statement" | "division" | "absolute_import"
                        | "print_function" | "unicode_literals" => {}
                        "generator_stop" => flags |= cf::CO_FUTURE_GENERATOR_STOP,
                        "barry_as_FLUFL" => flags |= cf::CO_FUTURE_BARRY_AS_BDFL,
                        "annotations" => flags |= cf::CO_FUTURE_ANNOTATIONS,
                        "braces" => return Err(CompileError::syntax("not a chance", a.loc)),
                        other => {
                            return Err(CompileError::syntax(format!("future feature {} is not defined", other), a.loc))
                        }
                    }
                }
            }
            StmtKind::Expr { value } if i == 0 && is_docstring_expr(value) => {}
            _ => done = true,
        }
    }
    Ok(flags)
}

pub(crate) fn is_docstring_expr(e: &Expr) -> bool {
    matches!(&e.node, ExprKind::Constant { value: ast::Constant::Str(_), .. })
}

/// The docstring of a body, if its first statement is a string literal.
pub(crate) fn get_docstring(body: &[Stmt]) -> Option<&ast::PyStr> {
    match body.first().map(|s| &s.node) {
        Some(StmtKind::Expr { value }) => match &value.node {
            ExprKind::Constant { value: ast::Constant::Str(s), .. } => Some(s),
            _ => None,
        },
        _ => None,
    }
}

/// Docstring cleaning (CPython 3.13+ `_PyCompile_CleanDoc`): expand tabs, strip the common
/// indentation of lines after the first, strip leading spaces of the first line.
pub(crate) fn clean_doc(doc: &ast::PyStr) -> Vec<u32> {
    // expandtabs(8)
    let mut s: Vec<u32> = Vec::new();
    let mut col = 0usize;
    for c in doc.code_points() {
        if c == '\t' as u32 {
            let n = 8 - (col % 8);
            for _ in 0..n {
                s.push(' ' as u32);
            }
            col += n;
        } else {
            s.push(c);
            if c == '\n' as u32 || c == '\r' as u32 {
                col = 0;
            } else {
                col += 1;
            }
        }
    }
    let nl = '\n' as u32;
    let sp = ' ' as u32;
    let n = s.len();
    let mut p = 0;
    while p < n && s[p] != nl {
        p += 1;
    }
    if p < n {
        p += 1;
    }
    let mut margin = usize::MAX;
    while p < n {
        let start = p;
        while p < n && s[p] == sp {
            p += 1;
        }
        if p < n && s[p] != nl {
            margin = margin.min(p - start);
        }
        while p < n && s[p] != nl {
            p += 1;
        }
        if p < n {
            p += 1;
        }
    }
    if margin == usize::MAX {
        margin = 0;
    }
    let mut p = 0;
    while p < n && s[p] == sp {
        p += 1;
    }
    if p == 0 && margin == 0 {
        return s;
    }
    let mut out = Vec::with_capacity(n);
    while p < n {
        let ch = s[p];
        out.push(ch);
        p += 1;
        if ch == nl {
            break;
        }
    }
    while p < n {
        let mut i = 0;
        while i < margin && p < n && s[p] == sp {
            i += 1;
            p += 1;
        }
        while p < n {
            let ch = s[p];
            out.push(ch);
            p += 1;
            if ch == nl {
                break;
            }
        }
    }
    out
}

pub(crate) fn compile_module(module: &Mod, opts: &CompileOptions) -> R<CompileOutput> {
    let future = check_future(module, opts.flags)?;
    if opts.mode == Mode::FuncType || matches!(module, Mod::FunctionType { .. }) {
        return Err(CompileError::other(ErrorKind::ValueError, "compile() of a func_type AST is not supported"));
    }
    // fold a private copy of the AST
    let mut m = module.clone();
    let mut warnings = Vec::new();
    crate::fold::fold_module(&mut m, opts.optimize, future, &mut warnings, opts.max_depth)?;
    let st = symtable::build(&m, future | (opts.flags & cf::PYCF_ALLOW_TOP_LEVEL_AWAIT), opts.max_depth)?;
    let mut c = Compiler {
        st,
        opts,
        future,
        optimize: opts.optimize.max(0),
        units: Vec::new(),
        warnings,
        depth: 0,
        interactive: opts.interactive || matches!(m, Mod::Interactive { .. }),
    };
    let code = c.compile_mod(&m)?;
    // dedupe warnings
    let mut seen = std::collections::HashSet::new();
    let mut ws = Vec::new();
    for w in c.warnings.drain(..) {
        if seen.insert((w.lineno, w.col_offset, w.msg.clone())) {
            ws.push(w);
        }
    }
    Ok(CompileOutput { code, warnings: ws, future_flags: future })
}

impl<'a> Compiler<'a> {
    // ------------------------------------------------------------------ unit management

    pub fn u(&mut self) -> &mut Unit<'a> {
        self.units.last_mut().unwrap()
    }
    pub fn ur(&self) -> &Unit<'a> {
        self.units.last().unwrap()
    }

    pub fn error(&self, msg: impl Into<String>, loc: Loc) -> CompileError {
        CompileError::syntax(msg, loc)
    }

    pub fn enter_depth(&mut self) -> R {
        self.depth += 1;
        if self.depth > self.opts.max_depth {
            return Err(CompileError::recursion());
        }
        Ok(())
    }

    /// Create a new compilation unit for symbol-table scope `scope`.
    pub fn enter_scope(&mut self, name: &str, kind: UnitKind, scope: ScopeId, firstlineno: i32) -> R {
        let sc = &self.st.scopes[scope];
        let function_like = sc.is_function_like();
        let mut u = Unit {
            ir: Ir::new(),
            scope,
            kind,
            name: name.to_string(),
            qualname: String::new(),
            firstlineno: firstlineno.max(0) as u32,
            argcount: 0,
            posonlyargcount: 0,
            kwonlyargcount: 0,
            flags: 0,
            consts: Vec::new(),
            const_map: HashMap::new(),
            code_ctr: 0,
            names: Vec::new(),
            name_map: HashMap::new(),
            varnames: Vec::new(),
            var_map: HashMap::new(),
            varkinds: Vec::new(),
            cellvars: Vec::new(),
            cell_map: HashMap::new(),
            cellkinds: Vec::new(),
            cell_arg: Vec::new(),
            freevars: Vec::new(),
            free_map: HashMap::new(),
            fblocks: Vec::new(),
            comps: Vec::new(),
            private: None,
            static_attrs: if kind == UnitKind::Class { Some(BTreeSet::new()) } else { None },
            deferred: Vec::new(),
            next_cond_index: 0,
            loc: NO_LOC,
            nparams: 0,
            function_like,
            prologue: 0,
        };
        for v in &sc.varnames {
            let i = u.varnames.len() as u32;
            u.varnames.push(v.clone());
            u.varkinds.push(fast::LOCAL);
            u.var_map.insert(v.clone(), i);
        }
        u.nparams = sc.varnames.len() as u32;
        for (i, cname) in self.st.cellvars(scope).into_iter().enumerate() {
            let arg = u.var_map.get(&cname).copied().filter(|&ix| ix < u.nparams);
            u.cell_map.insert(cname.clone(), i as u32);
            u.cellvars.push(cname);
            u.cellkinds.push(fast::CELL);
            u.cell_arg.push(arg);
        }
        for (i, fname) in self.st.freevars(scope).into_iter().enumerate() {
            u.free_map.insert(fname.clone(), i as u32);
            u.freevars.push(fname);
        }
        // private name
        u.private = if kind == UnitKind::Class {
            Some(name.to_string())
        } else {
            self.units.last().and_then(|p| p.private.clone())
        };
        if sc.kind == ScopeKind::TypeParams && sc.private.is_some() {
            u.private = sc.private.clone();
        }
        // flags
        let mut flags = 0;
        if function_like {
            flags |= co::NEWLOCALS | co::OPTIMIZED;
            if sc.nested {
                flags |= co::NESTED;
            }
            if sc.generator && !sc.coroutine {
                flags |= co::GENERATOR;
            }
            if sc.generator && sc.coroutine {
                flags |= co::ASYNC_GENERATOR;
            }
            if sc.varargs {
                flags |= co::VARARGS;
            }
            if sc.varkeywords {
                flags |= co::VARKEYWORDS;
            }
        }
        if sc.coroutine && !sc.generator && (function_like || kind == UnitKind::Module) {
            flags |= co::COROUTINE;
        }
        flags |= self.future & cf::PYCF_MASK;
        u.flags = flags;
        // body starts in a fresh block after the (later filled) prologue block
        let body = u.ir.new_label();
        u.ir.use_label(body);
        self.units.push(u);
        self.set_qualname();
        Ok(())
    }

    fn set_qualname(&mut self) {
        let n = self.units.len();
        let u = &self.units[n - 1];
        let name = u.name.clone();
        if n == 1 {
            self.units[0].qualname = name;
            return;
        }
        // annotation units of functions: <function qualname>.__annotate__ (set by caller)
        let mut pi = n - 2;
        if self.units[pi].kind == UnitKind::TypeParams && pi > 0 && u.kind != UnitKind::Annotations {
            pi -= 1;
        }
        let parent = &self.units[pi];
        let mut force_global = false;
        if matches!(u.kind, UnitKind::Function | UnitKind::AsyncFunction | UnitKind::Class) {
            let m = mangle(parent.private.as_deref(), &name);
            let psc = &self.st.scopes[parent.scope];
            if psc.scope_of(&m) == SymScope::GlobalExplicit {
                force_global = true;
            }
        }
        let q = if force_global || parent.kind == UnitKind::Module {
            name
        } else if matches!(parent.kind, UnitKind::Function | UnitKind::AsyncFunction | UnitKind::Lambda) {
            format!("{}.<locals>.{}", parent.qualname, name)
        } else {
            format!("{}.{}", parent.qualname, name)
        };
        self.units[n - 1].qualname = q;
    }

    /// Finish the current unit and assemble it.
    pub fn exit_scope(&mut self) -> R<CodeUnit> {
        let mut u = self.units.pop().unwrap();
        // prologue: CopyFreeVars, MakeCell, Resume
        let mut pro: Vec<Instr> = Vec::new();
        if !u.freevars.is_empty() {
            pro.push(Instr { op: IOp::Op(Op::CopyFreeVars), arg: u.freevars.len() as u32, target: NO_LABEL, loc: NO_LOC, flags: 0x80 });
        }
        for (i, c) in u.cellkinds.iter().enumerate() {
            if c & fast::HIDDEN != 0 {
                continue;
            }
            pro.push(Instr { op: IOp::Op(Op::MakeCell), arg: i as u32 | CELL_TAG, target: NO_LABEL, loc: NO_LOC, flags: 0x80 });
        }
        let resume_loc = Location { lineno: u.firstlineno, end_lineno: u.firstlineno, col: u32::MAX, end_col: u32::MAX };
        pro.push(Instr { op: IOp::Op(Op::Resume), arg: 0, target: NO_LABEL, loc: if u.kind == UnitKind::Module { NO_LOC } else { resume_loc }, flags: 0x80 });
        u.ir.blocks[u.prologue as usize].instrs = pro;
        let sc = &self.st.scopes[u.scope];
        let mut varnames = u.varnames;
        // the implicit `format` parameter is called `.format` in the symbol table
        for v in varnames.iter_mut() {
            if v == ".format" {
                *v = "format".to_string();
            }
        }
        // parameter kinds
        let mut varkinds = u.varkinds;
        let np = u.argcount as usize;
        let po = u.posonlyargcount as usize;
        let kw = u.kwonlyargcount as usize;
        for (i, k) in varkinds.iter_mut().enumerate().take(u.nparams as usize) {
            if i < po {
                *k |= fast::ARG_POS;
            } else if i < np {
                *k |= fast::ARG_POS | fast::ARG_KW;
            } else if i < np + kw {
                *k |= fast::ARG_KW;
            } else {
                // *args then **kwargs
                let is_varargs = sc.varargs && i == np + kw;
                *k |= fast::ARG_VAR | if is_varargs { fast::ARG_POS } else { fast::ARG_KW };
            }
        }
        let info = UnitInfo {
            name: u.name,
            qualname: u.qualname,
            filename: self.opts.filename.clone(),
            firstlineno: u.firstlineno,
            argcount: u.argcount,
            posonlyargcount: u.posonlyargcount,
            kwonlyargcount: u.kwonlyargcount,
            flags: u.flags,
            consts: u.consts,
            names: u.names,
            varnames,
            varkinds,
            cellvars: u.cellvars,
            cellkinds: u.cellkinds,
            cell_arg: u.cell_arg,
            freevars: u.freevars,
            nparams: u.nparams,
        };
        assemble::assemble(u.ir, info)
    }

    // ------------------------------------------------------------------ emission

    pub fn set_loc(&mut self, l: Loc) -> Location {
        let old = self.u().loc;
        self.u().loc = loc_of(l);
        old
    }

    pub fn emit(&mut self, op: Op, arg: u32) {
        let loc = self.ur().loc;
        self.u().ir.emit(Instr { op: IOp::Op(op), arg, target: NO_LABEL, loc, flags: 0 });
    }
    pub fn emit_loc(&mut self, op: Op, arg: u32, loc: Location) {
        self.u().ir.emit(Instr { op: IOp::Op(op), arg, target: NO_LABEL, loc, flags: 0 });
    }
    pub fn emit_noloc(&mut self, op: Op, arg: u32) {
        self.u().ir.emit(Instr { op: IOp::Op(op), arg, target: NO_LABEL, loc: NO_LOC, flags: 0 });
    }
    pub fn emit_jump(&mut self, op: Op, target: Label) {
        let loc = self.ur().loc;
        self.u().ir.emit(Instr { op: IOp::Op(op), arg: 0, target, loc, flags: 0 });
    }
    pub fn emit_jump_noloc(&mut self, op: Op, target: Label) {
        self.u().ir.emit(Instr { op: IOp::Op(op), arg: 0, target, loc: NO_LOC, flags: 0 });
    }
    pub fn emit_pseudo(&mut self, op: IOp, target: Label) {
        let loc = self.ur().loc;
        self.u().ir.emit(Instr { op, arg: 0, target, loc, flags: 0 });
    }
    pub fn pop_block(&mut self) {
        self.u().ir.emit(Instr { op: IOp::PopBlock, arg: 0, target: NO_LABEL, loc: NO_LOC, flags: 0 });
    }
    pub fn new_label(&mut self) -> Label {
        self.u().ir.new_label()
    }
    pub fn use_label(&mut self, l: Label) {
        self.u().ir.use_label(l);
    }
    pub fn load_const(&mut self, c: Constant) {
        let i = self.u().add_const(c);
        self.emit(Op::LoadConst, i);
    }
    pub fn load_const_noloc(&mut self, c: Constant) {
        let i = self.u().add_const(c);
        self.emit_noloc(Op::LoadConst, i);
    }
    pub fn load_none(&mut self) {
        self.load_const(Constant::None);
    }
    pub fn emit_name(&mut self, op: Op, name: &str) {
        let i = self.u().add_name(name);
        self.emit(op, i);
    }

    pub fn mangle(&self, name: &str) -> String {
        let u = self.ur();
        let sc = &self.st.scopes[u.scope];
        if let Some(m) = &sc.mangled_names {
            if !m.contains(name) {
                return name.to_string();
            }
        }
        mangle(u.private.as_deref(), name)
    }

    // ------------------------------------------------------------------ name resolution

    /// Emit a load/store/delete of `name`.
    pub fn nameop(&mut self, name: &str, ctx: ast::ExprContext) -> R {
        let mangled = self.mangle(name);
        if name == "__debug__" && ctx != ast::ExprContext::Load {
            let l = self.ur().loc;
            let loc = Loc::new(l.lineno as i32, l.col as i32, l.end_lineno as i32, l.end_col as i32);
            let what = if ctx == ast::ExprContext::Del { "cannot delete __debug__" } else { "cannot assign to __debug__" };
            return Err(self.error(what, loc));
        }
        if ctx == ast::ExprContext::Load && name == "__debug__" {
            self.load_const(Constant::Bool(self.optimize == 0));
            return Ok(());
        }
        // inlined comprehension contexts
        let ncomp = self.ur().comps.len();
        for ci in (0..ncomp).rev() {
            let sid = self.ur().comps[ci].scope;
            let s = self.st.scopes[sid].scope_of(&mangled);
            match s {
                SymScope::Local | SymScope::Cell => {
                    if let Some(slot) = self.ur().comps[ci].slots.get(&mangled).copied() {
                        return self.slot_op(slot, ctx);
                    }
                }
                SymScope::GlobalExplicit | SymScope::GlobalImplicit => {
                    let op = match ctx {
                        ast::ExprContext::Load => Op::LoadGlobal,
                        ast::ExprContext::Store => Op::StoreGlobal,
                        ast::ExprContext::Del => Op::DeleteGlobal,
                    };
                    self.emit_name(op, &mangled);
                    return Ok(());
                }
                _ => {}
            }
            if s == SymScope::Free {
                // resolved by an outer context; if the unit itself is a class or the name is
                // free/cell in the unit, use the deref slot directly
                if ci == 0 {
                    let u = self.ur();
                    let usc = &self.st.scopes[u.scope];
                    if !usc.is_function_like() {
                        if let Some(slot) = u.free_slot(&mangled).or_else(|| u.cell_slot(&mangled)) {
                            return self.slot_op(CompSlot { slot, is_cell: true }, ctx);
                        }
                    }
                }
            }
        }
        let u = self.ur();
        let sc = &self.st.scopes[u.scope];
        let scope = sc.scope_of(&mangled);
        let in_comp = ncomp > 0;
        let is_class = sc.kind == ScopeKind::Class;
        let can_see_class = sc.can_see_class_scope;
        let fl = sc.is_function_like();
        enum K {
            Fast,
            Deref(u32),
            Global,
            Name,
        }
        let k = match scope {
            SymScope::Free => match u.free_slot(&mangled) {
                Some(s) => K::Deref(s),
                None => K::Name,
            },
            SymScope::Cell => {
                if fl {
                    match u.cell_slot(&mangled) {
                        Some(s) => K::Deref(s),
                        None => K::Fast,
                    }
                } else {
                    // comprehension-only cell in a module/class: name op outside the comp
                    K::Name
                }
            }
            SymScope::Local => {
                if fl {
                    K::Fast
                } else {
                    K::Name
                }
            }
            SymScope::GlobalImplicit => {
                if fl {
                    K::Global
                } else {
                    K::Name
                }
            }
            SymScope::GlobalExplicit => K::Global,
            SymScope::Unknown => {
                if fl {
                    K::Fast
                } else {
                    K::Name
                }
            }
        };
        match k {
            K::Fast => {
                let i = self.u().var_index(&mangled);
                match ctx {
                    ast::ExprContext::Load => self.emit(Op::LoadFastCheck, i),
                    ast::ExprContext::Store => self.emit(Op::StoreFast, i),
                    ast::ExprContext::Del => self.emit(Op::DeleteFast, i),
                }
            }
            K::Deref(slot) => match ctx {
                ast::ExprContext::Load => {
                    if is_class && !in_comp {
                        self.emit(Op::LoadLocals, 0);
                        self.emit(Op::LoadFromDictOrDeref, slot);
                    } else if can_see_class && mangled != "__classdict__" {
                        let cd = self.classdict_slot()?;
                        self.emit(Op::LoadDeref, cd);
                        self.emit(Op::LoadFromDictOrDeref, slot);
                    } else {
                        self.emit(Op::LoadDeref, slot);
                    }
                }
                ast::ExprContext::Store => self.emit(Op::StoreDeref, slot),
                ast::ExprContext::Del => self.emit(Op::DeleteDeref, slot),
            },
            K::Global => match ctx {
                ast::ExprContext::Load => {
                    if can_see_class && scope == SymScope::GlobalImplicit {
                        let cd = self.classdict_slot()?;
                        self.emit(Op::LoadDeref, cd);
                        self.emit_name(Op::LoadFromDictOrGlobals, &mangled);
                    } else {
                        self.emit_name(Op::LoadGlobal, &mangled);
                    }
                }
                ast::ExprContext::Store => self.emit_name(Op::StoreGlobal, &mangled),
                ast::ExprContext::Del => self.emit_name(Op::DeleteGlobal, &mangled),
            },
            K::Name => match ctx {
                ast::ExprContext::Load => self.emit_name(Op::LoadName, &mangled),
                ast::ExprContext::Store => self.emit_name(Op::StoreName, &mangled),
                ast::ExprContext::Del => self.emit_name(Op::DeleteName, &mangled),
            },
        }
        Ok(())
    }

    fn classdict_slot(&self) -> R<u32> {
        let u = self.ur();
        u.free_slot("__classdict__")
            .or_else(|| u.cell_slot("__classdict__"))
            .ok_or_else(|| CompileError::other(ErrorKind::SystemError, "missing __classdict__ cell"))
    }

    fn slot_op(&mut self, slot: CompSlot, ctx: ast::ExprContext) -> R {
        if slot.is_cell {
            match ctx {
                ast::ExprContext::Load => self.emit(Op::LoadDeref, slot.slot),
                ast::ExprContext::Store => self.emit(Op::StoreDeref, slot.slot),
                ast::ExprContext::Del => self.emit(Op::DeleteDeref, slot.slot),
            }
        } else {
            match ctx {
                ast::ExprContext::Load => self.emit(Op::LoadFastCheck, slot.slot),
                ast::ExprContext::Store => self.emit(Op::StoreFast, slot.slot),
                ast::ExprContext::Del => self.emit(Op::DeleteFast, slot.slot),
            }
        }
        Ok(())
    }

    /// Slot holding the cell for `name` when building a closure for a child scope.
    pub fn closure_slot(&self, name: &str) -> R<u32> {
        let u = self.ur();
        for ci in (0..u.comps.len()).rev() {
            let cs = &u.comps[ci];
            let s = self.st.scopes[cs.scope].scope_of(name);
            if matches!(s, SymScope::Cell | SymScope::Local) {
                if let Some(sl) = cs.slots.get(name) {
                    if sl.is_cell {
                        return Ok(sl.slot);
                    }
                }
            }
        }
        let sc = &self.st.scopes[u.scope];
        let s = sc.scope_of(name);
        if s == SymScope::Cell || sc.kind == ScopeKind::Class {
            if let Some(c) = u.cell_slot(name) {
                return Ok(c);
            }
        }
        if let Some(f) = u.free_slot(name) {
            return Ok(f);
        }
        if let Some(c) = u.cell_slot(name) {
            return Ok(c);
        }
        Err(CompileError::other(
            ErrorKind::SystemError,
            format!("compiler: no cell for free variable '{}' in '{}'", name, u.qualname),
        ))
    }

    /// Push a function object for `code` (with closure if needed) and apply attributes
    /// already pushed below it (`flags` = make_fn bits set, in push order
    /// defaults, kwdefaults, annotate, closure).
    pub fn make_closure(&mut self, code: CodeUnit, mut flags: u32) -> R {
        let frees: Vec<String> = code
            .localsplusnames
            .iter()
            .zip(code.localspluskinds.iter())
            .filter(|(_, k)| **k & fast::FREE != 0)
            .map(|(n, _)| n.clone())
            .collect();
        if !frees.is_empty() {
            for f in &frees {
                let slot = self.closure_slot(f)?;
                self.emit(Op::LoadFast, slot);
            }
            self.emit(Op::BuildTuple, frees.len() as u32);
            flags |= make_fn::CLOSURE;
        }
        let i = self.u().add_const(Constant::Code(Box::new(code)));
        self.emit(Op::LoadConst, i);
        self.emit(Op::MakeFunction, 0);
        for bit in [make_fn::CLOSURE, make_fn::ANNOTATE, make_fn::ANNOTATIONS, make_fn::KWDEFAULTS, make_fn::DEFAULTS] {
            if flags & bit != 0 {
                self.emit(Op::SetFunctionAttribute, bit);
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------ module

    fn compile_mod(&mut self, m: &'a Mod) -> R<CodeUnit> {
        self.enter_scope("<module>", UnitKind::Module, 0, 1)?;
        self.u().firstlineno = 1;
        match m {
            Mod::Module { body, .. } => {
                if let Some(s) = body.first() {
                    let _ = s;
                }
                self.compile_body(body, true)?;
                self.process_deferred_annotations_module()?;
                self.u().loc = NO_LOC;
                if !self.ur().ir.last_is_terminal() {
                    self.load_const_noloc(Constant::None);
                    self.emit_noloc(Op::ReturnValue, 0);
                }
            }
            Mod::Interactive { body } => {
                self.compile_body(body, false)?;
                self.process_deferred_annotations_module()?;
                self.u().loc = NO_LOC;
                self.load_const_noloc(Constant::None);
                self.emit_noloc(Op::ReturnValue, 0);
            }
            Mod::Expression { body } => {
                self.compile_expr(body)?;
                self.emit(Op::ReturnValue, 0);
            }
            Mod::FunctionType { .. } => unreachable!(),
        }
        self.exit_scope()
    }

    /// Module / class body: docstring handling, SETUP_ANNOTATIONS, statements.
    fn compile_body(&mut self, body: &'a [Stmt], docstring_allowed: bool) -> R {
        let mut start = 0;
        let is_module = self.ur().kind == UnitKind::Module;
        let sc_kind = self.st.scopes[self.ur().scope].kind;
        // old-style annotations (future import)
        if self.future & cf::CO_FUTURE_ANNOTATIONS != 0 && find_ann(body) {
            if let Some(s) = body.first() {
                self.set_loc(s.loc);
            }
            self.emit(Op::SetupAnnotations, 0);
        }
        // conditional annotations set
        if self.future & cf::CO_FUTURE_ANNOTATIONS == 0 && self.st.scopes[self.ur().scope].has_conditional_annotations {
            if let Some(s) = body.first() {
                self.set_loc(s.loc);
            }
            self.emit(Op::BuildSet, 0);
            if sc_kind == ScopeKind::Class {
                let slot = self.ur().cell_slot("__conditional_annotations__").unwrap();
                self.emit(Op::StoreDeref, slot);
            } else {
                self.emit_name(Op::StoreName, "__conditional_annotations__");
            }
        }
        if docstring_allowed && !self.interactive {
            if let Some(doc) = get_docstring(body) {
                start = 1;
                if self.optimize < 2 {
                    let s = &body[0];
                    self.set_loc(s.loc);
                    let d = clean_doc(doc);
                    self.load_const(Constant::Str(d));
                    self.emit_name(Op::StoreName, "__doc__");
                } else if is_module || true {
                    // keep a line marker
                }
            }
        }
        for s in &body[start..] {
            self.compile_stmt(s)?;
        }
        Ok(())
    }

    /// Emit the module-level `__annotate__` function (into the prologue area).
    fn process_deferred_annotations_module(&mut self) -> R {
        if self.ur().deferred.is_empty() {
            return Ok(());
        }
        // emit into a block placed right after the prologue
        let saved_cur = self.ur().ir.cur;
        let lbl = self.new_label();
        // insert lbl in layout order right after the prologue (position 1)
        {
            let u = self.u();
            u.ir.order.insert(1, lbl);
            u.ir.cur = lbl;
        }
        let first = self.ur().deferred[0].0.loc;
        self.set_loc(first);
        self.compile_annotate_function(UnitKind::Module, first)?;
        self.emit_name(Op::StoreName, "__annotate__");
        self.u().ir.cur = saved_cur;
        Ok(())
    }

    /// Build the PEP 649 `__annotate__` function for the deferred annotations of the current
    /// module/class unit and push it.
    fn compile_annotate_function(&mut self, parent: UnitKind, loc: Loc) -> R {
        let deferred = self.ur().deferred.clone();
        let ann_scope = self.st.scopes[self.ur().scope].annotation_block.expect("annotation block");
        let parent_qual = self.ur().qualname.clone();
        let parent_kind = self.ur().kind;
        self.enter_scope("__annotate__", UnitKind::Annotations, ann_scope, loc.lineno)?;
        self.u().qualname =
            if parent_kind == UnitKind::Module { "__annotate__".to_string() } else { format!("{}.__annotate__", parent_qual) };
        self.u().argcount = 1;
        self.u().posonlyargcount = 1;
        self.set_loc(loc);
        self.emit_format_check();
        self.emit(Op::BuildMap, 0);
        for (st, idx) in &deferred {
            let (target, annotation) = match &st.node {
                StmtKind::AnnAssign { target, annotation, .. } => (target, annotation),
                _ => unreachable!(),
            };
            let name = match &target.node {
                ExprKind::Name { id, .. } => id.clone(),
                _ => unreachable!(),
            };
            self.set_loc(st.loc);
            let skip = self.new_label();
            if let Some(i) = idx {
                self.load_const(Constant::Int(*i as i64));
                if parent == UnitKind::Class {
                    let slot = self.closure_slot("__conditional_annotations__")?;
                    self.emit(Op::LoadDeref, slot);
                } else {
                    self.emit_name(Op::LoadGlobal, "__conditional_annotations__");
                }
                self.emit(Op::ContainsOp, 0);
                self.emit_jump(Op::PopJumpIfFalse, skip);
            }
            self.compile_expr(annotation)?;
            self.set_loc(st.loc);
            self.emit(Op::Copy, 2);
            let m = self.mangle_in_parent(&name);
            self.load_const(str_const(&m));
            self.emit(Op::StoreSubscr, 0);
            self.use_label(skip);
        }
        self.emit(Op::ReturnValue, 0);
        let code = self.exit_scope()?;
        self.make_closure(code, 0)
    }

    fn mangle_in_parent(&self, name: &str) -> String {
        let n = self.units.len();
        let p = &self.units[n.saturating_sub(2)];
        mangle(p.private.as_deref(), name)
    }

    /// `if format > 2: raise NotImplementedError` at the start of annotate-like functions.
    pub fn emit_format_check(&mut self) {
        let body = self.new_label();
        self.emit(Op::LoadFast, 0);
        self.load_const(Constant::Int(2));
        self.emit(Op::CompareOp, CmpOp::Gt as u32 | 16);
        self.emit_jump(Op::PopJumpIfFalse, body);
        self.emit(Op::LoadCommonConstant, CommonConstant::NotImplementedError as u32);
        self.emit(Op::RaiseVarargs, 1);
        self.use_label(body);
    }

    // ------------------------------------------------------------------ functions

    pub fn compile_function_def(&mut self, s: &'a Stmt, f: &'a ast::FunctionDef, is_async: bool) -> R {
        self.set_loc(s.loc);
        self.check_forbidden_args(&f.args)?;
        for d in &f.decorator_list {
            self.compile_expr(d)?;
            self.emit(Op::PushNull, 0);
        }
        let firstlineno = f.decorator_list.first().map(|d| d.loc.lineno).unwrap_or(s.loc.lineno);
        self.set_loc(s.loc);
        let generic = !f.type_params.is_empty();
        let has_defaults = !f.args.defaults.is_empty();
        let has_kwdefaults = f.args.kw_defaults.iter().any(|d| d.is_some());
        if generic {
            let tp_scope = self.st.scope_for(key(s, symtable::TAG_TP)).expect("tp scope");
            let tp_name = format!("<generic parameters of {}>", f.name);
            self.enter_scope(&tp_name, UnitKind::TypeParams, tp_scope, firstlineno)?;
            let nargs = has_defaults as u32 + has_kwdefaults as u32;
            self.u().argcount = nargs;
            self.u().posonlyargcount = nargs;
            self.set_loc(s.loc);
            self.compile_type_params(&f.type_params)?;
            let mut flags = 0;
            for i in 0..nargs {
                self.emit(Op::LoadFast, i);
            }
            if has_defaults {
                flags |= make_fn::DEFAULTS;
            }
            if has_kwdefaults {
                flags |= make_fn::KWDEFAULTS;
            }
            self.compile_function_body(s, f, is_async, flags, firstlineno)?;
            self.emit(Op::Swap, 2);
            self.emit(Op::CallIntrinsic2, Intrinsic2::SetFunctionTypeParams as u32);
            self.emit(Op::ReturnValue, 0);
            let code = self.exit_scope()?;
            self.set_loc(s.loc);
            self.make_closure(code, 0)?;
            self.emit(Op::PushNull, 0);
            if has_defaults {
                self.compile_defaults(&f.args)?;
            }
            if has_kwdefaults {
                self.compile_kwdefaults(&f.args)?;
            }
            self.set_loc(s.loc);
            self.emit(Op::Call, nargs);
        } else {
            let mut flags = 0;
            if has_defaults {
                self.compile_defaults(&f.args)?;
                flags |= make_fn::DEFAULTS;
            }
            if has_kwdefaults {
                self.compile_kwdefaults(&f.args)?;
                flags |= make_fn::KWDEFAULTS;
            }
            self.compile_function_body(s, f, is_async, flags, firstlineno)?;
        }
        self.set_loc(s.loc);
        for _ in &f.decorator_list {
            self.emit(Op::Call, 1);
        }
        self.nameop(&f.name, ast::ExprContext::Store)
    }

    fn compile_defaults(&mut self, a: &'a ast::Arguments) -> R {
        for d in &a.defaults {
            self.compile_expr(d)?;
        }
        self.emit(Op::BuildTuple, a.defaults.len() as u32);
        Ok(())
    }

    fn compile_kwdefaults(&mut self, a: &'a ast::Arguments) -> R {
        let mut n = 0;
        for (arg, d) in a.kwonlyargs.iter().zip(a.kw_defaults.iter()) {
            if let Some(d) = d {
                let m = self.mangle(&arg.arg);
                self.load_const(str_const(&m));
                self.compile_expr(d)?;
                n += 1;
            }
        }
        self.emit(Op::BuildMap, n);
        Ok(())
    }

    fn check_forbidden_args(&self, a: &ast::Arguments) -> R {
        for arg in a.posonlyargs.iter().chain(a.args.iter()).chain(a.kwonlyargs.iter()).chain(a.vararg.iter().map(|b| &**b)).chain(a.kwarg.iter().map(|b| &**b)) {
            if arg.arg == "__debug__" {
                return Err(self.error("cannot assign to __debug__", arg.loc));
            }
        }
        Ok(())
    }

    /// Annotations of a function's parameters: pushes the `__annotate__` function and returns
    /// true, or returns false if there are none.
    fn compile_param_annotations(&mut self, s: &'a Stmt, a: &'a ast::Arguments, returns: Option<&'a Expr>, fqualname: &str) -> R<bool> {
        let mut anns: Vec<(&'a str, &'a Expr)> = Vec::new();
        for arg in a.posonlyargs.iter().chain(a.args.iter()) {
            if let Some(x) = &arg.annotation {
                anns.push((&arg.arg, x));
            }
        }
        if let Some(v) = &a.vararg {
            if let Some(x) = &v.annotation {
                anns.push((&v.arg, x));
            }
        }
        for arg in &a.kwonlyargs {
            if let Some(x) = &arg.annotation {
                anns.push((&arg.arg, x));
            }
        }
        if let Some(v) = &a.kwarg {
            if let Some(x) = &v.annotation {
                anns.push((&v.arg, x));
            }
        }
        if let Some(r) = returns {
            anns.push(("return", r));
        }
        if anns.is_empty() {
            return Ok(false);
        }
        let ann_scope = self.st.scope_for(key(s, symtable::TAG_ANN)).expect("annotation scope");
        let future = self.future & cf::CO_FUTURE_ANNOTATIONS != 0;
        let mangled: Vec<String> = anns.iter().map(|(n, _)| self.mangle(n)).collect();
        self.enter_scope("__annotate__", UnitKind::Annotations, ann_scope, s.loc.lineno)?;
        self.u().qualname = format!("{}.__annotate__", fqualname);
        self.u().argcount = 1;
        self.u().posonlyargcount = 1;
        self.set_loc(s.loc);
        self.emit_format_check();
        for ((_, e), m) in anns.iter().zip(mangled.iter()) {
            self.load_const(str_const(m));
            if future {
                let txt = crate::unparse::unparse_annotation(e);
                self.load_const(Constant::Str(txt));
            } else if let ExprKind::Starred { value, .. } = &e.node {
                // *args: *Ts  ->  [*Ts][0]
                self.compile_expr(value)?;
                self.emit(Op::UnpackSequence, 1);
            } else {
                self.compile_expr(e)?;
            }
        }
        self.set_loc(s.loc);
        self.emit(Op::BuildMap, anns.len() as u32);
        self.emit(Op::ReturnValue, 0);
        let code = self.exit_scope()?;
        self.make_closure(code, 0)?;
        Ok(true)
    }

    /// Compiles the function body into a code object and pushes the function (attributes
    /// in `flags` must already be on the stack).
    fn compile_function_body(&mut self, s: &'a Stmt, f: &'a ast::FunctionDef, is_async: bool, mut flags: u32, firstlineno: i32) -> R {
        // qualname of the function (needed for the annotate function's qualname)
        let fscope = self.st.scope_for(key(s, symtable::TAG_FUNC)).expect("function scope");
        let kind = if is_async { UnitKind::AsyncFunction } else { UnitKind::Function };
        // compute qualname by entering then reading it
        self.enter_scope(&f.name, kind, fscope, firstlineno)?;
        let fqual = self.ur().qualname.clone();
        let fu = self.units.pop().unwrap();
        if self.compile_param_annotations(s, &f.args, f.returns.as_deref(), &fqual)? {
            flags |= make_fn::ANNOTATE;
        }
        self.units.push(fu);
        {
            let u = self.u();
            u.argcount = (f.args.posonlyargs.len() + f.args.args.len()) as u32;
            u.posonlyargcount = f.args.posonlyargs.len() as u32;
            u.kwonlyargcount = f.args.kwonlyargs.len() as u32;
        }
        let parent_is_class = {
            let n = self.units.len();
            let mut pi = n - 2;
            if self.units[pi].kind == UnitKind::TypeParams && pi > 0 {
                pi -= 1;
            }
            self.units[pi].kind == UnitKind::Class
        };
        if parent_is_class {
            self.u().flags |= co::METHOD;
        }
        let mut body: &'a [Stmt] = &f.body;
        if let Some(doc) = get_docstring(body) {
            if self.optimize < 2 {
                let d = clean_doc(doc);
                self.u().add_const(Constant::Str(d));
                self.u().flags |= co::HAS_DOCSTRING;
            }
            body = &body[1..];
            if body.is_empty() {
                // keep a line event for the docstring line
                self.set_loc(f.body[0].loc);
                self.emit(Op::Nop, 0);
            }
        }
        self.compile_function_statements(body)?;
        let code = self.exit_scope()?;
        self.set_loc(s.loc);
        self.make_closure(code, flags)
    }

    /// Body statements of a function-like unit, with the generator wrapper.
    pub fn compile_function_statements(&mut self, body: &'a [Stmt]) -> R {
        let is_gen = self.ur().flags & (co::GENERATOR | co::COROUTINE | co::ASYNC_GENERATOR) != 0;
        let handler = if is_gen {
            let h = self.new_label();
            self.u().loc = NO_LOC;
            self.emit_pseudo(IOp::SetupCleanup, h);
            self.u().fblocks.push(FBlock { kind: FbKind::StopIteration, block: h, exit: NO_LABEL, loc: NO_LOC, finally: None, name: None });
            Some(h)
        } else {
            None
        };
        for s in body {
            self.compile_stmt(s)?;
        }
        if !self.ur().ir.last_is_terminal() {
            self.u().loc = NO_LOC;
            self.load_const_noloc(Constant::None);
            self.emit_noloc(Op::ReturnValue, 0);
        }
        if let Some(h) = handler {
            self.u().fblocks.pop();
            self.pop_block();
            self.use_label(h);
            self.emit_noloc(Op::CallIntrinsic1, Intrinsic1::StopIterationError as u32);
            self.emit_noloc(Op::Reraise, 1);
        }
        Ok(())
    }

    pub fn compile_lambda(&mut self, e: &'a Expr, args: &'a ast::Arguments, body: &'a Expr) -> R {
        self.check_forbidden_args(args)?;
        let mut flags = 0;
        if !args.defaults.is_empty() {
            self.compile_defaults(args)?;
            flags |= make_fn::DEFAULTS;
        }
        if args.kw_defaults.iter().any(|d| d.is_some()) {
            self.compile_kwdefaults(args)?;
            flags |= make_fn::KWDEFAULTS;
        }
        let sid = self.st.scope_for(key(e, symtable::TAG_LAMBDA)).expect("lambda scope");
        self.enter_scope("<lambda>", UnitKind::Lambda, sid, e.loc.lineno)?;
        {
            let u = self.u();
            u.argcount = (args.posonlyargs.len() + args.args.len()) as u32;
            u.posonlyargcount = args.posonlyargs.len() as u32;
            u.kwonlyargcount = args.kwonlyargs.len() as u32;
        }
        self.set_loc(body.loc);
        let is_gen = self.ur().flags & (co::GENERATOR | co::COROUTINE | co::ASYNC_GENERATOR) != 0;
        let handler = if is_gen {
            let h = self.new_label();
            self.emit_pseudo(IOp::SetupCleanup, h);
            Some(h)
        } else {
            None
        };
        self.compile_expr(body)?;
        if is_gen && self.st.scopes[self.ur().scope].generator {
            // generator lambdas return None
            self.emit(Op::PopTop, 0);
            self.load_none();
        }
        self.emit(Op::ReturnValue, 0);
        if let Some(h) = handler {
            self.pop_block();
            self.use_label(h);
            self.emit_noloc(Op::CallIntrinsic1, Intrinsic1::StopIterationError as u32);
            self.emit_noloc(Op::Reraise, 1);
        }
        let code = self.exit_scope()?;
        self.set_loc(e.loc);
        self.make_closure(code, flags)
    }

    // ------------------------------------------------------------------ type parameters

    /// Creates the type parameters, binds their names and leaves a tuple of them on the stack.
    fn compile_type_params(&mut self, tps: &'a [ast::TypeParam]) -> R {
        let mut seen_default = false;
        for tp in tps {
            self.set_loc(tp.loc);
            let (name, intr) = match &tp.node {
                ast::TypeParamKind::TypeVar { name, .. } => (name, Intrinsic1::TypeVar),
                ast::TypeParamKind::ParamSpec { name, .. } => (name, Intrinsic1::ParamSpec),
                ast::TypeParamKind::TypeVarTuple { name, .. } => (name, Intrinsic1::TypeVarTuple),
            };
            // the TypeVar keeps its source name (only the binding is mangled)
            self.load_const(str_const(name));
            let mut done = false;
            if let ast::TypeParamKind::TypeVar { bound: Some(b), .. } = &tp.node {
                let constraints = matches!(b.node, ExprKind::Tuple { .. });
                self.compile_lazy_value(b, name, key(tp, symtable::TAG_BOUND), false)?;
                self.set_loc(tp.loc);
                let i = if constraints { Intrinsic2::TypeVarWithConstraints } else { Intrinsic2::TypeVarWithBound };
                self.emit(Op::CallIntrinsic2, i as u32);
                done = true;
            }
            if !done {
                self.emit(Op::CallIntrinsic1, intr as u32);
            }
            let default = match &tp.node {
                ast::TypeParamKind::TypeVar { default_value, .. }
                | ast::TypeParamKind::ParamSpec { default_value, .. }
                | ast::TypeParamKind::TypeVarTuple { default_value, .. } => default_value,
            };
            if let Some(d) = default {
                seen_default = true;
                let is_tvt = matches!(tp.node, ast::TypeParamKind::TypeVarTuple { .. });
                self.compile_lazy_value(d, name, key(tp, symtable::TAG_DEFAULT), is_tvt)?;
                self.set_loc(tp.loc);
                self.emit(Op::CallIntrinsic2, Intrinsic2::SetTypeparamDefault as u32);
            } else if seen_default {
                return Err(self.error(format!("non-default type parameter '{}' follows default type parameter", name), tp.loc));
            }
            self.emit(Op::Copy, 1);
            self.nameop(name, ast::ExprContext::Store)?;
        }
        self.set_loc(tps[0].loc);
        self.emit(Op::BuildTuple, tps.len() as u32);
        Ok(())
    }

    /// Lazily evaluated value (TypeVar bound/constraints/default, type alias value): a function
    /// taking `format` that returns the value.
    fn compile_lazy_value(&mut self, e: &'a Expr, name: &str, k: (usize, u8), unpack_starred: bool) -> R {
        let sid = self.st.scope_for(k).expect("lazy value scope");
        self.enter_scope(name, UnitKind::Annotations, sid, e.loc.lineno)?;
        self.u().argcount = 1;
        self.u().posonlyargcount = 1;
        self.set_loc(e.loc);
        self.emit_format_check();
        match (&e.node, unpack_starred) {
            (ExprKind::Starred { value, .. }, true) => {
                self.compile_expr(value)?;
                self.emit(Op::UnpackSequence, 1);
            }
            _ => self.compile_expr(e)?,
        }
        self.emit(Op::ReturnValue, 0);
        let code = self.exit_scope()?;
        self.make_closure(code, 0)
    }

    pub fn compile_type_alias(&mut self, s: &'a Stmt, name: &'a Expr, tps: &'a [ast::TypeParam], value: &'a Expr) -> R {
        let aname = match &name.node {
            ExprKind::Name { id, .. } => id.as_str(),
            _ => return Err(self.error("invalid type alias name", s.loc)),
        };
        self.set_loc(s.loc);
        let generic = !tps.is_empty();
        if generic {
            let tp_scope = self.st.scope_for(key(s, symtable::TAG_TP)).expect("tp scope");
            self.enter_scope(&format!("<generic parameters of {}>", aname), UnitKind::TypeParams, tp_scope, s.loc.lineno)?;
            self.set_loc(s.loc);
            self.load_const(str_const(aname));
            self.compile_type_params(tps)?;
        } else {
            self.load_const(str_const(aname));
            self.load_none();
        }
        self.compile_lazy_value(value, aname, key(s, symtable::TAG_ALIAS), false)?;
        self.set_loc(s.loc);
        self.emit(Op::BuildTuple, 3);
        self.emit(Op::CallIntrinsic1, Intrinsic1::TypeAlias as u32);
        if generic {
            self.emit(Op::ReturnValue, 0);
            let code = self.exit_scope()?;
            self.set_loc(s.loc);
            self.make_closure(code, 0)?;
            self.emit(Op::PushNull, 0);
            self.emit(Op::Call, 0);
        }
        self.nameop(aname, ast::ExprContext::Store)
    }

    // ------------------------------------------------------------------ classes

    pub fn compile_class_def(&mut self, s: &'a Stmt) -> R {
        let (name, bases, keywords, body, decorators, tps) = match &s.node {
            StmtKind::ClassDef { name, bases, keywords, body, decorator_list, type_params } => {
                (name, bases, keywords, body, decorator_list, type_params)
            }
            _ => unreachable!(),
        };
        self.set_loc(s.loc);
        for d in decorators {
            self.compile_expr(d)?;
            self.emit(Op::PushNull, 0);
        }
        let firstlineno = decorators.first().map(|d| d.loc.lineno).unwrap_or(s.loc.lineno);
        let generic = !tps.is_empty();
        if generic {
            let tp_scope = self.st.scope_for(key(s, symtable::TAG_TP)).expect("tp scope");
            self.enter_scope(&format!("<generic parameters of {}>", name), UnitKind::TypeParams, tp_scope, firstlineno)?;
            self.u().private = Some(name.clone());
            self.set_loc(s.loc);
            self.compile_type_params(tps)?;
            self.nameop(".type_params", ast::ExprContext::Store)?;
        }
        // class body
        let cscope = self.st.scope_for(key(s, symtable::TAG_CLASS)).expect("class scope");
        self.enter_scope(name, UnitKind::Class, cscope, firstlineno)?;
        self.compile_class_body(s, body, firstlineno, generic)?;
        let code = self.exit_scope()?;
        self.set_loc(s.loc);
        self.emit(Op::LoadBuildClass, 0);
        self.emit(Op::PushNull, 0);
        self.make_closure(code, 0)?;
        self.load_const(str_const(name));
        if generic {
            self.compile_call_helper(2, bases, keywords, true)?;
            self.emit(Op::ReturnValue, 0);
            let code = self.exit_scope()?;
            self.set_loc(s.loc);
            self.make_closure(code, 0)?;
            self.emit(Op::PushNull, 0);
            self.emit(Op::Call, 0);
        } else {
            self.compile_call_helper(2, bases, keywords, false)?;
        }
        self.set_loc(s.loc);
        for _ in decorators {
            self.emit(Op::Call, 1);
        }
        self.nameop(name, ast::ExprContext::Store)
    }

    fn compile_class_body(&mut self, s: &'a Stmt, body: &'a [Stmt], firstlineno: i32, generic: bool) -> R {
        self.set_loc(s.loc);
        let sid = self.ur().scope;
        if self.st.scopes[sid].needs_classdict {
            self.emit(Op::LoadLocals, 0);
            let slot = self.ur().cell_slot("__classdict__").unwrap();
            self.emit(Op::StoreDeref, slot);
        }
        self.emit_name(Op::LoadName, "__name__");
        self.emit_name(Op::StoreName, "__module__");
        let q = self.ur().qualname.clone();
        self.load_const(str_const(&q));
        self.emit_name(Op::StoreName, "__qualname__");
        self.load_const(Constant::Int(firstlineno as i64));
        self.emit_name(Op::StoreName, "__firstlineno__");
        if generic {
            self.nameop(".type_params", ast::ExprContext::Load)?;
            self.emit_name(Op::StoreName, "__type_params__");
        }
        self.compile_body(body, true)?;
        // deferred annotations
        self.u().loc = NO_LOC;
        if !self.ur().deferred.is_empty() {
            self.set_loc(s.loc);
            let first = self.ur().deferred[0].0.loc;
            self.compile_annotate_function(UnitKind::Class, first)?;
            self.set_loc(s.loc);
            self.emit_name(Op::StoreName, "__annotate_func__");
        }
        self.u().loc = NO_LOC;
        let attrs: Vec<Constant> = self.ur().static_attrs.as_ref().unwrap().iter().map(|a| str_const(a)).collect();
        self.load_const_noloc(Constant::Tuple(attrs));
        let i = self.u().add_name("__static_attributes__");
        self.emit_noloc(Op::StoreName, i);
        let (needs_classdict, needs_class_closure) = (self.st.scopes[sid].needs_classdict, self.st.scopes[sid].needs_class_closure);
        if needs_classdict {
            let slot = self.ur().cell_slot("__classdict__").unwrap();
            self.emit_noloc(Op::LoadFast, slot);
            let i = self.u().add_name("__classdictcell__");
            self.emit_noloc(Op::StoreName, i);
        }
        if needs_class_closure {
            let slot = self.ur().cell_slot("__class__").unwrap();
            self.emit_noloc(Op::LoadFast, slot);
            self.emit_noloc(Op::Copy, 1);
            let i = self.u().add_name("__classcell__");
            self.emit_noloc(Op::StoreName, i);
        } else {
            self.load_const_noloc(Constant::None);
        }
        self.emit_noloc(Op::ReturnValue, 0);
        Ok(())
    }

    /// Record `self.attr = ...` in the nearest enclosing class.
    pub fn note_static_attribute(&mut self, value: &Expr, attr: &str) {
        if let ExprKind::Name { id, .. } = &value.node {
            if id != "self" {
                return;
            }
            for u in self.units.iter_mut().rev() {
                if u.kind == UnitKind::Class {
                    if let Some(sa) = &mut u.static_attrs {
                        sa.insert(attr.to_string());
                    }
                    return;
                }
                if !matches!(u.kind, UnitKind::Function | UnitKind::AsyncFunction | UnitKind::TypeParams | UnitKind::Lambda | UnitKind::Comprehension) {
                    return;
                }
            }
        }
    }
}

/// Does the body contain an annotated assignment (not in nested scopes)?
pub(crate) fn find_ann(body: &[Stmt]) -> bool {
    for s in body {
        let found = match &s.node {
            StmtKind::AnnAssign { .. } => true,
            StmtKind::For(f) | StmtKind::AsyncFor(f) => find_ann(&f.body) || find_ann(&f.orelse),
            StmtKind::While { body, orelse, .. } | StmtKind::If { body, orelse, .. } => find_ann(body) || find_ann(orelse),
            StmtKind::With(w) | StmtKind::AsyncWith(w) => find_ann(&w.body),
            StmtKind::Try(t) | StmtKind::TryStar(t) => {
                find_ann(&t.body) || find_ann(&t.orelse) || find_ann(&t.finalbody) || t.handlers.iter().any(|h| find_ann(&h.body))
            }
            StmtKind::Match { cases, .. } => cases.iter().any(|c| find_ann(&c.body)),
            _ => false,
        };
        if found {
            return true;
        }
    }
    false
}
