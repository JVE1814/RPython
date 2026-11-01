//! test_coroutines.py: test_async_comprehension_scope
mod common;
use common::*;
use rpy_compiler::{compile, cf, CompileOptions, Mode};

fn compile_flags(src: &str, flags: u32) -> Result<(), String> {
    let m = parse_src(src, Mode::Exec);
    let mut o = CompileOptions::new("<test>", Mode::Exec);
    o.flags = flags;
    compile(&m, &o).map(|_| ()).map_err(|e| e.msg)
}

const ERR: &str = "asynchronous comprehension outside of an asynchronous function";

#[test]
fn async_comprehension_scope() {
    let allowed = [
        "async def f():\n    [await x for x in y]",
        "async def f():\n    {await x for x in y}",
        "async def f():\n    {k: await x for k, x in y}",
        "async def f():\n    [x async for x in y]",
        "async def f():\n    {x async for x in y}",
        "async def f():\n    {k: x async for k, x in y}",
        "async def f():\n    [[await x for x in y] for y in z]",
        "async def outer():\n    async def f(x=[await y for y in z]): pass",
        "async def f():\n    class C([await x for x in y]): pass",
        "async def f():\n    (x for x in [await y for y in z])",
        "async def f():\n    (x for x in [y async for y in z])",
        "(await x for x in y)",
        "def f():\n    (await x for x in y)",
        "class C:\n    (await x for x in y)",
        "lambda: (await x for x in y)",
        "([await x for x in y] for y in z)",
        "def f():\n    ([await x for x in y] for y in z)",
        "class C:\n    ([await x for x in y] for y in z)",
        "async def f():\n    ([await x for x in y] for y in z)",
    ];
    for code in allowed {
        assert_eq!(compile_flags(code, 0), Ok(()), "{}", code);
    }
    let invalid = [
        "[await x for x in y]",
        "{await x for x in y}",
        "{k: await x for k, x in y}",
        "[x async for x in y]",
        "{x async for x in y}",
        "{k: x async for k, x in y}",
        "[[await x for x in y] for y in z]",
        "[[x async for x in y] for y in z]",
        "def f():\n    [await x for x in y]",
        "def f():\n    [x async for x in y]",
        "async def f():\n    def g():\n        [await x for x in y]",
        "class C:\n    [await x for x in y]",
        "class C:\n    {await x for x in y}",
        "class C:\n    {k: await x for k, x in y}",
        "class C:\n    [x async for x in y]",
        "class C:\n    [[await x for x in y] for y in z]",
        "async def f():\n    class C:\n        x = [await y for y in z]",
        "async def f():\n    class C:\n        x = [y async for y in z]",
        "lambda: [await x for x in y]",
        "async def f():\n    lambda: [await x for x in y]",
        "class C:\n    f = lambda: [await x for x in y]",
        "(x for x in [await y for y in z])",
        "(x for x in [y async for y in z])",
        "def f():\n    (x for x in [await y for y in z])",
        "async def f(x=[await y for y in z]): pass",
        "def f(x=[await y for y in z]): pass",
        "class C:\n    def f(self, x=[await y for y in z]): pass",
        "class C([await x for x in y]): pass",
        "type T = [await x for x in y]",
        "type T = [x async for x in y]",
        "def f[T=[await x for x in y]](): pass",
        "def f[T: [await x for x in y]](): pass",
        "async def f[T=[await x for x in y]](): pass",
        "async def f(x: [await y for y in z]): pass",
    ];
    for code in invalid {
        assert_eq!(compile_flags(code, 0), Err(ERR.to_string()), "{}", code);
    }
    assert_eq!(compile_flags("await x", 0), Err("'await' outside function".into()));
    assert_eq!(compile_flags("class C:\n    await x", 0), Err("'await' outside function".into()));
    assert_eq!(compile_flags("def f():\n    await x", 0), Err("'await' outside async function".into()));
    let tla = cf::PYCF_ALLOW_TOP_LEVEL_AWAIT;
    for code in [
        "[await x for x in y]",
        "async def f(x=[await y for y in z]): pass",
        "class C([await x for x in y]): pass",
        "f'{[await x for x in y]}'",
        "(x for x in [await y for y in z])",
        "(x for x in [y async for y in z])",
        "await x\nasync for a in b: pass\nasync with c: pass\n",
    ] {
        assert_eq!(compile_flags(code, tla), Ok(()), "tla {}", code);
    }
    for code in [
        "lambda: [await x for x in y]",
        "def f():\n    (x for x in [await y for y in z])",
        "class C:\n    def f(self, x=[await y for y in z]): pass",
        "type T = [await x for x in y]",
        "async def f[T=[await x for x in y]](): pass",
    ] {
        assert_eq!(compile_flags(code, tla), Err(ERR.to_string()), "tla {}", code);
    }
    // module code with top-level await is a coroutine
    let m = parse_src("await x\n", Mode::Exec);
    let mut o = CompileOptions::new("<t>", Mode::Exec);
    o.flags = tla;
    let c = compile(&m, &o).unwrap().code;
    assert!(c.flags & rpy_compiler::bytecode::co::COROUTINE != 0);
}

#[test]
fn async_outside() {
    for (code, msg) in [
        ("def foo():\n    async for i in a: pass\n", "'async for' outside async function"),
        ("def foo():\n    async with a: pass\n", "'async with' outside async function"),
        ("async with a: pass\n", "'async with' outside async function"),
        ("async def f():\n    yield from x\n", "'yield from' inside async function"),
    ] {
        assert_eq!(compile_flags(code, 0), Err(msg.to_string()), "{}", code);
    }
}
