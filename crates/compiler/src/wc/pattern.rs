//! `match` statement code generation (same algorithm as CPython: captured values are kept on
//! the stack beneath the items still needed, and stored only after the whole pattern matched).

use super::*;
use crate::ast::{MatchCase, Pattern, PatternKind};

pub(crate) struct PatCtx {
    stores: Vec<String>,
    allow_irrefutable: bool,
    fail_pop: Vec<Label>,
    on_top: u32,
}

fn is_wildcard(p: &Pattern) -> bool {
    matches!(&p.node, PatternKind::MatchAs { pattern: None, name: None })
}

fn is_star_wildcard(p: &Pattern) -> bool {
    matches!(&p.node, PatternKind::MatchStar { name: None })
}

impl<'a> Compiler<'a> {
    pub fn compile_match(&mut self, s: &'a Stmt, subject: &'a Expr, cases: &'a [MatchCase]) -> R {
        self.compile_expr(subject)?;
        let end = self.new_label();
        let ncases = cases.len();
        let last = &cases[ncases - 1];
        let has_default = is_wildcard(&last.pattern) && ncases > 1;
        let n = ncases - has_default as usize;
        for (i, m) in cases[..n].iter().enumerate() {
            self.set_loc(m.pattern.loc);
            if i != n - 1 {
                self.emit(O::COPY, 1);
            }
            let mut pc = PatCtx { stores: Vec::new(), allow_irrefutable: m.guard.is_some() || i == ncases - 1, fail_pop: Vec::new(), on_top: 0 };
            self.compile_pattern(&m.pattern, &mut pc)?;
            self.set_loc(m.pattern.loc);
            let stores = pc.stores.clone();
            for name in &stores {
                self.nameop(name, ast::ExprContext::Store)?;
            }
            if let Some(g) = &m.guard {
                self.ensure_fail_pop(&mut pc, 0);
                let t = pc.fail_pop[0];
                self.compile_jump_if(g, false, t)?;
            }
            if i != n - 1 {
                self.set_loc(m.pattern.loc);
                self.emit(O::POP_TOP, 0);
            }
            for st in &m.body {
                self.compile_stmt(st)?;
            }
            self.emit_jump_noloc(O::JUMP_NO_INTERRUPT, end);
            self.set_loc(m.pattern.loc);
            self.emit_and_reset_fail_pop(&mut pc);
        }
        if has_default {
            self.set_loc(last.pattern.loc);
            if ncases == 1 {
                self.emit(O::POP_TOP, 0);
            } else {
                self.emit(O::NOP, 0);
            }
            if let Some(g) = &last.guard {
                self.compile_jump_if(g, false, end)?;
            }
            for st in &last.body {
                self.compile_stmt(st)?;
            }
        }
        let _ = s;
        self.use_label(end);
        Ok(())
    }

    fn ensure_fail_pop(&mut self, pc: &mut PatCtx, n: usize) {
        while pc.fail_pop.len() < n + 1 {
            let l = self.new_label();
            pc.fail_pop.push(l);
        }
    }

    fn jump_to_fail_pop(&mut self, pc: &mut PatCtx, op: u16) {
        let pops = pc.on_top as usize + pc.stores.len();
        self.ensure_fail_pop(pc, pops);
        let t = pc.fail_pop[pops];
        self.emit_jump(op, t);
    }

    fn emit_and_reset_fail_pop(&mut self, pc: &mut PatCtx) {
        if pc.fail_pop.is_empty() {
            return;
        }
        let n = pc.fail_pop.len();
        for k in (1..n).rev() {
            self.use_label(pc.fail_pop[k]);
            self.emit(O::POP_TOP, 0);
        }
        self.use_label(pc.fail_pop[0]);
        pc.fail_pop.clear();
    }

    fn pattern_rotate(&mut self, mut count: u32) {
        while count > 1 {
            self.emit(O::SWAP, count);
            count -= 1;
        }
    }

    fn pattern_store_name(&mut self, name: Option<&str>, loc: Loc, pc: &mut PatCtx) -> R {
        match name {
            None => {
                self.emit(O::POP_TOP, 0);
                Ok(())
            }
            Some(n) => {
                if n == "__debug__" {
                    return Err(self.error("cannot assign to __debug__", loc));
                }
                if pc.stores.iter().any(|s| s == n) {
                    return Err(self.error(format!("multiple assignments to name '{}' in pattern", n), loc));
                }
                let rot = pc.on_top + pc.stores.len() as u32 + 1;
                self.pattern_rotate(rot);
                pc.stores.push(n.to_string());
                Ok(())
            }
        }
    }

    fn compile_subpattern(&mut self, p: &'a Pattern, pc: &mut PatCtx) -> R {
        let allow = pc.allow_irrefutable;
        pc.allow_irrefutable = true;
        let r = self.compile_pattern(p, pc);
        pc.allow_irrefutable = allow;
        r
    }

