//! Expression code generation.

use super::*;
use crate::bytecode::BinOp;
use crate::ast::{BoolOp, Comprehension, ExprContext, Keyword, Operator, UnaryOp};
use crate::symtable::CompKind;

/// Displays and calls with more items than this are built incrementally so that the
/// value stack stays small.
const STACK_USE_GUIDELINE: usize = 30;

pub(crate) fn binop(op: Operator) -> BinOp {
    match op {
        Operator::Add => BinOp::Add,
        Operator::Sub => BinOp::Subtract,
        Operator::Mult => BinOp::Multiply,
        Operator::MatMult => BinOp::MatrixMultiply,
        Operator::Div => BinOp::TrueDivide,
        Operator::Mod => BinOp::Remainder,
        Operator::Pow => BinOp::Power,
        Operator::LShift => BinOp::Lshift,
        Operator::RShift => BinOp::Rshift,
        Operator::BitOr => BinOp::Or,
        Operator::BitXor => BinOp::Xor,
        Operator::BitAnd => BinOp::And,
        Operator::FloorDiv => BinOp::FloorDivide,
    }
}

pub(crate) fn inplace_binop(op: Operator) -> BinOp {
    match op {
        Operator::Add => BinOp::InplaceAdd,
        Operator::Sub => BinOp::InplaceSubtract,
        Operator::Mult => BinOp::InplaceMultiply,
        Operator::MatMult => BinOp::InplaceMatrixMultiply,
        Operator::Div => BinOp::InplaceTrueDivide,
        Operator::Mod => BinOp::InplaceRemainder,
        Operator::Pow => BinOp::InplacePower,
        Operator::LShift => BinOp::InplaceLshift,
        Operator::RShift => BinOp::InplaceRshift,
        Operator::BitOr => BinOp::InplaceOr,
        Operator::BitXor => BinOp::InplaceXor,
        Operator::BitAnd => BinOp::InplaceAnd,
        Operator::FloorDiv => BinOp::InplaceFloorDivide,
    }
}

/// Human-readable expression kind for error messages ("function call", "literal", ...).
pub(crate) fn describe(e: &Expr) -> &'static str {
    match &e.node {
        ExprKind::BoolOp { .. } | ExprKind::BinOp { .. } | ExprKind::UnaryOp { .. } => "expression",
        ExprKind::NamedExpr { .. } => "named expression",
        ExprKind::Lambda { .. } => "lambda",
        ExprKind::IfExp { .. } => "conditional expression",
        ExprKind::Dict { .. } => "dict literal",
        ExprKind::Set { .. } => "set display",
        ExprKind::ListComp { .. } => "list comprehension",
        ExprKind::SetComp { .. } => "set comprehension",
        ExprKind::DictComp { .. } => "dict comprehension",
        ExprKind::GeneratorExp { .. } => "generator expression",
        ExprKind::Await { .. } => "await expression",
        ExprKind::Yield { .. } | ExprKind::YieldFrom { .. } => "yield expression",
        ExprKind::Compare { .. } => "comparison",
        ExprKind::Call { .. } => "function call",
        ExprKind::FormattedValue { .. } | ExprKind::JoinedStr { .. } => "f-string expression",
        ExprKind::Interpolation { .. } | ExprKind::TemplateStr { .. } => "t-string expression",
        ExprKind::Constant { value, .. } => match value {
            ast::Constant::None => "None",
            ast::Constant::Bool(true) => "True",
            ast::Constant::Bool(false) => "False",
            ast::Constant::Ellipsis => "ellipsis",
            _ => "literal",
        },
        ExprKind::Attribute { .. } => "attribute",
        ExprKind::Subscript { .. } => "subscript",
        ExprKind::Starred { .. } => "starred",
        ExprKind::Name { .. } => "name",
        ExprKind::List { .. } => "list",
        ExprKind::Tuple { .. } => "tuple",
        ExprKind::Slice { .. } => "slice",
    }
}

fn cmp_arg(op: ast::CmpOp) -> (u16, u32) {
    use ast::CmpOp::*;
    match op {
        Lt => (O::COMPARE_OP, 0),
        LtE => (O::COMPARE_OP, 1),
        Eq => (O::COMPARE_OP, 2),
        NotEq => (O::COMPARE_OP, 3),
        Gt => (O::COMPARE_OP, 4),
        GtE => (O::COMPARE_OP, 5),
        Is => (O::IS_OP, 0),
        IsNot => (O::IS_OP, 1),
        In => (O::CONTAINS_OP, 0),
        NotIn => (O::CONTAINS_OP, 1),
    }
}

fn is_const(e: &Expr) -> bool {
    matches!(e.node, ExprKind::Constant { .. })
}

fn has_starred(v: &[Expr]) -> bool {
    v.iter().any(|e| matches!(e.node, ExprKind::Starred { .. }))
}

/// A slice whose bounds are all constants (or absent) is itself a constant.
pub(crate) fn const_slice(e: &Expr) -> Option<Constant> {
    let ExprKind::Slice { lower, upper, step } = &e.node else { return None };
    let part = |x: &Option<Box<Expr>>| -> Option<Constant> {
        match x {
            None => Some(Constant::None),
            Some(v) => match &v.node {
                ExprKind::Constant { value, .. } => Some(conv_const(value)),
                _ => None,
            },
        }
    };
    Some(Constant::Slice(Box::new([part(lower)?, part(upper)?, part(step)?])))
}

/// `x[a:b]` with non-constant bounds and no step uses BINARY_SLICE / STORE_SLICE.
pub(crate) fn is_two_element_slice(e: &Expr) -> bool {
    matches!(&e.node, ExprKind::Slice { step: None, .. }) && const_slice(e).is_none()
}

/// Compile-time value of a constant expression (literal or constant slice).
fn expr_const(e: &Expr) -> Option<Constant> {
    match &e.node {
        ExprKind::Constant { value, .. } => Some(conv_const(value)),
        ExprKind::Slice { .. } => const_slice(e),
        _ => None,
    }
}

/// Truthiness of a constant (None if not decidable).
pub(crate) fn const_truth(c: &ast::Constant) -> Option<bool> {
    Some(match c {
        ast::Constant::None => false,
        ast::Constant::Bool(b) => *b,
        ast::Constant::Ellipsis => true,
        ast::Constant::Int(ast::Int::Small(i)) => *i != 0,
        ast::Constant::Int(ast::Int::Big(_)) => true,
        ast::Constant::Float(f) => *f != 0.0,
        ast::Constant::Complex { real, imag } => *real != 0.0 || *imag != 0.0,
        ast::Constant::Str(s) => !s.is_empty(),
        ast::Constant::Bytes(b) => !b.is_empty(),
        ast::Constant::Tuple(v) => !v.is_empty(),
        ast::Constant::FrozenSet(v) => !v.is_empty(),
    })
}

/// Ops whose result is an exact bool.
fn produces_bool(op: u16, arg: u32) -> bool {
    match op {
        O::COMPARE_OP => arg & 16 != 0,
        O::IS_OP | O::CONTAINS_OP | O::UNARY_NOT | O::TO_BOOL => true,
        _ => false,
    }
}

impl<'a> Compiler<'a> {
    pub fn compile_expr(&mut self, e: &'a Expr) -> R {
        self.enter_depth()?;
        let saved = self.ur().loc;
        self.set_loc(e.loc);
        let r = self.compile_expr_inner(e);
        self.u().loc = saved;
        self.depth -= 1;
        r
    }

    fn last_op(&self) -> Option<(u16, u32)> {
        self.ur().ir.last_instr().map(|i| (i.op, i.arg as u32))
    }

    /// Emit ToBool unless the TOS is known to be a bool.
    fn to_bool(&mut self) {
        if let Some((op, arg)) = self.last_op() {
            if produces_bool(op, arg) {
                return;
            }
        }
        self.emit(O::TO_BOOL, 0);
    }

