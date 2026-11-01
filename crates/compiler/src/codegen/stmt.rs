//! Statement code generation.

use super::*;
use crate::ast::{Alias, ExceptHandler, ExprContext};

impl<'a> Compiler<'a> {
    pub fn compile_stmt(&mut self, s: &'a Stmt) -> R {
        self.enter_depth()?;
        let r = self.compile_stmt_inner(s);
        self.depth -= 1;
        r
    }

    fn compile_stmts(&mut self, body: &'a [Stmt]) -> R {
        for s in body {
            self.compile_stmt(s)?;
        }
        Ok(())
    }

    fn compile_stmt_inner(&mut self, s: &'a Stmt) -> R {
        self.set_loc(s.loc);
        match &s.node {
            StmtKind::FunctionDef(f) => self.compile_function_def(s, f, false),
            StmtKind::AsyncFunctionDef(f) => self.compile_function_def(s, f, true),
            StmtKind::ClassDef { .. } => self.compile_class_def(s),
            StmtKind::Return { value } => self.compile_return(s, value.as_deref()),
            StmtKind::Delete { targets } => {
                for t in targets {
                    self.compile_delete(t)?;
                }
                Ok(())
            }
            StmtKind::Assign { targets, value, .. } => {
                self.compile_expr(value)?;
                let n = targets.len();
                for (i, t) in targets.iter().enumerate() {
                    if i + 1 < n {
                        self.set_loc(s.loc);
                        self.emit(Op::Copy, 1);
                    }
                    self.compile_store(t)?;
                }
                Ok(())
            }
            StmtKind::TypeAlias { name, type_params, value } => self.compile_type_alias(s, name, type_params, value),
            StmtKind::AugAssign { target, op, value } => self.compile_augassign(s, target, *op, value),
            StmtKind::AnnAssign { target, annotation, value, simple } => {
                self.compile_annassign(s, target, annotation, value.as_deref(), *simple != 0)
            }
            StmtKind::For(f) => self.compile_for(s, f),
            StmtKind::AsyncFor(f) => self.compile_async_for(s, f),
            StmtKind::While { test, body, orelse } => self.compile_while(s, test, body, orelse),
            StmtKind::If { test, body, orelse } => {
                let end = self.new_label();
                let next = if orelse.is_empty() { end } else { self.new_label() };
                self.compile_jump_if(test, false, next)?;
                self.compile_stmts(body)?;
                if !orelse.is_empty() {
                    self.emit_jump_noloc(Op::Jump, end);
                    self.use_label(next);
                    self.compile_stmts(orelse)?;
                }
                self.use_label(end);
                Ok(())
            }
            StmtKind::With(w) => self.compile_with(s, w, 0),
            StmtKind::AsyncWith(w) => {
                if !self.in_async_context() {
                    return Err(self.error("'async with' outside async function", s.loc));
                }
                self.compile_async_with(s, w, 0)
            }
            StmtKind::Match { subject, cases } => self.compile_match(s, subject, cases),
            StmtKind::Raise { exc, cause } => {
                let mut n = 0;
                if let Some(e) = exc {
                    self.compile_expr(e)?;
                    n = 1;
                    if let Some(c) = cause {
                        self.compile_expr(c)?;
                        n = 2;
                    }
                }
                self.set_loc(s.loc);
                self.emit(Op::RaiseVarargs, n);
                Ok(())
            }
            StmtKind::Try(t) => {
                if !t.finalbody.is_empty() {
                    self.compile_try_finally(s, t, false)
                } else {
                    self.compile_try_except(s, t)
                }
            }
            StmtKind::TryStar(t) => {
                if !t.finalbody.is_empty() {
                    self.compile_try_finally(s, t, true)
                } else {
                    self.compile_try_star_except(s, t)
                }
            }
            StmtKind::Assert { test, msg } => {
                if self.optimize > 0 {
                    return Ok(());
                }
                let end = self.new_label();
                self.compile_jump_if(test, true, end)?;
                self.set_loc(s.loc);
                self.emit(Op::LoadCommonConstant, CommonConstant::AssertionError as u32);
                if let Some(m) = msg {
                    self.emit(Op::PushNull, 0);
                    self.compile_expr(m)?;
                    self.set_loc(s.loc);
                    self.emit(Op::Call, 1);
                }
                self.emit(Op::RaiseVarargs, 1);
                self.use_label(end);
                Ok(())
            }
            StmtKind::Import { names, is_lazy } => self.compile_import(names, *is_lazy != 0),
            StmtKind::ImportFrom { module, names, level, is_lazy } => {
                self.compile_import_from(s, module.as_deref(), names, *level, *is_lazy != 0)
            }
            StmtKind::Global { .. } | StmtKind::Nonlocal { .. } => {
                self.emit(Op::Nop, 0);
                Ok(())
            }
            StmtKind::Expr { value } => {
                if self.interactive && self.units.len() == 1 {
                    self.compile_expr(value)?;
                    self.set_loc(s.loc);
                    self.emit(Op::CallIntrinsic1, Intrinsic1::Print as u32);
                    self.emit(Op::PopTop, 0);
                    return Ok(());
                }
                if let ExprKind::Constant { .. } = value.node {
                    self.emit(Op::Nop, 0);
                    return Ok(());
                }
                self.compile_expr(value)?;
                self.set_loc(s.loc);
                self.emit(Op::PopTop, 0);
                Ok(())
            }
            StmtKind::Pass => {
                self.emit(Op::Nop, 0);
                Ok(())
            }
            StmtKind::Break => self.compile_break(s.loc),
            StmtKind::Continue => self.compile_continue(s.loc),
        }
    }

