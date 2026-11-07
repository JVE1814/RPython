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
        let malformed = match &s.node {
            StmtKind::Match { cases, .. } => cases.is_empty(),
            StmtKind::With(w) | StmtKind::AsyncWith(w) => w.items.is_empty(),
            StmtKind::Try(t) | StmtKind::TryStar(t) => t.handlers.is_empty() && t.finalbody.is_empty(),
            _ => false,
        };
        if malformed {
            return Err(CompileError::other(ErrorKind::ValueError, format!("malformed {} node", s.class_name())));
        }
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
                        self.emit(O::COPY, 1);
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
                    self.emit_jump_noloc(O::JUMP_NO_INTERRUPT, end);
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
                self.emit(O::RAISE_VARARGS, n);
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
                // building the AssertionError belongs to the statement; the raise itself is
                // reported at the asserted expression
                self.set_loc(s.loc);
                self.emit(O::LOAD_COMMON_CONSTANT, common::ASSERTION_ERROR);
                if let Some(m) = msg {
                    // the message travels in the self_or_null slot: AssertionError(msg)
                    self.compile_expr(m)?;
                    self.set_loc(s.loc);
                    self.emit(O::CALL, 0);
                }
                self.set_loc(test.loc);
                self.emit(O::RAISE_VARARGS, 1);
                self.use_label(end);
                Ok(())
            }
            StmtKind::Import { names, is_lazy } => self.compile_import(names, *is_lazy != 0),
            StmtKind::ImportFrom { module, names, level, is_lazy } => {
                self.compile_import_from(s, module.as_deref(), names, *level, *is_lazy != 0)
            }
            StmtKind::Global { .. } | StmtKind::Nonlocal { .. } => {
                self.emit(O::NOP, 0);
                Ok(())
            }
            StmtKind::Expr { value } => {
                if self.interactive && self.units.len() == 1 {
                    self.compile_expr(value)?;
                    self.set_loc(s.loc);
                    self.emit(O::CALL_INTRINSIC_1, I1::PRINT);
                    self.emit(O::POP_TOP, 0);
                    return Ok(());
                }
                if let ExprKind::Constant { .. } = value.node {
                    self.emit(O::NOP, 0);
                    return Ok(());
                }
                self.compile_expr(value)?;
                self.set_loc(value.loc);
                self.emit(O::POP_TOP, 0);
                Ok(())
            }
            StmtKind::Pass => {
                self.emit(O::NOP, 0);
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
            self.emit(O::NOP, 0);
        }
        let mut loc = loc_of(s.loc);
        if value.is_none() || value.unwrap().loc.lineno != s.loc.lineno {
            self.emit_loc(O::NOP, 0, loc);
        }
        self.unwind_fblock_stack(&mut loc, preserve_tos, false)?;
        match value {
            None => {
                let i = self.u().add_const(Constant::None);
                self.emit_loc(O::LOAD_CONST, i, loc);
            }
            Some(v) if !preserve_tos => {
                if let ExprKind::Constant { value: c, .. } = &v.node {
                    let i = self.u().add_const(conv_const(c));
                    self.emit_loc(O::LOAD_CONST, i, loc);
                }
            }
            _ => {}
        }
        self.emit_loc(O::RETURN_VALUE, 0, loc);
        Ok(())
    }

    fn compile_break(&mut self, l: Loc) -> R {
        let mut loc = loc_of(l);
        self.emit_loc(O::NOP, 0, loc);
        let lp = self.unwind_fblock_stack(&mut loc, false, true)?;
        match lp {
            None => Err(self.error("'break' outside loop", l)),
            Some(fb) => {
                self.unwind_fblock(&fb, &mut loc, false)?;
                self.u().ir.emit(Instr::jump(O::JUMP, fb.exit, loc));
                Ok(())
            }
        }
    }

    fn compile_continue(&mut self, l: Loc) -> R {
        let mut loc = loc_of(l);
        self.emit_loc(O::NOP, 0, loc);
        let lp = self.unwind_fblock_stack(&mut loc, false, true)?;
        match lp {
            None => Err(self.error("'continue' not properly in loop", l)),
            Some(fb) => {
                self.u().ir.emit(Instr::jump(O::JUMP, fb.block, loc));
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
            let l = super::ast_loc(*loc);
            return Err(self.error("'break', 'continue' and 'return' cannot appear in an except* block", l));
        }
        if want_loop && matches!(top.kind, FbKind::WhileLoop | FbKind::ForLoop | FbKind::AsyncForLoop) {
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
                // the iterator occupies two stack slots
                if preserve_tos {
                    self.emit_loc(O::SWAP, 3, l);
                }
                self.emit_loc(O::POP_TOP, 0, l);
                self.emit_loc(O::POP_TOP, 0, l);
            }
            FbKind::AsyncForLoop => {
                if preserve_tos {
                    self.emit_loc(O::SWAP, 2, l);
                }
                self.emit_loc(O::POP_TOP, 0, l);
            }
            FbKind::TryExcept => {
                self.u().ir.emit(Instr::new(O::POP_BLOCK, 0, l));
            }
            FbKind::FinallyTry => {
                self.u().ir.emit(Instr::new(O::POP_BLOCK, 0, l));
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
                    self.emit_loc(O::SWAP, 2, l);
                }
                self.emit_loc(O::POP_TOP, 0, l);
                if preserve_tos {
                    self.emit_loc(O::SWAP, 2, l);
                }
                self.u().ir.emit(Instr::new(O::POP_BLOCK, 0, l));
                self.emit_loc(O::POP_EXCEPT, 0, l);
            }
            FbKind::With | FbKind::AsyncWith => {
                let wl = fb.loc;
                self.u().ir.emit(Instr::new(O::POP_BLOCK, 0, wl));
                if preserve_tos {
                    self.emit_loc(O::SWAP, 3, wl);
                    self.emit_loc(O::SWAP, 2, wl);
                }
                self.u().loc = wl;
                self.call_exit_with_nones();
                if fb.kind == FbKind::AsyncWith {
                    self.emit(O::GET_AWAITABLE, 2);
                    self.emit_await()?;
                }
                self.emit(O::POP_TOP, 0);
                *loc = NO_LOC;
            }
            FbKind::HandlerCleanup => {
                if fb.name.is_some() {
                    self.u().ir.emit(Instr::new(O::POP_BLOCK, 0, l));
                }
                if preserve_tos {
                    self.emit_loc(O::SWAP, 2, l);
                }
                self.u().ir.emit(Instr::new(O::POP_BLOCK, 0, l));
                self.emit_loc(O::POP_EXCEPT, 0, l);
                if let Some(n) = fb.name {
                    self.u().loc = l;
                    self.load_none();
                    self.nameop(n, ExprContext::Store)?;
                    self.nameop(n, ExprContext::Del)?;
                }
            }
            FbKind::PopValue => {
                if preserve_tos {
                    self.emit_loc(O::SWAP, 2, l);
                }
                self.emit_loc(O::POP_TOP, 0, l);
            }
        }
        Ok(())
    }

    pub fn call_exit_with_nones(&mut self) {
        self.load_none();
        self.load_none();
        self.load_none();
        self.emit(O::CALL, 3);
    }

    fn guard_block(&mut self) -> R {
        let at = self.ur().loc;
        self.check_block_limit(21, super::ast_loc(at))
    }

    fn push_fblock(&mut self, kind: FbKind, block: Label, exit: Label, loc: Location) -> R {
        self.guard_block()?;
        self.u().fblocks.push(FBlock { kind, block, exit, loc, finally: None, name: None });
        Ok(())
    }

    // ------------------------------------------------------------------ loops

    fn compile_for(&mut self, s: &'a Stmt, f: &'a ast::For) -> R {
        let start = self.new_label();
        let cleanup = self.new_label();
        let end = self.new_label();
        self.compile_expr(&f.iter)?;
        self.set_loc(f.iter.loc);
        // [iterable] -> [iterator, NULL-or-index]
        self.emit(O::GET_ITER, 0);
        self.use_label(start);
        self.push_fblock(FbKind::ForLoop, start, end, loc_of(s.loc))?;
        self.set_loc(f.iter.loc);
        self.emit_jump(O::FOR_ITER, cleanup);
        // keeps a line event for the target of a multi-line `for` header
        self.set_loc(f.target.loc);
        self.emit(O::NOP, 0);
        self.compile_store(&f.target)?;
        self.compile_stmts(&f.body)?;
        self.u().loc = NO_LOC;
        self.emit_jump(O::JUMP, start);
        self.use_label(cleanup);
        self.emit_noloc(O::END_FOR, 0);
        self.emit_noloc(O::POP_ITER, 0);
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
        self.emit(O::GET_AITER, 0);
        self.use_label(start);
        self.push_fblock(FbKind::AsyncForLoop, start, end, loc_of(s.loc))?;
        self.emit_setup(O::SETUP_FINALLY, except);
        self.emit(O::GET_ANEXT, 0);
        self.emit(O::PUSH_NULL, 0);
        self.load_none();
        let send = self.compile_yield_from_loop(true)?;
        self.pop_block();
        self.compile_store(&f.target)?;
        self.compile_stmts(&f.body)?;
        self.u().loc = NO_LOC;
        self.emit_jump(O::JUMP, start);
        self.u().fblocks.pop();
        self.use_label(except);
        self.set_loc(f.iter.loc);
        // END_ASYNC_FOR refers back to the SEND whose StopAsyncIteration it handles
        self.emit_jump(O::END_ASYNC_FOR, send);
        self.compile_stmts(&f.orelse)?;
        self.use_label(end);
        Ok(())
    }

    fn compile_while(&mut self, s: &'a Stmt, test: &'a Expr, body: &'a [Stmt], orelse: &'a [Stmt]) -> R {
        let lp = self.new_label();
        let anchor = self.new_label();
        let end = self.new_label();
        self.use_label(lp);
        self.push_fblock(FbKind::WhileLoop, lp, end, loc_of(s.loc))?;
        self.set_loc(s.loc);
        self.emit(O::NOP, 0);
        self.compile_jump_if(test, false, anchor)?;
        self.compile_stmts(body)?;
        self.u().loc = NO_LOC;
        self.emit_jump(O::JUMP, lp);
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
        self.emit(O::NOP, 0);
        self.emit_setup(O::SETUP_FINALLY, end);
        self.use_label(body);
        self.guard_block()?;
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
        self.emit_jump_noloc(O::JUMP_NO_INTERRUPT, exit);
        // exceptional path
        self.use_label(end);
        self.u().loc = NO_LOC;
        self.emit_setup(O::SETUP_CLEANUP, cleanup);
        self.emit_noloc(O::PUSH_EXC_INFO, 0);
        self.push_fblock(FbKind::FinallyEnd, end, NO_LABEL, NO_LOC)?;
        self.compile_stmts(&t.finalbody)?;
        self.u().fblocks.pop();
        self.emit_noloc(O::RERAISE, 0);
        self.use_label(cleanup);
        self.pop_except_and_reraise();
        self.use_label(exit);
        Ok(())
    }

    pub fn pop_except_and_reraise(&mut self) {
        self.emit_noloc(O::COPY, 3);
        self.emit_noloc(O::POP_EXCEPT, 0);
        self.emit_noloc(O::RERAISE, 1);
    }

    fn compile_try_except(&mut self, s: &'a Stmt, t: &'a ast::Try) -> R {
        let body = self.new_label();
        let except = self.new_label();
        let end = self.new_label();
        let cleanup = self.new_label();
        self.set_loc(s.loc);
        self.emit(O::NOP, 0);
        self.emit_setup(O::SETUP_FINALLY, except);
        self.use_label(body);
        self.push_fblock(FbKind::TryExcept, body, NO_LABEL, loc_of(s.loc))?;
        self.compile_stmts(&t.body)?;
        self.u().fblocks.pop();
        self.pop_block();
        self.compile_stmts(&t.orelse)?;
        self.emit_jump_noloc(O::JUMP_NO_INTERRUPT, end);
        let n = t.handlers.len();
        self.use_label(except);
        self.u().loc = NO_LOC;
        self.emit_setup(O::SETUP_CLEANUP, cleanup);
        self.emit_noloc(O::PUSH_EXC_INFO, 0);
        self.push_fblock(FbKind::ExceptionHandler, NO_LABEL, NO_LABEL, NO_LOC)?;
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
                self.emit(O::CHECK_EXC_MATCH, 0);
                self.emit_jump(O::POP_JUMP_IF_FALSE, next);
            }
            if let Some(name) = &h.name {
                let cleanup_end = self.new_label();
                let cleanup_body = self.new_label();
                self.nameop(name, ExprContext::Store)?;
                self.emit_setup(O::SETUP_CLEANUP, cleanup_end);
                self.use_label(cleanup_body);
                self.guard_block()?;
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
                self.emit_noloc(O::POP_EXCEPT, 0);
                self.load_const_noloc(Constant::None);
                self.nameop(name, ExprContext::Store)?;
                self.nameop(name, ExprContext::Del)?;
                self.emit_jump_noloc(O::JUMP_NO_INTERRUPT, end);
                self.use_label(cleanup_end);
                self.u().loc = NO_LOC;
                self.load_const_noloc(Constant::None);
                self.nameop(name, ExprContext::Store)?;
                self.nameop(name, ExprContext::Del)?;
                self.emit_noloc(O::RERAISE, 1);
            } else {
                let cleanup_body = self.new_label();
                self.emit(O::POP_TOP, 0);
                self.use_label(cleanup_body);
                self.guard_block()?;
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
                self.emit_noloc(O::POP_EXCEPT, 0);
                self.emit_jump_noloc(O::JUMP_NO_INTERRUPT, end);
            }
            self.use_label(next);
        }
        self.u().fblocks.pop();
        self.u().loc = NO_LOC;
        self.emit_noloc(O::RERAISE, 0);
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
        self.emit(O::NOP, 0);
        self.emit_setup(O::SETUP_FINALLY, except);
        self.use_label(body);
        self.push_fblock(FbKind::TryExcept, body, NO_LABEL, loc_of(s.loc))?;
        self.compile_stmts(&t.body)?;
        self.u().fblocks.pop();
        self.pop_block();
        self.emit_jump_noloc(O::JUMP_NO_INTERRUPT, orelse);
        let n = t.handlers.len();
        self.use_label(except);
        self.u().loc = NO_LOC;
        self.emit_setup(O::SETUP_CLEANUP, cleanup);
        self.emit_noloc(O::PUSH_EXC_INFO, 0);
        self.push_fblock(FbKind::ExceptionGroupHandler, NO_LABEL, NO_LABEL, NO_LOC)?;
        for (i, h) in t.handlers.iter().enumerate() {
            let h: &'a ExceptHandler = h;
            self.set_loc(h.loc);
            let next_except = self.new_label();
            let except_with_error = self.new_label();
            let no_match = self.new_label();
            if i == 0 {
                self.emit(O::BUILD_LIST, 0);
                self.emit(O::COPY, 2);
            }
            if let Some(ty) = &h.type_ {
                self.compile_expr(ty)?;
                self.set_loc(h.loc);
                self.emit(O::CHECK_EG_MATCH, 0);
                self.emit(O::COPY, 1);
                self.emit_jump(O::POP_JUMP_IF_NONE, no_match);
            }
            let cleanup_end = self.new_label();
            let cleanup_body = self.new_label();
            if let Some(name) = &h.name {
                self.nameop(name, ExprContext::Store)?;
            } else {
                self.emit(O::POP_TOP, 0);
            }
            self.emit_setup(O::SETUP_CLEANUP, cleanup_end);
            self.use_label(cleanup_body);
            self.guard_block()?;
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
            self.emit_jump_noloc(O::JUMP_NO_INTERRUPT, next_except);
            self.use_label(cleanup_end);
            self.u().loc = NO_LOC;
            if let Some(name) = &h.name {
                self.load_const_noloc(Constant::None);
                self.nameop(name, ExprContext::Store)?;
                self.nameop(name, ExprContext::Del)?;
            }
            self.emit_noloc(O::LIST_APPEND, 3);
            self.emit_noloc(O::POP_TOP, 0);
            self.emit_jump_noloc(O::JUMP_NO_INTERRUPT, except_with_error);
            self.use_label(next_except);
            self.emit_noloc(O::NOP, 0);
            self.emit_jump_noloc(O::JUMP_NO_INTERRUPT, except_with_error);
            self.use_label(no_match);
            self.set_loc(h.loc);
            self.emit(O::POP_TOP, 0);
            self.use_label(except_with_error);
            if i == n - 1 {
                self.emit_noloc(O::LIST_APPEND, 1);
                self.emit_jump_noloc(O::JUMP_NO_INTERRUPT, reraise_star);
            }
        }
        self.u().fblocks.pop();
        let reraise = self.new_label();
        self.use_label(reraise_star);
        self.u().loc = NO_LOC;
        self.emit_noloc(O::CALL_INTRINSIC_2, I2::PREP_RERAISE_STAR);
        self.emit_noloc(O::COPY, 1);
        self.emit_jump_noloc(O::POP_JUMP_IF_NOT_NONE, reraise);
        self.emit_noloc(O::POP_TOP, 0);
        self.pop_block();
        self.emit_noloc(O::POP_EXCEPT, 0);
        self.emit_jump_noloc(O::JUMP_NO_INTERRUPT, end);
        self.use_label(reraise);
        self.pop_block();
        self.emit_noloc(O::SWAP, 2);
        self.emit_noloc(O::POP_EXCEPT, 0);
        self.emit_noloc(O::RERAISE, 0);
        self.use_label(cleanup);
        self.pop_except_and_reraise();
        self.use_label(orelse);
        self.compile_stmts(&t.orelse)?;
        self.use_label(end);
        Ok(())
    }

    // ------------------------------------------------------------------ with

    /// `[mgr] -> [exit, self_or_null, enter_result]` via LOAD_SPECIAL (sync or async names).
    fn emit_with_enter(&mut self, is_async: bool) {
        let (exit, enter) = if is_async {
            (wordcode::special::AEXIT, wordcode::special::AENTER)
        } else {
            (wordcode::special::EXIT, wordcode::special::ENTER)
        };
        self.emit(O::COPY, 1);
        self.emit(O::LOAD_SPECIAL, exit);
        self.emit(O::SWAP, 2);
        self.emit(O::SWAP, 3);
        self.emit(O::LOAD_SPECIAL, enter);
        self.emit(O::CALL, 0);
    }

    fn compile_with(&mut self, s: &'a Stmt, w: &'a ast::With, pos: usize) -> R {
        let item = &w.items[pos];
        let block = self.new_label();
        let final_ = self.new_label();
        let exit = self.new_label();
        let cleanup = self.new_label();
        self.compile_expr(&item.context_expr)?;
        self.u().loc = loc_of(item.context_expr.loc);
        self.emit_with_enter(false);
        self.emit_setup(O::SETUP_WITH, final_);
        self.use_label(block);
        self.push_fblock(FbKind::With, block, final_, loc_of(item.context_expr.loc))?;
        match &item.optional_vars {
            Some(v) => self.compile_store(v)?,
            None => self.emit(O::POP_TOP, 0),
        }
        if pos + 1 == w.items.len() {
            self.compile_stmts(&w.body)?;
        } else {
            self.compile_with(s, w, pos + 1)?;
        }
        self.u().fblocks.pop();
        self.pop_block();
        // normal exit: __exit__(None, None, None)
        self.u().loc = loc_of(item.context_expr.loc);
        self.call_exit_with_nones();
        self.emit(O::POP_TOP, 0);
        self.emit_jump(O::JUMP_NO_INTERRUPT, exit);
        // exceptional exit
        self.use_label(final_);
        self.u().loc = loc_of(item.context_expr.loc);
        self.emit_setup(O::SETUP_CLEANUP, cleanup);
        self.emit(O::PUSH_EXC_INFO, 0);
        self.emit(O::WITH_EXCEPT_START, 0);
        self.with_except_finish(cleanup);
        self.use_label(exit);
        Ok(())
    }

    /// After WITH_EXCEPT_START: [exit, sn, lasti, prev, exc, res] -> suppress or reraise.
    fn with_except_finish(&mut self, cleanup: Label) {
        let suppress = self.new_label();
        let exit = self.new_label();
        self.emit(O::TO_BOOL, 0);
        self.emit_jump(O::POP_JUMP_IF_TRUE, suppress);
        // re-raise exc, restoring the original lasti
        self.emit(O::RERAISE, 2);
        self.use_label(suppress);
        self.emit(O::POP_TOP, 0);
        self.pop_block();
        self.emit(O::POP_EXCEPT, 0);
        self.emit(O::POP_TOP, 0);
        self.emit(O::POP_TOP, 0);
        self.emit(O::POP_TOP, 0);
        self.emit_jump_noloc(O::JUMP_NO_INTERRUPT, exit);
        self.use_label(cleanup);
        self.pop_except_and_reraise();
        self.use_label(exit);
    }

    /// `[awaitable] -> [result]`: PUSH_NULL; LOAD_CONST None; the SEND loop.
    pub fn emit_await(&mut self) -> R {
        self.emit(O::PUSH_NULL, 0);
        self.load_none();
        self.compile_yield_from_loop(true)?;
        Ok(())
    }

    fn compile_async_with(&mut self, s: &'a Stmt, w: &'a ast::With, pos: usize) -> R {
        let item = &w.items[pos];
        let block = self.new_label();
        let final_ = self.new_label();
        let exit = self.new_label();
        let cleanup = self.new_label();
        self.compile_expr(&item.context_expr)?;
        self.u().loc = loc_of(item.context_expr.loc);
        self.emit_with_enter(true);
        self.emit(O::GET_AWAITABLE, 1);
        self.emit_await()?;
        self.emit_setup(O::SETUP_WITH, final_);
        self.use_label(block);
        self.push_fblock(FbKind::AsyncWith, block, final_, loc_of(item.context_expr.loc))?;
        match &item.optional_vars {
            Some(v) => self.compile_store(v)?,
            None => self.emit(O::POP_TOP, 0),
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
        self.emit(O::GET_AWAITABLE, 2);
        self.emit_await()?;
        self.emit(O::POP_TOP, 0);
        self.emit_jump(O::JUMP_NO_INTERRUPT, exit);
        self.use_label(final_);
        self.u().loc = loc_of(item.context_expr.loc);
        self.emit_setup(O::SETUP_CLEANUP, cleanup);
        self.emit(O::PUSH_EXC_INFO, 0);
        self.emit(O::WITH_EXCEPT_START, 0);
        self.emit(O::GET_AWAITABLE, 2);
        self.emit_await()?;
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
            let mode = self.import_mode(lazy);
            self.emit(O::IMPORT_NAME, (i << 2) | mode);
            match &a.asname {
                Some(asname) => {
                    if let Some(dot) = a.name.find('.') {
                        let rest = &a.name[dot + 1..];
                        let parts: Vec<&str> = rest.split('.').collect();
                        for p in parts.iter() {
                            self.emit_name(O::IMPORT_FROM, p);
                            self.emit(O::SWAP, 2);
                            self.emit(O::POP_TOP, 0);
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

    /// IMPORT_NAME mode (oparg & 3): 1 = `lazy` keyword; 0 = may become lazy implicitly
    /// (module level, outside try/except/finally; decided at run time from the lazy-imports
    /// mode and `__lazy_modules__`); 2 = always eager.
    fn import_mode(&self, lazy: bool) -> u32 {
        let u = self.ur();
        let eligible = u.kind == super::UnitKind::Module
            && !u.fblocks.iter().any(|b| {
                matches!(
                    b.kind,
                    super::FbKind::TryExcept
                        | super::FbKind::FinallyTry
                        | super::FbKind::FinallyEnd
                        | super::FbKind::HandlerCleanup
                        | super::FbKind::ExceptionHandler
                        | super::FbKind::ExceptionGroupHandler
                )
            });
        if lazy {
            1
        } else if eligible {
            0
        } else {
            2
        }
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
        let star = names.len() == 1 && names[0].name == "*";
        let mode = if star { if lazy { 1 } else { 2 } } else { self.import_mode(lazy) };
        self.emit(O::IMPORT_NAME, (i << 2) | mode);
        if star {
            self.emit(O::CALL_INTRINSIC_1, I1::IMPORT_STAR);
            self.emit(O::POP_TOP, 0);
            return Ok(());
        }
        for a in names {
            self.set_loc(a.loc);
            self.emit_name(O::IMPORT_FROM, &a.name);
            let store = a.asname.as_deref().unwrap_or(&a.name);
            self.store_import_name(store, a.loc)?;
        }
        self.set_loc(s.loc);
        self.emit(O::POP_TOP, 0);
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
                self.set_loc(super::expr::attr_loc(t.loc, t, attr));
                let m = self.mangle(attr);
                self.emit_name(O::STORE_ATTR, &m);
                Ok(())
            }
            ExprKind::Subscript { value, slice, .. } => {
                self.compile_expr(value)?;
                if super::expr::is_two_element_slice(slice) {
                    self.compile_slice_bounds(slice)?;
                    self.set_loc(t.loc);
                    self.emit(O::STORE_SLICE, 0);
                } else {
                    self.compile_expr(slice)?;
                    self.set_loc(t.loc);
                    self.emit(O::STORE_SUBSCR, 0);
                }
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
                self.emit(O::UNPACK_EX, (before | (after << 8)) as u32);
            }
            None => self.emit(O::UNPACK_SEQUENCE, elts.len() as u32),
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
                self.set_loc(super::expr::attr_loc(t.loc, t, attr));
                let m = self.mangle(attr);
                self.emit_name(O::DELETE_ATTR, &m);
                Ok(())
            }
            ExprKind::Subscript { value, slice, .. } => {
                self.compile_expr(value)?;
                self.compile_expr(slice)?;
                self.set_loc(t.loc);
                self.emit(O::DELETE_SUBSCR, 0);
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
                self.emit(O::BINARY_OP, binop as u32);
                self.set_loc(target.loc);
                self.nameop(id, ExprContext::Store)
            }
            ExprKind::Attribute { value: obj, attr, .. } => {
                self.compile_expr(obj)?;
                let aloc = super::expr::attr_loc(target.loc, target, attr);
                self.set_loc(aloc);
                self.emit(O::COPY, 1);
                let m = self.mangle(attr);
                self.emit_load_attr(&m, false);
                self.compile_expr(value)?;
                self.set_loc(s.loc);
                self.emit(O::BINARY_OP, binop as u32);
                self.set_loc(aloc);
                self.emit(O::SWAP, 2);
                self.emit_name(O::STORE_ATTR, &m);
                Ok(())
            }
            ExprKind::Subscript { value: obj, slice, .. } if super::expr::is_two_element_slice(slice) => {
                self.compile_expr(obj)?;
                self.compile_slice_bounds(slice)?;
                self.set_loc(target.loc);
                self.emit(O::COPY, 3);
                self.emit(O::COPY, 3);
                self.emit(O::COPY, 3);
                self.emit(O::BINARY_SLICE, 0);
                self.compile_expr(value)?;
                self.set_loc(s.loc);
                self.emit(O::BINARY_OP, binop as u32);
                self.set_loc(target.loc);
                self.emit(O::SWAP, 4);
                self.emit(O::SWAP, 3);
                self.emit(O::SWAP, 2);
                self.emit(O::STORE_SLICE, 0);
                Ok(())
            }
            ExprKind::Subscript { value: obj, slice, .. } => {
                self.compile_expr(obj)?;
                self.compile_expr(slice)?;
                self.set_loc(target.loc);
                self.emit(O::COPY, 2);
                self.emit(O::COPY, 2);
                self.emit(O::BINARY_OP, wordcode::NB_SUBSCR);
                self.compile_expr(value)?;
                self.set_loc(s.loc);
                self.emit(O::BINARY_OP, binop as u32);
                self.set_loc(target.loc);
                self.emit(O::SWAP, 3);
                self.emit(O::SWAP, 2);
                self.emit(O::STORE_SUBSCR, 0);
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
                        self.emit_name(O::LOAD_NAME, "__annotations__");
                        let m = self.mangle(id);
                        self.load_const(str_const(&m));
                        self.emit(O::STORE_SUBSCR, 0);
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
                                self.emit(O::LOAD_DEREF, slot);
                            } else {
                                self.emit_name(O::LOAD_NAME, "__conditional_annotations__");
                            }
                            self.load_const(Constant::Int(i as i64));
                            self.emit(O::SET_ADD, 1);
                            self.emit(O::POP_TOP, 0);
                        }
                    }
                }
            }
            ExprKind::Attribute { value: obj, attr, .. } => {
                if attr == "__debug__" {
                    return Err(self.error("cannot assign to __debug__", target.loc));
                }
                if value.is_none() {
                    self.compile_expr(obj)?;
                    self.emit(O::POP_TOP, 0);
                }
            }
            ExprKind::Subscript { value: obj, slice, .. } => {
                if value.is_none() {
                    self.compile_expr(obj)?;
                    self.emit(O::POP_TOP, 0);
                    self.check_ann_subscr(slice)?;
                }
            }
            _ => return Err(self.error("invalid node type for annotated assignment", target.loc)),
        }
        if value.is_none() && !matches!(target.node, ExprKind::Name { .. }) {
            // nothing else
        }
        // an annotation without a value is no code in a function (a line marker elsewhere)
        if value.is_none() && !self.ur().function_like {
            self.set_loc(s.loc);
            self.emit(O::NOP, 0);
        }
        Ok(())
    }

    fn check_ann_subscr(&mut self, e: &'a Expr) -> R {
        match &e.node {
            ExprKind::Slice { lower, upper, step } => {
                for x in [lower, upper, step].into_iter().flatten() {
                    self.compile_expr(x)?;
                    self.emit(O::POP_TOP, 0);
                }
            }
            ExprKind::Tuple { elts, .. } => {
                for x in elts {
                    self.check_ann_subscr(x)?;
                }
            }
            _ => {
                self.compile_expr(e)?;
                self.emit(O::POP_TOP, 0);
            }
        }
        Ok(())
    }
}
