"""WSGI (PEP 3333) utilities and a reference server.

* util: URL reconstruction, environ defaults, FileWrapper
* headers: a mutable view of response headers
* handlers: base classes for running WSGI applications (CGI and others)
* simple_server: a small single-threaded HTTP server for WSGI apps
* validate: middleware checking an application and its server for
  conformance
* types: typing protocols for WSGI
"""
