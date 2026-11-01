//! The Python 3.15 abstract syntax tree.
//!
//! Mirrors the `ast` module 1:1: same node names, same field names and order, same
//! optional/list-ness.  Every node that has position attributes in the `ast` module carries a
//! [`Loc`] (`lineno`, `col_offset`, `end_lineno`, `end_col_offset`; 1-based lines, UTF-8 byte
//! columns).
//!
//! A machine readable description of all node classes is available through
//! [`node_classes`] (built from [`ASDL`]).

use std::sync::OnceLock;

/// Source position of a node (1-based lines, 0-based UTF-8 byte column offsets).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub struct Loc {
    pub lineno: i32,
    pub col_offset: i32,
    pub end_lineno: i32,
    pub end_col_offset: i32,
}

impl Loc {
    pub const fn new(lineno: i32, col_offset: i32, end_lineno: i32, end_col_offset: i32) -> Loc {
        Loc { lineno, col_offset, end_lineno, end_col_offset }
    }
}

pub type Identifier = String;

// ---------------------------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------------------------

/// A Python `str` value. Python strings may contain lone surrogates, which cannot be
/// represented in a Rust `String`; such strings are stored as a vector of code points.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PyStr {
    /// Valid Unicode (no surrogates).
    Utf8(String),
    /// Arbitrary code points (contains at least one surrogate in U+D800..U+DFFF).
    CodePoints(Vec<u32>),
}

impl PyStr {
    pub fn new() -> PyStr {
        PyStr::Utf8(String::new())
    }
    /// Returns the string if it is valid UTF-8 (has no surrogates).
    pub fn as_str(&self) -> Option<&str> {
        match self {
            PyStr::Utf8(s) => Some(s),
            PyStr::CodePoints(_) => None,
        }
    }
    pub fn is_empty(&self) -> bool {
        match self {
            PyStr::Utf8(s) => s.is_empty(),
            PyStr::CodePoints(v) => v.is_empty(),
        }
    }
    /// Number of code points.
    pub fn char_len(&self) -> usize {
        match self {
            PyStr::Utf8(s) => s.chars().count(),
            PyStr::CodePoints(v) => v.len(),
        }
    }
    /// Iterate over the code points.
    pub fn code_points(&self) -> Box<dyn Iterator<Item = u32> + '_> {
        match self {
            PyStr::Utf8(s) => Box::new(s.chars().map(|c| c as u32)),
            PyStr::CodePoints(v) => Box::new(v.iter().copied()),
        }
    }
    pub fn push_char(&mut self, c: char) {
        match self {
            PyStr::Utf8(s) => s.push(c),
            PyStr::CodePoints(v) => v.push(c as u32),
        }
    }
    pub fn push_str(&mut self, t: &str) {
        match self {
            PyStr::Utf8(s) => s.push_str(t),
            PyStr::CodePoints(v) => v.extend(t.chars().map(|c| c as u32)),
        }
    }
    /// Push an arbitrary code point (may be a surrogate).
    pub fn push_code_point(&mut self, cp: u32) {
        if let Some(c) = char::from_u32(cp) {
            self.push_char(c);
            return;
        }
        if let PyStr::Utf8(s) = self {
            let v: Vec<u32> = s.chars().map(|c| c as u32).collect();
            *self = PyStr::CodePoints(v);
        }
        if let PyStr::CodePoints(v) = self {
            v.push(cp);
        }
    }
    pub fn push_pystr(&mut self, other: &PyStr) {
        match other {
            PyStr::Utf8(t) => self.push_str(t),
            PyStr::CodePoints(v) => {
                for &cp in v {
                    self.push_code_point(cp);
                }
            }
        }
    }
    /// Lossy conversion (surrogates become U+FFFD).
    pub fn to_string_lossy(&self) -> String {
        match self {
            PyStr::Utf8(s) => s.clone(),
            PyStr::CodePoints(v) => v.iter().map(|&c| char::from_u32(c).unwrap_or('\u{FFFD}')).collect(),
        }
    }
}

