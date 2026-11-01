# Running "python -m _pyrepl" starts the REPL; keep this module tiny since
# its globals become the REPL's namespace.
if __name__ == "__main__":
    from .main import interactive_console as __start_repl
    __start_repl()