    pub(crate) fn compile_pattern(&mut self, p: &'a Pattern, pc: &mut PatCtx) -> R {
        self.enter_depth()?;
        self.set_loc(p.loc);
        let r = self.compile_pattern_inner(p, pc);
        self.depth -= 1;
        r
    }

    fn compile_pattern_inner(&mut self, p: &'a Pattern, pc: &mut PatCtx) -> R {
        match &p.node {
            PatternKind::MatchValue { value } => {
                match &value.node {
                    ExprKind::Constant { .. } | ExprKind::Attribute { .. } => {}
                    _ => return Err(self.error("patterns may only match literals and attribute lookups", p.loc)),
                }
                self.compile_expr(value)?;
                self.set_loc(p.loc);
                self.emit_compare(2, true);
                self.jump_to_fail_pop(pc, O::POP_JUMP_IF_FALSE);
            }
            PatternKind::MatchSingleton { value } => {
                self.load_const(conv_const(value));
                self.emit(O::IS_OP, 0);
                self.jump_to_fail_pop(pc, O::POP_JUMP_IF_FALSE);
            }
            PatternKind::MatchAs { pattern, name } => match pattern {
                None => {
                    if !pc.allow_irrefutable {
                        if let Some(n) = name {
                            return Err(self.error(format!("name capture '{}' makes remaining patterns unreachable", n), p.loc));
                        }
                        return Err(self.error("wildcard makes remaining patterns unreachable", p.loc));
                    }
                    self.pattern_store_name(name.as_deref(), p.loc, pc)?;
                }
                Some(sub) => {
                    pc.on_top += 1;
                    self.emit(O::COPY, 1);
                    self.compile_pattern(sub, pc)?;
                    pc.on_top -= 1;
                    self.set_loc(p.loc);
                    self.pattern_store_name(name.as_deref(), p.loc, pc)?;
                }
            },
            PatternKind::MatchStar { name } => {
                self.pattern_store_name(name.as_deref(), p.loc, pc)?;
            }
            PatternKind::MatchSequence { patterns } => self.compile_pattern_sequence(p, patterns, pc)?,
            PatternKind::MatchMapping { keys, patterns, rest } => self.compile_pattern_mapping(p, keys, patterns, rest.as_deref(), pc)?,
            PatternKind::MatchClass { cls, patterns, kwd_attrs, kwd_patterns } => {
                let nargs = patterns.len();
                let nattrs = kwd_attrs.len();
                if nattrs != kwd_patterns.len() {
                    return Err(self.error("kwd_attrs / kwd_patterns length mismatch in class pattern", p.loc));
                }
                for (i, a) in kwd_attrs.iter().enumerate() {
                    if a == "__debug__" {
                        return Err(self.error("cannot assign to __debug__", kwd_patterns[i].loc));
                    }
                    if kwd_attrs[i + 1..].iter().any(|b| b == a) {
                        let j = i + 1 + kwd_attrs[i + 1..].iter().position(|b| b == a).unwrap();
                        return Err(self.error(format!("attribute name repeated in class pattern: {}", a), kwd_patterns[j].loc));
                    }
                }
                self.compile_expr(cls)?;
                self.set_loc(p.loc);
                let names: Vec<Constant> = kwd_attrs.iter().map(|a| str_const(a)).collect();
                self.load_const(Constant::Tuple(names));
                self.emit(O::MATCH_CLASS, nargs as u32);
                self.emit(O::COPY, 1);
                self.load_none();
                self.emit(O::IS_OP, 1);
                pc.on_top += 1;
                self.jump_to_fail_pop(pc, O::POP_JUMP_IF_FALSE);
                let total = nargs + nattrs;
                self.emit(O::UNPACK_SEQUENCE, total as u32);
                pc.on_top += total as u32;
                pc.on_top -= 1;
                for i in 0..total {
                    pc.on_top -= 1;
                    let sub = if i < nargs { &patterns[i] } else { &kwd_patterns[i - nargs] };
                    if is_wildcard(sub) {
                        self.set_loc(p.loc);
                        self.emit(O::POP_TOP, 0);
                        continue;
                    }
                    self.compile_subpattern(sub, pc)?;
                }
                // undo the off-by-one bookkeeping (CPython: on_top += total - 1)
                pc.on_top += 1;
                pc.on_top -= 1;
            }
            PatternKind::MatchOr { patterns } => self.compile_pattern_or(p, patterns, pc)?,
        }
        Ok(())
    }

