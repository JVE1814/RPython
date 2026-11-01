//! Expression code generation.

use super::*;
use crate::assemble::F_MAY_STORE_NULL;
use crate::ast::{BoolOp, Comprehension, ExprContext, Keyword, Operator, UnaryOp};
use crate::symtable::CompKind;

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

fn cmp_arg(op: ast::CmpOp) -> (Op, u32) {
    use ast::CmpOp::*;
    match op {
        Eq => (Op::CompareOp, CmpOp::Eq as u32),
        NotEq => (Op::CompareOp, CmpOp::Ne as u32),
        Lt => (Op::CompareOp, CmpOp::Lt as u32),
        LtE => (Op::CompareOp, CmpOp::Le as u32),
        Gt => (Op::CompareOp, CmpOp::Gt as u32),
        GtE => (Op::CompareOp, CmpOp::Ge as u32),
        Is => (Op::IsOp, 0),
        IsNot => (Op::IsOp, 1),
        In => (Op::ContainsOp, 0),
        NotIn => (Op::ContainsOp, 1),
    }
}

fn is_const(e: &Expr) -> bool {
    matches!(e.node, ExprKind::Constant { .. })
}

fn has_starred(v: &[Expr]) -> bool {
    v.iter().any(|e| matches!(e.node, ExprKind::Starred { .. }))
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
fn produces_bool(op: Op, arg: u32) -> bool {
    match op {
        Op::CompareOp => arg & 16 != 0,
        Op::IsOp | Op::ContainsOp | Op::UnaryNot | Op::ToBool => true,
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

    fn last_op(&self) -> Option<(Op, u32)> {
        let u = self.ur();
        u.ir.blocks[u.ir.cur as usize].instrs.last().and_then(|i| match i.op {
            IOp::Op(o) => Some((o, i.arg)),
            _ => None,
        })
    }

    /// Emit ToBool unless the TOS is known to be a bool.
    fn to_bool(&mut self) {
        if let Some((op, arg)) = self.last_op() {
            if produces_bool(op, arg) {
                return;
            }
        }
        self.emit(Op::ToBool, 0);
    }

    fn compile_expr_inner(&mut self, e: &'a Expr) -> R {
        match &e.node {
            ExprKind::BoolOp { op, values } => {
                let end = self.new_label();
                let jop = if *op == BoolOp::And { Op::JumpIfFalseOrPop } else { Op::JumpIfTrueOrPop };
                let n = values.len();
                for (i, v) in values.iter().enumerate() {
                    self.compile_expr(v)?;
                    if i + 1 < n {
                        self.set_loc(e.loc);
                        self.emit_jump(jop, end);
                    }
                }
                self.use_label(end);
            }
            ExprKind::NamedExpr { target, value } => {
                self.compile_expr(value)?;
                self.set_loc(e.loc);
                self.emit(Op::Copy, 1);
                self.compile_store(target)?;
            }
            ExprKind::BinOp { left, op, right } => {
                self.compile_expr(left)?;
                self.compile_expr(right)?;
                self.set_loc(e.loc);
                self.emit(Op::BinaryOp, binop(*op) as u32);
            }
            ExprKind::UnaryOp { op, operand } => {
                self.compile_expr(operand)?;
                self.set_loc(e.loc);
                match op {
                    UnaryOp::Invert => self.emit(Op::UnaryInvert, 0),
                    UnaryOp::Not => {
                        self.to_bool();
                        self.emit(Op::UnaryNot, 0)
                    }
                    UnaryOp::UAdd => self.emit(Op::CallIntrinsic1, Intrinsic1::UnaryPositive as u32),
                    UnaryOp::USub => self.emit(Op::UnaryNegative, 0),
                }
            }
            ExprKind::Lambda { args, body } => self.compile_lambda(e, args, body)?,
            ExprKind::IfExp { test, body, orelse } => {
                let end = self.new_label();
                let next = self.new_label();
                self.compile_jump_if(test, false, next)?;
                self.compile_expr(body)?;
                self.emit_jump_noloc(Op::Jump, end);
                self.use_label(next);
                self.compile_expr(orelse)?;
                self.use_label(end);
            }
            ExprKind::Dict { keys, values } => self.compile_dict(keys, values)?,
            ExprKind::Set { elts } => self.starunpack(elts, Op::BuildSet, Op::SetAdd, Op::SetUpdate, false)?,
            ExprKind::List { elts, ctx } => {
                if *ctx != ExprContext::Load {
                    return Err(self.error("invalid list context", e.loc));
                }
                self.starunpack(elts, Op::BuildList, Op::ListAppend, Op::ListExtend, false)?
            }
            ExprKind::Tuple { elts, ctx } => {
                if *ctx != ExprContext::Load {
                    return Err(self.error("invalid tuple context", e.loc));
                }
                self.starunpack(elts, Op::BuildList, Op::ListAppend, Op::ListExtend, true)?
            }
            ExprKind::GeneratorExp { .. } | ExprKind::ListComp { .. } | ExprKind::SetComp { .. } | ExprKind::DictComp { .. } => {
                self.compile_comprehension(e)?
            }
            ExprKind::Await { value } => {
                if !self.in_async_context() {
                    let sc = &self.st.scopes[self.ur().scope];
                    if !(sc.comprehension != CompKind::None && sc.coroutine) && !self.ur().comps.is_empty() {
                        // inlined async comprehension in an async function: fine
                    } else if sc.comprehension == CompKind::None {
                        return Err(self.error("'await' outside async function", e.loc));
                    }
                }
                self.compile_expr(value)?;
                self.set_loc(e.loc);
                self.emit(Op::GetAwaitable, 0);
                self.load_none();
                self.compile_yield_from_loop(true)?;
            }
            ExprKind::Yield { value } => {
                self.check_yield_allowed(e)?;
                match value {
                    Some(v) => self.compile_expr(v)?,
                    None => self.load_none(),
                }
                self.set_loc(e.loc);
                if self.ur().flags & co::ASYNC_GENERATOR != 0 {
                    self.emit(Op::CallIntrinsic1, Intrinsic1::AsyncGenWrap as u32);
                }
                self.emit(Op::YieldValue, 0);
                self.emit(Op::Resume, 1);
            }
            ExprKind::YieldFrom { value } => {
                self.check_yield_allowed(e)?;
                if self.st.scopes[self.ur().scope].coroutine {
                    return Err(self.error("'yield from' inside async function", e.loc));
                }
                self.compile_expr(value)?;
                self.set_loc(e.loc);
                self.emit(Op::GetYieldFromIter, 0);
                self.load_none();
                self.compile_yield_from_loop(false)?;
            }
            ExprKind::Compare { left, ops, comparators } => self.compile_compare(e, left, ops, comparators, false)?,
            ExprKind::Call { func, args, keywords } => self.compile_call(e, func, args, keywords)?,
            ExprKind::FormattedValue { value, conversion, format_spec } => {
                self.compile_formatted_value(value, *conversion, format_spec.as_deref())?
            }
            ExprKind::Interpolation { .. } => {
                return Err(self.error("Interpolation outside of a t-string", e.loc));
            }
            ExprKind::JoinedStr { values } => {
                if values.is_empty() {
                    self.load_const(Constant::Str(Vec::new()));
                } else if values.len() == 1 && matches!(values[0].node, ExprKind::FormattedValue { .. }) {
                    self.compile_expr(&values[0])?;
                } else {
                    for v in values {
                        match &v.node {
                            ExprKind::Starred { .. } => return Err(self.error("can't use starred expression here", v.loc)),
                            _ => self.compile_expr(v)?,
                        }
                    }
                    self.set_loc(e.loc);
                    self.emit(Op::BuildString, values.len() as u32);
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
                self.set_loc(e.loc);
                let m = self.mangle(attr);
                self.emit_name(Op::LoadAttr, &m);
            }
            ExprKind::Subscript { value, slice, ctx } => {
                if *ctx != ExprContext::Load {
                    return Err(self.error("invalid subscript context", e.loc));
                }
                self.compile_expr(value)?;
                self.compile_expr(slice)?;
                self.set_loc(e.loc);
                self.emit(Op::BinarySubscr, 0);
            }
            ExprKind::Starred { .. } => return Err(self.error("can't use starred expression here", e.loc)),
            ExprKind::Name { id, ctx } => {
                if *ctx != ExprContext::Load {
                    return Err(self.error("invalid name context", e.loc));
                }
                self.nameop(id, ExprContext::Load)?;
            }
            ExprKind::Slice { lower, upper, step } => {
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
                self.emit(Op::BuildSlice, n);
            }
        }
        Ok(())
    }

    fn check_yield_allowed(&mut self, e: &Expr) -> R {
        let sc = &self.st.scopes[self.ur().scope];
        if !sc.is_function_like() {
            return Err(self.error("'yield' outside function", e.loc));
        }
        Ok(())
    }

    /// The await / yield-from loop. Stack: [receiver, value] -> [result].
    ///
    /// ```text
    ///   send:  Send exit           [recv, v] -> [recv, res] | -> [retval] jump exit
    ///          (SetupYield fail)   handler depth = recv-level
    ///          YieldValue 1        [recv, res] -> [recv, sent]
    ///          Jump send
    ///   fail:  [recv, exc] LoadConst None; Swap 2; CleanupThrow -> [None, value]; Swap 2; PopTop
    ///   exit:  [result]
    /// ```
    pub fn compile_yield_from_loop(&mut self, is_await: bool) -> R {
        let send = self.new_label();
        let fail = self.new_label();
        let exit = self.new_label();
        self.use_label(send);
        self.emit_jump(Op::Send, exit);
        self.emit_pseudo(IOp::SetupYield, fail);
        self.emit(Op::YieldValue, 1);
        self.pop_block();
        self.emit(Op::Resume, if is_await { 3 } else { 2 });
        self.emit_jump(Op::Jump, send);
        self.use_label(fail);
        self.load_const_noloc(Constant::None);
        self.emit_noloc(Op::Swap, 2);
        self.emit(Op::CleanupThrow, 0);
        self.emit_noloc(Op::Swap, 2);
        self.emit_noloc(Op::PopTop, 0);
        self.use_label(exit);
        Ok(())
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
                self.emit_jump_noloc(Op::Jump, end);
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
                    self.emit(Op::Swap, 2);
                    self.emit(Op::Copy, 2);
                    self.emit_compare(ops[i], true);
                    self.emit_jump(Op::PopJumpIfFalse, cleanup);
                }
                self.compile_expr(&comparators[n])?;
                self.set_loc(e.loc);
                self.emit_compare(ops[n], true);
                self.emit_jump(if cond { Op::PopJumpIfTrue } else { Op::PopJumpIfFalse }, target);
                self.emit_jump_noloc(Op::Jump, end);
                self.use_label(cleanup);
                self.emit_noloc(Op::PopTop, 0);
                if !cond {
                    self.emit_jump_noloc(Op::Jump, target);
                }
                self.use_label(end);
                return Ok(());
            }
            ExprKind::Constant { value, .. } => {
                if let Some(t) = const_truth(value) {
                    self.set_loc(e.loc);
                    if t == cond {
                        self.emit_jump(Op::Jump, target);
                    } else {
                        self.emit(Op::Nop, 0);
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
                    let op = if is == cond { Op::PopJumpIfNone } else { Op::PopJumpIfNotNone };
                    self.emit_jump(op, target);
                    return Ok(());
                }
                self.compile_compare(e, left, ops, comparators, true)?;
                self.emit_jump(if cond { Op::PopJumpIfTrue } else { Op::PopJumpIfFalse }, target);
                return Ok(());
            }
            _ => {}
        }
        self.compile_expr(e)?;
        self.set_loc(e.loc);
        self.to_bool();
        self.emit_jump(if cond { Op::PopJumpIfTrue } else { Op::PopJumpIfFalse }, target);
        Ok(())
    }

    fn emit_compare(&mut self, op: ast::CmpOp, as_bool: bool) {
        let (o, mut arg) = cmp_arg(op);
        if o == Op::CompareOp && as_bool {
            arg |= 16;
        }
        self.emit(o, arg);
    }

    fn compile_compare(&mut self, e: &'a Expr, left: &'a Expr, ops: &'a [ast::CmpOp], comps: &'a [Expr], as_bool: bool) -> R {
        self.compile_expr(left)?;
        let n = ops.len();
        if n == 1 {
            self.compile_expr(&comps[0])?;
            self.set_loc(e.loc);
            self.emit_compare(ops[0], as_bool);
            return Ok(());
        }
        let cleanup = self.new_label();
        let end = self.new_label();
        for i in 0..n - 1 {
            self.compile_expr(&comps[i])?;
            self.set_loc(e.loc);
            self.emit(Op::Swap, 2);
            self.emit(Op::Copy, 2);
            self.emit_compare(ops[i], as_bool);
            self.emit_jump(Op::JumpIfFalseOrPop, cleanup);
        }
        self.compile_expr(&comps[n - 1])?;
        self.set_loc(e.loc);
        self.emit_compare(ops[n - 1], as_bool);
        self.emit_jump_noloc(Op::Jump, end);
        self.use_label(cleanup);
        self.emit(Op::Swap, 2);
        self.emit(Op::PopTop, 0);
        self.use_label(end);
        Ok(())
    }

    // ------------------------------------------------------------------ collections

    /// Build a list/set/tuple from `elts` (possibly with starred items).
    fn starunpack(&mut self, elts: &'a [Expr], build: Op, add: Op, extend: Op, tuple: bool) -> R {
        let n = elts.len();
        if !has_starred(elts) {
            if tuple {
                if n > 0 && elts.iter().all(is_const) {
                    let v: Vec<Constant> = elts
                        .iter()
                        .map(|x| match &x.node {
                            ExprKind::Constant { value, .. } => conv_const(value),
                            _ => unreachable!(),
                        })
                        .collect();
                    self.load_const(Constant::Tuple(v));
                    return Ok(());
                }
                for x in elts {
                    self.compile_expr(x)?;
                }
                self.emit(Op::BuildTuple, n as u32);
                return Ok(());
            }
            if n >= 3 && elts.iter().all(is_const) {
                let v: Vec<Constant> = elts
                    .iter()
                    .map(|x| match &x.node {
                        ExprKind::Constant { value, .. } => conv_const(value),
                        _ => unreachable!(),
                    })
                    .collect();
                self.emit(build, 0);
                if build == Op::BuildSet {
                    self.load_const(Constant::FrozenSet(v));
                } else {
                    self.load_const(Constant::Tuple(v));
                }
                self.emit(extend, 1);
                return Ok(());
            }
            for x in elts {
                self.compile_expr(x)?;
            }
            self.emit(build, n as u32);
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
            self.emit(Op::CallIntrinsic1, Intrinsic1::ListToTuple as u32);
        }
        Ok(())
    }

    fn compile_dict(&mut self, keys: &'a [Option<Expr>], values: &'a [Expr]) -> R {
        let mut have_dict = false;
        let mut elements = 0u32;
        for (k, v) in keys.iter().zip(values.iter()) {
            match k {
                Some(k) => {
                    self.compile_expr(k)?;
                    self.compile_expr(v)?;
                    elements += 1;
                    if elements == 16 {
                        self.emit(Op::BuildMap, elements);
                        if have_dict {
                            self.emit(Op::DictUpdate, 1);
                        }
                        have_dict = true;
                        elements = 0;
                    }
                }
                None => {
                    if elements > 0 {
                        self.emit(Op::BuildMap, elements);
                        if have_dict {
                            self.emit(Op::DictUpdate, 1);
                        }
                        have_dict = true;
                        elements = 0;
                    }
                    if !have_dict {
                        self.emit(Op::BuildMap, 0);
                        have_dict = true;
                    }
                    self.compile_expr(v)?;
                    self.emit(Op::DictUpdate, 1);
                }
            }
        }
        if elements > 0 || !have_dict {
            self.emit(Op::BuildMap, elements);
            if have_dict {
                self.emit(Op::DictUpdate, 1);
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
            115 => self.emit(Op::ConvertValue, 1),
            114 => self.emit(Op::ConvertValue, 2),
            97 => self.emit(Op::ConvertValue, 3),
            _ => {}
        }
        match spec {
            Some(s) => {
                self.compile_expr(s)?;
                self.emit(Op::FormatWithSpec, 0);
            }
            None => self.emit(Op::FormatSimple, 0),
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
                self.emit(Op::BuildInterpolation, (conv << 2) | has_spec);
            }
        }
        self.set_loc(e.loc);
        self.emit(Op::BuildTuple, interps.len() as u32);
        self.emit(Op::BuildTemplate, 0);
        Ok(())
    }

    // ------------------------------------------------------------------ calls

    fn compile_call(&mut self, e: &'a Expr, func: &'a Expr, args: &'a [Expr], keywords: &'a [Keyword]) -> R {
        self.validate_keywords(keywords)?;
        let simple = !has_starred(args) && keywords.iter().all(|k| k.arg.is_some());
        if simple {
            if let ExprKind::Attribute { value, attr, ctx: ExprContext::Load } = &func.node {
                // method call
                if self.try_super_attr(func, value, attr, true)?.is_none() {
                    self.compile_expr(value)?;
                    self.set_loc(func.loc);
                    let m = self.mangle(attr);
                    self.emit_name(Op::LoadMethod, &m);
                }
                return self.compile_call_args(e, 0, args, keywords);
            }
        }
        self.compile_expr(func)?;
        self.set_loc(e.loc);
        self.emit(Op::PushNull, 0);
        self.compile_call_args(e, 0, args, keywords)
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

    fn compile_call_args(&mut self, e: &'a Expr, pushed: u32, args: &'a [Expr], keywords: &'a [Keyword]) -> R {
        if has_starred(args) || keywords.iter().any(|k| k.arg.is_none()) {
            return self.compile_call_ex(e, pushed, args, keywords, false);
        }
        for a in args {
            self.compile_expr(a)?;
        }
        let nargs = pushed + args.len() as u32;
        if keywords.is_empty() {
            self.set_loc(e.loc);
            self.emit(Op::Call, nargs);
        } else {
            for k in keywords {
                self.compile_expr(&k.value)?;
            }
            let names: Vec<Constant> = keywords.iter().map(|k| str_const(k.arg.as_ref().unwrap())).collect();
            self.set_loc(e.loc);
            self.load_const(Constant::Tuple(names));
            self.emit(Op::CallKw, nargs + keywords.len() as u32);
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
                self.emit(Op::Call, nargs);
            } else {
                for k in keywords {
                    self.compile_expr(&k.value)?;
                }
                let names: Vec<Constant> = keywords.iter().map(|k| str_const(k.arg.as_ref().unwrap())).collect();
                self.u().loc = loc_e;
                self.load_const(Constant::Tuple(names));
                self.emit(Op::CallKw, nargs + keywords.len() as u32);
            }
            return Ok(());
        }
        if generic && !has_starred(args) && keywords.iter().all(|k| k.arg.is_some()) {
            for a in args {
                self.compile_expr(a)?;
            }
            self.u().loc = loc_e;
            self.nameop(".type_params", ExprContext::Load)?;
            self.emit(Op::CallIntrinsic1, Intrinsic1::SubscriptGeneric as u32);
            let nargs = pushed + args.len() as u32 + 1;
            if keywords.is_empty() {
                self.emit(Op::Call, nargs);
            } else {
                for k in keywords {
                    self.compile_expr(&k.value)?;
                }
                let names: Vec<Constant> = keywords.iter().map(|k| str_const(k.arg.as_ref().unwrap())).collect();
                self.u().loc = loc_e;
                self.load_const(Constant::Tuple(names));
                self.emit(Op::CallKw, nargs + keywords.len() as u32);
            }
            return Ok(());
        }
        let dummy: &'a Expr = args.first().unwrap_or_else(|| &keywords[0].value);
        let _ = dummy;
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
                self.emit(Op::BuildList, 0);
                self.emit(Op::Swap, 2);
                self.emit(Op::ListExtend, 1);
                self.emit(Op::CallIntrinsic1, Intrinsic1::ListToTuple as u32);
            } else {
                self.compile_expr(&args[0])?;
                self.emit(Op::BuildTuple, 1);
            }
        } else {
            self.u().loc = l;
            self.emit(Op::BuildList, pushed);
            for a in args {
                match &a.node {
                    ExprKind::Starred { value, .. } => {
                        self.compile_expr(value)?;
                        self.emit(Op::ListExtend, 1);
                    }
                    _ => {
                        self.compile_expr(a)?;
                        self.emit(Op::ListAppend, 1);
                    }
                }
            }
            if generic {
                self.nameop(".type_params", ExprContext::Load)?;
                self.emit(Op::CallIntrinsic1, Intrinsic1::SubscriptGeneric as u32);
                self.emit(Op::ListAppend, 1);
            }
            self.u().loc = l;
            self.emit(Op::CallIntrinsic1, Intrinsic1::ListToTuple as u32);
        }
        // keyword args -> dict
        if keywords.is_empty() {
            self.emit(Op::PushNull, 0);
        } else {
            let mut have_dict = false;
            let mut i = 0;
            while i < keywords.len() {
                if keywords[i].arg.is_none() {
                    if !have_dict {
                        self.emit(Op::BuildMap, 0);
                        have_dict = true;
                    }
                    self.compile_expr(&keywords[i].value)?;
                    self.u().loc = l;
                    self.emit(Op::DictMerge, 1);
                    i += 1;
                } else {
                    let start = i;
                    while i < keywords.len() && keywords[i].arg.is_some() {
                        i += 1;
                    }
                    for k in &keywords[start..i] {
                        self.load_const(str_const(k.arg.as_ref().unwrap()));
                        self.compile_expr(&k.value)?;
                    }
                    self.u().loc = l;
                    self.emit(Op::BuildMap, (i - start) as u32);
                    if have_dict {
                        self.emit(Op::DictMerge, 1);
                    }
                    have_dict = true;
                }
            }
        }
        self.u().loc = l;
        self.emit(Op::CallFunctionEx, 0);
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
        self.emit(Op::LoadSuperAttr, arg);
        Ok(Some(()))
    }

    // ------------------------------------------------------------------ comprehensions

    fn compile_comprehension(&mut self, e: &'a Expr) -> R {
        let (kind, generators, elt, value) = match &e.node {
            ExprKind::GeneratorExp { elt, generators } => (CompKind::Gen, generators, &**elt, None),
            ExprKind::ListComp { elt, generators } => (CompKind::List, generators, &**elt, None),
            ExprKind::SetComp { elt, generators } => (CompKind::Set, generators, &**elt, None),
            ExprKind::DictComp { key: k, value, generators } => (CompKind::Dict, generators, &**k, value.as_deref()),
            _ => unreachable!(),
        };
        let sid = self.st.scope_for(key(e, symtable::TAG_COMP)).expect("comprehension scope");
        let inlined = self.st.scopes[sid].comp_inlined;
        let is_async_gen = self.st.scopes[sid].coroutine;
        let outermost = &generators[0];
        if inlined {
            return self.compile_inlined_comprehension(e, sid, kind, generators, elt, value);
        }
        // separate function
        let name = match kind {
            CompKind::Gen => "<genexpr>",
            CompKind::List => "<listcomp>",
            CompKind::Set => "<setcomp>",
            CompKind::Dict => "<dictcomp>",
            CompKind::None => unreachable!(),
        };
        self.enter_scope(name, UnitKind::Comprehension, sid, e.loc.lineno)?;
        self.u().argcount = 1;
        self.set_loc(e.loc);
        let is_gen_flags = self.ur().flags & (co::GENERATOR | co::COROUTINE | co::ASYNC_GENERATOR) != 0;
        let handler = if is_gen_flags {
            let h = self.new_label();
            self.emit_pseudo(IOp::SetupCleanup, h);
            Some(h)
        } else {
            None
        };
        if kind != CompKind::Gen {
            let build = match kind {
                CompKind::List => Op::BuildList,
                CompKind::Set => Op::BuildSet,
                _ => Op::BuildMap,
            };
            self.emit(build, 0);
        }
        self.emit(Op::LoadFast, 0); // .0
        self.comprehension_generator(e, generators, 0, 0, elt, value, kind, false)?;
        self.set_loc(e.loc);
        if kind == CompKind::Gen {
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
        self.make_closure(code, 0)?;
        self.emit(Op::PushNull, 0);
        self.compile_expr(&outermost.iter)?;
        self.set_loc(e.loc);
        if outermost.is_async != 0 {
            self.emit(Op::GetAiter, 0);
        } else {
            self.emit(Op::GetIter, 0);
        }
        self.emit(Op::Call, 1);
        if is_async_gen && kind != CompKind::Gen {
            self.emit(Op::GetAwaitable, 0);
            self.load_none();
            self.compile_yield_from_loop(true)?;
        }
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
        if outermost.is_async != 0 {
            self.emit(Op::GetAiter, 0);
        } else {
            self.emit(Op::GetIter, 0);
        }
        // isolate names bound in the comprehension
        let mut ctx = CompCtx { scope: sid, slots: HashMap::new() };
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
            self.emit(Op::LoadFastAndClear, slot.slot);
            if slot.is_cell {
                self.emit(Op::MakeCell, slot.slot);
            }
            ctx.slots.insert(name, slot);
            pushed.push(slot);
        }
        let cleanup = self.new_label();
        let end = self.new_label();
        if !pushed.is_empty() {
            self.emit(Op::Swap, pushed.len() as u32 + 1);
            self.emit_pseudo(IOp::SetupFinally, cleanup);
        }
        self.u().comps.push(ctx);
        if kind != CompKind::Gen {
            let build = match kind {
                CompKind::List => Op::BuildList,
                CompKind::Set => Op::BuildSet,
                _ => Op::BuildMap,
            };
            self.emit(build, 0);
            self.emit(Op::Swap, 2);
        }
        let r = self.comprehension_generator(e, generators, 0, 0, elt, value, kind, true);
        self.u().comps.pop();
        r?;
        if !pushed.is_empty() {
            self.pop_block();
            self.emit_jump_noloc(Op::Jump, end);
            self.use_label(cleanup);
            // [saved..., partial result, exc]
            self.emit_noloc(Op::Swap, 2);
            self.emit_noloc(Op::PopTop, 0);
            self.restore_comp_locals(&pushed);
            self.emit_noloc(Op::Reraise, 0);
            self.use_label(end);
            self.restore_comp_locals(&pushed);
        }
        Ok(())
    }

    fn restore_comp_locals(&mut self, pushed: &[CompSlot]) {
        self.emit_noloc(Op::Swap, pushed.len() as u32 + 1);
        for s in pushed.iter().rev() {
            let loc = NO_LOC;
            self.u().ir.emit(Instr { op: IOp::Op(Op::StoreFast), arg: s.slot, target: NO_LABEL, loc, flags: F_MAY_STORE_NULL });
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
        inlined: bool,
    ) -> R {
        let g = &generators[gen_index];
        if g.is_async != 0 {
            self.async_comprehension_generator(e, generators, gen_index, depth, elt, value, kind, inlined)
        } else {
            self.sync_comprehension_generator(e, generators, gen_index, depth, elt, value, kind, inlined)
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
        inlined: bool,
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
        let _ = inlined;
        if gen_index > 0 && single.is_none() {
            self.compile_expr(&g.iter)?;
            self.set_loc(g.iter.loc);
            self.emit(Op::GetIter, 0);
        }
        if let Some(x) = single {
            self.compile_expr(x)?;
        } else {
            depth += 1;
            self.use_label(start);
            self.set_loc(g.iter.loc);
            self.emit_jump(Op::ForIter, anchor);
        }
        self.compile_store(&g.target)?;
        for cond in &g.ifs {
            self.compile_jump_if(cond, false, if_cleanup)?;
        }
        if gen_index + 1 < generators.len() {
            self.comprehension_generator(e, generators, gen_index + 1, depth, elt, value, kind, inlined)?;
        } else {
            self.comprehension_element(e, depth, elt, value, kind)?;
        }
        self.use_label(if_cleanup);
        if single.is_none() {
            self.u().loc = NO_LOC;
            self.emit_jump(Op::Jump, start);
            self.use_label(anchor);
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
        inlined: bool,
    ) -> R {
        let g = &generators[gen_index];
        let start = self.new_label();
        let except = self.new_label();
        let if_cleanup = self.new_label();
        if gen_index > 0 {
            self.compile_expr(&g.iter)?;
            self.set_loc(g.iter.loc);
            self.emit(Op::GetAiter, 0);
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
        self.emit_pseudo(IOp::SetupFinally, except);
        self.emit(Op::GetAnext, 0);
        self.load_none();
        self.compile_yield_from_loop(true)?;
        self.pop_block();
        self.compile_store(&g.target)?;
        for cond in &g.ifs {
            self.compile_jump_if(cond, false, if_cleanup)?;
        }
        if gen_index + 1 < generators.len() {
            self.comprehension_generator(e, generators, gen_index + 1, depth, elt, value, kind, inlined)?;
        } else {
            self.comprehension_element(e, depth, elt, value, kind)?;
        }
        self.use_label(if_cleanup);
        self.u().loc = NO_LOC;
        self.emit_jump(Op::Jump, start);
        self.u().fblocks.pop();
        self.use_label(except);
        self.set_loc(g.iter.loc);
        self.emit(Op::EndAsyncFor, 0);
        Ok(())
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
                        self.emit(Op::GetIter, 0);
                        self.use_label(start);
                        self.emit_jump(Op::ForIter, done);
                        self.emit(Op::CallIntrinsic1, Intrinsic1::AsyncGenWrap as u32);
                        self.emit(Op::YieldValue, 0);
                        self.emit(Op::Resume, 1);
                        self.emit(Op::PopTop, 0);
                        self.emit_jump(Op::Jump, start);
                        self.use_label(done);
                    } else {
                        self.emit(Op::GetYieldFromIter, 0);
                        self.load_none();
                        self.compile_yield_from_loop(false)?;
                        self.emit(Op::PopTop, 0);
                    }
                } else {
                    self.compile_expr(elt)?;
                    self.set_loc(elt.loc);
                    if self.ur().flags & co::ASYNC_GENERATOR != 0 {
                        self.emit(Op::CallIntrinsic1, Intrinsic1::AsyncGenWrap as u32);
                    }
                    self.emit(Op::YieldValue, 0);
                    self.emit(Op::Resume, 1);
                    self.emit(Op::PopTop, 0);
                }
            }
            CompKind::List | CompKind::Set => {
                let (add, ext) = if kind == CompKind::List { (Op::ListAppend, Op::ListExtend) } else { (Op::SetAdd, Op::SetUpdate) };
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
                    self.emit(Op::MapAdd, depth + 1);
                }
                None => {
                    // PEP 798: {**d for d in it}
                    self.compile_expr(elt)?;
                    self.set_loc(elt.loc);
                    self.emit(Op::DictUpdate, depth + 1);
                }
            },
            CompKind::None => unreachable!(),
        }
        Ok(())
    }
}
