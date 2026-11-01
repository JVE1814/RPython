/* _ctypes_test: a shared library of C functions and data used by the ctypes
 * test-suite (spec/Lib/test/test_ctypes), plus a tiny Python extension module
 * so that the tests can locate it with `import _ctypes_test`.
 *
 * Written for this project from the expectations in the test files; built by
 * tools/ctypes_test/build.sh.
 */
#define PY_SSIZE_T_CLEAN
#include "Python.h"

#include <complex.h>
#include <math.h>
#include <stdlib.h>
#include <string.h>
#include <wchar.h>
#undef I  /* complex.h defines I; the bit-field tests use it as a field name */

#define EXPORT(type) __attribute__((visibility("default"))) type

/* ------------------------------------------------------------------ data */

EXPORT(int) an_integer = 42;

EXPORT(int) get_an_integer(void) { return an_integer; }

/* ------------------------------------------------------------------ simple numeric functions */

static long long last_tf_arg_s;
static unsigned long long last_tf_arg_u;

#define SIGNED_TF(name, type) \
    EXPORT(type) tf_##name(type c) { last_tf_arg_s = (long long)c; return c / 3; } \
    EXPORT(type) tf_b##name(signed char x, type c) { (void)x; last_tf_arg_s = (long long)c; return c / 3; }
#define UNSIGNED_TF(name, type) \
    EXPORT(type) tf_##name(type c) { last_tf_arg_u = (unsigned long long)c; return c / 3; } \
    EXPORT(type) tf_b##name(signed char x, type c) { (void)x; last_tf_arg_u = (unsigned long long)c; return c / 3; }

SIGNED_TF(b, signed char)
UNSIGNED_TF(B, unsigned char)
SIGNED_TF(h, short)
UNSIGNED_TF(H, unsigned short)
SIGNED_TF(i, int)
UNSIGNED_TF(I, unsigned int)
SIGNED_TF(l, long)
UNSIGNED_TF(L, unsigned long)
SIGNED_TF(q, long long)
UNSIGNED_TF(Q, unsigned long long)
SIGNED_TF(f, float)
SIGNED_TF(d, double)
SIGNED_TF(D, long double)

EXPORT(void) tv_i(int c) { last_tf_arg_s = c; }

/* ------------------------------------------------------------------ mixed argument lists */

EXPORT(int) _testfunc_i_bhilfd(signed char b, short h, int i, long l, float f, double d)
{
    return (int)(b + h + i + l + f + d);
}

EXPORT(float) _testfunc_f_bhilfd(signed char b, short h, int i, long l, float f, double d)
{
    return (float)(b + h + i + l + f + d);
}

EXPORT(double) _testfunc_d_bhilfd(signed char b, short h, int i, long l, float f, double d)
{
    return (double)(b + h + i + l + f + d);
}

EXPORT(long double) _testfunc_D_bhilfD(signed char b, short h, int i, long l, float f, long double d)
{
    return (long double)(b + h + i + l + f + d);
}

EXPORT(long long) _testfunc_q_bhilfd(signed char b, short h, int i, long l, float f, double d)
{
    return (long long)(b + h + i + l + f + d);
}

EXPORT(long long) _testfunc_q_bhilfdq(signed char b, short h, int i, long l, float f, double d, long long q)
{
    return (long long)(b + h + i + l + f + d + q);
}

EXPORT(void) _testfunc_v(int a, int b, int *presult) { *presult = a + b; }

EXPORT(void *) _testfunc_p_p(void *s) { return s; }

EXPORT(char *) _testfunc_c_p_p(int *argcp, char **argv) { return argv[(*argcp) - 1]; }

EXPORT(int *) _testfunc_ai8(int a[8]) { return a; }

EXPORT(int) _testfunc_callback_i_if(int value, int (*func)(int))
{
    int sum = 0;
    while (value != 0) {
        sum += func(value);
        value /= 2;
    }
    return sum;
}

EXPORT(long long) _testfunc_callback_q_qf(long long value, long long (*func)(long long))
{
    long long sum = 0;
    while (value != 0) {
        sum += func(value);
        value /= 2;
    }
    return sum;
}

EXPORT(int) _testfunc_callback_with_pointer(int (*func)(int *))
{
    static int table[] = {1, 2, 3, 4, 5, 6, 7, 8, 9, 10};
    return func(table);
}