    fn compile_expr_inner(&mut self, e: &'a Expr) -> R {
        // guards against malformed hand-built ASTs (see validate_ast)
        let malformed = match &e.node {
            ExprKind::BoolOp { values, .. } => values.len() < 2,
            ExprKind::Compare { ops, comparators, .. } => ops.is_empty() || ops.len() != comparators.len(),
            ExprKind::ListComp { generators, .. } | ExprKind::SetComp { generators, .. } | ExprKind::GeneratorExp { generators, .. } | ExprKind::DictComp { generators, .. } => generators.is_empty(),
            ExprKind::Dict { keys, values } => keys.len() != values.len(),
            _ => false,
        };
        if malformed {
            return Err(CompileError::other(ErrorKind::ValueError, format!("malformed {} node", e.class_name())));
        }
        match &e.node {
            ExprKind::BoolOp { op, values } => {
                let end = self.new_label();
                let jop = if *op == BoolOp::And { O::POP_JUMP_IF_FALSE } else { O::POP_JUMP_IF_TRUE };
                let n = values.len();
                for (i, v) in values.iter().enumerate() {
                    // `a and b or c`: when `a` is falsy the `or` needs no second truth test
                    // of it, so a nested boolop of the other kind exits straight to the
                    // next operand.
                    if i + 1 < n {
                        if let ExprKind::BoolOp { op: inner_op, values: inner } = &v.node {
                            if inner_op != op && inner.len() >= 2 {
                                let next = self.new_label();
                                let drop_value = self.new_label();
                                let exit = if *inner_op == BoolOp::And { O::POP_JUMP_IF_FALSE } else { O::POP_JUMP_IF_TRUE };
                                for w in &inner[..inner.len() - 1] {
                                    self.compile_expr(w)?;
                                    self.set_loc(v.loc);
                                    self.emit(O::COPY, 1);
                                    self.emit(O::TO_BOOL, 0);
                                    self.emit_jump(exit, drop_value);
                                    self.emit(O::POP_TOP, 0);
                                }
                                self.compile_expr(&inner[inner.len() - 1])?;
                                self.set_loc(e.loc);
                                self.emit_jump_keep(jop, end);
                                self.emit_jump_noloc(O::JUMP_NO_INTERRUPT, next);
                                self.use_label(drop_value);
                                self.emit_noloc(O::POP_TOP, 0);
                                self.use_label(next);
                                continue;
                            }
                        }
                    }
                    self.compile_expr(v)?;
                    if i + 1 < n {
                        self.set_loc(e.loc);
                        self.emit_jump_keep(jop, end);
                    }
                }
                self.use_label(end);
            }
            ExprKind::NamedExpr { target, value } => {
                self.compile_expr(value)?;
                self.set_loc(e.loc);
                self.emit(O::COPY, 1);
                self.compile_store(target)?;
            }
            ExprKind::BinOp { left, op, right } => {
                self.compile_expr(left)?;
                self.compile_expr(right)?;
                self.set_loc(e.loc);
                self.emit(O::BINARY_OP, binop(*op) as u32);
            }
            ExprKind::UnaryOp { op, operand } => {
                self.compile_expr(operand)?;
                self.set_loc(e.loc);
                match op {
                    UnaryOp::Invert => self.emit(O::UNARY_INVERT, 0),
                    UnaryOp::Not => {
                        self.to_bool();
                        self.emit(O::UNARY_NOT, 0)
                    }
                    UnaryOp::UAdd => self.emit(O::CALL_INTRINSIC_1, I1::UNARY_POSITIVE),
                    UnaryOp::USub => self.emit(O::UNARY_NEGATIVE, 0),
                }
            }
            ExprKind::Lambda { args, body } => self.compile_lambda(e, args, body)?,
            ExprKind::IfExp { test, body, orelse } => {
                let end = self.new_label();
                let next = self.new_label();
                self.compile_jump_if(test, false, next)?;
                self.compile_expr(body)?;
                self.emit_jump_noloc(O::JUMP_NO_INTERRUPT, end);
                self.use_label(next);
                self.compile_expr(orelse)?;
                self.use_label(end);
            }
            ExprKind::Dict { keys, values } => self.compile_dict(keys, values)?,
            ExprKind::Set { elts } => self.starunpack(elts, O::BUILD_SET, O::SET_ADD, O::SET_UPDATE, false)?,
            ExprKind::List { elts, ctx } => {
                if *ctx != ExprContext::Load {
                    return Err(self.error("invalid list context", e.loc));
                }
                self.starunpack(elts, O::BUILD_LIST, O::LIST_APPEND, O::LIST_EXTEND, false)?
            }
            ExprKind::Tuple { elts, ctx } => {
                if *ctx != ExprContext::Load {
                    return Err(self.error("invalid tuple context", e.loc));
                }
                self.starunpack(elts, O::BUILD_LIST, O::LIST_APPEND, O::LIST_EXTEND, true)?
            }
            ExprKind::GeneratorExp { .. } | ExprKind::ListComp { .. } | ExprKind::SetComp { .. } | ExprKind::DictComp { .. } => {
                self.compile_comprehension(e)?
            }
            ExprKind::Await { value } => {
                // placement errors were reported by the symbol table
                self.compile_expr(value)?;
                self.set_loc(e.loc);
                self.emit(O::GET_AWAITABLE, 0);
                self.emit_await()?;
            }
            ExprKind::Yield { value } => {
                self.check_yield_allowed(e)?;
                match value {
                    Some(v) => self.compile_expr(v)?,
                    None => self.load_none(),
                }
                self.set_loc(e.loc);
                if self.ur().flags & co::ASYNC_GENERATOR != 0 {
                    self.emit(O::CALL_INTRINSIC_1, I1::ASYNC_GEN_WRAP);
                }
                self.emit(O::YIELD_VALUE, 0);
                self.emit(O::RESUME, wordcode::resume::AFTER_YIELD);
            }
            ExprKind::YieldFrom { value } => {
                self.check_yield_allowed(e)?;
                if self.st.scopes[self.ur().scope].coroutine {
                    return Err(self.error("'yield from' inside async function", e.loc));
                }
                self.compile_expr(value)?;
                self.set_loc(e.loc);
                // GET_ITER 1: the `yield from` flavour of iter() (generators and coroutines
                // pass through); pushes [iterator, NULL]
                self.emit(O::GET_ITER, 1);
                self.load_none();
                self.compile_yield_from_loop(false)?;
            }
            ExprKind::Compare { left, ops, comparators } => self.compile_compare(e, left, ops, comparators, false)?,
            ExprKind::Call { func, args, keywords } => self.compile_call(e, func, args, keywords)?,
            ExprKind::FormattedValue { value, conversion, format_spec } => {
                self.compile_formatted_value(value, *conversion, format_spec.as_deref())?
            }
            // nested replacement fields inside a t-string format spec format like f-strings
            ExprKind::Interpolation { value, conversion, format_spec, .. } => {
                self.compile_formatted_value(value, *conversion, format_spec.as_deref())?
            }
            ExprKind::JoinedStr { values } => {
                if values.is_empty() {
                    self.load_const(Constant::Str(Vec::new()));
                } else if values.len() == 1 && matches!(values[0].node, ExprKind::FormattedValue { .. } | ExprKind::Interpolation { .. } | ExprKind::Constant { .. }) {
                    self.compile_expr(&values[0])?;
                } else {
                    for v in values {
                        match &v.node {
                            ExprKind::Starred { .. } => return Err(self.error("can't use starred expression here", v.loc)),
                            _ => self.compile_expr(v)?,
                        }
                    }
                    self.set_loc(e.loc);
                    self.emit(O::BUILD_STRING, values.len() as u32);
                }
            }
            ExprKind::TemplateStr { values } => self.compile_template(e, values)?,
            ExprKind::Constant { value, .. } => self.load_const(conv_const(value)),
            ExprKind::Attribute { value, attr, ctx } => {
                if *ctx != ExprContext::Load {
                    return Err(self.error("invalid attribute context", e.loc));
                }
                if let Some(()) = self.try_super_attr(e, value, attr, false)? {
                    return Ok(());
                }
                self.compile_expr(value)?;
                self.set_loc(attr_loc(e.loc, e, attr));
                let m = self.mangle(attr);
                self.emit_load_attr(&m, false);
            }
            ExprKind::Subscript { value, slice, ctx } => {
                if *ctx != ExprContext::Load {
                    return Err(self.error("invalid subscript context", e.loc));
                }
                self.compile_expr(value)?;
                if is_two_element_slice(slice) {
                    self.compile_slice_bounds(slice)?;
                    self.set_loc(e.loc);
                    self.emit(O::BINARY_SLICE, 0);
                } else {
                    self.compile_expr(slice)?;
                    self.set_loc(e.loc);
                    self.emit(O::BINARY_OP, wordcode::NB_SUBSCR);
                }
            }
            ExprKind::Starred { .. } => return Err(self.error("can't use starred expression here", e.loc)),
            ExprKind::Name { id, ctx } => {
                if *ctx != ExprContext::Load {
                    return Err(self.error("invalid name context", e.loc));
                }
                self.nameop(id, ExprContext::Load)?;
            }
            ExprKind::Slice { lower, upper, step } => {
                if let Some(c) = const_slice(e) {
                    self.load_const(c);
                    return Ok(());
                }
                for x in [lower, upper] {
                    match x {
                        Some(v) => self.compile_expr(v)?,
                        None => self.load_none(),
                    }
                }
                let mut n = 2;
                if let Some(s) = step {
                    self.compile_expr(s)?;
                    n = 3;
                }
                self.set_loc(e.loc);
                self.emit(O::BUILD_SLICE, n);
            }
        }
        Ok(())
    }

