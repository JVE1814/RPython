//! Token types (numbered as in the stdlib `token` module) and keyword classification.

/// Token types, numbered like the stdlib `token` module of CPython 3.14/3.15.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
pub enum TokenType {
    ENDMARKER = 0,
    NAME = 1,
    NUMBER = 2,
    STRING = 3,
    NEWLINE = 4,
    INDENT = 5,
    DEDENT = 6,
    LPAR = 7,
    RPAR = 8,
    LSQB = 9,
    RSQB = 10,
    COLON = 11,
    COMMA = 12,
    SEMI = 13,
    PLUS = 14,
    MINUS = 15,
    STAR = 16,
    SLASH = 17,
    VBAR = 18,
    AMPER = 19,
    LESS = 20,
    GREATER = 21,
    EQUAL = 22,
    DOT = 23,
    PERCENT = 24,
    LBRACE = 25,
    RBRACE = 26,
    EQEQUAL = 27,
    NOTEQUAL = 28,
    LESSEQUAL = 29,
    GREATEREQUAL = 30,
    TILDE = 31,
    CIRCUMFLEX = 32,
    LEFTSHIFT = 33,
    RIGHTSHIFT = 34,
    DOUBLESTAR = 35,
    PLUSEQUAL = 36,
    MINEQUAL = 37,
    STAREQUAL = 38,
    SLASHEQUAL = 39,
    PERCENTEQUAL = 40,
    AMPEREQUAL = 41,
    VBAREQUAL = 42,
    CIRCUMFLEXEQUAL = 43,
    LEFTSHIFTEQUAL = 44,
    RIGHTSHIFTEQUAL = 45,
    DOUBLESTAREQUAL = 46,
    DOUBLESLASH = 47,
    DOUBLESLASHEQUAL = 48,
    AT = 49,
    ATEQUAL = 50,
    RARROW = 51,
    ELLIPSIS = 52,
    COLONEQUAL = 53,
    EXCLAMATION = 54,
    OP = 55,
    TYPE_IGNORE = 56,
    TYPE_COMMENT = 57,
    SOFT_KEYWORD = 58,
    FSTRING_START = 59,
    FSTRING_MIDDLE = 60,
    FSTRING_END = 61,
    TSTRING_START = 62,
    TSTRING_MIDDLE = 63,
    TSTRING_END = 64,
    COMMENT = 65,
    NL = 66,
    ERRORTOKEN = 67,
    ENCODING = 68,
}

/// Number of token types (`token.N_TOKENS`).
pub const N_TOKENS: u8 = 69;
/// `token.NT_OFFSET`.
pub const NT_OFFSET: u32 = 256;

impl TokenType {
    pub fn name(self) -> &'static str {
        TOKEN_NAMES[self as usize]
    }
    pub fn from_u8(v: u8) -> Option<TokenType> {
        if v <= TokenType::ENCODING as u8 {
            // SAFETY: repr(u8) with contiguous discriminants 0..=68
            Some(unsafe { std::mem::transmute::<u8, TokenType>(v) })
        } else {
            None
        }
    }
    /// True for operator tokens (reported as `OP` by the tokenize module).
    pub fn is_operator(self) -> bool {
        (self as u8 >= TokenType::LPAR as u8 && self as u8 <= TokenType::EXCLAMATION as u8) || self == TokenType::OP
    }
}

pub const TOKEN_NAMES: [&str; 69] = [
    "ENDMARKER",
    "NAME",
    "NUMBER",
    "STRING",
    "NEWLINE",
    "INDENT",
    "DEDENT",
    "LPAR",
    "RPAR",
    "LSQB",
    "RSQB",
    "COLON",
    "COMMA",
    "SEMI",
    "PLUS",
    "MINUS",
    "STAR",
    "SLASH",
    "VBAR",
    "AMPER",
    "LESS",
    "GREATER",
    "EQUAL",
    "DOT",
    "PERCENT",
    "LBRACE",
    "RBRACE",
    "EQEQUAL",
    "NOTEQUAL",
    "LESSEQUAL",
    "GREATEREQUAL",
    "TILDE",
    "CIRCUMFLEX",
    "LEFTSHIFT",
    "RIGHTSHIFT",
    "DOUBLESTAR",
    "PLUSEQUAL",
    "MINEQUAL",
    "STAREQUAL",
    "SLASHEQUAL",
    "PERCENTEQUAL",
    "AMPEREQUAL",
    "VBAREQUAL",
    "CIRCUMFLEXEQUAL",
    "LEFTSHIFTEQUAL",
    "RIGHTSHIFTEQUAL",
    "DOUBLESTAREQUAL",
    "DOUBLESLASH",
    "DOUBLESLASHEQUAL",
    "AT",
    "ATEQUAL",
    "RARROW",
    "ELLIPSIS",
    "COLONEQUAL",
    "EXCLAMATION",
    "OP",
    "TYPE_IGNORE",
    "TYPE_COMMENT",
    "SOFT_KEYWORD",
    "FSTRING_START",
    "FSTRING_MIDDLE",
    "FSTRING_END",
    "TSTRING_START",
    "TSTRING_MIDDLE",
    "TSTRING_END",
    "COMMENT",
    "NL",
    "ERRORTOKEN",
    "ENCODING",
];

