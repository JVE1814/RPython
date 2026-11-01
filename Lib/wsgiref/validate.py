"""Middleware that checks a WSGI application (and its server) for
conformance with PEP 3333.

    from wsgiref.validate import validator
    app = validator(app)

Violations raise AssertionError; questionable but legal usage emits a
WSGIWarning.
"""

__all__ = ['validator']

import re
import sys
import warnings

_header_name_re = re.compile(r'^[a-zA-Z][a-zA-Z0-9\-_]*$')
_bad_header_value_re = re.compile(r'[\000-\037]')


class WSGIWarning(Warning):
    """Raised (as a warning) for questionable WSGI usage."""


def _check(condition, *args):
    if not condition:
        raise AssertionError(*args)


def check_string_type(value, title):
    if type(value) is str:
        return value
    raise AssertionError(f'{title} must be of type str (got {value!r})')


def validator(application):
    """Wrap *application* so that both it and the server calling it are
    checked."""

    def lint_app(*args, **kw):
        _check(len(args) == 2, 'Two arguments required')
        _check(not kw, 'No keyword arguments allowed')
        environ, start_response = args

        check_environ(environ)

        # becomes non-empty once the application called start_response
        started = []

        def start_response_wrapper(*args, **kw):
            _check(len(args) in (2, 3),
                   f'Invalid number of arguments: {args}')
            _check(not kw, 'No keyword arguments allowed')
            status = args[0]
            headers = args[1]
            exc_info = args[2] if len(args) == 3 else None

            check_status(status)
            check_headers(headers)
            check_content_type(status, headers)
            check_exc_info(exc_info)

            started.append(None)
            return WriteWrapper(start_response(*args))

        environ['wsgi.input'] = InputWrapper(environ['wsgi.input'])
        environ['wsgi.errors'] = ErrorWrapper(environ['wsgi.errors'])

        iterator = application(environ, start_response_wrapper)
        _check(iterator is not None and iterator != False,
               'The application must return an iterator, if only an empty '
               'list')
        check_iterator(iterator)
        return IteratorWrapper(iterator, started)

    return lint_app


class InputWrapper:

    def __init__(self, wsgi_input):
        self.input = wsgi_input

    def read(self, *args):
        _check(len(args) == 1)
        data = self.input.read(*args)
        _check(type(data) is bytes)
        return data

    def readline(self, *args):
        _check(len(args) <= 1)
        data = self.input.readline(*args)
        _check(type(data) is bytes)
        return data

    def readlines(self, *args):
        _check(len(args) <= 1)
        lines = self.input.readlines(*args)
        _check(type(lines) is list)
        for line in lines:
            _check(type(line) is bytes)
        return lines

    def __iter__(self):
        while True:
            line = self.readline()
            if not line:
                return
            yield line

    def close(self):
        _check(0, 'input.close() must not be called')


class ErrorWrapper:

    def __init__(self, wsgi_errors):
        self.errors = wsgi_errors

    def write(self, s):
        _check(type(s) is str)
        self.errors.write(s)

    def flush(self):
        self.errors.flush()

    def writelines(self, seq):
        for line in seq:
            self.write(line)

    def close(self):
        _check(0, 'errors.close() must not be called')


class WriteWrapper:

    def __init__(self, wsgi_writer):
        self.writer = wsgi_writer

    def __call__(self, s):
        _check(type(s) is bytes)
        self.writer(s)


class PartialIteratorWrapper:

    def __init__(self, wsgi_iterator):
        self.iterator = wsgi_iterator

    def __iter__(self):
        return IteratorWrapper(self.iterator, None)


class IteratorWrapper:

    def __init__(self, wsgi_iterator, check_start_response):
        self.original_iterator = wsgi_iterator
        self.iterator = iter(wsgi_iterator)
        self.closed = False
        self.check_start_response = check_start_response

    def __iter__(self):
        return self

    def __next__(self):
        _check(not self.closed, 'Iterator read after closed')
        value = next(self.iterator)
        if type(value) is not bytes:
            _check(False, f'Iterator yielded non-bytestring ({value!r})')
        if self.check_start_response is not None:
            _check(self.check_start_response,
                   'The application returns and we started iterating over '
                   'its body, but start_response has not yet been called')
            self.check_start_response = None
        return value

    def close(self):
        self.closed = True
        if hasattr(self.original_iterator, 'close'):
            self.original_iterator.close()

    def __del__(self):
        if not self.closed:
            sys.stderr.write('Iterator garbage collected without being '
                             'closed')
        _check(self.closed,
               'Iterator garbage collected without being closed')