impl Default for PyStr {
    fn default() -> Self {
        PyStr::new()
    }
}

impl From<String> for PyStr {
    fn from(s: String) -> Self {
        PyStr::Utf8(s)
    }
}
impl From<&str> for PyStr {
    fn from(s: &str) -> Self {
        PyStr::Utf8(s.to_string())
    }
}

/// An integer literal value.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Int {
    /// Fits in an i64 (parser literals are never negative).
    Small(i64),
    /// Arbitrary size: normalized decimal digit string (no sign, no leading zeros, no
    /// underscores), optionally preceded by '-' (only for constants produced by folding).
    Big(String),
}

/// The value of a `Constant` node (and of `MatchSingleton`).
#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
    None,
    Bool(bool),
    Ellipsis,
    Int(Int),
    Float(f64),
    /// Complex number; literals always have `real == 0.0`.
    Complex { real: f64, imag: f64 },
    Str(PyStr),
    Bytes(Vec<u8>),
    Tuple(Vec<Constant>),
    FrozenSet(Vec<Constant>),
}

// ---------------------------------------------------------------------------------------------
// mod
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Mod {
    Module { body: Vec<Stmt>, type_ignores: Vec<TypeIgnore> },
    Interactive { body: Vec<Stmt> },
    Expression { body: Box<Expr> },
    FunctionType { argtypes: Vec<Expr>, returns: Box<Expr> },
}

// ---------------------------------------------------------------------------------------------
// stmt
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub node: StmtKind,
    pub loc: Loc,
}

/// Shared payload of `FunctionDef` and `AsyncFunctionDef`.
#[derive(Clone, Debug, PartialEq)]
pub struct FunctionDef {
    pub name: Identifier,
    pub args: Box<Arguments>,
    pub body: Vec<Stmt>,
    pub decorator_list: Vec<Expr>,
    pub returns: Option<Box<Expr>>,
    pub type_comment: Option<String>,
    pub type_params: Vec<TypeParam>,
}

/// Shared payload of `For` and `AsyncFor`.
#[derive(Clone, Debug, PartialEq)]
pub struct For {
    pub target: Box<Expr>,
    pub iter: Box<Expr>,
    pub body: Vec<Stmt>,
    pub orelse: Vec<Stmt>,
    pub type_comment: Option<String>,
}

/// Shared payload of `With` and `AsyncWith`.
#[derive(Clone, Debug, PartialEq)]
pub struct With {
    pub items: Vec<WithItem>,
    pub body: Vec<Stmt>,
    pub type_comment: Option<String>,
}

