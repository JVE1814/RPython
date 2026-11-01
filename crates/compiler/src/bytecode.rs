//! The bytecode contract between the compiler (this crate) and the VM (rpy-runtime).
//! Our own design (not CPython's): fixed-width 32-bit instructions
//!   bits 0..8   opcode
//!   bits 8..32  argument (24 bits); EXTENDED_ARG prefixes supply higher bits.
//! Jump arguments are ABSOLUTE instruction indices.
//! Inline caches are NOT interleaved: the VM keeps per-instruction side tables
//! (HotSpot-style method data / constant-pool cache), indexed by instruction index.
//!
//! Stack notation: [before] -> [after], top of stack on the right.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Op {
    Nop = 0,
    /// [x] -> []
    PopTop,
    /// [] -> [NULL]
    PushNull,
    /// [.., x(at depth n)] -> [.., x, .., x]   (COPY 1 duplicates TOS)
    Copy,
    /// swap TOS with the item at depth n (n >= 2)
    Swap,
    /// [] -> [consts[arg]]
    LoadConst,
    /// [] -> [fast[arg]]  (compiler proved it is bound)
    LoadFast,
    /// [] -> [fast[arg]]  raises UnboundLocalError if unbound
    LoadFastCheck,
    /// [] -> [fast[arg] or NULL], and clears the slot (comprehension inlining)
    LoadFastAndClear,
    /// [x] -> []
    StoreFast,
    /// raises UnboundLocalError if unbound
    DeleteFast,
    /// Convert local slot arg into a cell (cell variables that are also arguments
    /// or otherwise initialised). Emitted at function start.
    MakeCell,
    /// Copy `arg` closure cells from the function into the free-variable slots.
    CopyFreeVars,
    /// [] -> [cell[arg].contents]  (NameError/UnboundLocalError on empty)
    LoadDeref,
    /// [x] -> []
    StoreDeref,
    DeleteDeref,
    /// [mapping] -> [value]  (class bodies: look in mapping first, then cell arg)
    LoadFromDictOrDeref,
    /// [] -> [locals()]  (the frame's locals mapping)
    LoadLocals,
    /// [] -> [value]  names[arg] via locals, globals, builtins
    LoadName,
    StoreName,
    DeleteName,
    /// [] -> [value]  names[arg] via globals, builtins
    LoadGlobal,
    StoreGlobal,
    DeleteGlobal,
    /// [mapping] -> [value]  names[arg]: mapping, then globals, then builtins
    LoadFromDictOrGlobals,
    /// [obj] -> [obj.names[arg]]
    LoadAttr,
    /// [obj] -> [method, obj] if names[arg] is a plain method, else [attr, NULL]
    /// i.e. the pair is (callable, self_or_null) as expected by Call.
    LoadMethod,
    /// [value, obj] -> []
    StoreAttr,
    /// [obj] -> []
    DeleteAttr,
    /// [global_super, class, self] -> [attr]  names[arg>>2]; arg&1: load method form
    /// pushing (callable, self_or_null); arg&2: two-arg form super(class, self)
    LoadSuperAttr,
    /// [a, b] -> [a op b]  arg = BinOp code (see BinOp)
    BinaryOp,
    /// [a] -> [-a]
    UnaryNegative,
    /// [a] -> [+a]
    UnaryPositive,
    /// [a] -> [~a]
    UnaryInvert,
    /// [a] -> [not a]
    UnaryNot,
    /// [a] -> [bool(a)]
    ToBool,
    /// [a, b] -> [a cmp b]  arg = CmpOp (0..6) ; arg & 16: result converted to bool
    CompareOp,
    /// [a, b] -> [a is b]  arg=1: is not
    IsOp,
    /// [a, b] -> [a in b]  arg=1: not in
    ContainsOp,
    /// [container, key] -> [container[key]]
    BinarySubscr,
    /// [value, container, key] -> []
    StoreSubscr,
    /// [container, key] -> []
    DeleteSubscr,
    /// [container, start, stop] -> [container[start:stop]]
    BinarySlice,
    /// [value, container, start, stop] -> []
    StoreSlice,
    /// [start, stop] -> [slice]  (arg=2) or [start, stop, step] -> [slice] (arg=3)
    BuildSlice,
    /// [x1..xn] -> [tuple]
    BuildTuple,
    BuildList,
    BuildSet,
    /// [k1, v1, .. kn, vn] -> [dict]
    BuildMap,
    /// [s1..sn] -> [concatenated str]
    BuildString,
    /// [list, ..(arg-1 items).., x] -> [list, ...]  list at depth arg+1 after pop: list.append(x)
    ListAppend,
    SetAdd,
    /// [dict, ..., key, value] -> [dict, ...]
    MapAdd,
    /// [list, ..., iterable] -> [list, ...]  list at depth arg (after popping iterable)
    ListExtend,
    SetUpdate,
    DictUpdate,
    /// like DictUpdate but raises on duplicate keys (for f(**a, **b)); function at depth arg+2
    DictMerge,
    /// [seq] -> [x_n-1 .. x0]  (x0 on top)
    UnpackSequence,
    /// arg = before | (after << 8): [seq] -> [after items.., list, before items..]
    UnpackEx,
    /// unconditional jump to arg
    Jump,
    /// [x] -> [] ; jump if false
    PopJumpIfFalse,
    PopJumpIfTrue,
    PopJumpIfNone,
    PopJumpIfNotNone,
    /// [x] -> [x] and jump if false, else [x] -> []
    JumpIfFalseOrPop,
    JumpIfTrueOrPop,
    /// [x] -> [iter(x)]
    GetIter,
    /// [iter] -> [iter, next(iter)] ; on exhaustion pop iter and jump to arg
    ForIter,
    /// [x] -> [aiter(x)]
    GetAiter,
    /// [aiter] -> [aiter, awaitable(anext)]
    GetAnext,
    /// [aiter, exc] -> [] if exc is StopAsyncIteration else reraise
    EndAsyncFor,
    /// [x] -> [awaitable iterator]; arg: 0 plain await, 1 after __aenter__, 2 after __aexit__
    GetAwaitable,
    /// [x] -> [iter] (yield from: generator/coroutine pass through, else iter())
    GetYieldFromIter,
    /// [receiver, value] -> [receiver, result] ; if receiver finished:
    /// [receiver, value] -> [return value] and jump to arg
    Send,
    /// [value] -> [sent value]   (suspends the frame; arg: depth of exception stack (unused))
    YieldValue,
    /// [sub_iter, last_sent_val, exc] -> [None, value] if StopIteration else reraise
    CleanupThrow,
    /// [value] -> return
    ReturnValue,
    /// [callable, self_or_null, a1..an] -> [result]  arg = n
    Call,
    /// [callable, self_or_null, a1..an, kwnames_tuple] -> [result] arg = n (incl. kw values)
    CallKw,
    /// [callable, self_or_null, args_tuple, kwargs_or_NULL] -> [result]
    CallFunctionEx,
    /// [x] -> [intrinsic(x)]  arg = Intrinsic1
    CallIntrinsic1,
    /// [x, y] -> [intrinsic(x, y)]  arg = Intrinsic2
    CallIntrinsic2,
    /// [code] -> [function]
    MakeFunction,
    /// [attr, func] -> [func]  arg = MakeFunctionFlag (one bit)
    SetFunctionAttribute,
    /// [] -> [builtins.__build_class__]
    LoadBuildClass,
    /// arg 0: re-raise current; 1: [exc] raise exc; 2: [exc, cause] raise exc from cause
    RaiseVarargs,
    /// [.., exc] -> re-raise exc. arg != 0: [lasti, exc] also restore f_lasti from lasti
    Reraise,
    /// [exc] -> [prev_exc_info, exc]  (exc becomes the handled exception)
    PushExcInfo,
    /// [prev_exc_info] -> []  restores the handled exception
    PopExcept,
    /// [exc, type] -> [exc, bool]
    CheckExcMatch,
    /// [exc, type] -> [rest, match]  (except*)
    CheckEgMatch,
    /// [exit_func, self_or_null, lasti, prev_exc, val] -> [.., res]  calls exit(type, val, tb)
    WithExceptStart,
    /// [mgr] -> [exit_func, self_or_null, enter_result]
    BeforeWith,
    /// [mgr] -> [aexit_func, self_or_null, awaitable(aenter)]
    BeforeAsyncWith,
    /// ensure __annotations__ exists in locals (module/class scope, old-style)
    SetupAnnotations,
    /// [level, fromlist] -> [module]  names[arg]; arg >> 23 == 1 for lazy imports
    ImportName,
    /// [module] -> [module, module.names[arg]]
    ImportFrom,
    /// [value] -> [str(value)]  (format(value, ''))
    FormatSimple,
    /// [value, spec] -> [format(value, spec)]
    FormatWithSpec,
    /// [value] -> [converted]  arg: 1 str, 2 repr, 3 ascii
    ConvertValue,
    /// [strings_tuple, interpolations_tuple] -> [Template]
    BuildTemplate,
    /// [value, str, (format_spec)] -> [Interpolation]; arg = (conversion << 2) | has_spec
    BuildInterpolation,
    /// [subject, type, names_tuple] -> [attrs_tuple or None]  arg = positional count
    MatchClass,
    /// [subject] -> [subject, bool]
    MatchMapping,
    MatchSequence,
    /// [subject, keys] -> [subject, keys, values_tuple or None]
    MatchKeys,
    /// [x] -> [x, len(x)]
    GetLen,
    /// [] -> [common constant arg] (CommonConstant)
    LoadCommonConstant,
    /// [] -> [] marker at function/generator start (also used for tracing 'call' events)
    Resume,
    /// prefix: arg contributes the high bits of the next instruction's argument
    ExtendedArg,
    /// [x] -> [] ; x must be None at generator/coroutine end of `yield from` cleanup (unused)
    Unused1,
    /// [exc] -> raise RuntimeError if StopIteration leaks from a generator (handled by VM)
    Unused2,
    /// [value] -> [] (for `print` of interactive expressions, also see Intrinsic1::Print)
    Unused3,
}

