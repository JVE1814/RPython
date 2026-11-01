"""Help texts for language keywords, symbols and topics, as shown by
help('keyword') and the interactive help utility.  The texts are short
summaries; the language reference has the full details."""

topics = {
    'assert': '''The "assert" statement
**********************

   assert_stmt ::= "assert" expression ["," expression]

An assert statement checks that a condition holds while debugging.
"assert expr" raises AssertionError if "expr" is false;
"assert expr, msg" passes "msg" to AssertionError.  Assertions are
removed when Python runs with optimizations (-O), when "__debug__" is
False.
''',
    'assignment': '''Assignment statements
*********************

Assignment statements bind names to values and modify attributes or
items of mutable objects:

   target_list "=" expression

Each target may be a name, an attribute reference ("obj.attr"), a
subscription ("seq[i]"), a slicing, or a (possibly starred) list or
tuple of targets that unpacks an iterable.
''',
    'async': '''Coroutines
**********

"async def" defines a coroutine function.  Inside it, "await" suspends
execution until an awaitable completes, "async for" iterates over an
asynchronous iterator and "async with" uses an asynchronous context
manager.
''',
    'atom-identifiers': '''Identifiers (Names)
*******************

An identifier occurring as an atom is a name.  Evaluating it looks the
name up in the local, enclosing, global and builtin namespaces, in that
order; NameError is raised if it is not bound anywhere.
''',
    'atom-literals': '''Literals
********

Python has string and bytes literals and numeric literals (integers,
floating-point numbers and imaginary numbers).  Evaluating a literal
yields an object of the given type with the given value.
''',
    'attribute-references': '''Attribute references
********************

   attributeref ::= primary "." identifier

The primary must evaluate to an object that supports attribute
references; the attribute is looked up with type(obj).__getattribute__,
falling back to __getattr__.  AttributeError is raised if it is not
found.
''',
    'augassign': '''Augmented assignment statements
*******************************

   augtarget augop expression

"x += 1" combines a binary operation and an assignment; when possible
the operation is performed in place (the in-place special methods such
as __iadd__ are tried first).
''',
    'await': '''Await expression
****************

"await expr" suspends the coroutine until the awaitable "expr" has
produced its result.  It can only be used inside a coroutine function.
''',
    'binary': '''Binary arithmetic operations
****************************

The binary arithmetic operators are "*", "@", "/", "//", "%", "+" and
"-".  They call __mul__, __matmul__, __truediv__, __floordiv__,
__mod__, __add__ and __sub__ (or the reflected variants on the right
operand).  "%" also formats strings (printf-style formatting).
''',
    'bitwise': '''Binary bitwise operations
*************************

"&" (and), "^" (exclusive or) and "|" (or) operate on integers and
call __and__, __xor__ and __or__.
''',
    'booleans': '''Boolean operations
******************

   or_test  ::= and_test | or_test "or" and_test
   and_test ::= not_test | and_test "and" not_test
   not_test ::= comparison | "not" not_test

"x or y" evaluates to x if x is true, else y; "x and y" evaluates to x
if x is false, else y; "not x" is True if x is false.  False, None,
numeric zero and empty containers are false; everything else is true.
''',
    'break': '''The "break" statement
*********************

"break" ends the nearest enclosing "for" or "while" loop, skipping its
optional "else" clause.
''',
    'callable-types': '''Emulating callable objects
**************************

object.__call__(self[, args...])

   Called when the instance is "called" as a function.
''',
    'calls': '''Calls
*****

   call ::= primary "(" [argument_list [","]] ")"

A call evaluates the primary and the arguments (positional, "*"
unpacking, keyword and "**" unpacking) and invokes the callable.
''',
    'class': '''Class definitions
*****************

   classdef ::= [decorators] "class" classname [type_params] [inheritance] ":" suite

A class definition executes its body in a new namespace and creates a
class object from the base classes and that namespace (using the
metaclass, "type" by default).  The class name is then bound to it.
''',
    'comparisons': '''Comparisons
***********

The comparison operators are "<", ">", "==", ">=", "<=", "!=", "is",
"is not", "in" and "not in".  Comparisons can be chained: "a < b < c"
means "a < b and b < c" (b is evaluated once).
''',
    'compound': '''Compound statements
*******************

Compound statements ("if", "while", "for", "try", "with", "match",
function and class definitions) contain groups of other statements.
''',
    'context-managers': '''With Statement Context Managers
*******************************

object.__enter__(self)

   Enter the runtime context; the result is bound to the "as" target.

object.__exit__(self, exc_type, exc_value, traceback)

   Exit the runtime context; returning a true value suppresses the
   exception.
''',
    'continue': '''The "continue" statement
************************

"continue" proceeds with the next iteration of the nearest enclosing
"for" or "while" loop.
''',
    'conversions': '''Arithmetic conversions
**********************

When a binary arithmetic operator has numeric arguments of different
types, the "narrower" one is converted: int to float, and float to
complex.
''',
    'customization': '''Basic customization
*******************

Special methods such as __new__, __init__, __del__, __repr__, __str__,
__format__, __bytes__, the rich comparison methods, __hash__ and
__bool__ customize how instances are created, displayed, compared and
tested for truth.
''',
    'del': '''The "del" statement
*******************

   del_stmt ::= "del" target_list

Deleting a name removes its binding; deleting an attribute, item or
slice calls __delattr__, __delitem__ respectively.
''',
    'dict': '''Dictionary displays
*******************

   dict_display ::= "{" [dict_item_list | dict_comprehension] "}"

A dictionary display creates a new dict from key/value pairs, "**"
unpackings or a comprehension.
''',
    'exceptions': '''Exceptions
**********

Exceptions break out of the normal flow of control when an error or
another exceptional condition occurs.  They are raised with "raise"
and handled with "try" ... "except".
''',
    'execmodel': '''Execution model
***************

A Python program is built from code blocks (modules, function bodies,
class definitions).  Names are bound in namespaces; scoping is lexical,
resolved from the local block outwards to the globals and builtins.
''',
    'exprlists': '''Expression lists
****************

An expression list containing at least one comma yields a tuple; "*"
unpacks an iterable into the tuple.
''',
    'floating': '''Floating-point literals
***********************

   floatnumber ::= pointfloat | exponentfloat

Examples: 3.14, 10., .001, 1e100, 3.14_15_93.
''',
    'for': '''The "for" statement
*******************

   for_stmt ::= "for" target_list "in" starred_list ":" suite
                ["else" ":" suite]

The iterable is evaluated once; the suite runs once per item, with the
item assigned to the target list.  The optional "else" suite runs when
the loop finishes without "break".
''',
    'formatstrings': '''Format String Syntax
********************

str.format() and f-strings replace fields in braces, "{name!conv:spec}",
with formatted values; the format specification mini-language controls
alignment, width, precision and type.
''',
    'function': '''Function definitions
********************

   funcdef ::= [decorators] "def" funcname [type_params] "(" [parameter_list] ")"
               ["->" expression] ":" suite

A function definition creates a function object and binds it to the
function name.  The body runs each time the function is called.
''',
    'global': '''The "global" statement
**********************

   global_stmt ::= "global" identifier ("," identifier)*

The listed names refer to the module's global namespace for the
current code block.
''',
    'id-classes': '''Reserved classes of identifiers
*******************************

"_*" names are not imported by "from module import *"; "__*__" names
are defined by the system; "__*" names in classes are mangled to avoid
clashes with subclasses.
''',
    'identifiers': '''Identifiers and keywords
************************

Identifiers (names) start with a letter or underscore followed by
letters, digits or underscores (Unicode letters are allowed).  Keywords
cannot be used as identifiers.
''',
    'if': '''The "if" statement
******************

   if_stmt ::= "if" assignment_expression ":" suite
               ("elif" assignment_expression ":" suite)*
               ["else" ":" suite]

The first suite whose condition is true runs; otherwise the "else"
suite, if present.
''',
    'imaginary': '''Imaginary literals
******************

An imaginary literal (e.g. 3.14j) yields a complex number with real
part 0.0.
''',
    'import': '''The "import" statement
**********************

   import_stmt ::= "import" module ["as" identifier] ("," module ["as" identifier])*
                   | "from" relative_module "import" identifier ["as" identifier] ...

"import" finds and loads a module (initializing it if needed) and binds
names in the local namespace.  "lazy import" defers loading until the
name is first used.
''',
    'in': '''Membership test operations
**************************

"x in s" is True if x is a member of s ("__contains__"); "not in" is
the negation.
''',
    'integers': '''Integer literals
****************

Integer literals are decimal (123), hexadecimal (0x7f), octal (0o17)
or binary (0b101); underscores may group digits.  Integers have
unlimited precision.
''',
    'lambda': '''Lambdas
*******

   lambda_expr ::= "lambda" [parameter_list] ":" expression

A lambda expression creates an anonymous function whose body is a
single expression.
''',
    'lists': '''List displays
*************

   list_display ::= "[" [flexible_expression_list | comprehension] "]"

A list display creates a new list from its items or a comprehension.
''',
    'naming': '''Naming and binding
******************

Names are bound by assignment, imports, function and class definitions,
"for" targets, "as" clauses and "del".  A name bound in a block is local
to it unless declared "global" or "nonlocal".
''',
    'nonlocal': '''The "nonlocal" statement
************************

   nonlocal_stmt ::= "nonlocal" identifier ("," identifier)*

The listed names refer to bindings in the nearest enclosing function
scope.
''',
    'numbers': '''Numeric literals
****************

There are integers, floating-point numbers and imaginary numbers;
complex numbers are formed by adding a real number and an imaginary
one.
''',
    'numeric-types': '''Emulating numeric types
***********************

Methods such as __add__, __sub__, __mul__, __truediv__, their reflected
(__radd__, ...) and in-place (__iadd__, ...) variants, and __neg__,
__abs__, __int__, __float__, __index__ implement numeric behaviour.
''',
    'objects': '''Objects, values and types
*************************

Every object has an identity, a type and a value.  The type determines
the operations it supports; mutable objects can change their value.
''',
    'operator-summary': '''Operator precedence
*******************

From lowest to highest precedence:

   ":="                          assignment expression
   "lambda"                      lambda expression
   "if" - "else"                 conditional expression
   "or"                          boolean OR
   "and"                         boolean AND
   "not x"                       boolean NOT
   "in", "not in", "is", "is not", "<", "<=", ">", ">=", "!=", "=="
                                 comparisons and identity/membership tests
   "|"                           bitwise OR
   "^"                           bitwise XOR
   "&"                           bitwise AND
   "<<", ">>"                    shifts
   "+", "-"                      addition and subtraction
   "*", "@", "/", "//", "%"      multiplication, matrix multiplication,
                                 division, floor division, remainder
   "+x", "-x", "~x"              positive, negative, bitwise NOT
   "**"                          exponentiation
   "await x"                     await expression
   "x[index]", "x(args...)", "x.attribute"
                                 subscription, slicing, call, attribute
   "(...)", "[...]", "{...}"     displays

Operators in the same box group left to right, except exponentiation
and conditional expressions, which group from right to left.

Notes: The power operator "**" binds less tightly than an arithmetic or
bitwise unary operator on its right, so "2**-1" is 0.5.  The "%"
operator is also used for string formatting.
''',
    'pass': '''The "pass" statement
********************

"pass" does nothing; it is a placeholder where a statement is required
syntactically.
''',
    'power': '''The power operator
******************

   power ::= (await_expr | primary) ["**" u_expr]

The power operator binds more tightly than unary operators on its
left and less tightly than unary operators on its right.
''',
    'raise': '''The "raise" statement
*********************

   raise_stmt ::= "raise" [expression ["from" expression]]

"raise" without arguments re-raises the exception being handled;
otherwise the given exception (class or instance) is raised, optionally
chained to another with "from".
''',
    'return': '''The "return" statement
**********************

   return_stmt ::= "return" [expression_list]

"return" leaves the current function call with the given value (or
None).
''',
    'sequence-types': '''Emulating container types
*************************

__len__, __getitem__, __setitem__, __delitem__, __iter__, __reversed__
and __contains__ implement sequences and mappings.
''',
    'shifting': '''Shifting operations
*******************

"<<" and ">>" shift integers left or right by the given number of
bits.
''',
    'slicings': '''Slicings
********

A slicing ("a[start:stop:step]") selects a range of items of a
sequence; it calls __getitem__ with a slice object.
''',
    'specialattrs': '''Special Attributes
******************

Objects have special attributes such as __dict__, __class__,
__name__, __qualname__, __module__, __doc__ and __mro__.
''',
    'specialnames': '''Special method names
********************

A class can implement operations invoked by special syntax (arithmetic,
subscripting, ...) by defining methods with special names such as
__add__ or __getitem__.
''',
    'string-methods': '''String Methods
**************

Strings implement the common sequence operations plus methods such as
str.split(), str.join(), str.replace(), str.strip(), str.format(),
str.startswith() and str.upper().
''',
    'strings': '''String and Bytes literals
*************************

String literals are written in single or double quotes, or tripled
quotes for multi-line strings.  Prefixes: "r" (raw), "b" (bytes),
"f" (formatted string literal), "t" (template string literal).
Backslash escapes such as "\\n" and "\\x41" are processed unless the
literal is raw.
''',
    'subscriptions': '''Subscriptions
*************

"a[key]" selects an item of a sequence or mapping by calling
__getitem__ (or __class_getitem__ on classes).
''',
    'truth': '''Truth Value Testing
*******************

Any object can be tested for truth; by default it is true unless its
class defines __bool__ returning False or __len__ returning zero.
''',
    'try': '''The "try" statement
*******************

   try_stmt ::= try1_stmt | try2_stmt | try3_stmt

"try" runs its suite and dispatches exceptions to the first matching
"except" (or "except*") clause; the "else" clause runs if no exception
occurred and the "finally" clause always runs last.
''',
    'types': '''The standard type hierarchy
***************************

Below is a summary of the built-in types:

None, NotImplemented, Ellipsis
   Singletons.

numbers.Number
   int, bool, float and complex.

Sequences
   Immutable: str, tuple, bytes.  Mutable: list, bytearray.

Set types
   set and frozenset.

Mappings
   dict.

Callable types
   Functions, methods, generators, coroutines, built-in functions and
   classes.

Modules, custom classes, class instances, I/O objects and internal
types (code, frame, traceback and slice objects).
''',
    'typesfunctions': '''Functions
*********

Function objects are created by function definitions; the only
operation on them is calling them.
''',
    'typesmapping': '''Mapping Types --- dict
**********************

A mapping maps hashable keys to arbitrary objects.  The dict type
supports d[key], d[key] = value, del d[key], key in d, len(d),
iteration and methods such as get(), items(), keys(), values(),
update(), pop() and setdefault().
''',
    'typesmethods': '''Methods
*******

Methods are functions called with attribute notation; bound methods
remember their instance in __self__ and the function in __func__.
''',
    'typesmodules': '''Modules
*******

The only special operation on a module is attribute access; its
namespace is the dictionary __dict__.
''',
    'typesseq': '''Sequence Types --- list, tuple, range
*************************************

Sequences support "x in s", concatenation, repetition, indexing,
slicing, len(), min(), max(), s.index() and s.count().
''',
    'typesseq-mutable': '''Mutable Sequence Types
**********************

Mutable sequences support item and slice assignment and deletion and
the methods append(), extend(), insert(), pop(), remove(), reverse(),
clear() and copy().
''',
    'unary': '''Unary arithmetic and bitwise operations
***************************************

"-x" negates, "+x" leaves unchanged and "~x" inverts the bits of an
integer.
''',
    'while': '''The "while" statement
*********************

   while_stmt ::= "while" assignment_expression ":" suite
                  ["else" ":" suite]

The suite runs repeatedly as long as the condition is true; the
optional "else" suite runs when the loop ends without "break".
''',
    'with': '''The "with" statement
********************

   with_stmt ::= "with" ( "(" with_stmt_contents ","? ")" | with_stmt_contents ) ":" suite

The "with" statement wraps the execution of a block with methods
defined by a context manager: __enter__ is called on entry (its result
is bound to the "as" target) and __exit__ on exit, even if an
exception occurred.
''',
    'yield': '''The "yield" statement
*********************

"yield" in a function body makes the function a generator; each
"yield" hands a value to the consumer and suspends the generator.
''',
    'match': '''The "match" statement
*********************

   match_stmt ::= "match" subject_expr ":" NEWLINE INDENT case_block+ DEDENT

The subject is matched against the patterns of the "case" blocks in
order; the first matching block runs.
''',
    'lazy': '''Lazy imports
************

"lazy import module" and "lazy from module import name" bind names
whose module is only loaded when the name is first used.
''',
    'conditional': '''Conditional expressions
***********************

"x if C else y" evaluates C, then x if C is true, otherwise y.
''',
    'formatting': '''printf-style String Formatting
******************************

"format % values" replaces conversion specifications such as "%s",
"%d" and "%.2f" in the string with the values.
''',
}