/// Shared payload of `Try` and `TryStar`.
#[derive(Clone, Debug, PartialEq)]
pub struct Try {
    pub body: Vec<Stmt>,
    pub handlers: Vec<ExceptHandler>,
    pub orelse: Vec<Stmt>,
    pub finalbody: Vec<Stmt>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StmtKind {
    FunctionDef(FunctionDef),
    AsyncFunctionDef(FunctionDef),
    ClassDef {
        name: Identifier,
        bases: Vec<Expr>,
        keywords: Vec<Keyword>,
        body: Vec<Stmt>,
        decorator_list: Vec<Expr>,
        type_params: Vec<TypeParam>,
    },
    Return { value: Option<Box<Expr>> },
    Delete { targets: Vec<Expr> },
    Assign { targets: Vec<Expr>, value: Box<Expr>, type_comment: Option<String> },
    TypeAlias { name: Box<Expr>, type_params: Vec<TypeParam>, value: Box<Expr> },
    AugAssign { target: Box<Expr>, op: Operator, value: Box<Expr> },
    AnnAssign { target: Box<Expr>, annotation: Box<Expr>, value: Option<Box<Expr>>, simple: i32 },
    For(For),
    AsyncFor(For),
    While { test: Box<Expr>, body: Vec<Stmt>, orelse: Vec<Stmt> },
    If { test: Box<Expr>, body: Vec<Stmt>, orelse: Vec<Stmt> },
    With(With),
    AsyncWith(With),
    Match { subject: Box<Expr>, cases: Vec<MatchCase> },
    Raise { exc: Option<Box<Expr>>, cause: Option<Box<Expr>> },
    Try(Try),
    TryStar(Try),
    Assert { test: Box<Expr>, msg: Option<Box<Expr>> },
    Import { names: Vec<Alias>, is_lazy: i32 },
    ImportFrom { module: Option<Identifier>, names: Vec<Alias>, level: i32, is_lazy: i32 },
    Global { names: Vec<Identifier> },
    Nonlocal { names: Vec<Identifier> },
    Expr { value: Box<Expr> },
    Pass,
    Break,
    Continue,
}

// ---------------------------------------------------------------------------------------------
// expr
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub node: ExprKind,
    pub loc: Loc,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    BoolOp { op: BoolOp, values: Vec<Expr> },
    NamedExpr { target: Box<Expr>, value: Box<Expr> },
    BinOp { left: Box<Expr>, op: Operator, right: Box<Expr> },
    UnaryOp { op: UnaryOp, operand: Box<Expr> },
    Lambda { args: Box<Arguments>, body: Box<Expr> },
    IfExp { test: Box<Expr>, body: Box<Expr>, orelse: Box<Expr> },
    /// `keys[i]` is `None` for `**d` entries.
    Dict { keys: Vec<Option<Expr>>, values: Vec<Expr> },
    Set { elts: Vec<Expr> },
    ListComp { elt: Box<Expr>, generators: Vec<Comprehension> },
    SetComp { elt: Box<Expr>, generators: Vec<Comprehension> },
    /// PEP 798: `{**d for d in x}` is `DictComp(key=d, value=None)`.
    DictComp { key: Box<Expr>, value: Option<Box<Expr>>, generators: Vec<Comprehension> },
    GeneratorExp { elt: Box<Expr>, generators: Vec<Comprehension> },
    Await { value: Box<Expr> },
    Yield { value: Option<Box<Expr>> },
    YieldFrom { value: Box<Expr> },
    Compare { left: Box<Expr>, ops: Vec<CmpOp>, comparators: Vec<Expr> },
    Call { func: Box<Expr>, args: Vec<Expr>, keywords: Vec<Keyword> },
    /// `conversion` is -1 (none), or the code of 's', 'r', 'a'.
    FormattedValue { value: Box<Expr>, conversion: i32, format_spec: Option<Box<Expr>> },
    /// `str` is the source text of the interpolated expression.
    Interpolation { value: Box<Expr>, str: PyStr, conversion: i32, format_spec: Option<Box<Expr>> },
    JoinedStr { values: Vec<Expr> },
    TemplateStr { values: Vec<Expr> },
    /// `kind` is `Some("u")` for u-prefixed string literals.
    Constant { value: Constant, kind: Option<String> },
    Attribute { value: Box<Expr>, attr: Identifier, ctx: ExprContext },
    Subscript { value: Box<Expr>, slice: Box<Expr>, ctx: ExprContext },
    Starred { value: Box<Expr>, ctx: ExprContext },
    Name { id: Identifier, ctx: ExprContext },
    List { elts: Vec<Expr>, ctx: ExprContext },
    Tuple { elts: Vec<Expr>, ctx: ExprContext },
    Slice { lower: Option<Box<Expr>>, upper: Option<Box<Expr>>, step: Option<Box<Expr>> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExprContext {
    Load,
    Store,
    Del,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BoolOp {
    And,
    Or,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Operator {
    Add,
    Sub,
    Mult,
    MatMult,
    Div,
    Mod,
    Pow,
    LShift,
    RShift,
    BitOr,
    BitXor,
    BitAnd,
    FloorDiv,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Invert,
    Not,
    UAdd,
    USub,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CmpOp {
    Eq,
    NotEq,
    Lt,
    LtE,
    Gt,
    GtE,
    Is,
    IsNot,
    In,
    NotIn,
}

impl ExprContext {
    pub fn name(self) -> &'static str {
        match self {
            ExprContext::Load => "Load",
            ExprContext::Store => "Store",
            ExprContext::Del => "Del",
        }
    }
}
impl BoolOp {
    pub fn name(self) -> &'static str {
        match self {
            BoolOp::And => "And",
            BoolOp::Or => "Or",
        }
    }
}
impl Operator {
    pub fn name(self) -> &'static str {
        use Operator::*;
        match self {
            Add => "Add",
            Sub => "Sub",
            Mult => "Mult",
            MatMult => "MatMult",
            Div => "Div",
            Mod => "Mod",
            Pow => "Pow",
            LShift => "LShift",
            RShift => "RShift",
            BitOr => "BitOr",
            BitXor => "BitXor",
            BitAnd => "BitAnd",
            FloorDiv => "FloorDiv",
        }
    }
    /// The operator symbol (e.g. "+").
    pub fn symbol(self) -> &'static str {
        use Operator::*;
        match self {
            Add => "+",
            Sub => "-",
            Mult => "*",
            MatMult => "@",
            Div => "/",
            Mod => "%",
            Pow => "**",
            LShift => "<<",
            RShift => ">>",
            BitOr => "|",
            BitXor => "^",
            BitAnd => "&",
            FloorDiv => "//",
        }
    }
}
impl UnaryOp {
    pub fn name(self) -> &'static str {
        match self {
            UnaryOp::Invert => "Invert",
            UnaryOp::Not => "Not",
            UnaryOp::UAdd => "UAdd",
            UnaryOp::USub => "USub",
        }
    }
}
impl CmpOp {
    pub fn name(self) -> &'static str {
        use CmpOp::*;
        match self {
            Eq => "Eq",
            NotEq => "NotEq",
            Lt => "Lt",
            LtE => "LtE",
            Gt => "Gt",
            GtE => "GtE",
            Is => "Is",
            IsNot => "IsNot",
            In => "In",
            NotIn => "NotIn",
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Products and remaining sums
// ---------------------------------------------------------------------------------------------

/// `comprehension` (no position attributes).
#[derive(Clone, Debug, PartialEq)]
pub struct Comprehension {
    pub target: Expr,
    pub iter: Expr,
    pub ifs: Vec<Expr>,
    pub is_async: i32,
}

/// `excepthandler = ExceptHandler(expr? type, identifier? name, stmt* body)`.
#[derive(Clone, Debug, PartialEq)]
pub struct ExceptHandler {
    pub type_: Option<Box<Expr>>,
    pub name: Option<Identifier>,
    pub body: Vec<Stmt>,
    pub loc: Loc,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Arguments {
    pub posonlyargs: Vec<Arg>,
    pub args: Vec<Arg>,
    pub vararg: Option<Box<Arg>>,
    pub kwonlyargs: Vec<Arg>,
    /// `None` entries for keyword-only arguments without default.
    pub kw_defaults: Vec<Option<Expr>>,
    pub kwarg: Option<Box<Arg>>,
    pub defaults: Vec<Expr>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Arg {
    pub arg: Identifier,
    pub annotation: Option<Box<Expr>>,
    pub type_comment: Option<String>,
    pub loc: Loc,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Keyword {
    /// `None` for `**kwargs`.
    pub arg: Option<Identifier>,
    pub value: Expr,
    pub loc: Loc,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Alias {
    pub name: Identifier,
    pub asname: Option<Identifier>,
    pub loc: Loc,
}

/// `withitem` (no position attributes).
#[derive(Clone, Debug, PartialEq)]
pub struct WithItem {
    pub context_expr: Expr,
    pub optional_vars: Option<Box<Expr>>,
}

/// `match_case` (no position attributes).
#[derive(Clone, Debug, PartialEq)]
pub struct MatchCase {
    pub pattern: Pattern,
    pub guard: Option<Box<Expr>>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    pub node: PatternKind,
    pub loc: Loc,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PatternKind {
    MatchValue { value: Box<Expr> },
    MatchSingleton { value: Constant },
    MatchSequence { patterns: Vec<Pattern> },
    MatchMapping { keys: Vec<Expr>, patterns: Vec<Pattern>, rest: Option<Identifier> },
    MatchClass { cls: Box<Expr>, patterns: Vec<Pattern>, kwd_attrs: Vec<Identifier>, kwd_patterns: Vec<Pattern> },
    MatchStar { name: Option<Identifier> },
    MatchAs { pattern: Option<Box<Pattern>>, name: Option<Identifier> },
    MatchOr { patterns: Vec<Pattern> },
}

/// `type_ignore = TypeIgnore(int lineno, string tag)`.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeIgnore {
    pub lineno: i32,
    pub tag: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypeParam {
    pub node: TypeParamKind,
    pub loc: Loc,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TypeParamKind {
    TypeVar { name: Identifier, bound: Option<Box<Expr>>, default_value: Option<Box<Expr>> },
    ParamSpec { name: Identifier, default_value: Option<Box<Expr>> },
    TypeVarTuple { name: Identifier, default_value: Option<Box<Expr>> },
}

// ---------------------------------------------------------------------------------------------
// Convenience
// ---------------------------------------------------------------------------------------------

impl Expr {
    pub fn new(node: ExprKind, loc: Loc) -> Expr {
        Expr { node, loc }
    }
    /// Name of the node class (e.g. "BinOp").
    pub fn class_name(&self) -> &'static str {
        use ExprKind::*;
        match &self.node {
            BoolOp { .. } => "BoolOp",
            NamedExpr { .. } => "NamedExpr",
            BinOp { .. } => "BinOp",
            UnaryOp { .. } => "UnaryOp",
            Lambda { .. } => "Lambda",
            IfExp { .. } => "IfExp",
            Dict { .. } => "Dict",
            Set { .. } => "Set",
            ListComp { .. } => "ListComp",
            SetComp { .. } => "SetComp",
            DictComp { .. } => "DictComp",
            GeneratorExp { .. } => "GeneratorExp",
            Await { .. } => "Await",
            Yield { .. } => "Yield",
            YieldFrom { .. } => "YieldFrom",
            Compare { .. } => "Compare",
            Call { .. } => "Call",
            FormattedValue { .. } => "FormattedValue",
            Interpolation { .. } => "Interpolation",
            JoinedStr { .. } => "JoinedStr",
            TemplateStr { .. } => "TemplateStr",
            Constant { .. } => "Constant",
            Attribute { .. } => "Attribute",
            Subscript { .. } => "Subscript",
            Starred { .. } => "Starred",
            Name { .. } => "Name",
            List { .. } => "List",
            Tuple { .. } => "Tuple",
            Slice { .. } => "Slice",
        }
    }
}

impl Stmt {
    pub fn new(node: StmtKind, loc: Loc) -> Stmt {
        Stmt { node, loc }
    }
    pub fn class_name(&self) -> &'static str {
        use StmtKind::*;
        match &self.node {
            FunctionDef(_) => "FunctionDef",
            AsyncFunctionDef(_) => "AsyncFunctionDef",
            ClassDef { .. } => "ClassDef",
            Return { .. } => "Return",
            Delete { .. } => "Delete",
            Assign { .. } => "Assign",
            TypeAlias { .. } => "TypeAlias",
            AugAssign { .. } => "AugAssign",
            AnnAssign { .. } => "AnnAssign",
            For(_) => "For",
            AsyncFor(_) => "AsyncFor",
            While { .. } => "While",
            If { .. } => "If",
            With(_) => "With",
            AsyncWith(_) => "AsyncWith",
            Match { .. } => "Match",
            Raise { .. } => "Raise",
            Try(_) => "Try",
            TryStar(_) => "TryStar",
            Assert { .. } => "Assert",
            Import { .. } => "Import",
            ImportFrom { .. } => "ImportFrom",
            Global { .. } => "Global",
            Nonlocal { .. } => "Nonlocal",
            Expr { .. } => "Expr",
            Pass => "Pass",
            Break => "Break",
            Continue => "Continue",
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Machine readable node class description
// ---------------------------------------------------------------------------------------------

/// The ASDL description of the Python 3.15 AST (the source of [`node_classes`]).
pub const ASDL: &str = r#"
module Python
{
    mod = Module(stmt* body, type_ignore* type_ignores)
        | Interactive(stmt* body)
        | Expression(expr body)
        | FunctionType(expr* argtypes, expr returns)

    stmt = FunctionDef(identifier name, arguments args,
                       stmt* body, expr* decorator_list, expr? returns,
                       string? type_comment, type_param* type_params)
          | AsyncFunctionDef(identifier name, arguments args,
                             stmt* body, expr* decorator_list, expr? returns,
                             string? type_comment, type_param* type_params)

          | ClassDef(identifier name,
             expr* bases,
             keyword* keywords,
             stmt* body,
             expr* decorator_list,
             type_param* type_params)
          | Return(expr? value)

          | Delete(expr* targets)
          | Assign(expr* targets, expr value, string? type_comment)
          | TypeAlias(expr name, type_param* type_params, expr value)
          | AugAssign(expr target, operator op, expr value)
          | AnnAssign(expr target, expr annotation, expr? value, int simple)

          | For(expr target, expr iter, stmt* body, stmt* orelse, string? type_comment)
          | AsyncFor(expr target, expr iter, stmt* body, stmt* orelse, string? type_comment)
          | While(expr test, stmt* body, stmt* orelse)
          | If(expr test, stmt* body, stmt* orelse)
          | With(withitem* items, stmt* body, string? type_comment)
          | AsyncWith(withitem* items, stmt* body, string? type_comment)

          | Match(expr subject, match_case* cases)

          | Raise(expr? exc, expr? cause)
          | Try(stmt* body, excepthandler* handlers, stmt* orelse, stmt* finalbody)
          | TryStar(stmt* body, excepthandler* handlers, stmt* orelse, stmt* finalbody)
          | Assert(expr test, expr? msg)

          | Import(alias* names, int? is_lazy)
          | ImportFrom(identifier? module, alias* names, int? level, int? is_lazy)

          | Global(identifier* names)
          | Nonlocal(identifier* names)
          | Expr(expr value)
          | Pass | Break | Continue

          attributes (int lineno, int col_offset, int? end_lineno, int? end_col_offset)

    expr = BoolOp(boolop op, expr* values)
         | NamedExpr(expr target, expr value)
         | BinOp(expr left, operator op, expr right)
         | UnaryOp(unaryop op, expr operand)
         | Lambda(arguments args, expr body)
         | IfExp(expr test, expr body, expr orelse)
         | Dict(expr?* keys, expr* values)
         | Set(expr* elts)
         | ListComp(expr elt, comprehension* generators)
         | SetComp(expr elt, comprehension* generators)
         | DictComp(expr key, expr? value, comprehension* generators)
         | GeneratorExp(expr elt, comprehension* generators)
         | Await(expr value)
         | Yield(expr? value)
         | YieldFrom(expr value)
         | Compare(expr left, cmpop* ops, expr* comparators)
         | Call(expr func, expr* args, keyword* keywords)
         | FormattedValue(expr value, int conversion, expr? format_spec)
         | Interpolation(expr value, constant str, int conversion, expr? format_spec)
         | JoinedStr(expr* values)
         | TemplateStr(expr* values)
         | Constant(constant value, string? kind)

         | Attribute(expr value, identifier attr, expr_context ctx)
         | Subscript(expr value, expr slice, expr_context ctx)
         | Starred(expr value, expr_context ctx)
         | Name(identifier id, expr_context ctx)
         | List(expr* elts, expr_context ctx)
         | Tuple(expr* elts, expr_context ctx)

         | Slice(expr? lower, expr? upper, expr? step)

          attributes (int lineno, int col_offset, int? end_lineno, int? end_col_offset)

    expr_context = Load | Store | Del

    boolop = And | Or

    operator = Add | Sub | Mult | MatMult | Div | Mod | Pow | LShift
                 | RShift | BitOr | BitXor | BitAnd | FloorDiv

    unaryop = Invert | Not | UAdd | USub

    cmpop = Eq | NotEq | Lt | LtE | Gt | GtE | Is | IsNot | In | NotIn

    comprehension = (expr target, expr iter, expr* ifs, int is_async)

    excepthandler = ExceptHandler(expr? type, identifier? name, stmt* body)
                    attributes (int lineno, int col_offset, int? end_lineno, int? end_col_offset)

    arguments = (arg* posonlyargs, arg* args, arg? vararg, arg* kwonlyargs,
                 expr* kw_defaults, arg? kwarg, expr* defaults)

    arg = (identifier arg, expr? annotation, string? type_comment)
           attributes (int lineno, int col_offset, int? end_lineno, int? end_col_offset)

    keyword = (identifier? arg, expr value)
               attributes (int lineno, int col_offset, int? end_lineno, int? end_col_offset)

    alias = (identifier name, identifier? asname)
             attributes (int lineno, int col_offset, int? end_lineno, int? end_col_offset)

    withitem = (expr context_expr, expr? optional_vars)

    match_case = (pattern pattern, expr? guard, stmt* body)

    pattern = MatchValue(expr value)
            | MatchSingleton(constant value)
            | MatchSequence(pattern* patterns)
            | MatchMapping(expr* keys, pattern* patterns, identifier? rest)
            | MatchClass(expr cls, pattern* patterns, identifier* kwd_attrs, pattern* kwd_patterns)

            | MatchStar(identifier? name)

            | MatchAs(pattern? pattern, identifier? name)
            | MatchOr(pattern* patterns)

             attributes (int lineno, int col_offset, int end_lineno, int end_col_offset)

    type_ignore = TypeIgnore(int lineno, string tag)

    type_param = TypeVar(identifier name, expr? bound, expr? default_value)
               | ParamSpec(identifier name, expr? default_value)
               | TypeVarTuple(identifier name, expr? default_value)
               attributes (int lineno, int col_offset, int end_lineno, int end_col_offset)
}
"#;

/// Cardinality of an AST field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// `T`
    Single,
    /// `T?`
    Optional,
    /// `T*`
    Sequence,
    /// `T?*` (a list whose items may be None; only `Dict.keys`)
    OptionalSequence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldDesc {
    pub name: &'static str,
    /// ASDL type: a builtin (`identifier`, `string`, `int`, `constant`) or a node type name
    /// (`expr`, `stmt`, `arguments`, ...).
    pub ty: &'static str,
    pub kind: FieldKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeClass {
    /// Class name (e.g. "BinOp", "expr", "comprehension").
    pub name: &'static str,
    /// Base class name: "AST" for abstract sum types and products, else the sum type name.
    pub base: &'static str,
    /// True for the abstract base of a sum type (e.g. `expr`).
    pub is_abstract: bool,
    pub fields: Vec<FieldDesc>,
    /// Attribute names (positions) with their optionality (`int?` attributes are optional).
    pub attributes: Vec<FieldDesc>,
    /// Docstring as CPython builds it, e.g. "BinOp(expr left, operator op, expr right)".
    pub doc: String,
}

/// All node classes in ASDL order: for each type, the abstract base (for sum types) followed by
/// its constructors; product types are a single class.
pub fn node_classes() -> &'static [NodeClass] {
    static CELL: OnceLock<Vec<NodeClass>> = OnceLock::new();
    CELL.get_or_init(|| parse_asdl(ASDL))
}

/// Look up a node class by name.
pub fn node_class(name: &str) -> Option<&'static NodeClass> {
    node_classes().iter().find(|c| c.name == name)
}

fn parse_asdl(src: &'static str) -> Vec<NodeClass> {
    // Tokenize: identifiers, punctuation.
    let mut toks: Vec<&'static str> = Vec::new();
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'-' && i + 1 < b.len() && b[i + 1] == b'-' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c.is_ascii_alphanumeric() || c == b'_' {
            let s = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            toks.push(&src[s..i]);
        } else {
            toks.push(&src[i..i + 1]);
            i += 1;
        }
    }
    let mut out = Vec::new();
    let mut p = 0;
    // skip "module Python {"
    while toks[p] != "{" {
        p += 1;
    }
    p += 1;
    fn parse_fields(toks: &[&'static str], p: &mut usize) -> Vec<FieldDesc> {
        let mut v = Vec::new();
        assert_eq!(toks[*p], "(");
        *p += 1;
        loop {
            if toks[*p] == ")" {
                *p += 1;
                break;
            }
            let ty = toks[*p];
            *p += 1;
            let mut kind = FieldKind::Single;
            if toks[*p] == "?" {
                *p += 1;
                kind = FieldKind::Optional;
                if toks[*p] == "*" {
                    *p += 1;
                    kind = FieldKind::OptionalSequence;
                }
            } else if toks[*p] == "*" {
                *p += 1;
                kind = FieldKind::Sequence;
            }
            let name = toks[*p];
            *p += 1;
            v.push(FieldDesc { name, ty, kind });
            if toks[*p] == "," {
                *p += 1;
            }
        }
        v
    }
    fn sig(name: &str, fields: &[FieldDesc]) -> String {
        let mut s = String::from(name);
        if !fields.is_empty() {
            s.push('(');
            for (i, f) in fields.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(f.ty);
                match f.kind {
                    FieldKind::Single => {}
                    FieldKind::Optional => s.push('?'),
                    FieldKind::Sequence => s.push('*'),
                    FieldKind::OptionalSequence => s.push_str("?*"),
                }
                s.push(' ');
                s.push_str(f.name);
            }
            s.push(')');
        }
        s
    }
    while toks[p] != "}" {
        let tyname = toks[p];
        p += 2; // name '='
        if toks[p] == "(" {
            // product
            let fields = parse_fields(&toks, &mut p);
            let mut attributes = Vec::new();
            if toks[p] == "attributes" {
                p += 1;
                attributes = parse_fields(&toks, &mut p);
            }
            let doc = sig(tyname, &fields);
            out.push(NodeClass { name: tyname, base: "AST", is_abstract: false, fields, attributes, doc });
            continue;
        }
        // sum
        let mut ctors: Vec<(&'static str, Vec<FieldDesc>)> = Vec::new();
        loop {
            let cname = toks[p];
            p += 1;
            let fields = if toks[p] == "(" { parse_fields(&toks, &mut p) } else { Vec::new() };
            ctors.push((cname, fields));
            if toks[p] == "|" {
                p += 1;
                continue;
            }
            break;
        }
        let mut attributes = Vec::new();
        if toks[p] == "attributes" {
            p += 1;
            attributes = parse_fields(&toks, &mut p);
        }
        let mut doc = String::from(tyname);
        doc.push_str(" = ");
        for (i, (cname, fields)) in ctors.iter().enumerate() {
            if i > 0 {
                doc.push_str("\n  | ");
            }
            doc.push_str(&sig(cname, fields));
        }
        out.push(NodeClass {
            name: tyname,
            base: "AST",
            is_abstract: true,
            fields: Vec::new(),
            attributes: attributes.clone(),
            doc,
        });
        for (cname, fields) in ctors {
            let doc = sig(cname, &fields);
            out.push(NodeClass { name: cname, base: tyname, is_abstract: false, fields, attributes: attributes.clone(), doc });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classes() {
        let c = node_class("FunctionDef").unwrap();
        assert_eq!(c.base, "stmt");
        assert_eq!(c.fields.len(), 7);
        assert_eq!(c.attributes.len(), 4);
        let a = node_class("arguments").unwrap();
        assert_eq!(a.fields.iter().map(|f| f.name).collect::<Vec<_>>(), ["posonlyargs", "args", "vararg", "kwonlyargs", "kw_defaults", "kwarg", "defaults"]);
        assert!(node_class("Load").unwrap().fields.is_empty());
        assert_eq!(node_class("Dict").unwrap().fields[0].kind, FieldKind::OptionalSequence);
        assert_eq!(node_class("BinOp").unwrap().doc, "BinOp(expr left, operator op, expr right)");
    }
}