    /// Push the lower and upper bounds of a two-element slice (None for missing ones).
    pub fn compile_slice_bounds(&mut self, s: &'a Expr) -> R {
        if let ExprKind::Slice { lower, upper, .. } = &s.node {
            for x in [lower, upper] {
                match x {
                    Some(v) => self.compile_expr(v)?,
                    None => {
                        self.set_loc(s.loc);
                        self.load_none()
                    }
                }
            }
        }
        Ok(())
    }

    fn check_yield_allowed(&mut self, e: &Expr) -> R {
        let sc = &self.st.scopes[self.ur().scope];
        if !sc.is_function_like() {
            let what = if matches!(e.node, ExprKind::YieldFrom { .. }) { "'yield from'" } else { "'yield'" };
            return Err(self.error(format!("{} outside function", what), e.loc));
        }
        Ok(())
    }

    /// The await / yield-from loop. Stack: [receiver, NULL, value] -> [result].
    ///
    /// ```text
    ///   send:  SEND exit            [recv, NULL, v] -> [recv, NULL, res]
    ///          SETUP_FINALLY fail   (handler keeps the whole stack, incl. the yielded value)
    ///          YIELD_VALUE 1
    ///          POP_BLOCK
    ///          RESUME 2|3
    ///          JUMP_NO_INTERRUPT send
    ///   fail:  CLEANUP_THROW        [recv, NULL, v, exc] -> [recv, NULL, value]
    ///   exit:  END_SEND             [recv, NULL, value] -> [value]
    /// ```
    /// Returns the label of the SEND.
    pub fn compile_yield_from_loop(&mut self, is_await: bool) -> R<Label> {
        let send = self.new_label();
        let fail = self.new_label();
        let exit = self.new_label();
        self.use_label(send);
        self.emit_jump(O::SEND, exit);
        self.emit_setup(O::SETUP_FINALLY, fail);
        self.emit(O::YIELD_VALUE, 1);
        self.pop_block();
        self.emit(O::RESUME, if is_await { wordcode::resume::AFTER_AWAIT } else { wordcode::resume::AFTER_YIELD_FROM });
        self.emit_jump(O::JUMP_NO_INTERRUPT, send);
        self.use_label(fail);
        self.emit(O::CLEANUP_THROW, 0);
        self.use_label(exit);
        self.emit(O::END_SEND, 0);
        Ok(send)
    }

    // ------------------------------------------------------------------ conditions

    /// Jump to `target` if truth(e) == cond.
    pub fn compile_jump_if(&mut self, e: &'a Expr, cond: bool, target: Label) -> R {
        self.enter_depth()?;
        let r = self.compile_jump_if_inner(e, cond, target);
        self.depth -= 1;
        r
    }

    fn compile_jump_if_inner(&mut self, e: &'a Expr, cond: bool, target: Label) -> R {
        match &e.node {
            ExprKind::UnaryOp { op: UnaryOp::Not, operand } => return self.compile_jump_if(operand, !cond, target),
            ExprKind::BoolOp { op, values } => {
                let cond2 = *op == BoolOp::Or;
                let next2 = if cond == cond2 { target } else { self.new_label() };
                let n = values.len();
                for v in &values[..n - 1] {
                    self.compile_jump_if(v, cond2, next2)?;
                }
                self.compile_jump_if(&values[n - 1], cond, target)?;
                if next2 != target {
                    self.use_label(next2);
                }
                return Ok(());
            }
            ExprKind::IfExp { test, body, orelse } => {
                let end = self.new_label();
                let next2 = self.new_label();
                self.compile_jump_if(test, false, next2)?;
                self.compile_jump_if(body, cond, target)?;
                self.emit_jump_noloc(O::JUMP_NO_INTERRUPT, end);
                self.use_label(next2);
                self.compile_jump_if(orelse, cond, target)?;
                self.use_label(end);
                return Ok(());
            }
            ExprKind::Compare { left, ops, comparators } if ops.len() > 1 => {
                let cleanup = self.new_label();
                let end = self.new_label();
                self.compile_expr(left)?;
                let n = ops.len() - 1;
                for i in 0..n {
                    self.compile_expr(&comparators[i])?;
                    self.set_loc(e.loc);
                    self.emit(O::SWAP, 2);
                    self.emit(O::COPY, 2);
                    self.emit_cmpop(ops[i], true);
                    self.emit_jump(O::POP_JUMP_IF_FALSE, cleanup);
                }
                self.compile_expr(&comparators[n])?;
                self.set_loc(e.loc);
                self.emit_cmpop(ops[n], true);
                self.emit_jump(if cond { O::POP_JUMP_IF_TRUE } else { O::POP_JUMP_IF_FALSE }, target);
                self.emit_jump_noloc(O::JUMP_NO_INTERRUPT, end);
                self.use_label(cleanup);
                self.emit_noloc(O::POP_TOP, 0);
                if !cond {
                    self.emit_jump_noloc(O::JUMP_NO_INTERRUPT, target);
                }
                self.use_label(end);
                return Ok(());
            }
            ExprKind::Constant { value, .. } => {
                if let Some(t) = const_truth(value) {
                    self.set_loc(e.loc);
                    if t == cond {
                        self.emit_jump(O::JUMP, target);
                    } else {
                        self.emit(O::NOP, 0);
                    }
                    return Ok(());
                }
            }
            ExprKind::Compare { left, ops, comparators } if ops.len() == 1 => {
                // `x is None` / `x is not None` -> PopJumpIfNone/NotNone
                let is_none = |x: &Expr| matches!(x.node, ExprKind::Constant { value: ast::Constant::None, .. });
                if matches!(ops[0], ast::CmpOp::Is | ast::CmpOp::IsNot) && is_none(&comparators[0]) {
                    self.compile_expr(left)?;
                    self.set_loc(e.loc);
                    let is = ops[0] == ast::CmpOp::Is;
                    let op = if is == cond { O::POP_JUMP_IF_NONE } else { O::POP_JUMP_IF_NOT_NONE };
                    self.emit_jump(op, target);
                    return Ok(());
                }
                self.compile_compare(e, left, ops, comparators, true)?;
                self.emit_jump(if cond { O::POP_JUMP_IF_TRUE } else { O::POP_JUMP_IF_FALSE }, target);
                return Ok(());
            }
            _ => {}
        }
        self.compile_expr(e)?;
        self.set_loc(e.loc);
        self.to_bool();
        self.emit_jump(if cond { O::POP_JUMP_IF_TRUE } else { O::POP_JUMP_IF_FALSE }, target);
        Ok(())
    }