    pub fn in_async_context(&self) -> bool {
        let u = self.ur();
        let sc = &self.st.scopes[u.scope];
        if sc.coroutine && u.function_like {
            return true;
        }
        if u.kind == UnitKind::AsyncFunction {
            return true;
        }
        u.kind == UnitKind::Module && self.opts.flags & cf::PYCF_ALLOW_TOP_LEVEL_AWAIT != 0
    }

    // ------------------------------------------------------------------ return/break/continue

    fn compile_return(&mut self, s: &'a Stmt, value: Option<&'a Expr>) -> R {
        let u = self.ur();
        let sc = &self.st.scopes[u.scope];
        if !sc.is_function_like() {
            return Err(self.error("'return' outside function", s.loc));
        }
        if value.is_some() && sc.coroutine && sc.generator {
            return Err(self.error("'return' with value in async generator", s.loc));
        }
        let is_const = matches!(value.map(|v| &v.node), Some(ExprKind::Constant { .. }));
        let preserve_tos = value.is_some() && !is_const;
        if preserve_tos {
            self.compile_expr(value.unwrap())?;
        } else if let Some(v) = value {
            self.set_loc(v.loc);
            self.emit(Op::Nop, 0);
        }
        let mut loc = loc_of(s.loc);
        if value.is_none() || value.unwrap().loc.lineno != s.loc.lineno {
            self.emit_loc(Op::Nop, 0, loc);
        }
        self.unwind_fblock_stack(&mut loc, preserve_tos, false)?;
        match value {
            None => {
                let i = self.u().add_const(Constant::None);
                self.emit_loc(Op::LoadConst, i, loc);
            }
            Some(v) if !preserve_tos => {
                if let ExprKind::Constant { value: c, .. } = &v.node {
                    let i = self.u().add_const(conv_const(c));
                    self.emit_loc(Op::LoadConst, i, loc);
                }
            }
            _ => {}
        }
        self.emit_loc(Op::ReturnValue, 0, loc);
        Ok(())
    }

    fn compile_break(&mut self, l: Loc) -> R {
        let mut loc = loc_of(l);
        self.emit_loc(Op::Nop, 0, loc);
        let lp = self.unwind_fblock_stack(&mut loc, false, true)?;
        match lp {
            None => Err(self.error("'break' outside loop", l)),
            Some(fb) => {
                self.unwind_fblock(&fb, &mut loc, false)?;
                self.u().ir.emit(Instr { op: IOp::Op(Op::Jump), arg: 0, target: fb.exit, loc, flags: 0 });
                Ok(())
            }
        }
    }

    fn compile_continue(&mut self, l: Loc) -> R {
        let mut loc = loc_of(l);
        self.emit_loc(Op::Nop, 0, loc);
        let lp = self.unwind_fblock_stack(&mut loc, false, true)?;
        match lp {
            None => Err(self.error("'continue' not properly in loop", l)),
            Some(fb) => {
                self.u().ir.emit(Instr { op: IOp::Op(Op::Jump), arg: 0, target: fb.block, loc, flags: 0 });
                Ok(())
            }
        }
    }