pub const NUM_OPS: usize = Op::Unused3 as usize + 1;

impl Op {
    pub fn from_u8(b: u8) -> Op {
        assert!((b as usize) < NUM_OPS);
        unsafe { std::mem::transmute(b) }
    }
    pub fn is_jump(self) -> bool {
        matches!(
            self,
            Op::Jump
                | Op::PopJumpIfFalse
                | Op::PopJumpIfTrue
                | Op::PopJumpIfNone
                | Op::PopJumpIfNotNone
                | Op::JumpIfFalseOrPop
                | Op::JumpIfTrueOrPop
                | Op::ForIter
                | Op::Send
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum BinOp {
    Add = 0,
    And,
    FloorDivide,
    Lshift,
    MatrixMultiply,
    Multiply,
    Remainder,
    Or,
    Power,
    Rshift,
    Subtract,
    TrueDivide,
    Xor,
    InplaceAdd,
    InplaceAnd,
    InplaceFloorDivide,
    InplaceLshift,
    InplaceMatrixMultiply,
    InplaceMultiply,
    InplaceRemainder,
    InplaceOr,
    InplacePower,
    InplaceRshift,
    InplaceSubtract,
    InplaceTrueDivide,
    InplaceXor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CmpOp {
    Lt = 0,
    Le = 1,
    Eq = 2,
    Ne = 3,
    Gt = 4,
    Ge = 5,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Intrinsic1 {
    /// print the value of an interactive expression statement (sys.displayhook)
    Print = 1,
    /// from module import *  : [module] -> [None]
    ImportStar,
    /// convert StopIteration raised inside a generator into RuntimeError
    StopIterationError,
    /// unary +
    UnaryPositive,
    /// list -> tuple
    ListToTuple,
    /// name -> TypeVar(name)
    TypeVar,
    ParamSpec,
    TypeVarTuple,
    /// [type_params tuple] -> Generic[*params]
    SubscriptGeneric,
    /// (name, type_params, compute_value) -> TypeAliasType
    TypeAlias,
    /// ADDED by the compiler: [value] -> [wrapped] marks a value yielded by a `yield`
    /// expression in an async generator (CPython's INTRINSIC_ASYNC_GEN_WRAP). Values yielded
    /// by the `await` loops inside the same frame are NOT wrapped; the async generator
    /// machinery must treat wrapped values as items produced by `__anext__`/`asend` and
    /// unwrapped ones as values for the event loop.
    AsyncGenWrap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Intrinsic2 {
    /// [exc, value] -> re-raise in except* context (prep_reraise_star)
    PrepReraiseStar = 1,
    /// (name, bound) -> TypeVar
    TypeVarWithBound,
    TypeVarWithConstraints,
    /// set __type_params__ on a function
    SetFunctionTypeParams,
    /// set the default of a type parameter
    SetTypeparamDefault,
}

/// SetFunctionAttribute flags (one per instruction).
pub mod make_fn {
    pub const DEFAULTS: u32 = 0x01;
    pub const KWDEFAULTS: u32 = 0x02;
    /// old-style annotations dict
    pub const ANNOTATIONS: u32 = 0x04;
    pub const CLOSURE: u32 = 0x08;
    /// PEP 649 __annotate__ function
    pub const ANNOTATE: u32 = 0x10;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CommonConstant {
    AssertionError = 0,
    NotImplementedError = 1,
    BuiltinTuple = 2,
    BuiltinAll = 3,
    BuiltinAny = 4,
}

/// Code flags (same values as CPython's CO_* so `inspect` works).
pub mod co {
    pub const OPTIMIZED: u32 = 0x0001;
    pub const NEWLOCALS: u32 = 0x0002;
    pub const VARARGS: u32 = 0x0004;
    pub const VARKEYWORDS: u32 = 0x0008;
    pub const NESTED: u32 = 0x0010;
    pub const GENERATOR: u32 = 0x0020;
    pub const NOFREE: u32 = 0x0040;
    pub const COROUTINE: u32 = 0x0080;
    pub const ITERABLE_COROUTINE: u32 = 0x0100;
    pub const ASYNC_GENERATOR: u32 = 0x0200;
    pub const HAS_DOCSTRING: u32 = 0x4000000;
    pub const METHOD: u32 = 0x8000000;
}

/// Kinds of entries in localsplus (same bit values as CPython's CO_FAST_*).
pub mod fast {
    pub const HIDDEN: u8 = 0x10;
    pub const LOCAL: u8 = 0x20;
    pub const CELL: u8 = 0x40;
    pub const FREE: u8 = 0x80;
    pub const ARG_POS: u8 = 0x02;
    pub const ARG_KW: u8 = 0x04;
    pub const ARG_VAR: u8 = 0x08;
}

#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
    None,
    Bool(bool),
    Ellipsis,
    /// small int
    Int(i64),
    /// big int as decimal digits with sign
    BigInt { neg: bool, decimal: String },
    Float(f64),
    Complex { real: f64, imag: f64 },
    /// code points (may contain lone surrogates)
    Str(Vec<u32>),
    Bytes(Vec<u8>),
    Tuple(Vec<Constant>),
    FrozenSet(Vec<Constant>),
    Code(Box<CodeUnit>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Location {
    /// 0 means "no location" (e.g. artificial instructions)
    pub lineno: u32,
    pub end_lineno: u32,
    /// UTF-8 byte offsets; u32::MAX means unknown
    pub col: u32,
    pub end_col: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExceptionEntry {
    /// instruction range [start, end)
    pub start: u32,
    pub end: u32,
    /// handler instruction index
    pub target: u32,
    /// value-stack depth to unwind to before pushing (lasti?) and the exception
    pub depth: u32,
    /// push the raising instruction index before the exception
    pub lasti: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CodeUnit {
    pub name: String,
    pub qualname: String,
    pub filename: String,
    pub firstlineno: u32,
    pub argcount: u32,
    pub posonlyargcount: u32,
    pub kwonlyargcount: u32,
    pub flags: u32,
    pub stacksize: u32,
    pub code: Vec<u32>,
    /// one entry per instruction in `code`
    pub locations: Vec<Location>,
    pub consts: Vec<Constant>,
    pub names: Vec<String>,
    /// all local slots: arguments, locals, cells, then free vars (CPython's localsplus order)
    pub localsplusnames: Vec<String>,
    pub localspluskinds: Vec<u8>,
    pub exception_table: Vec<ExceptionEntry>,
}

#[inline]
pub fn encode(op: Op, arg: u32) -> u32 {
    debug_assert!(arg < (1 << 24));
    (op as u32) | (arg << 8)
}
#[inline]
pub fn decode(w: u32) -> (Op, u32) {
    (Op::from_u8((w & 0xff) as u8), w >> 8)
}