    /// Jump to `target` keeping the value if its truth matches `op` (POP_JUMP_IF_FALSE:
    /// falsy), otherwise pop it: COPY 1; TO_BOOL; POP_JUMP_IF_x; POP_TOP.
    fn emit_jump_keep(&mut self, op: u16, target: Label) {
        self.emit(O::COPY, 1);
        self.emit(O::TO_BOOL, 0);
        self.emit_jump(op, target);
        self.emit(O::POP_TOP, 0);
    }

    fn emit_cmpop(&mut self, op: ast::CmpOp, as_bool: bool) {
        let (o, arg) = cmp_arg(op);
        if o == O::COMPARE_OP {
            self.emit_compare(arg, as_bool);
        } else {
            self.emit(o, arg);
        }
    }

    fn compile_compare(&mut self, e: &'a Expr, left: &'a Expr, ops: &'a [ast::CmpOp], comps: &'a [Expr], as_bool: bool) -> R {
        self.compile_expr(left)?;
        let n = ops.len();
        if n == 1 {
            self.compile_expr(&comps[0])?;
            self.set_loc(e.loc);
            self.emit_cmpop(ops[0], as_bool);
            return Ok(());
        }
        let cleanup = self.new_label();
        let end = self.new_label();
        for i in 0..n - 1 {
            self.compile_expr(&comps[i])?;
            self.set_loc(e.loc);
            self.emit(O::SWAP, 2);
            self.emit(O::COPY, 2);
            self.emit_cmpop(ops[i], as_bool);
            self.emit_jump_keep(O::POP_JUMP_IF_FALSE, cleanup);
        }
        self.compile_expr(&comps[n - 1])?;
        self.set_loc(e.loc);
        self.emit_cmpop(ops[n - 1], as_bool);
        self.emit_jump_noloc(O::JUMP_NO_INTERRUPT, end);
        self.use_label(cleanup);
        self.emit(O::SWAP, 2);
        self.emit(O::POP_TOP, 0);
        self.use_label(end);
        Ok(())
    }

    // ------------------------------------------------------------------ collections

    /// Build a list/set/tuple from `elts` (possibly with starred items).
    fn starunpack(&mut self, elts: &'a [Expr], build: u16, add: u16, extend: u16, tuple: bool) -> R {
        let n = elts.len();
        if !has_starred(elts) {
            if n > STACK_USE_GUIDELINE && !elts.iter().all(|x| expr_const(x).is_some()) {
                // long displays are built one item at a time to bound the stack
                self.emit(build, 0);
                for x in elts {
                    self.compile_expr(x)?;
                    self.emit(add, 1);
                }
                if tuple {
                    self.emit(O::CALL_INTRINSIC_1, I1::LIST_TO_TUPLE);
                }
                return Ok(());
            }
            // constant displays are folded by the CFG optimiser
            for x in elts {
                self.compile_expr(x)?;
            }
            self.emit(if tuple { O::BUILD_TUPLE } else { build }, n as u32);
            return Ok(());
        }
        // with starred items: build incrementally
        let mut seen_star = false;
        let mut pending = 0u32;
        for x in elts {
            match &x.node {
                ExprKind::Starred { value, .. } => {
                    if !seen_star {
                        self.emit(build, pending);
                        seen_star = true;
                    }
                    self.compile_expr(value)?;
                    self.emit(extend, 1);
                }
                _ => {
                    self.compile_expr(x)?;
                    if seen_star {
                        self.emit(add, 1);
                    } else {
                        pending += 1;
                    }
                }
            }
        }
        if tuple {
            self.emit(O::CALL_INTRINSIC_1, I1::LIST_TO_TUPLE);
        }
        Ok(())
    }