EXPORT(long long) _testfunc_callback_int_to_longlong(int (*func)(void))
{
    return (long long)func();
}

EXPORT(int) _testfunc_cbk_reg_int(int a, int b, int c, int d, int e, int (*func)(int, int, int, int, int))
{
    return func(a * a, b * b, c * c, d * d, e * e);
}

EXPORT(double) _testfunc_cbk_reg_double(double a, double b, double c, double d, double e,
                                        double (*func)(double, double, double, double, double))
{
    return func(a * a, b * b, c * c, d * d, e * e);
}

EXPORT(long) _test_i38748_runCallback(long (*func)(long, long long), long a, long long b)
{
    return func(a, b);
}

EXPORT(double) integrate(double a, double b, double (*f)(double), long nstep)
{
    double x, sum = 0.0, dx = (b - a) / (double)nstep;
    for (x = a + 0.5 * dx; (b - x) * (x - a) > 0.0; x += dx)
        sum += f(x);
    return sum / (double)nstep;
}

EXPORT(int) _testfunc_pylist_append(PyObject *list, PyObject *item)
{
    return PyList_Append(list, item);
}

/* ------------------------------------------------------------------ structures by value */

typedef struct { int x; int y; } point;

EXPORT(int) _testfunc_byval(point in, point *pout)
{
    if (pout) {
        pout->x = in.x;
        pout->y = in.y;
    }
    return in.x + in.y;
}

typedef struct { short x; short y; } S2H;

EXPORT(S2H) ret_2h_func(S2H inp)
{
    inp.x *= 2;
    inp.y *= 3;
    return inp;
}

typedef struct { int a, b, c, d, e, f, g, h; } S8I;

EXPORT(S8I) ret_8i_func(S8I inp)
{
    inp.a *= 2; inp.b *= 3; inp.c *= 4; inp.d *= 5;
    inp.e *= 6; inp.f *= 7; inp.g *= 8; inp.h *= 9;
    return inp;
}

typedef struct { unsigned long first, second, third; } Test;

EXPORT(void) _testfunc_cbk_large_struct(Test in, void (*func)(Test))
{
    func(in);
}

EXPORT(void) _testfunc_large_struct_update_value(Test in)
{
    in.first = 0x0badf00d;
    in.second = 0x0badf00d;
    in.third = 0x0badf00d;
    (void)in;
}

typedef struct { unsigned int first, second; } TestReg;

static TestReg last_tfrsuv_arg;

EXPORT(void) _testfunc_reg_struct_update_value(TestReg in)
{
    last_tfrsuv_arg = in;
    in.first = 0x0badf00d;
    in.second = 0x0badf00d;
    (void)in;
}

EXPORT(TestReg) get_last_tfrsuv_arg(void) { return last_tfrsuv_arg; }

typedef struct { unsigned char data[16]; } Test2;

EXPORT(int) _testfunc_array_in_struct2(Test2 in)
{
    int result = 0;
    for (unsigned i = 0; i < 16; i++) {
        result += in.data[i];
    }
    memset(in.data, 0, sizeof(in.data));
    return result;
}

typedef struct { float data[2]; float more_data[2]; } Test3A;

EXPORT(double) _testfunc_array_in_struct3A(Test3A in)
{
    double result = 0;
    for (unsigned i = 0; i < 2; i++) {
        result += in.data[i];
        result += in.more_data[i];
    }
    memset(in.data, 0, sizeof(in.data));
    return result;
}

#define ARRAY_STRUCT(NAME, N) \
    typedef struct { double data[N]; } Test##NAME; \
    EXPORT(double) _testfunc_array_in_struct##NAME(Test##NAME in) { \
        double result = 0; \
        for (unsigned i = 0; i < N; i++) result += in.data[i]; \
        memset(in.data, 0, sizeof(in.data)); \
        return result; \
    } \
    EXPORT(Test##NAME) _testfunc_array_in_struct##NAME##_set_defaults(void) { \
        Test##NAME s; \
        for (unsigned i = 0; i < N; i++) s.data[i] = i + 1; \
        return s; \
    }