    fn compile_pattern_sequence(&mut self, p: &'a Pattern, patterns: &'a [Pattern], pc: &mut PatCtx) -> R {
        let size = patterns.len();
        let mut star: Option<usize> = None;
        let mut only_wildcard = true;
        let mut star_wildcard = false;
        for (i, q) in patterns.iter().enumerate() {
            if let PatternKind::MatchStar { .. } = q.node {
                if star.is_some() {
                    return Err(self.error("multiple starred names in sequence pattern", p.loc));
                }
                star_wildcard = is_star_wildcard(q);
                only_wildcard &= star_wildcard;
                star = Some(i);
                continue;
            }
            only_wildcard &= is_wildcard(q);
        }
        pc.on_top += 1;
        self.emit(O::MATCH_SEQUENCE, 0);
        self.jump_to_fail_pop(pc, O::POP_JUMP_IF_FALSE);
        match star {
            None => {
                self.emit(O::GET_LEN, 0);
                self.load_const(Constant::Int(size as i64));
                self.emit_compare(2, true);
                self.jump_to_fail_pop(pc, O::POP_JUMP_IF_FALSE);
            }
            Some(_) if size > 1 => {
                self.emit(O::GET_LEN, 0);
                self.load_const(Constant::Int(size as i64 - 1));
                self.emit_compare(5, true);
                self.jump_to_fail_pop(pc, O::POP_JUMP_IF_FALSE);
            }
            _ => {}
        }
        pc.on_top -= 1;
        if only_wildcard {
            self.emit(O::POP_TOP, 0);
        } else if star_wildcard {
            // index-based extraction (no unpacking of the star part)
            let star = star.unwrap();
            pc.on_top += 1;
            for (i, q) in patterns.iter().enumerate() {
                if is_wildcard(q) || i == star {
                    continue;
                }
                self.set_loc(p.loc);
                self.emit(O::COPY, 1);
                if i < star {
                    self.load_const(Constant::Int(i as i64));
                } else {
                    self.emit(O::GET_LEN, 0);
                    self.load_const(Constant::Int((size - i) as i64));
                    self.emit(O::BINARY_OP, 10);
                }
                self.emit(O::BINARY_OP, wordcode::NB_SUBSCR);
                self.compile_subpattern(q, pc)?;
            }
            pc.on_top -= 1;
            self.set_loc(p.loc);
            self.emit(O::POP_TOP, 0);
        } else {
            match star {
                Some(i) => {
                    if i >= 256 || size - i - 1 >= (1 << 16) {
                        return Err(self.error("too many expressions in star-unpacking sequence pattern", p.loc));
                    }
                    self.emit(O::UNPACK_EX, (i | ((size - i - 1) << 8)) as u32);
                }
                None => self.emit(O::UNPACK_SEQUENCE, size as u32),
            }
            pc.on_top += size as u32;
            for q in patterns {
                pc.on_top -= 1;
                self.compile_subpattern(q, pc)?;
            }
        }
        Ok(())
    }

    fn compile_pattern_mapping(&mut self, p: &'a Pattern, keys: &'a [Expr], patterns: &'a [Pattern], rest: Option<&'a str>, pc: &mut PatCtx) -> R {
        let size = keys.len();
        if size != patterns.len() {
            return Err(self.error("keys / patterns length mismatch in mapping pattern", p.loc));
        }
        pc.on_top += 1;
        self.emit(O::MATCH_MAPPING, 0);
        self.jump_to_fail_pop(pc, O::POP_JUMP_IF_FALSE);
        if size == 0 && rest.is_none() {
            pc.on_top -= 1;
            self.emit(O::POP_TOP, 0);
            return Ok(());
        }
        if size > 0 {
            self.emit(O::GET_LEN, 0);
            self.load_const(Constant::Int(size as i64));
            self.emit_compare(5, true);
            self.jump_to_fail_pop(pc, O::POP_JUMP_IF_FALSE);
        }
        let mut seen: Vec<&ast::Constant> = Vec::new();
        for k in keys {
            match &k.node {
                ExprKind::Constant { value, .. } => {
                    if seen.iter().any(|s| const_key_eq(s, value)) {
                        return Err(self.error(format!("mapping pattern checks duplicate key ({})", crate::unparse::const_repr(value)), p.loc));
                    }
                    seen.push(value);
                }
                ExprKind::Attribute { .. } => {}
                _ => return Err(self.error("mapping pattern keys may only match literals and attribute lookups", p.loc)),
            }
            self.compile_expr(k)?;
        }
        self.set_loc(p.loc);
        self.emit(O::BUILD_TUPLE, size as u32);
        self.emit(O::MATCH_KEYS, 0);
        pc.on_top += 2;
        self.emit(O::COPY, 1);
        self.load_none();
        self.emit(O::IS_OP, 1);
        self.jump_to_fail_pop(pc, O::POP_JUMP_IF_FALSE);
        self.emit(O::UNPACK_SEQUENCE, size as u32);
        pc.on_top += size as u32;
        pc.on_top -= 1;
        for q in patterns {
            pc.on_top -= 1;
            self.compile_subpattern(q, pc)?;
        }
        pc.on_top += 1;
        pc.on_top -= 1;
        pc.on_top -= 2;
        self.set_loc(p.loc);
        if let Some(r) = rest {
            self.emit(O::BUILD_MAP, 0);
            self.emit(O::SWAP, 3);
            self.emit(O::DICT_UPDATE, 2);
            self.emit(O::UNPACK_SEQUENCE, size as u32);
            let mut sz = size as u32;
            while sz > 0 {
                self.emit(O::COPY, 1 + sz);
                sz -= 1;
                self.emit(O::SWAP, 2);
                self.emit(O::DELETE_SUBSCR, 0);
            }
            self.pattern_store_name(Some(r), p.loc, pc)?;
        } else {
            self.emit(O::POP_TOP, 0);
            self.emit(O::POP_TOP, 0);
        }
        Ok(())
    }