    fn compile_dict(&mut self, keys: &'a [Option<Expr>], values: &'a [Expr]) -> R {
        if keys.len() > STACK_USE_GUIDELINE / 2 && keys.iter().all(|k| k.is_some()) {
            self.emit(O::BUILD_MAP, 0);
            for (k, v) in keys.iter().zip(values.iter()) {
                self.compile_expr(k.as_ref().unwrap())?;
                self.compile_expr(v)?;
                self.emit(O::MAP_ADD, 1);
            }
            return Ok(());
        }
        let mut have_dict = false;
        let mut elements = 0u32;
        for (k, v) in keys.iter().zip(values.iter()) {
            match k {
                Some(k) => {
                    self.compile_expr(k)?;
                    self.compile_expr(v)?;
                    elements += 1;
                    if elements == 16 {
                        self.emit(O::BUILD_MAP, elements);
                        if have_dict {
                            self.emit(O::DICT_UPDATE, 1);
                        }
                        have_dict = true;
                        elements = 0;
                    }
                }
                None => {
                    if elements > 0 {
                        self.emit(O::BUILD_MAP, elements);
                        if have_dict {
                            self.emit(O::DICT_UPDATE, 1);
                        }
                        have_dict = true;
                        elements = 0;
                    }
                    if !have_dict {
                        self.emit(O::BUILD_MAP, 0);
                        have_dict = true;
                    }
                    self.compile_expr(v)?;
                    self.emit(O::DICT_UPDATE, 1);
                }
            }
        }
        if elements > 0 || !have_dict {
            self.emit(O::BUILD_MAP, elements);
            if have_dict {
                self.emit(O::DICT_UPDATE, 1);
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------ strings

    fn compile_formatted_value(&mut self, value: &'a Expr, conversion: i32, spec: Option<&'a Expr>) -> R {
        if let ExprKind::Starred { .. } = value.node {
            return Err(self.error("can't use starred expression here", value.loc));
        }
        self.compile_expr(value)?;
        match conversion {
            115 => self.emit(O::CONVERT_VALUE, 1),
            114 => self.emit(O::CONVERT_VALUE, 2),
            97 => self.emit(O::CONVERT_VALUE, 3),
            _ => {}
        }
        match spec {
            Some(s) => {
                self.compile_expr(s)?;
                self.emit(O::FORMAT_WITH_SPEC, 0);
            }
            None => self.emit(O::FORMAT_SIMPLE, 0),
        }
        Ok(())
    }

    fn compile_template(&mut self, e: &'a Expr, values: &'a [Expr]) -> R {
        let mut strings: Vec<Constant> = Vec::new();
        let mut cur: Vec<u32> = Vec::new();
        let mut interps: Vec<&'a Expr> = Vec::new();
        for v in values {
            match &v.node {
                ExprKind::Constant { value: ast::Constant::Str(s), .. } => cur.extend(s.code_points()),
                ExprKind::Interpolation { .. } => {
                    strings.push(Constant::Str(std::mem::take(&mut cur)));
                    interps.push(v);
                }
                _ => return Err(self.error("unexpected node in t-string", v.loc)),
            }
        }
        strings.push(Constant::Str(cur));
        self.set_loc(e.loc);
        self.load_const(Constant::Tuple(strings));
        for iv in &interps {
            if let ExprKind::Interpolation { value, str, conversion, format_spec } = &iv.node {
                if let ExprKind::Starred { .. } = value.node {
                    return Err(self.error("can't use starred expression here", value.loc));
                }
                self.compile_expr(value)?;
                self.load_const(Constant::Str(str.code_points().collect()));
                let conv = match *conversion {
                    115 => 1,
                    114 => 2,
                    97 => 3,
                    _ => 0,
                };
                let mut has_spec = 0;
                if let Some(s) = format_spec {
                    self.compile_expr(s)?;
                    has_spec = 1;
                }
                self.set_loc(iv.loc);
                self.emit(O::BUILD_INTERPOLATION, (conv << 2) | has_spec);
            }
        }
        self.set_loc(e.loc);
        self.emit(O::BUILD_TUPLE, interps.len() as u32);
        self.emit(O::BUILD_TEMPLATE, 0);
        Ok(())
    }

    // ------------------------------------------------------------------ calls

    fn compile_call(&mut self, e: &'a Expr, func: &'a Expr, args: &'a [Expr], keywords: &'a [Keyword]) -> R {
        self.validate_keywords(keywords)?;
        let simple = !has_starred(args) && keywords.iter().all(|k| k.arg.is_some());
        if simple && args.len() + keywords.len() <= STACK_USE_GUIDELINE {
            if let ExprKind::Attribute { value, attr, ctx: ExprContext::Load } = &func.node {
                // method call
                if self.try_super_attr(func, value, attr, true)?.is_none() {
                    self.compile_expr(value)?;
                    self.set_loc(attr_loc(func.loc, func, attr));
                    let m = self.mangle(attr);
                    self.emit_load_attr(&m, true);
                }
                return self.compile_call_args(e, attr_loc(e.loc, func, attr), 0, args, keywords);
            }
        }
        if self.try_builtin_genexpr_call(e, func, args, keywords)? {
            return Ok(());
        }
        self.compile_expr(func)?;
        self.set_loc(e.loc);
        self.emit(O::PUSH_NULL, 0);
        self.compile_call_args(e, e.loc, 0, args, keywords)
    }

    /// `list(genexpr)`, `tuple(...)`, `set(...)`, `all(...)`, `any(...)`: when the name is
    /// still bound to the builtin at run time, the generator is consumed by an inline loop;
    /// otherwise the call happens normally.
    fn try_builtin_genexpr_call(&mut self, e: &'a Expr, func: &'a Expr, args: &'a [Expr], keywords: &'a [Keyword]) -> R<bool> {
        let ExprKind::Name { id, .. } = &func.node else { return Ok(false) };
        let which = match id.as_str() {
            "tuple" => common::BUILTIN_TUPLE,
            "all" => common::BUILTIN_ALL,
            "any" => common::BUILTIN_ANY,
            "list" => common::BUILTIN_LIST,
            "set" => common::BUILTIN_SET,
            _ => return Ok(false),
        };
        if args.len() != 1 || !keywords.is_empty() {
            return Ok(false);
        }
        let ExprKind::GeneratorExp { generators, .. } = &args[0].node else { return Ok(false) };
        if generators[0].is_async != 0 {
            return Ok(false);
        }
        // only when the name resolves to a global (implicit) in a function-like unit
        {
            let u = self.ur();
            let sc = &self.st.scopes[u.scope];
            if !u.comps.is_empty() || !sc.is_function_like() || sc.scope_of(&self.mangle(id)) != SymScope::GlobalImplicit {
                return Ok(false);
            }
        }
        let sid = self.st.scope_for(key(&args[0], symtable::TAG_COMP)).expect("genexpr scope");
        if self.st.scopes[sid].coroutine {
            return Ok(false);
        }
        let genexpr = &args[0];
        let f = self.compile_comprehension_code(genexpr)?;
        let slow = self.new_label();
        let done = self.new_label();
        self.compile_expr(func)?;
        self.set_loc(e.loc);
        self.emit(O::COPY, 1);
        self.emit(O::LOAD_COMMON_CONSTANT, which);
        self.emit(O::IS_OP, 0);
        self.emit_jump(O::POP_JUMP_IF_FALSE, slow);
        self.emit(O::POP_TOP, 0);
        let start = self.new_label();
        let exhausted = self.new_label();
        match which {
            common::BUILTIN_LIST | common::BUILTIN_TUPLE | common::BUILTIN_SET => {
                let (build, add) = if which == common::BUILTIN_SET { (O::BUILD_SET, O::SET_ADD) } else { (O::BUILD_LIST, O::LIST_APPEND) };
                self.emit(build, 0);
                self.emit_comprehension_call(genexpr, &f)?;
                self.set_loc(e.loc);
                self.emit(O::PUSH_NULL, 0);
                self.use_label(start);
                self.emit_jump(O::FOR_ITER, exhausted);
                self.emit(add, 3);
                self.emit_jump(O::JUMP, start);
                self.use_label(exhausted);
                self.emit(O::END_FOR, 0);
                self.emit(O::POP_ITER, 0);
                if which == common::BUILTIN_TUPLE {
                    self.emit(O::CALL_INTRINSIC_1, I1::LIST_TO_TUPLE);
                }
                self.emit_jump(O::JUMP_NO_INTERRUPT, done);
            }
            _ => {
                // all(): stop at the first false item; any(): at the first true one
                let found = self.new_label();
                let is_all = which == common::BUILTIN_ALL;
                self.emit_comprehension_call(genexpr, &f)?;
                self.set_loc(e.loc);
                self.emit(O::PUSH_NULL, 0);
                self.use_label(start);
                self.emit_jump(O::FOR_ITER, exhausted);
                self.emit(O::TO_BOOL, 0);
                self.emit_jump(if is_all { O::POP_JUMP_IF_FALSE } else { O::POP_JUMP_IF_TRUE }, found);
                self.emit_jump(O::JUMP, start);
                self.use_label(found);
                self.emit(O::POP_ITER, 0);
                self.load_const(Constant::Bool(!is_all));
                self.emit_jump(O::JUMP_NO_INTERRUPT, done);
                self.use_label(exhausted);
                self.emit(O::END_FOR, 0);
                self.emit(O::POP_ITER, 0);
                self.load_const(Constant::Bool(is_all));
                self.emit_jump(O::JUMP_NO_INTERRUPT, done);
            }
        }
        self.use_label(slow);
        self.set_loc(e.loc);
        self.emit(O::PUSH_NULL, 0);
        self.emit_comprehension_call(genexpr, &f)?;
        self.set_loc(e.loc);
        self.emit(O::CALL, 1);
        self.use_label(done);
        Ok(true)
    }

    fn validate_keywords(&self, keywords: &[Keyword]) -> R {
        for (i, k) in keywords.iter().enumerate() {
            if let Some(a) = &k.arg {
                if a == "__debug__" {
                    return Err(self.error("cannot assign to __debug__", k.loc));
                }
                for k2 in &keywords[i + 1..] {
                    if k2.arg.as_deref() == Some(a.as_str()) {
                        return Err(self.error(format!("keyword argument repeated: {}", a), k2.loc));
                    }
                }
            }
        }
        Ok(())
    }

    fn compile_call_args(&mut self, e: &'a Expr, cloc: Loc, pushed: u32, args: &'a [Expr], keywords: &'a [Keyword]) -> R {
        if has_starred(args) || keywords.iter().any(|k| k.arg.is_none()) || args.len() + keywords.len() > STACK_USE_GUIDELINE {
            return self.compile_call_ex(e, pushed, args, keywords, false);
        }
        for a in args {
            self.compile_expr(a)?;
        }
        let nargs = pushed + args.len() as u32;
        if keywords.is_empty() {
            self.set_loc(cloc);
            self.emit(O::CALL, nargs);
        } else {
            for k in keywords {
                self.compile_expr(&k.value)?;
            }
            let names: Vec<Constant> = keywords.iter().map(|k| str_const(k.arg.as_ref().unwrap())).collect();
            self.set_loc(cloc);
            self.load_const(Constant::Tuple(names));
            self.emit(O::CALL_KW, nargs + keywords.len() as u32);
        }
        Ok(())
    }

    /// Call helper used for class creation: `pushed` positional args already on the stack
    /// (after callable/NULL); `generic`: append `Generic[*.type_params]` as last base.
    pub fn compile_call_helper(&mut self, pushed: u32, args: &'a [Expr], keywords: &'a [Keyword], generic: bool) -> R {
        self.validate_keywords(keywords)?;
        let loc_e = self.ur().loc;
        if !generic && !has_starred(args) && keywords.iter().all(|k| k.arg.is_some()) {
            for a in args {
                self.compile_expr(a)?;
            }
            let nargs = pushed + args.len() as u32;
            self.u().loc = loc_e;
            if keywords.is_empty() {
                self.emit(O::CALL, nargs);
            } else {
                for k in keywords {
                    self.compile_expr(&k.value)?;
                }
                let names: Vec<Constant> = keywords.iter().map(|k| str_const(k.arg.as_ref().unwrap())).collect();
                self.u().loc = loc_e;
                self.load_const(Constant::Tuple(names));
                self.emit(O::CALL_KW, nargs + keywords.len() as u32);
            }
            return Ok(());
        }
        if generic && !has_starred(args) && keywords.iter().all(|k| k.arg.is_some()) {
            for a in args {
                self.compile_expr(a)?;
            }
            self.u().loc = loc_e;
            self.nameop(".type_params", ExprContext::Load)?;
            self.emit(O::CALL_INTRINSIC_1, I1::SUBSCRIPT_GENERIC);
            let nargs = pushed + args.len() as u32 + 1;
            if keywords.is_empty() {
                self.emit(O::CALL, nargs);
            } else {
                for k in keywords {
                    self.compile_expr(&k.value)?;
                }
                let names: Vec<Constant> = keywords.iter().map(|k| str_const(k.arg.as_ref().unwrap())).collect();
                self.u().loc = loc_e;
                self.load_const(Constant::Tuple(names));
                self.emit(O::CALL_KW, nargs + keywords.len() as u32);
            }
            return Ok(());
        }
        self.compile_call_ex_loc(loc_e, pushed, args, keywords, generic)
    }

    fn compile_call_ex(&mut self, e: &'a Expr, pushed: u32, args: &'a [Expr], keywords: &'a [Keyword], generic: bool) -> R {
        let l = loc_of(e.loc);
        self.compile_call_ex_loc(l, pushed, args, keywords, generic)
    }

    fn compile_call_ex_loc(&mut self, l: Location, pushed: u32, args: &'a [Expr], keywords: &'a [Keyword], generic: bool) -> R {
        // positional args -> tuple
        if pushed == 0 && args.len() == 1 && !generic {
            if let ExprKind::Starred { value, .. } = &args[0].node {
                // f(*a): pass a as is if it's already a tuple-producing expression
                self.compile_expr(value)?;
                self.u().loc = l;
                self.emit(O::BUILD_LIST, 0);
                self.emit(O::SWAP, 2);
                self.emit(O::LIST_EXTEND, 1);
                self.emit(O::CALL_INTRINSIC_1, I1::LIST_TO_TUPLE);
            } else {
                self.compile_expr(&args[0])?;
                self.emit(O::BUILD_TUPLE, 1);
            }
        } else {
            self.u().loc = l;
            self.emit(O::BUILD_LIST, pushed);
            for a in args {
                match &a.node {
                    ExprKind::Starred { value, .. } => {
                        self.compile_expr(value)?;
                        self.emit(O::LIST_EXTEND, 1);
                    }
                    _ => {
                        self.compile_expr(a)?;
                        self.emit(O::LIST_APPEND, 1);
                    }
                }
            }
            if generic {
                self.nameop(".type_params", ExprContext::Load)?;
                self.emit(O::CALL_INTRINSIC_1, I1::SUBSCRIPT_GENERIC);
                self.emit(O::LIST_APPEND, 1);
            }
            self.u().loc = l;
            self.emit(O::CALL_INTRINSIC_1, I1::LIST_TO_TUPLE);
        }
        // keyword args -> dict
        if keywords.is_empty() {
            self.emit(O::PUSH_NULL, 0);
        } else {
            let mut have_dict = false;
            let mut i = 0;
            while i < keywords.len() {
                if keywords[i].arg.is_none() {
                    if !have_dict {
                        self.emit(O::BUILD_MAP, 0);
                        have_dict = true;
                    }
                    self.compile_expr(&keywords[i].value)?;
                    self.u().loc = l;
                    self.emit(O::DICT_MERGE, 1);
                    i += 1;
                } else {
                    let start = i;
                    while i < keywords.len() && keywords[i].arg.is_some() {
                        i += 1;
                    }
                    if i - start > STACK_USE_GUIDELINE / 2 {
                        // many keywords: grow the dict one item at a time
                        self.u().loc = l;
                        if !have_dict {
                            self.emit(O::BUILD_MAP, 0);
                        }
                        for k in &keywords[start..i] {
                            self.load_const(str_const(k.arg.as_ref().unwrap()));
                            self.compile_expr(&k.value)?;
                            self.emit(O::MAP_ADD, 1);
                        }
                        have_dict = true;
                        continue;
                    }
                    for k in &keywords[start..i] {
                        self.load_const(str_const(k.arg.as_ref().unwrap()));
                        self.compile_expr(&k.value)?;
                    }
                    self.u().loc = l;
                    self.emit(O::BUILD_MAP, (i - start) as u32);
                    if have_dict {
                        self.emit(O::DICT_MERGE, 1);
                    }
                    have_dict = true;
                }
            }
        }
        self.u().loc = l;
        self.emit(O::CALL_FUNCTION_EX, 0);
        Ok(())
    }

    /// `super().attr` / `super().meth(...)` optimisation (LoadSuperAttr).
    fn try_super_attr(&mut self, attr_expr: &'a Expr, value: &'a Expr, attr: &str, method: bool) -> R<Option<()>> {
        let (func, args, keywords) = match &value.node {
            ExprKind::Call { func, args, keywords } => (func, args, keywords),
            _ => return Ok(None),
        };
        let fname = match &func.node {
            ExprKind::Name { id, .. } if id == "super" => id,
            _ => return Ok(None),
        };
        if attr == "__class__" || !keywords.is_empty() || !self.ur().comps.is_empty() {
            return Ok(None);
        }
        let u = self.ur();
        let sc = &self.st.scopes[u.scope];
        if !sc.is_function_like() || sc.scope_of(fname) != SymScope::GlobalImplicit {
            return Ok(None);
        }
        if self.st.scopes[0].lookup(fname).is_some() {
            return Ok(None);
        }
        let two = args.len() == 2;
        if two {
            if has_starred(args) {
                return Ok(None);
            }
        } else if !args.is_empty() {
            return Ok(None);
        } else {
            if u.argcount == 0 && u.posonlyargcount == 0 {
                return Ok(None);
            }
            if sc.scope_of("__class__") != SymScope::Free {
                return Ok(None);
            }
        }
        self.set_loc(func.loc);
        self.nameop(fname, ExprContext::Load)?;
        if two {
            self.compile_expr(&args[0])?;
            self.compile_expr(&args[1])?;
        } else {
            self.set_loc(value.loc);
            self.nameop("__class__", ExprContext::Load)?;
            let first = self.ur().varnames[0].clone();
            self.nameop(&first, ExprContext::Load)?;
        }
        self.set_loc(attr_expr.loc);
        let m = self.mangle(attr);
        let ni = self.u().add_name(&m);
        let arg = (ni << 2) | if method { 1 } else { 0 } | if two { 2 } else { 0 };
        self.emit(O::LOAD_SUPER_ATTR, arg);
        Ok(Some(()))
    }

    // ------------------------------------------------------------------ comprehensions

    fn comprehension_parts(e: &'a Expr) -> (CompKind, &'a [Comprehension], &'a Expr, Option<&'a Expr>) {
        match &e.node {
            ExprKind::GeneratorExp { elt, generators } => (CompKind::Gen, generators, &**elt, None),
            ExprKind::ListComp { elt, generators } => (CompKind::List, generators, &**elt, None),
            ExprKind::SetComp { elt, generators } => (CompKind::Set, generators, &**elt, None),
            ExprKind::DictComp { key: k, value, generators } => (CompKind::Dict, generators, &**k, value.as_deref()),
            _ => unreachable!(),
        }
    }

    fn compile_comprehension(&mut self, e: &'a Expr) -> R {
        let (kind, generators, elt, value) = Self::comprehension_parts(e);
        let sid = self.st.scope_for(key(e, symtable::TAG_COMP)).expect("comprehension scope");
        if self.st.scopes[sid].comp_inlined {
            return self.compile_inlined_comprehension(e, sid, kind, generators, elt, value);
        }
        let f = self.compile_comprehension_code(e)?;
        self.emit_comprehension_call(e, &f)?;
        let is_async_gen = self.st.scopes[sid].coroutine;
        if is_async_gen && kind != CompKind::Gen {
            self.set_loc(e.loc);
            self.emit(O::GET_AWAITABLE, 0);
            self.emit_await()?;
        }
        Ok(())
    }

    /// Compiles a comprehension that is not inlined (generator expressions, comprehensions in
    /// class-visible annotation scopes) into its own code object, added to the constants.
    /// The function receives the iterable of the outermost loop (the async iterator when the
    /// outermost loop is `async for`) as its only argument `.0`.
    pub fn compile_comprehension_code(&mut self, e: &'a Expr) -> R<CompFn> {
        let (kind, generators, elt, value) = Self::comprehension_parts(e);
        let sid = self.st.scope_for(key(e, symtable::TAG_COMP)).expect("comprehension scope");
        let name = match kind {
            CompKind::Gen => "<genexpr>",
            CompKind::List => "<listcomp>",
            CompKind::Set => "<setcomp>",
            CompKind::Dict => "<dictcomp>",
            CompKind::None => unreachable!(),
        };
        let async_outer = generators[0].is_async != 0;
        if kind == CompKind::Gen && !async_outer {
            self.pending_genexpr = Some(loc_of(e.loc));
        }
        let r = self.enter_scope(name, UnitKind::Comprehension, sid, e.loc.lineno);
        self.pending_genexpr = None;
        r?;
        self.u().argcount = 1;
        self.set_loc(e.loc);
        if kind != CompKind::Gen {
            let build = match kind {
                CompKind::List => O::BUILD_LIST,
                CompKind::Set => O::BUILD_SET,
                _ => O::BUILD_MAP,
            };
            self.emit(build, 0);
            self.emit(O::LOAD_FAST, 0); // .0
            if !async_outer {
                self.emit(O::GET_ITER, 0);
            }
        } else if async_outer {
            self.emit(O::LOAD_FAST, 0);
        }
        // (a synchronous genexpr's prologue already pushed [iterator, NULL])
        self.comprehension_generator(e, generators, 0, 0, elt, value, kind)?;
        self.set_loc(e.loc);
        if kind == CompKind::Gen {
            self.load_none();
        }
        self.emit(O::RETURN_VALUE, 0);
        let code = self.exit_scope()?;
        let frees = code_freevars(&code);
        let idx = self.u().add_const(Constant::Code(Box::new(code)));
        Ok(CompFn { idx, frees, async_outer })
    }

    /// `<function>; <outermost iterable>; CALL 0` (the iterable travels in the
    /// self_or_null slot and becomes the function's first argument).
    pub fn emit_comprehension_call(&mut self, e: &'a Expr, f: &CompFn) -> R {
        let (_, generators, _, _) = Self::comprehension_parts(e);
        self.set_loc(e.loc);
        self.emit_make_function(&f.frees, f.idx, 0)?;
        self.compile_expr(&generators[0].iter)?;
        self.set_loc(e.loc);
        if f.async_outer {
            self.emit(O::GET_AITER, 0);
        }
        self.emit(O::CALL, 0);
        Ok(())
    }

    fn compile_inlined_comprehension(
        &mut self,
        e: &'a Expr,
        sid: ScopeId,
        kind: CompKind,
        generators: &'a [Comprehension],
        elt: &'a Expr,
        value: Option<&'a Expr>,
    ) -> R {
        let outermost = &generators[0];
        self.compile_expr(&outermost.iter)?;
        self.set_loc(e.loc);
        // isolate names bound in the comprehension
        let mut ctx = CompCtx { scope: sid, slots: HashMap::new(), has_setup: false };
        let mut pushed: Vec<CompSlot> = Vec::new();
        let syms: Vec<(String, u32, SymScope)> =
            self.st.scopes[sid].symbols.iter().map(|s| (s.name.clone(), s.flags, s.scope)).collect();
        for (name, flags, scope) in syms {
            if flags & symtable::DEF_PARAM != 0 {
                continue;
            }
            if !matches!(scope, SymScope::Local | SymScope::Cell) {
                continue;
            }
            let slot = self.comp_slot(&name, scope == SymScope::Cell);
            self.emit(O::LOAD_FAST_AND_CLEAR, slot.slot);
            if slot.is_cell {
                self.emit(O::MAKE_CELL, slot.slot);
            }
            ctx.slots.insert(name, slot);
            pushed.push(slot);
        }
        let cleanup = self.new_label();
        let end = self.new_label();
        if !pushed.is_empty() {
            self.check_block_limit(21, e.loc)?;
            ctx.has_setup = true;
            self.emit(O::SWAP, pushed.len() as u32 + 1);
            self.emit_setup(O::SETUP_FINALLY, cleanup);
        }
        self.u().comps.push(ctx);
        let build = match kind {
            CompKind::List => O::BUILD_LIST,
            CompKind::Set => O::BUILD_SET,
            _ => O::BUILD_MAP,
        };
        self.emit(build, 0);
        self.emit(O::SWAP, 2);
        if outermost.is_async != 0 {
            self.emit(O::GET_AITER, 0);
        } else {
            self.emit(O::GET_ITER, 0);
        }
        let r = self.comprehension_generator(e, generators, 0, 0, elt, value, kind);
        self.u().comps.pop();
        r?;
        if !pushed.is_empty() {
            self.pop_block();
            self.emit_jump_noloc(O::JUMP_NO_INTERRUPT, end);
            self.use_label(cleanup);
            // [saved..., partial result, exc]
            self.emit_noloc(O::SWAP, 2);
            self.emit_noloc(O::POP_TOP, 0);
            self.restore_comp_locals(&pushed);
            self.emit_noloc(O::RERAISE, 0);
            self.use_label(end);
            self.restore_comp_locals(&pushed);
        }
        Ok(())
    }

    fn restore_comp_locals(&mut self, pushed: &[CompSlot]) {
        self.emit_noloc(O::SWAP, pushed.len() as u32 + 1);
        for s in pushed.iter().rev() {
            self.emit_noloc(O::STORE_FAST_MAYBE_NULL, s.slot);
        }
    }

    /// Slot in the current unit for a name bound by an inlined comprehension.
    fn comp_slot(&mut self, name: &str, comp_cell: bool) -> CompSlot {
        // nested inlined comprehension: reuse the outer comprehension's slot
        let u = self.ur();
        let usc = &self.st.scopes[u.scope];
        if usc.is_function_like() {
            match usc.scope_of(name) {
                SymScope::Local if !comp_cell => {
                    let i = self.u().var_index(name);
                    return CompSlot { slot: i, is_cell: false };
                }
                SymScope::Cell => {
                    if let Some(c) = u.cell_slot(name) {
                        return CompSlot { slot: c, is_cell: true };
                    }
                }
                SymScope::Free => {
                    if let Some(f) = u.free_slot(name) {
                        return CompSlot { slot: f, is_cell: true };
                    }
                }
                _ => {}
            }
        }
        if comp_cell {
            let c = self.u().hidden_cell(name);
            CompSlot { slot: c, is_cell: true }
        } else {
            let i = self.u().hidden_var(name);
            CompSlot { slot: i, is_cell: false }
        }
    }

    /// `depth`: stack slots used by the enclosing loops of this comprehension (an iterator
    /// takes two slots, an async iterator one).
    #[allow(clippy::too_many_arguments)]
    fn comprehension_generator(
        &mut self,
        e: &'a Expr,
        generators: &'a [Comprehension],
        gen_index: usize,
        depth: u32,
        elt: &'a Expr,
        value: Option<&'a Expr>,
        kind: CompKind,
    ) -> R {
        let g = &generators[gen_index];
        if g.is_async != 0 {
            self.async_comprehension_generator(e, generators, gen_index, depth, elt, value, kind)
        } else {
            self.sync_comprehension_generator(e, generators, gen_index, depth, elt, value, kind)
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn sync_comprehension_generator(
        &mut self,
        e: &'a Expr,
        generators: &'a [Comprehension],
        gen_index: usize,
        mut depth: u32,
        elt: &'a Expr,
        value: Option<&'a Expr>,
        kind: CompKind,
    ) -> R {
        let g = &generators[gen_index];
        let start = self.new_label();
        let if_cleanup = self.new_label();
        let anchor = self.new_label();
        // `for x in [y]` with a single element: no loop
        let single = if gen_index > 0 {
            match &g.iter.node {
                ExprKind::List { elts, .. } | ExprKind::Tuple { elts, .. }
                    if elts.len() == 1 && !matches!(elts[0].node, ExprKind::Starred { .. }) =>
                {
                    Some(&elts[0])
                }
                _ => None,
            }
        } else {
            None
        };
        if gen_index > 0 && single.is_none() {
            self.compile_expr(&g.iter)?;
            self.set_loc(g.iter.loc);
            self.emit(O::GET_ITER, 0);
        }
        if let Some(x) = single {
            self.compile_expr(x)?;
        } else {
            depth += 2;
            self.use_label(start);
            self.set_loc(g.iter.loc);
            self.emit_jump(O::FOR_ITER, anchor);
            self.set_loc(g.target.loc);
            self.emit(O::NOP, 0);
        }
        self.compile_store(&g.target)?;
        for cond in &g.ifs {
            self.compile_jump_if(cond, false, if_cleanup)?;
        }
        if gen_index + 1 < generators.len() {
            self.comprehension_generator(e, generators, gen_index + 1, depth, elt, value, kind)?;
        } else {
            self.comprehension_element(e, depth, elt, value, kind)?;
        }
        self.use_label(if_cleanup);
        if single.is_none() {
            self.u().loc = NO_LOC;
            self.emit_jump(O::JUMP, start);
            self.use_label(anchor);
            self.set_loc(g.iter.loc);
            self.emit(O::END_FOR, 0);
            self.emit(O::POP_ITER, 0);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn async_comprehension_generator(
        &mut self,
        e: &'a Expr,
        generators: &'a [Comprehension],
        gen_index: usize,
        mut depth: u32,
        elt: &'a Expr,
        value: Option<&'a Expr>,
        kind: CompKind,
    ) -> R {
        let g = &generators[gen_index];
        let start = self.new_label();
        let except = self.new_label();
        let if_cleanup = self.new_label();
        if gen_index > 0 {
            self.compile_expr(&g.iter)?;
            self.set_loc(g.iter.loc);
            self.emit(O::GET_AITER, 0);
        }
        depth += 1;
        self.use_label(start);
        self.u().fblocks.push(FBlock {
            kind: FbKind::AsyncComprehensionGenerator,
            block: start,
            exit: NO_LABEL,
            loc: loc_of(g.iter.loc),
            finally: None,
            name: None,
        });
        self.set_loc(g.iter.loc);
        self.emit_setup(O::SETUP_FINALLY, except);
        self.emit(O::GET_ANEXT, 0);
        self.emit_await_noloop_prefix();
        let send = self.compile_yield_from_loop(true)?;
        self.pop_block();
        self.compile_store(&g.target)?;
        for cond in &g.ifs {
            self.compile_jump_if(cond, false, if_cleanup)?;
        }
        if gen_index + 1 < generators.len() {
            self.comprehension_generator(e, generators, gen_index + 1, depth, elt, value, kind)?;
        } else {
            self.comprehension_element(e, depth, elt, value, kind)?;
        }
        self.use_label(if_cleanup);
        self.u().loc = NO_LOC;
        self.emit_jump(O::JUMP, start);
        self.u().fblocks.pop();
        self.use_label(except);
        self.set_loc(g.iter.loc);
        self.emit_jump(O::END_ASYNC_FOR, send);
        Ok(())
    }

    /// PUSH_NULL; LOAD_CONST None (the part of an await before its SEND loop).
    fn emit_await_noloop_prefix(&mut self) {
        self.emit(O::PUSH_NULL, 0);
        self.load_none();
    }

    fn comprehension_element(&mut self, e: &'a Expr, depth: u32, elt: &'a Expr, value: Option<&'a Expr>, kind: CompKind) -> R {
        match kind {
            CompKind::Gen => {
                if let ExprKind::Starred { value: inner, .. } = &elt.node {
                    // PEP 798: (*x for x in it)  ==  yield from x
                    self.compile_expr(inner)?;
                    self.set_loc(e.loc);
                    if self.ur().flags & co::ASYNC_GENERATOR != 0 {
                        let start = self.new_label();
                        let done = self.new_label();
                        self.emit(O::GET_ITER, 0);
                        self.use_label(start);
                        self.emit_jump(O::FOR_ITER, done);
                        self.emit(O::CALL_INTRINSIC_1, I1::ASYNC_GEN_WRAP);
                        self.emit(O::YIELD_VALUE, 0);
                        self.emit(O::RESUME, wordcode::resume::AFTER_YIELD);
                        self.emit(O::POP_TOP, 0);
                        self.emit_jump(O::JUMP, start);
                        self.use_label(done);
                        self.emit(O::END_FOR, 0);
                        self.emit(O::POP_ITER, 0);
                    } else {
                        self.emit(O::GET_ITER, 1);
                        self.load_none();
                        self.compile_yield_from_loop(false)?;
                        self.emit(O::POP_TOP, 0);
                    }
                } else {
                    self.compile_expr(elt)?;
                    self.set_loc(elt.loc);
                    if self.ur().flags & co::ASYNC_GENERATOR != 0 {
                        self.emit(O::CALL_INTRINSIC_1, I1::ASYNC_GEN_WRAP);
                    }
                    self.emit(O::YIELD_VALUE, 0);
                    self.emit(O::RESUME, wordcode::resume::AFTER_YIELD);
                    self.emit(O::POP_TOP, 0);
                }
            }
            CompKind::List | CompKind::Set => {
                let (add, ext) = if kind == CompKind::List { (O::LIST_APPEND, O::LIST_EXTEND) } else { (O::SET_ADD, O::SET_UPDATE) };
                if let ExprKind::Starred { value: inner, .. } = &elt.node {
                    self.compile_expr(inner)?;
                    self.set_loc(elt.loc);
                    self.emit(ext, depth + 1);
                } else {
                    self.compile_expr(elt)?;
                    self.set_loc(elt.loc);
                    self.emit(add, depth + 1);
                }
            }
            CompKind::Dict => match value {
                Some(v) => {
                    self.compile_expr(elt)?;
                    self.compile_expr(v)?;
                    self.set_loc(e.loc);
                    self.emit(O::MAP_ADD, depth + 1);
                }
                None => {
                    // PEP 798: {**d for d in it}
                    self.compile_expr(elt)?;
                    self.set_loc(elt.loc);
                    self.emit(O::DICT_UPDATE, depth + 1);
                }
            },
            CompKind::None => unreachable!(),
        }
        Ok(())
    }
}

/// A compiled comprehension function: its constant index and free variables.
pub(crate) struct CompFn {
    pub idx: u32,
    pub frees: Vec<String>,
    pub async_outer: bool,
}

/// Names of a code object's free variables (closure order).
pub(crate) fn code_freevars(code: &CodeObject) -> Vec<String> {
    code.localsplusnames
        .iter()
        .zip(code.localspluskinds.iter())
        .filter(|(_, k)| **k & fast::FREE != 0)
        .map(|(n, _)| n.clone())
        .collect()
}

/// CPython's update_start_location_to_match_attr: when an attribute access spans lines, the
/// instruction location starts at the attribute name (for tracebacks and line events).
pub(crate) fn attr_loc(mut loc: Loc, attr_expr: &Expr, attr: &str) -> Loc {
    let a = attr_expr.loc;
    if loc.lineno != a.end_lineno {
        loc.lineno = a.end_lineno;
        let len = attr.len() as i32;
        if len <= a.end_col_offset {
            loc.col_offset = a.end_col_offset - len;
        } else {
            loc.col_offset = -1;
            loc.end_col_offset = -1;
        }
        loc.end_lineno = loc.lineno.max(loc.end_lineno);
        if loc.lineno == loc.end_lineno {
            loc.end_col_offset = loc.col_offset.max(loc.end_col_offset);
        }
    }
    loc
}
