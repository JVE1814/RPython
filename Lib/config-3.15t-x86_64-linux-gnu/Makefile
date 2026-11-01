# Build configuration of this interpreter, in Makefile syntax.
# sysconfig and python-config style tools read the variable assignments;
# the interpreter itself is built with cargo, not with this file.

VERSION=	3.15
ABIFLAGS=	t
LDVERSION=	$(VERSION)$(ABIFLAGS)
MULTIARCH=	x86_64-linux-gnu
SOABI=		cpython-315t-x86_64-linux-gnu
EXT_SUFFIX=	.cpython-315t-x86_64-linux-gnu.so
SHLIB_SUFFIX=	.so
MACHDEP=	linux
HOST_GNU_TYPE=	x86_64-pc-linux-gnu

prefix=		/usr/local
exec_prefix=	${prefix}
BINDIR=		${exec_prefix}/bin
LIBDIR=		${exec_prefix}/lib
PLATLIBDIR=	lib
INCLUDEDIR=	${prefix}/include
CONFINCLUDEDIR=	$(exec_prefix)/include
INCLUDEPY=	$(INCLUDEDIR)/python$(LDVERSION)
CONFINCLUDEPY=	$(CONFINCLUDEDIR)/python$(LDVERSION)
LIBDEST=	$(prefix)/$(PLATLIBDIR)/python$(LDVERSION)
BINLIBDEST=	$(exec_prefix)/$(PLATLIBDIR)/python$(LDVERSION)
DESTSHARED=	$(BINLIBDEST)/lib-dynload
LIBPL=		$(LIBDEST)/config-$(LDVERSION)-$(MULTIARCH)
LIBPC=		$(LIBDIR)/pkgconfig

CC=		cc
CXX=		c++
AR=		ar
ARFLAGS=	rcs
OPT=		-DNDEBUG -O2
CFLAGS=		-O2 -Wall
CPPFLAGS=	-I. -I./Include
LDFLAGS=
CCSHARED=	-fPIC
LDSHARED=	$(CC) -shared $(LDFLAGS)
BLDSHARED=	$(CC) -shared $(LDFLAGS)
PY_CFLAGS=	$(CFLAGS)
PY_CPPFLAGS=	$(CPPFLAGS)
PY_LDFLAGS=	$(LDFLAGS)
LIBS=		-ldl -lpthread
LIBRARY=	libpython$(LDVERSION).a
LDLIBRARY=	libpython$(LDVERSION).a
INSTSONAME=	$(LDLIBRARY)
EXE=

Py_GIL_DISABLED=	1
Py_DEBUG=	0
Py_ENABLE_SHARED=	0