def check_environ(environ):
    _check(type(environ) is dict,
           f'Environment is not of the right type: {type(environ)!r} '
           f'(environment: {environ!r})')

    for key in ('REQUEST_METHOD', 'SERVER_NAME', 'SERVER_PORT',
                'wsgi.version', 'wsgi.input', 'wsgi.errors',
                'wsgi.multithread', 'wsgi.multiprocess', 'wsgi.run_once'):
        _check(key in environ,
               f'Environment missing required key: {key!r}')

    for key in ('HTTP_CONTENT_TYPE', 'HTTP_CONTENT_LENGTH'):
        _check(key not in environ,
               f'Environment should not have the key: {key} (use '
               f'{key[5:]} instead)')

    if 'QUERY_STRING' not in environ:
        warnings.warn('QUERY_STRING is not in the WSGI environment; the cgi '
                      'module will use sys.argv when this variable is '
                      'missing, so application errors are more likely',
                      WSGIWarning)

    for key in environ:
        if '.' in key:
            continue        # extension variables may have any type
        _check(type(environ[key]) is str,
               f'Environmental variable {key} is not a string: '
               f'{type(environ[key])!r} (value: {environ[key]!r})')

    _check(type(environ['wsgi.version']) is tuple,
           f"wsgi.version should be a tuple ({environ['wsgi.version']!r})")
    _check(environ['wsgi.url_scheme'] in ('http', 'https'),
           f"wsgi.url_scheme unknown: {environ['wsgi.url_scheme']!r}")

    check_input(environ['wsgi.input'])
    check_errors(environ['wsgi.errors'])

    if environ['REQUEST_METHOD'] not in ('GET', 'HEAD', 'POST', 'OPTIONS',
                                         'PATCH', 'PUT', 'DELETE', 'TRACE'):
        warnings.warn(f"Unknown REQUEST_METHOD: {environ['REQUEST_METHOD']!r}",
                      WSGIWarning)

    _check(not environ.get('SCRIPT_NAME') or
           environ['SCRIPT_NAME'].startswith('/'),
           f"SCRIPT_NAME doesn't start with /: {environ['SCRIPT_NAME']!r}")
    _check(not environ.get('PATH_INFO') or
           environ['PATH_INFO'].startswith('/'),
           f"PATH_INFO doesn't start with /: {environ['PATH_INFO']!r}")
    if environ.get('CONTENT_LENGTH'):
        _check(int(environ['CONTENT_LENGTH']) >= 0,
               f"Invalid CONTENT_LENGTH: {environ['CONTENT_LENGTH']!r}")

    if not environ.get('SCRIPT_NAME'):
        _check('PATH_INFO' in environ,
               'One of SCRIPT_NAME or PATH_INFO are required (PATH_INFO '
               'should at least be "/" if SCRIPT_NAME is empty)')
    _check(environ.get('SCRIPT_NAME') != '/',
           'SCRIPT_NAME cannot be "/"; it should instead be "", and '
           'PATH_INFO should be "/"')


def check_input(wsgi_input):
    for attr in ('read', 'readline', 'readlines', '__iter__'):
        _check(hasattr(wsgi_input, attr),
               f"wsgi.input ({wsgi_input!r}) doesn't have the attribute "
               f"{attr}")


def check_errors(wsgi_errors):
    for attr in ('flush', 'write', 'writelines'):
        _check(hasattr(wsgi_errors, attr),
               f"wsgi.errors ({wsgi_errors!r}) doesn't have the attribute "
               f"{attr}")


def check_status(status):
    status = check_string_type(status, 'Status')
    _check(len(status) >= 4, 'Status must be at least 4 characters')
    _check(status[:3].isdigit(), 'Status message must begin w/3-digit code')
    _check(status[3] == ' ', 'Status message must have a space after code')
    code = int(status[:3])
    _check(code >= 100, f'Status code is invalid: {code!r}')


def check_headers(headers):
    _check(type(headers) is list,
           f'Headers ({headers!r}) must be of type list: {type(headers)!r}')
    for item in headers:
        _check(type(item) is tuple,
               f'Individual headers ({item!r}) must be of type tuple: '
               f'{type(item)!r}')
        _check(len(item) == 2)
        name, value = item
        name = check_string_type(name, 'Header name')
        value = check_string_type(value, 'Header value')
        _check(name.lower() != 'status',
               'The Status header cannot be used; it conflicts with CGI '
               'script, and HTTP status is not given through headers '
               f'(value: {value!r}).')
        _check('\n' not in name and ':' not in name,
               f"Header names may not contain ':' or '\\n': {name!r}")
        _check(_header_name_re.search(name), f'Bad header name: {name!r}')
        _check(not name.endswith(('-', '_')),
               f"Names may not end in '-' or '_': {name!r}")
        bad = _bad_header_value_re.search(value)
        if bad:
            _check(0, f'Bad header value: {value!r} (bad char: '
                      f'{bad.group(0)!r})')


def check_content_type(status, headers):
    status = check_string_type(status, 'Status')
    code = int(status.split(None, 1)[0])
    no_body = (204, 304)
    for name, value in headers:
        name = check_string_type(name, 'Header name')
        if name.lower() == 'content-type':
            if code not in no_body:
                return
            _check(0, f'Content-Type header found in a {code} response, '
                      'which must not return content.')
    if code not in no_body:
        _check(0, f'No Content-Type header found in headers ({headers})')


def check_exc_info(exc_info):
    _check(exc_info is None or type(exc_info) is tuple,
           f'exc_info ({exc_info!r}) is not a tuple: {type(exc_info)!r}')


def check_iterator(iterator):
    _check(not isinstance(iterator, (str, bytes)),
           'You should not return a string as your application iterator, '
           'instead return a single-item list containing a bytestring.')