    /// Unwind frame blocks (emitting cleanup code). If `want_loop`, stops at the innermost
    /// loop and returns it.
    fn unwind_fblock_stack(&mut self, loc: &mut Location, preserve_tos: bool, want_loop: bool) -> R<Option<FBlock<'a>>> {
        let top = match self.ur().fblocks.last() {
            None => return Ok(None),
            Some(t) => t.clone(),
        };
        if top.kind == FbKind::ExceptionGroupHandler {
            let l = ast::Loc::new(loc.lineno as i32, loc.col as i32, loc.end_lineno as i32, loc.end_col as i32);
            return Err(self.error("'break', 'continue' and 'return' cannot appear in an except* block", l));
        }
        if want_loop && matches!(top.kind, FbKind::WhileLoop | FbKind::ForLoop) {
            return Ok(Some(top));
        }
        let copy = self.u().fblocks.pop().unwrap();
        let r = (|| {
            self.unwind_fblock(&copy, loc, preserve_tos)?;
            self.unwind_fblock_stack(loc, preserve_tos, want_loop)
        })();
        self.u().fblocks.push(copy);
        r
    }

    fn unwind_fblock(&mut self, fb: &FBlock<'a>, loc: &mut Location, preserve_tos: bool) -> R {
        let l = *loc;
        match fb.kind {
            FbKind::WhileLoop
            | FbKind::ExceptionHandler
            | FbKind::ExceptionGroupHandler
            | FbKind::AsyncComprehensionGenerator
            | FbKind::StopIteration => {}
            FbKind::ForLoop => {
                if preserve_tos {
                    self.emit_loc(Op::Swap, 2, l);
                }
                self.emit_loc(Op::PopTop, 0, l);
            }
            FbKind::TryExcept => {
                self.u().ir.emit(Instr { op: IOp::PopBlock, arg: 0, target: NO_LABEL, loc: l, flags: 0 });
            }
            FbKind::FinallyTry => {
                self.u().ir.emit(Instr { op: IOp::PopBlock, arg: 0, target: NO_LABEL, loc: l, flags: 0 });
                if preserve_tos {
                    self.u().fblocks.push(FBlock { kind: FbKind::PopValue, block: NO_LABEL, exit: NO_LABEL, loc: l, finally: None, name: None });
                }
                let saved = self.ur().loc;
                self.compile_stmts(fb.finally.unwrap())?;
                self.u().loc = saved;
                if preserve_tos {
                    self.u().fblocks.pop();
                }
                *loc = NO_LOC;
            }
            FbKind::FinallyEnd => {
                if preserve_tos {
                    self.emit_loc(Op::Swap, 2, l);
                }
                self.emit_loc(Op::PopTop, 0, l);
                if preserve_tos {
                    self.emit_loc(Op::Swap, 2, l);
                }
                self.u().ir.emit(Instr { op: IOp::PopBlock, arg: 0, target: NO_LABEL, loc: l, flags: 0 });
                self.emit_loc(Op::PopExcept, 0, l);
            }
            FbKind::With | FbKind::AsyncWith => {
                let wl = fb.loc;
                self.u().ir.emit(Instr { op: IOp::PopBlock, arg: 0, target: NO_LABEL, loc: wl, flags: 0 });
                if preserve_tos {
                    self.emit_loc(Op::Swap, 3, wl);
                    self.emit_loc(Op::Swap, 2, wl);
                }
                self.u().loc = wl;
                self.call_exit_with_nones();
                if fb.kind == FbKind::AsyncWith {
                    self.emit(Op::GetAwaitable, 2);
                    self.load_none();
                    self.compile_yield_from_loop(true)?;
                }
                self.emit(Op::PopTop, 0);
                *loc = NO_LOC;
            }
            FbKind::HandlerCleanup => {
                if fb.name.is_some() {
                    self.u().ir.emit(Instr { op: IOp::PopBlock, arg: 0, target: NO_LABEL, loc: l, flags: 0 });
                }
                if preserve_tos {
                    self.emit_loc(Op::Swap, 2, l);
                }
                self.u().ir.emit(Instr { op: IOp::PopBlock, arg: 0, target: NO_LABEL, loc: l, flags: 0 });
                self.emit_loc(Op::PopExcept, 0, l);
                if let Some(n) = fb.name {
                    self.u().loc = l;
                    self.load_none();
                    self.nameop(n, ExprContext::Store)?;
                    self.nameop(n, ExprContext::Del)?;
                }
            }
            FbKind::PopValue => {
                if preserve_tos {
                    self.emit_loc(Op::Swap, 2, l);
                }
                self.emit_loc(Op::PopTop, 0, l);
            }
        }
        Ok(())
    }

    pub fn call_exit_with_nones(&mut self) {
        self.load_none();
        self.load_none();
        self.load_none();
        self.emit(Op::Call, 3);
    }

    fn push_fblock(&mut self, kind: FbKind, block: Label, exit: Label, loc: Location) {
        self.u().fblocks.push(FBlock { kind, block, exit, loc, finally: None, name: None });
    }

    // ------------------------------------------------------------------ loops

    fn compile_for(&mut self, s: &'a Stmt, f: &'a ast::For) -> R {
        let start = self.new_label();
        let cleanup = self.new_label();
        let end = self.new_label();
        self.compile_expr(&f.iter)?;
        self.set_loc(f.iter.loc);
        self.emit(Op::GetIter, 0);
        self.use_label(start);
        self.push_fblock(FbKind::ForLoop, start, end, loc_of(s.loc));
        self.set_loc(f.iter.loc);
        self.emit_jump(Op::ForIter, cleanup);
        self.compile_store(&f.target)?;
        self.compile_stmts(&f.body)?;
        self.set_loc(f.iter.loc);
        self.u().loc.lineno = s.loc.lineno as u32;
        self.u().loc = NO_LOC;
        self.emit_jump(Op::Jump, start);
        self.use_label(cleanup);
        self.u().fblocks.pop();
        self.compile_stmts(&f.orelse)?;
        self.use_label(end);
        Ok(())
    }

    fn compile_async_for(&mut self, s: &'a Stmt, f: &'a ast::For) -> R {
        if !self.in_async_context() {
            return Err(self.error("'async for' outside async function", s.loc));
        }
        let start = self.new_label();
        let except = self.new_label();
        let end = self.new_label();
        self.compile_expr(&f.iter)?;
        self.set_loc(f.iter.loc);
        self.emit(Op::GetAiter, 0);
        self.use_label(start);
        self.push_fblock(FbKind::ForLoop, start, end, loc_of(s.loc));
        self.emit_pseudo(IOp::SetupFinally, except);
        self.emit(Op::GetAnext, 0);
        self.load_none();
        self.compile_yield_from_loop(true)?;
        self.pop_block();
        self.compile_store(&f.target)?;
        self.compile_stmts(&f.body)?;
        self.u().loc = NO_LOC;
        self.emit_jump(Op::Jump, start);
        self.u().fblocks.pop();
        self.use_label(except);
        self.set_loc(f.iter.loc);
        self.emit(Op::EndAsyncFor, 0);
        self.compile_stmts(&f.orelse)?;
        self.use_label(end);
        Ok(())
    }

    fn compile_while(&mut self, s: &'a Stmt, test: &'a Expr, body: &'a [Stmt], orelse: &'a [Stmt]) -> R {
        let lp = self.new_label();
        let anchor = self.new_label();
        let end = self.new_label();
        self.use_label(lp);
        self.push_fblock(FbKind::WhileLoop, lp, end, loc_of(s.loc));
        self.set_loc(s.loc);
        self.emit(Op::Nop, 0);
        self.compile_jump_if(test, false, anchor)?;
        self.compile_stmts(body)?;
        self.u().loc = NO_LOC;
        self.emit_jump(Op::Jump, lp);
        self.u().fblocks.pop();
        self.use_label(anchor);
        self.compile_stmts(orelse)?;
        self.use_label(end);
        Ok(())
    }

    // ------------------------------------------------------------------ try

    fn compile_try_finally(&mut self, s: &'a Stmt, t: &'a ast::Try, star: bool) -> R {
        let body = self.new_label();
        let end = self.new_label();
        let exit = self.new_label();
        let cleanup = self.new_label();
        self.set_loc(s.loc);
        self.emit(Op::Nop, 0);
        self.emit_pseudo(IOp::SetupFinally, end);
        self.use_label(body);
        self.u().fblocks.push(FBlock {
            kind: FbKind::FinallyTry,
            block: body,
            exit: end,
            loc: loc_of(s.loc),
            finally: Some(&t.finalbody),
            name: None,
        });
        if !t.handlers.is_empty() {
            if star {
                self.compile_try_star_except(s, t)?;
            } else {
                self.compile_try_except(s, t)?;
            }
        } else {
            self.compile_stmts(&t.body)?;
        }
        self.pop_block();
        self.u().fblocks.pop();
        self.compile_stmts(&t.finalbody)?;
        self.emit_jump_noloc(Op::Jump, exit);
        // exceptional path
        self.use_label(end);
        self.u().loc = NO_LOC;
        self.emit_pseudo(IOp::SetupCleanup, cleanup);
        self.emit_noloc(Op::PushExcInfo, 0);
        self.push_fblock(FbKind::FinallyEnd, end, NO_LABEL, NO_LOC);
        self.compile_stmts(&t.finalbody)?;
        self.u().fblocks.pop();
        self.emit_noloc(Op::Reraise, 0);
        self.use_label(cleanup);
        self.pop_except_and_reraise();
        self.use_label(exit);
        Ok(())
    }

    pub fn pop_except_and_reraise(&mut self) {
        self.emit_noloc(Op::Copy, 3);
        self.emit_noloc(Op::PopExcept, 0);
        self.emit_noloc(Op::Reraise, 1);
    }

    fn compile_try_except(&mut self, s: &'a Stmt, t: &'a ast::Try) -> R {
        let body = self.new_label();
        let except = self.new_label();
        let end = self.new_label();
        let cleanup = self.new_label();
        self.set_loc(s.loc);
        self.emit(Op::Nop, 0);
        self.emit_pseudo(IOp::SetupFinally, except);
        self.use_label(body);
        self.push_fblock(FbKind::TryExcept, body, NO_LABEL, loc_of(s.loc));
        self.compile_stmts(&t.body)?;
        self.u().fblocks.pop();
        self.pop_block();
        self.compile_stmts(&t.orelse)?;
        self.emit_jump_noloc(Op::Jump, end);
        let n = t.handlers.len();
        self.use_label(except);
        self.u().loc = NO_LOC;
        self.emit_pseudo(IOp::SetupCleanup, cleanup);
        self.emit_noloc(Op::PushExcInfo, 0);
        self.push_fblock(FbKind::ExceptionHandler, NO_LABEL, NO_LABEL, NO_LOC);
        for (i, h) in t.handlers.iter().enumerate() {
            let h: &'a ExceptHandler = h;
            if h.type_.is_none() && i < n - 1 {
                return Err(self.error("default 'except:' must be last", h.loc));
            }
            self.set_loc(h.loc);
            let next = self.new_label();
            if let Some(ty) = &h.type_ {
                self.compile_expr(ty)?;
                self.set_loc(h.loc);
                self.emit(Op::CheckExcMatch, 0);
                self.emit_jump(Op::PopJumpIfFalse, next);
            }
            if let Some(name) = &h.name {
                let cleanup_end = self.new_label();
                let cleanup_body = self.new_label();
                self.nameop(name, ExprContext::Store)?;
                self.emit_pseudo(IOp::SetupCleanup, cleanup_end);
                self.use_label(cleanup_body);
                self.u().fblocks.push(FBlock {
                    kind: FbKind::HandlerCleanup,
                    block: cleanup_body,
                    exit: NO_LABEL,
                    loc: loc_of(h.loc),
                    finally: None,
                    name: Some(name.as_str()),
                });
                self.compile_stmts(&h.body)?;
                self.u().fblocks.pop();
                self.u().loc = NO_LOC;
                self.pop_block();
                self.pop_block();
                self.emit_noloc(Op::PopExcept, 0);
                self.load_const_noloc(Constant::None);
                self.nameop(name, ExprContext::Store)?;
                self.nameop(name, ExprContext::Del)?;
                self.emit_jump_noloc(Op::Jump, end);
                self.use_label(cleanup_end);
                self.u().loc = NO_LOC;
                self.load_const_noloc(Constant::None);
                self.nameop(name, ExprContext::Store)?;
                self.nameop(name, ExprContext::Del)?;
                self.emit_noloc(Op::Reraise, 1);
            } else {
                let cleanup_body = self.new_label();
                self.emit(Op::PopTop, 0);
                self.use_label(cleanup_body);
                self.u().fblocks.push(FBlock {
                    kind: FbKind::HandlerCleanup,
                    block: cleanup_body,
                    exit: NO_LABEL,
                    loc: loc_of(h.loc),
                    finally: None,
                    name: None,
                });
                self.compile_stmts(&h.body)?;
                self.u().fblocks.pop();
                self.u().loc = NO_LOC;
                self.pop_block();
                self.emit_noloc(Op::PopExcept, 0);
                self.emit_jump_noloc(Op::Jump, end);
            }
            self.use_label(next);
        }
        self.u().fblocks.pop();
        self.u().loc = NO_LOC;
        self.emit_noloc(Op::Reraise, 0);
        self.use_label(cleanup);
        self.pop_except_and_reraise();
        self.use_label(end);
        Ok(())
    }

    fn compile_try_star_except(&mut self, s: &'a Stmt, t: &'a ast::Try) -> R {
        let body = self.new_label();
        let except = self.new_label();
        let orelse = self.new_label();
        let end = self.new_label();
        let cleanup = self.new_label();
        let reraise_star = self.new_label();
        self.set_loc(s.loc);
        self.emit(Op::Nop, 0);
        self.emit_pseudo(IOp::SetupFinally, except);
        self.use_label(body);
        self.push_fblock(FbKind::TryExcept, body, NO_LABEL, loc_of(s.loc));
        self.compile_stmts(&t.body)?;
        self.u().fblocks.pop();
        self.pop_block();
        self.emit_jump_noloc(Op::Jump, orelse);
        let n = t.handlers.len();
        self.use_label(except);
        self.u().loc = NO_LOC;
        self.emit_pseudo(IOp::SetupCleanup, cleanup);
        self.emit_noloc(Op::PushExcInfo, 0);
        self.push_fblock(FbKind::ExceptionGroupHandler, NO_LABEL, NO_LABEL, NO_LOC);
        for (i, h) in t.handlers.iter().enumerate() {
            let h: &'a ExceptHandler = h;
            self.set_loc(h.loc);
            let next_except = self.new_label();
            let except_with_error = self.new_label();
            let no_match = self.new_label();
            if i == 0 {
                self.emit(Op::BuildList, 0);
                self.emit(Op::Copy, 2);
            }
            if let Some(ty) = &h.type_ {
                self.compile_expr(ty)?;
                self.set_loc(h.loc);
                self.emit(Op::CheckEgMatch, 0);
                self.emit(Op::Copy, 1);
                self.emit_jump(Op::PopJumpIfNone, no_match);
            }
            let cleanup_end = self.new_label();
            let cleanup_body = self.new_label();
            if let Some(name) = &h.name {
                self.nameop(name, ExprContext::Store)?;
            } else {
                self.emit(Op::PopTop, 0);
            }
            self.emit_pseudo(IOp::SetupCleanup, cleanup_end);
            self.use_label(cleanup_body);
            self.u().fblocks.push(FBlock {
                kind: FbKind::HandlerCleanup,
                block: cleanup_body,
                exit: NO_LABEL,
                loc: loc_of(h.loc),
                finally: None,
                name: h.name.as_deref(),
            });
            self.compile_stmts(&h.body)?;
            self.u().fblocks.pop();
            self.u().loc = NO_LOC;
            self.pop_block();
            if let Some(name) = &h.name {
                self.load_const_noloc(Constant::None);
                self.nameop(name, ExprContext::Store)?;
                self.nameop(name, ExprContext::Del)?;
            }
            self.emit_jump_noloc(Op::Jump, next_except);
            self.use_label(cleanup_end);
            self.u().loc = NO_LOC;
            if let Some(name) = &h.name {
                self.load_const_noloc(Constant::None);
                self.nameop(name, ExprContext::Store)?;
                self.nameop(name, ExprContext::Del)?;
            }
            self.emit_noloc(Op::ListAppend, 3);
            self.emit_noloc(Op::PopTop, 0);
            self.emit_jump_noloc(Op::Jump, except_with_error);
            self.use_label(next_except);
            self.emit_noloc(Op::Nop, 0);
            self.emit_jump_noloc(Op::Jump, except_with_error);
            self.use_label(no_match);
            self.set_loc(h.loc);
            self.emit(Op::PopTop, 0);
            self.use_label(except_with_error);
            if i == n - 1 {
                self.emit_noloc(Op::ListAppend, 1);
                self.emit_jump_noloc(Op::Jump, reraise_star);
            }
        }
        self.u().fblocks.pop();
        let reraise = self.new_label();
        self.use_label(reraise_star);
        self.u().loc = NO_LOC;
        self.emit_noloc(Op::CallIntrinsic2, Intrinsic2::PrepReraiseStar as u32);
        self.emit_noloc(Op::Copy, 1);
        self.emit_jump_noloc(Op::PopJumpIfNotNone, reraise);
        self.emit_noloc(Op::PopTop, 0);
        self.pop_block();
        self.emit_noloc(Op::PopExcept, 0);
        self.emit_jump_noloc(Op::Jump, end);
        self.use_label(reraise);
        self.pop_block();
        self.emit_noloc(Op::Swap, 2);
        self.emit_noloc(Op::PopExcept, 0);
        self.emit_noloc(Op::Reraise, 0);
        self.use_label(cleanup);
        self.pop_except_and_reraise();
        self.use_label(orelse);
        self.compile_stmts(&t.orelse)?;
        self.use_label(end);
        Ok(())
    }

    // ------------------------------------------------------------------ with

    fn compile_with(&mut self, s: &'a Stmt, w: &'a ast::With, pos: usize) -> R {
        let item = &w.items[pos];
        let wloc = loc_of(s.loc);
        let block = self.new_label();
        let final_ = self.new_label();
        let exit = self.new_label();
        let cleanup = self.new_label();
        self.compile_expr(&item.context_expr)?;
        self.u().loc = loc_of(item.context_expr.loc);
        self.emit(Op::BeforeWith, 0);
        self.emit_pseudo(IOp::SetupWith, final_);
        self.use_label(block);
        self.push_fblock(FbKind::With, block, final_, loc_of(item.context_expr.loc));
        match &item.optional_vars {
            Some(v) => self.compile_store(v)?,
            None => self.emit(Op::PopTop, 0),
        }
        if pos + 1 == w.items.len() {
            self.compile_stmts(&w.body)?;
        } else {
            self.compile_with(s, w, pos + 1)?;
        }
        self.u().fblocks.pop();
        self.pop_block();
        // normal exit
        self.u().loc = loc_of(item.context_expr.loc);
        let _ = wloc;
        self.call_exit_with_nones();
        self.emit(Op::PopTop, 0);
        self.emit_jump(Op::Jump, exit);
        // exceptional exit
        self.use_label(final_);
        self.u().loc = loc_of(item.context_expr.loc);
        self.emit_pseudo(IOp::SetupCleanup, cleanup);
        self.emit(Op::PushExcInfo, 0);
        self.emit(Op::WithExceptStart, 0);
        self.with_except_finish(cleanup);
        self.use_label(exit);
        Ok(())
    }

    /// After WithExceptStart: [exit, sn, lasti, prev, exc, res] -> suppress or reraise.
    fn with_except_finish(&mut self, cleanup: Label) {
        let suppress = self.new_label();
        let exit = self.new_label();
        self.emit(Op::ToBool, 0);
        self.emit_jump(Op::PopJumpIfTrue, suppress);
        // reraise exc with the original lasti
        self.emit_noloc(Op::Copy, 3);
        self.emit_noloc(Op::Swap, 2);
        self.emit_noloc(Op::Reraise, 1);
        self.use_label(suppress);
        self.emit(Op::PopTop, 0);
        self.pop_block();
        self.emit(Op::PopExcept, 0);
        self.emit(Op::PopTop, 0);
        self.emit(Op::PopTop, 0);
        self.emit(Op::PopTop, 0);
        self.emit_jump_noloc(Op::Jump, exit);
        self.use_label(cleanup);
        self.pop_except_and_reraise();
        self.use_label(exit);
    }

    fn compile_async_with(&mut self, s: &'a Stmt, w: &'a ast::With, pos: usize) -> R {
        let item = &w.items[pos];
        let block = self.new_label();
        let final_ = self.new_label();
        let exit = self.new_label();
        let cleanup = self.new_label();
        self.compile_expr(&item.context_expr)?;
        self.u().loc = loc_of(item.context_expr.loc);
        self.emit(Op::BeforeAsyncWith, 0);
        self.emit(Op::GetAwaitable, 1);
        self.load_none();
        self.compile_yield_from_loop(true)?;
        self.emit_pseudo(IOp::SetupWith, final_);
        self.use_label(block);
        self.push_fblock(FbKind::AsyncWith, block, final_, loc_of(item.context_expr.loc));
        match &item.optional_vars {
            Some(v) => self.compile_store(v)?,
            None => self.emit(Op::PopTop, 0),
        }
        if pos + 1 == w.items.len() {
            self.compile_stmts(&w.body)?;
        } else {
            self.compile_async_with(s, w, pos + 1)?;
        }
        self.u().fblocks.pop();
        self.pop_block();
        self.u().loc = loc_of(item.context_expr.loc);
        self.call_exit_with_nones();
        self.emit(Op::GetAwaitable, 2);
        self.load_none();
        self.compile_yield_from_loop(true)?;
        self.emit(Op::PopTop, 0);
        self.emit_jump(Op::Jump, exit);
        self.use_label(final_);
        self.u().loc = loc_of(item.context_expr.loc);
        self.emit_pseudo(IOp::SetupCleanup, cleanup);
        self.emit(Op::PushExcInfo, 0);
        self.emit(Op::WithExceptStart, 0);
        self.emit(Op::GetAwaitable, 2);
        self.load_none();
        self.compile_yield_from_loop(true)?;
        self.with_except_finish(cleanup);
        self.use_label(exit);
        Ok(())
    }

    // ------------------------------------------------------------------ imports

    fn compile_import(&mut self, names: &'a [Alias], lazy: bool) -> R {
        for a in names {
            self.set_loc(a.loc);
            self.load_const(Constant::Int(0));
            self.load_none();
            let i = self.u().add_name(&a.name);
            self.emit(Op::ImportName, i | if lazy { 1 << 23 } else { 0 });
            match &a.asname {
                Some(asname) => {
                    if let Some(dot) = a.name.find('.') {
                        let rest = &a.name[dot + 1..];
                        let parts: Vec<&str> = rest.split('.').collect();
                        for (k, p) in parts.iter().enumerate() {
                            self.emit_name(Op::ImportFrom, p);
                            if k + 1 < parts.len() || true {
                                self.emit(Op::Swap, 2);
                                self.emit(Op::PopTop, 0);
                            }
                        }
                    }
                    self.store_import_name(asname, a.loc)?;
                }
                None => {
                    let store = match a.name.find('.') {
                        Some(d) => &a.name[..d],
                        None => a.name.as_str(),
                    };
                    self.store_import_name(store, a.loc)?;
                }
            }
        }
        Ok(())
    }

    fn store_import_name(&mut self, name: &str, loc: Loc) -> R {
        if name == "__debug__" {
            return Err(self.error("cannot assign to __debug__", loc));
        }
        self.nameop(name, ExprContext::Store)
    }

    fn compile_import_from(&mut self, s: &'a Stmt, module: Option<&'a str>, names: &'a [Alias], level: i32, lazy: bool) -> R {
        self.set_loc(s.loc);
        self.load_const(Constant::Int(level as i64));
        let fromlist: Vec<Constant> = names.iter().map(|a| str_const(&a.name)).collect();
        self.load_const(Constant::Tuple(fromlist));
        let i = self.u().add_name(module.unwrap_or(""));
        self.emit(Op::ImportName, i | if lazy { 1 << 23 } else { 0 });
        if names.len() == 1 && names[0].name == "*" {
            self.emit(Op::CallIntrinsic1, Intrinsic1::ImportStar as u32);
            self.emit(Op::PopTop, 0);
            return Ok(());
        }
        for a in names {
            self.set_loc(a.loc);
            self.emit_name(Op::ImportFrom, &a.name);
            let store = a.asname.as_deref().unwrap_or(&a.name);
            self.store_import_name(store, a.loc)?;
        }
        self.set_loc(s.loc);
        self.emit(Op::PopTop, 0);
        Ok(())
    }

    // ------------------------------------------------------------------ assignment

    pub fn compile_store(&mut self, t: &'a Expr) -> R {
        self.set_loc(t.loc);
        match &t.node {
            ExprKind::Name { id, .. } => {
                if id == "__debug__" {
                    return Err(self.error("cannot assign to __debug__", t.loc));
                }
                self.nameop(id, ExprContext::Store)
            }
            ExprKind::Attribute { value, attr, .. } => {
                self.compile_expr(value)?;
                self.set_loc(t.loc);
                if attr == "__debug__" {
                    return Err(self.error("cannot assign to __debug__", t.loc));
                }
                self.note_static_attribute(value, attr);
                let m = self.mangle(attr);
                self.emit_name(Op::StoreAttr, &m);
                Ok(())
            }
            ExprKind::Subscript { value, slice, .. } => {
                self.compile_expr(value)?;
                self.compile_expr(slice)?;
                self.set_loc(t.loc);
                self.emit(Op::StoreSubscr, 0);
                Ok(())
            }
            ExprKind::Tuple { elts, .. } | ExprKind::List { elts, .. } => self.compile_unpack_store(t, elts),
            ExprKind::Starred { .. } => Err(self.error("starred assignment target must be in a list or tuple", t.loc)),
            _ => Err(self.error(format!("cannot assign to {}", super::expr::describe(t)), t.loc)),
        }
    }

    fn compile_unpack_store(&mut self, t: &'a Expr, elts: &'a [Expr]) -> R {
        let mut star: Option<usize> = None;
        for (i, e) in elts.iter().enumerate() {
            if let ExprKind::Starred { .. } = e.node {
                if star.is_some() {
                    return Err(self.error("multiple starred expressions in assignment", e.loc));
                }
                star = Some(i);
            }
        }
        self.set_loc(t.loc);
        match star {
            Some(i) => {
                let before = i;
                let after = elts.len() - i - 1;
                if before >= 256 || after >= (1 << 16) {
                    return Err(self.error("too many expressions in star-unpacking assignment", t.loc));
                }
                self.emit(Op::UnpackEx, (before | (after << 8)) as u32);
            }
            None => self.emit(Op::UnpackSequence, elts.len() as u32),
        }
        for e in elts {
            match &e.node {
                ExprKind::Starred { value, .. } => self.compile_store(value)?,
                _ => self.compile_store(e)?,
            }
        }
        Ok(())
    }

    fn compile_delete(&mut self, t: &'a Expr) -> R {
        self.set_loc(t.loc);
        match &t.node {
            ExprKind::Name { id, .. } => {
                if id == "__debug__" {
                    return Err(self.error("cannot delete __debug__", t.loc));
                }
                self.nameop(id, ExprContext::Del)
            }
            ExprKind::Attribute { value, attr, .. } => {
                self.compile_expr(value)?;
                self.set_loc(t.loc);
                let m = self.mangle(attr);
                self.emit_name(Op::DeleteAttr, &m);
                Ok(())
            }
            ExprKind::Subscript { value, slice, .. } => {
                self.compile_expr(value)?;
                self.compile_expr(slice)?;
                self.set_loc(t.loc);
                self.emit(Op::DeleteSubscr, 0);
                Ok(())
            }
            ExprKind::Tuple { elts, .. } | ExprKind::List { elts, .. } => {
                for e in elts {
                    self.compile_delete(e)?;
                }
                Ok(())
            }
            _ => Err(self.error(format!("cannot delete {}", super::expr::describe(t)), t.loc)),
        }
    }

    fn compile_augassign(&mut self, s: &'a Stmt, target: &'a Expr, op: ast::Operator, value: &'a Expr) -> R {
        let binop = super::expr::inplace_binop(op);
        match &target.node {
            ExprKind::Name { id, .. } => {
                if id == "__debug__" {
                    return Err(self.error("cannot assign to __debug__", target.loc));
                }
                self.set_loc(target.loc);
                self.nameop(id, ExprContext::Load)?;
                self.compile_expr(value)?;
                self.set_loc(s.loc);
                self.emit(Op::BinaryOp, binop as u32);
                self.set_loc(target.loc);
                self.nameop(id, ExprContext::Store)
            }
            ExprKind::Attribute { value: obj, attr, .. } => {
                self.compile_expr(obj)?;
                self.set_loc(target.loc);
                self.emit(Op::Copy, 1);
                let m = self.mangle(attr);
                self.emit_name(Op::LoadAttr, &m);
                self.compile_expr(value)?;
                self.set_loc(s.loc);
                self.emit(Op::BinaryOp, binop as u32);
                self.set_loc(target.loc);
                self.emit(Op::Swap, 2);
                self.emit_name(Op::StoreAttr, &m);
                Ok(())
            }
            ExprKind::Subscript { value: obj, slice, .. } => {
                self.compile_expr(obj)?;
                self.compile_expr(slice)?;
                self.set_loc(target.loc);
                self.emit(Op::Copy, 2);
                self.emit(Op::Copy, 2);
                self.emit(Op::BinarySubscr, 0);
                self.compile_expr(value)?;
                self.set_loc(s.loc);
                self.emit(Op::BinaryOp, binop as u32);
                self.set_loc(target.loc);
                self.emit(Op::Swap, 3);
                self.emit(Op::Swap, 2);
                self.emit(Op::StoreSubscr, 0);
                Ok(())
            }
            _ => Err(self.error(format!("'{}' is an illegal expression for augmented assignment", super::expr::describe(target)), target.loc)),
        }
    }

    fn compile_annassign(&mut self, s: &'a Stmt, target: &'a Expr, annotation: &'a Expr, value: Option<&'a Expr>, simple: bool) -> R {
        if let Some(v) = value {
            self.compile_expr(v)?;
            self.compile_store(target)?;
        }
        let sc_kind = self.st.scopes[self.ur().scope].kind;
        let future = self.future & cf::CO_FUTURE_ANNOTATIONS != 0;
        match &target.node {
            ExprKind::Name { id, .. } => {
                if id == "__debug__" {
                    return Err(self.error("cannot assign to __debug__", target.loc));
                }
                if simple && matches!(sc_kind, ScopeKind::Module | ScopeKind::Class) {
                    if future {
                        self.set_loc(s.loc);
                        let txt = crate::unparse::unparse_annotation(annotation);
                        self.load_const(Constant::Str(txt));
                        self.emit_name(Op::LoadName, "__annotations__");
                        let m = self.mangle(id);
                        self.load_const(str_const(&m));
                        self.emit(Op::StoreSubscr, 0);
                    } else {
                        let has_cond = self.st.scopes[self.ur().scope].has_conditional_annotations;
                        let idx = if has_cond {
                            let i = self.ur().next_cond_index;
                            self.u().next_cond_index += 1;
                            Some(i)
                        } else {
                            None
                        };
                        self.u().deferred.push((s, idx));
                        if let Some(i) = idx {
                            self.set_loc(s.loc);
                            if sc_kind == ScopeKind::Class {
                                let slot = self.ur().cell_slot("__conditional_annotations__").unwrap();
                                self.emit(Op::LoadDeref, slot);
                            } else {
                                self.emit_name(Op::LoadName, "__conditional_annotations__");
                            }
                            self.load_const(Constant::Int(i as i64));
                            self.emit(Op::SetAdd, 1);
                            self.emit(Op::PopTop, 0);
                        }
                    }
                }
            }
            ExprKind::Attribute { value: obj, .. } => {
                if value.is_none() {
                    self.compile_expr(obj)?;
                    self.emit(Op::PopTop, 0);
                }
            }
            ExprKind::Subscript { value: obj, slice, .. } => {
                if value.is_none() {
                    self.compile_expr(obj)?;
                    self.emit(Op::PopTop, 0);
                    self.check_ann_subscr(slice)?;
                }
            }
            _ => return Err(self.error("invalid node type for annotated assignment", target.loc)),
        }
        if value.is_none() && !matches!(target.node, ExprKind::Name { .. }) {
            // nothing else
        }
        if value.is_none() {
            self.set_loc(s.loc);
            self.emit(Op::Nop, 0);
        }
        Ok(())
    }

    fn check_ann_subscr(&mut self, e: &'a Expr) -> R {
        match &e.node {
            ExprKind::Slice { lower, upper, step } => {
                for x in [lower, upper, step].into_iter().flatten() {
                    self.compile_expr(x)?;
                    self.emit(Op::PopTop, 0);
                }
            }
            ExprKind::Tuple { elts, .. } => {
                for x in elts {
                    self.check_ann_subscr(x)?;
                }
            }
            _ => {
                self.compile_expr(e)?;
                self.emit(Op::PopTop, 0);
            }
        }
        Ok(())
    }
}