/// Operator strings and their exact token types (the `token.EXACT_TOKEN_TYPES` table).
pub const EXACT_TOKEN_TYPES: &[(&str, TokenType)] = &[
    ("!", TokenType::EXCLAMATION),
    ("!=", TokenType::NOTEQUAL),
    ("%", TokenType::PERCENT),
    ("%=", TokenType::PERCENTEQUAL),
    ("&", TokenType::AMPER),
    ("&=", TokenType::AMPEREQUAL),
    ("(", TokenType::LPAR),
    (")", TokenType::RPAR),
    ("*", TokenType::STAR),
    ("**", TokenType::DOUBLESTAR),
    ("**=", TokenType::DOUBLESTAREQUAL),
    ("*=", TokenType::STAREQUAL),
    ("+", TokenType::PLUS),
    ("+=", TokenType::PLUSEQUAL),
    (",", TokenType::COMMA),
    ("-", TokenType::MINUS),
    ("-=", TokenType::MINEQUAL),
    ("->", TokenType::RARROW),
    (".", TokenType::DOT),
    ("...", TokenType::ELLIPSIS),
    ("/", TokenType::SLASH),
    ("//", TokenType::DOUBLESLASH),
    ("//=", TokenType::DOUBLESLASHEQUAL),
    ("/=", TokenType::SLASHEQUAL),
    (":", TokenType::COLON),
    (":=", TokenType::COLONEQUAL),
    (";", TokenType::SEMI),
    ("<", TokenType::LESS),
    ("<<", TokenType::LEFTSHIFT),
    ("<<=", TokenType::LEFTSHIFTEQUAL),
    ("<=", TokenType::LESSEQUAL),
    ("=", TokenType::EQUAL),
    ("==", TokenType::EQEQUAL),
    (">", TokenType::GREATER),
    (">=", TokenType::GREATEREQUAL),
    (">>", TokenType::RIGHTSHIFT),
    (">>=", TokenType::RIGHTSHIFTEQUAL),
    ("@", TokenType::AT),
    ("@=", TokenType::ATEQUAL),
    ("[", TokenType::LSQB),
    ("]", TokenType::RSQB),
    ("^", TokenType::CIRCUMFLEX),
    ("^=", TokenType::CIRCUMFLEXEQUAL),
    ("{", TokenType::LBRACE),
    ("|", TokenType::VBAR),
    ("|=", TokenType::VBAREQUAL),
    ("}", TokenType::RBRACE),
    ("~", TokenType::TILDE),
];

/// Keywords (hard) and soft keywords, as classified on NAME tokens.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kw {
    None = 0,
    False,
    NoneKw,
    True,
    And,
    As,
    Assert,
    Async,
    Await,
    Break,
    Class,
    Continue,
    Def,
    Del,
    Elif,
    Else,
    Except,
    Finally,
    For,
    From,
    Global,
    If,
    Import,
    In,
    Is,
    Lambda,
    Nonlocal,
    Not,
    Or,
    Pass,
    Raise,
    Return,
    Try,
    While,
    With,
    Yield,
    // soft keywords (still NAMEs)
    SoftMatch,
    SoftCase,
    SoftType,
    SoftUnderscore,
    SoftLazy,
}

impl Kw {
    #[inline]
    pub fn is_hard(self) -> bool {
        (self as u8) >= Kw::False as u8 && (self as u8) <= Kw::Yield as u8
    }
    #[inline]
    pub fn is_soft(self) -> bool {
        (self as u8) >= Kw::SoftMatch as u8
    }
}

/// The hard keywords (`keyword.kwlist`).
pub const KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
    "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda",
    "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield",
];

/// The soft keywords (`keyword.softkwlist`).
pub const SOFT_KEYWORDS: &[&str] = &["_", "case", "lazy", "match", "type"];

/// Classify an identifier.
#[inline]
pub fn classify_name(s: &[u8]) -> Kw {
    use Kw::*;
    match s {
        b"False" => False,
        b"None" => NoneKw,
        b"True" => True,
        b"and" => And,
        b"as" => As,
        b"assert" => Assert,
        b"async" => Async,
        b"await" => Await,
        b"break" => Break,
        b"class" => Class,
        b"continue" => Continue,
        b"def" => Def,
        b"del" => Del,
        b"elif" => Elif,
        b"else" => Else,
        b"except" => Except,
        b"finally" => Finally,
        b"for" => For,
        b"from" => From,
        b"global" => Global,
        b"if" => If,
        b"import" => Import,
        b"in" => In,
        b"is" => Is,
        b"lambda" => Lambda,
        b"nonlocal" => Nonlocal,
        b"not" => Not,
        b"or" => Or,
        b"pass" => Pass,
        b"raise" => Raise,
        b"return" => Return,
        b"try" => Try,
        b"while" => While,
        b"with" => With,
        b"yield" => Yield,
        b"match" => SoftMatch,
        b"case" => SoftCase,
        b"type" => SoftType,
        b"_" => SoftUnderscore,
        b"lazy" => SoftLazy,
        _ => Kw::None,
    }
}

/// A token produced by the tokenizer. Positions: `start`/`end` are byte offsets into the
/// (decoded, UTF-8) source; lines are 1-based; columns are 0-based byte offsets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub ty: TokenType,
    /// Keyword classification for NAME tokens.
    pub kw: Kw,
    pub start: u32,
    pub end: u32,
    pub lineno: u32,
    pub col: i32,
    pub end_lineno: u32,
    pub end_col: i32,
    /// Bracket nesting level after this token (as CPython's token level).
    pub level: u16,
}

impl Token {
    #[inline]
    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.start as usize..self.end as usize]
    }
}