ARRAY_STRUCT(3B, 2)
ARRAY_STRUCT(3C, 4)
ARRAY_STRUCT(3D, 8)
ARRAY_STRUCT(3E, 9)

/* rectangles: four exported coordinates and functions mixing by-value and
   by-reference structure arguments */

EXPORT(long) left = 10;
EXPORT(long) top = 20;
EXPORT(long) right = 30;
EXPORT(long) bottom = 40;

typedef struct { long left, top, right, bottom; } RECT;
typedef struct { long x, y; } POINT;

static int same_rect(RECT a, RECT b)
{
    return a.left == b.left && a.top == b.top && a.right == b.right && a.bottom == b.bottom;
}

EXPORT(int) PointInRect(RECT *prc, POINT pt)
{
    if (prc == NULL)
        return 0;
    return pt.x >= prc->left && pt.x <= prc->right && pt.y >= prc->top && pt.y <= prc->bottom;
}

EXPORT(RECT) ReturnRect(int i, RECT ar, RECT *br, POINT cp, RECT dr, RECT *er, POINT fp, RECT gr)
{
    RECT bad = {0, 0, 0, 0};
    if (!same_rect(ar, *br) || !same_rect(ar, dr) || !same_rect(ar, *er) || !same_rect(ar, gr))
        return bad;
    if (cp.x != fp.x || cp.y != fp.y)
        return bad;
    switch (i) {
    case 0: return ar;
    case 1: return *br;
    case 2: return dr;
    case 3: return *er;
    default: return gr;
    }
}

/* unions and bit fields (the by-value variants are rejected by ctypes itself) */

typedef struct { int an_int; int another_int; } Nested1;
typedef union { long a_long; Nested1 a_struct; } Test4;
typedef struct { int an_int; Test4 a_union; } Nested2;
typedef struct { int an_int; Nested2 nested; int another_int; } Test5;

EXPORT(long) _testfunc_union_by_value1(Test4 in)
{
    return in.a_long + in.a_struct.an_int + in.a_struct.another_int;
}

EXPORT(long) _testfunc_union_by_value2(Test5 in)
{
    return in.an_int + in.nested.an_int;
}

EXPORT(long) _testfunc_union_by_reference1(Test4 *in)
{
    long result = in->a_long;
    memset(in, 0, sizeof(Test4));
    return result;
}

EXPORT(long) _testfunc_union_by_reference2(Test4 *in)
{
    long result = in->a_struct.an_int + in->a_struct.another_int;
    memset(in, 0, sizeof(Test4));
    return result;
}

EXPORT(long) _testfunc_union_by_reference3(Test5 *in)
{
    long result = in->an_int + in->nested.an_int + in->another_int;
    memset(in, 0, sizeof(Test5));
    return result;
}

typedef struct { signed int A: 1, B: 2, C: 3, D: 2; } Test6;

EXPORT(long) _testfunc_bitfield_by_value1(Test6 in)
{
    return in.A + in.B + in.C + in.D;
}

EXPORT(long) _testfunc_bitfield_by_reference1(Test6 *in)
{
    long result = in->A + in->B + in->C + in->D;
    memset(in, 0, sizeof(Test6));
    return result;
}

typedef struct { unsigned int A: 1, B: 2, C: 3, D: 2; } Test7;

EXPORT(long) _testfunc_bitfield_by_reference2(Test7 *in)
{
    long result = in->A + in->B + in->C + in->D;
    memset(in, 0, sizeof(Test7));
    return result;
}

typedef union { signed int A: 1, B: 2, C: 3, D: 2; } Test8;

EXPORT(long) _testfunc_bitfield_by_value2(Test8 in)
{
    return in.A;
}

/* ------------------------------------------------------------------ bit fields */

struct BITS {
    signed int A: 1, B: 2, C: 3, D: 4, E: 5, F: 6, G: 7, H: 8, I: 9;
    signed short M: 1, N: 2, O: 3, P: 4, Q: 5, R: 6, S: 7;
};

EXPORT(int) unpack_bitfields(struct BITS *bits, char name)
{
    switch (name) {
    case 'A': return bits->A;
    case 'B': return bits->B;
    case 'C': return bits->C;
    case 'D': return bits->D;
    case 'E': return bits->E;
    case 'F': return bits->F;
    case 'G': return bits->G;
    case 'H': return bits->H;
    case 'I': return bits->I;
    case 'M': return bits->M;
    case 'N': return bits->N;
    case 'O': return bits->O;
    case 'P': return bits->P;
    case 'Q': return bits->Q;
    case 'R': return bits->R;
    case 'S': return bits->S;
    }
    return 999;
}

