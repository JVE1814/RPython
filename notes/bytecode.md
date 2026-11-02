# D24 coordination: public CPython 3.15 wordcode (bc-compiler <-> bc-vm)

(bc-compiler owns the sections above this line in its branch; on merge keep both sides.)

## bc-vm section
Branch: **bc-vm = `worktree-agent-ae0417e805548192f`**.

### Runtime plan (bc-vm)
* Code objects store the public form only (`co_code`, `co_linetable`, `co_exceptiontable`,
  consts/names/localsplus...). The internal executable form (`types::code::CodeData`: the
  existing 32-bit `bytecode::Op` stream + per-instruction locations + internal exception
  table + inline caches) is produced by `interp/translate.rs` on first use and published once
  (CAS, release/acquire). `code_data(co)` keeps its signature, so the interpreter, the JIT
  (jit3) and f_lineno jumps keep working on the internal form unchanged.
* Translation is semantic (CPython 3.15 instruction semantics), not compiler-pattern based,
  except for one point: the second slot of the 3.15 iterator pair (`GET_ITER` ->
  `[iter, NULL-or-index]`) and the NULL of the SEND protocol (`[receiver, NULL, value]`) are
  *not materialised* internally ("phantom" stack slots). The translator runs an abstract
  interpretation with a real/phantom flag per stack slot; exception-table depths are
  converted accordingly. A `PUSH_NULL` becomes phantom when its value is consumed by
  `FOR_ITER`/`SEND`/`END_SEND`/`POP_ITER`/`CLEANUP_THROW` in the null position.
  bc-compiler: please list here any place where such a NULL is produced by something other
  than `GET_ITER` / `PUSH_NULL` (I know of: genexpr fast path `CALL 0; PUSH_NULL; FOR_ITER`,
  `GET_AWAITABLE/GET_ANEXT; PUSH_NULL; LOAD_CONST None; SEND`).
* Internal pc <-> public offset maps live in CodeData; everything observable (f_lasti,
  tb_lasti, monitoring offsets, co_positions, f_lineno jumps) is converted to public units.
* `RETURN_GENERATOR`: when only COPY_FREE_VARS/MAKE_CELL/RESUME/NOP precede it, the runtime
  keeps today's model (generator object created at call time, body starts at internal 0);
  otherwise (genexpr: `LOAD_FAST .0; GET_ITER`) the prologue runs eagerly at call time.
* New internal ops (appended to `bytecode::Op`, bc-vm adds them; jit3: one line each in
  `jit/analysis.rs::effect`, tier1 deopts on them): `LoadSpecial` ([obj] -> [meth, self_or_null]),
  `ReturnGenerator`. **bc-compiler: please keep `bytecode::Op`/`ExceptionEntry`/`Location`/
  `encode`/`decode` after the switch (they are the internal format now); `CodeUnit`/`Constant`
  can go.** (Alternatively bc-vm moves them to `crates/runtime/src/interp/ops.rs` later.)
* Runtime entry point for compiler output: `types::code::new_code_from_wordcode(&wordcode::CodeObject,
  filename)`. Switch-over: env `RPYTHON_WORDCODE=1` selects `compile_wordcode` in the runtime
  until both sides pass; then the default flips and the legacy pipeline can be deleted.

### Log (bc-vm)
* started: reading runtime; translator design above.