    fn compile_pattern_or(&mut self, p: &'a Pattern, alts: &'a [Pattern], pc: &mut PatCtx) -> R {
        let end = self.new_label();
        let old_stores = std::mem::take(&mut pc.stores);
        let old_allow = pc.allow_irrefutable;
        let old_fail_pop = std::mem::take(&mut pc.fail_pop);
        let old_on_top = pc.on_top;
        let mut control: Option<Vec<String>> = None;
        let size = alts.len();
        for (i, alt) in alts.iter().enumerate() {
            pc.stores = Vec::new();
            pc.allow_irrefutable = i == size - 1 && old_allow;
            pc.fail_pop = Vec::new();
            pc.on_top = 0;
            self.set_loc(alt.loc);
            self.emit(O::COPY, 1);
            self.compile_pattern(alt, pc)?;
            let nstores = pc.stores.len();
            match &control {
                None => control = Some(pc.stores.clone()),
                Some(ctl) => {
                    if nstores != ctl.len() {
                        return Err(self.error("alternative patterns bind different names", p.loc));
                    }
                    if nstores > 0 {
                        let mut icontrol = nstores;
                        while icontrol > 0 {
                            icontrol -= 1;
                            let name = &ctl[icontrol];
                            let istores = match pc.stores.iter().position(|s| s == name) {
                                Some(x) => x,
                                None => return Err(self.error("alternative patterns bind different names", p.loc)),
                            };
                            if icontrol != istores {
                                let rotations = istores + 1;
                                let rotated: Vec<String> = pc.stores.drain(0..rotations).collect();
                                let at = icontrol - istores;
                                for (k, r) in rotated.into_iter().enumerate() {
                                    pc.stores.insert(at + k, r);
                                }
                                for _ in 0..rotations {
                                    self.pattern_rotate(icontrol as u32 + 1);
                                }
                            }
                        }
                    }
                }
            }
            self.set_loc(alt.loc);
            self.emit_jump(O::JUMP, end);
            self.emit_and_reset_fail_pop(pc);
        }
        pc.stores = old_stores;
        pc.allow_irrefutable = old_allow;
        pc.fail_pop = old_fail_pop;
        pc.on_top = old_on_top;
        self.set_loc(p.loc);
        self.emit(O::POP_TOP, 0);
        self.jump_to_fail_pop(pc, O::JUMP_NO_INTERRUPT);
        self.use_label(end);
        let control = control.unwrap_or_default();
        let nstores = control.len() as u32;
        let nrots = nstores + 1 + pc.on_top + pc.stores.len() as u32;
        for name in control {
            self.pattern_rotate(nrots);
            if pc.stores.contains(&name) {
                return Err(self.error(format!("multiple assignments to name '{}' in pattern", name), p.loc));
            }
            pc.stores.push(name);
        }
        self.emit(O::POP_TOP, 0);
        Ok(())
    }
}

fn const_key_eq(a: &ast::Constant, b: &ast::Constant) -> bool {
    use ast::Constant as C;
    // Python equality for hashable constants used as mapping keys
    fn num(c: &C) -> Option<f64> {
        match c {
            C::Bool(b) => Some(*b as i64 as f64),
            C::Int(ast::Int::Small(i)) => Some(*i as f64),
            C::Float(f) => Some(*f),
            C::Complex { real, imag } if *imag == 0.0 => Some(*real),
            _ => None,
        }
    }
    match (a, b) {
        (C::Int(ast::Int::Small(x)), C::Int(ast::Int::Small(y))) => x == y,
        (C::Int(ast::Int::Big(x)), C::Int(ast::Int::Big(y))) => x == y,
        (C::Complex { real: r1, imag: i1 }, C::Complex { real: r2, imag: i2 }) => r1 == r2 && i1 == i2,
        _ => match (num(a), num(b)) {
            (Some(x), Some(y)) => x == y,
            (None, None) => a == b,
            _ => false,
        },
    }
}
