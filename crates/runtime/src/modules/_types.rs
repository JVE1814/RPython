//! The `_types` module: the type objects re-exported by `types`.
//!
//! Most of them are obtained by evaluating small expressions (exactly what the
//! pure-Python fallback in types.py does), so both implementations agree on
//! object identity; SimpleNamespace comes from the runtime directly.

use crate::errors::*;
use crate::object::*;
use crate::types::dict;

const SRC: &str = r#"
import sys as _sys

def _f(): pass
FunctionType = type(_f)
LambdaType = type(lambda: None)
CodeType = type(_f.__code__)
MappingProxyType = type(type.__dict__)

def _cell_factory():
    a = 1
    def f():
        nonlocal a
    return f.__closure__[0]
CellType = type(_cell_factory())

def _g():
    yield 1
GeneratorType = type(_g())

async def _c(): pass
_c = _c()
CoroutineType = type(_c)
_c.close()

async def _ag():
    yield
_ag = _ag()
AsyncGeneratorType = type(_ag)

class _C:
    def _m(self): pass
MethodType = type(_C()._m)

BuiltinFunctionType = type(len)
BuiltinMethodType = type([].append)

WrapperDescriptorType = type(object.__init__)
MethodWrapperType = type(object().__str__)
MethodDescriptorType = type(str.join)
ClassMethodDescriptorType = type(dict.__dict__['fromkeys'])

ModuleType = type(_sys)

try:
    raise TypeError
except TypeError as exc:
    TracebackType = type(exc.__traceback__)
    FrameType = type(exc.__traceback__.tb_frame)

def _get_framelocalsproxy():
    return type(_sys._getframe().f_locals)
FrameLocalsProxyType = _get_framelocalsproxy()

GetSetDescriptorType = type(FunctionType.__code__)
MemberDescriptorType = type(FunctionType.__globals__)

try:
    GenericAlias = type(list[int])
except TypeError:
    pass
try:
    UnionType = type(int | str)
except TypeError:
    pass

EllipsisType = type(Ellipsis)
NoneType = type(None)
NotImplementedType = type(NotImplemented)

__all__ = [
    'AsyncGeneratorType', 'BuiltinFunctionType', 'BuiltinMethodType',
    'CapsuleType', 'CellType', 'ClassMethodDescriptorType', 'CodeType',
    'CoroutineType', 'EllipsisType', 'FrameType', 'FunctionType',
    'FrameLocalsProxyType', 'GeneratorType', 'GenericAlias',
    'GetSetDescriptorType', 'LambdaType', 'LazyImportType',
    'MappingProxyType', 'MemberDescriptorType', 'MethodDescriptorType',
    'MethodType', 'MethodWrapperType', 'ModuleType', 'NoneType',
    'NotImplementedType', 'SimpleNamespace', 'TracebackType', 'UnionType',
    'WrapperDescriptorType',
]

def _capsule_type():
    # Only use a capsule from an already loaded module: importing extension
    # modules here would be a side effect of `import types`.
    for modname, attr in (('_socket', 'CAPI'), ('_datetime', 'datetime_CAPI')):
        mod = _sys.modules.get(modname)
        if mod is not None and hasattr(mod, attr):
            return type(getattr(mod, attr))
    return globals().get('_rt_capsule_type')

for _n in ('GenericAlias', 'UnionType'):
    if _n not in globals():
        __all__.remove(_n)
del _n

_t = _capsule_type()
if _t is not None:
    CapsuleType = _t
else:
    __all__.remove('CapsuleType')
del _t

_t = getattr(_sys, '_lazy_import_type', None)
if _t is not None:
    LazyImportType = _t
else:
    __all__.remove('LazyImportType')
del _t

del _sys, _f, _g, _C, _c, _ag, _cell_factory, _get_framelocalsproxy, _capsule_type, _rt_capsule_type
"#;

pub fn init_module() -> PyResult {
    let m = crate::types::module::new_module("_types")?;
    let d = crate::types::module::module_dict(&m);
    dict::set_item_str(&d, "__builtins__", &crate::builtins::builtins_module())?;
    dict::set_item_str(&d, "SimpleNamespace", crate::types::typeobject::type_obj(crate::types::namespace::namespace_type()))?;
    dict::set_item_str(&d, "_rt_capsule_type", crate::types::typeobject::type_obj(crate::capi::capsule::capsule_type()))?;
    let code = crate::compile::compile_str(SRC, "<_types>", crate::compile::Mode::Exec, 0, -1)?;
    crate::interp::eval_code(&code.code, &d, Some(&d))?;
    let _: Option<PyRef> = None;
    Ok(m)
}