#if defined(__x86_64__) || defined(__i386__)
struct __attribute__((ms_struct)) BITS_msvc {
    signed int A: 1, B: 2, C: 3, D: 4, E: 5, F: 6, G: 7, H: 8, I: 9;
    signed short M: 1, N: 2, O: 3, P: 4, Q: 5, R: 6, S: 7;
};

EXPORT(int) unpack_bitfields_msvc(struct BITS_msvc *bits, char name)
{
    switch (name) {
    case 'A': return bits->A;
    case 'B': return bits->B;
    case 'C': return bits->C;
    case 'D': return bits->D;
    case 'E': return bits->E;
    case 'F': return bits->F;
    case 'G': return bits->G;
    case 'H': return bits->H;
    case 'I': return bits->I;
    case 'M': return bits->M;
    case 'N': return bits->N;
    case 'O': return bits->O;
    case 'P': return bits->P;
    case 'Q': return bits->Q;
    case 'R': return bits->R;
    case 'S': return bits->S;
    }
    return 999;
}
#endif

/* ------------------------------------------------------------------ libc-like helpers */

#include <stdarg.h>

EXPORT(int) myprintf(const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    int n = vprintf(fmt, ap);
    va_end(ap);
    fflush(stdout);
    return n;
}

EXPORT(char *) my_strtok(char *token, const char *delim) { return strtok(token, delim); }

EXPORT(char *) my_strchr(const char *s, int c) { return strchr(s, c); }

EXPORT(double) my_sqrt(double a) { return sqrt(a); }

EXPORT(double complex) my_csqrt(double complex a) { return csqrt(a); }
EXPORT(float complex) my_csqrtf(float complex a) { return csqrtf(a); }
EXPORT(long double complex) my_csqrtl(long double complex a) { return csqrtl(a); }

EXPORT(void) my_qsort(void *base, size_t num, size_t width, int (*compare)(const void *, const void *))
{
    qsort(base, num, width, compare);
}

EXPORT(char *) my_strdup(char *src) { return strdup(src); }

EXPORT(void) my_free(void *ptr) { free(ptr); }

EXPORT(wchar_t *) my_wcsdup(wchar_t *src)
{
    size_t n = wcslen(src);
    wchar_t *p = malloc((n + 1) * sizeof(wchar_t));
    if (p == NULL)
        return NULL;
    memcpy(p, src, (n + 1) * sizeof(wchar_t));
    return p;
}

EXPORT(size_t) my_wcslen(wchar_t *src) { return wcslen(src); }

typedef char *(*strchr_t)(const char *, int);

EXPORT(strchr_t) get_strchr(void) { return (strchr_t)strchr; }

/* ------------------------------------------------------------------ Python module */

static PyObject *
py_get_last_tf_arg_s(PyObject *self, PyObject *noargs)
{
    return PyLong_FromLongLong(last_tf_arg_s);
}

static PyObject *
py_get_last_tf_arg_u(PyObject *self, PyObject *noargs)
{
    return PyLong_FromUnsignedLongLong(last_tf_arg_u);
}

#include "_ctypes_test_generated.c.h"

static PyMethodDef module_methods[] = {
    {"get_last_tf_arg_s", py_get_last_tf_arg_s, METH_NOARGS},
    {"get_last_tf_arg_u", py_get_last_tf_arg_u, METH_NOARGS},
    {"get_generated_test_data", get_generated_test_data, METH_O},
    {NULL, NULL}
};

static PyModuleDef_Slot module_slots[] = {
    {Py_mod_gil, Py_MOD_GIL_NOT_USED},
    {0, NULL}
};

static struct PyModuleDef module_def = {
    PyModuleDef_HEAD_INIT,
    .m_name = "_ctypes_test",
    .m_size = 0,
    .m_methods = module_methods,
    .m_slots = module_slots,
};

PyMODINIT_FUNC
PyInit__ctypes_test(void)
{
    return PyModuleDef_Init(&module_def);
}
