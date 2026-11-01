"""Support for TestCase.assertLogs() and assertNoLogs()."""

import logging
import collections

from .case import _BaseTestCaseContext

__unittest = True

_LoggingWatcher = collections.namedtuple('_LoggingWatcher',
                                         ['records', 'output'])


class _CapturingHandler(logging.Handler):
    """Handler collecting records and their formatted text."""

    def __init__(self):
        logging.Handler.__init__(self)
        self.watcher = _LoggingWatcher([], [])

    def flush(self):
        pass

    def emit(self, record):
        self.watcher.records.append(record)
        self.watcher.output.append(self.format(record))


class _AssertLogsContext(_BaseTestCaseContext):
    """Temporarily replaces the handlers of a logger by a capturing one."""

    LOGGING_FORMAT = '%(levelname)s:%(name)s:%(message)s'

    def __init__(self, test_case, logger_name, level, no_logs,
                 formatter=None):
        _BaseTestCaseContext.__init__(self, test_case)
        self.logger_name = logger_name
        if level:
            self.level = logging._nameToLevel.get(level, level)
        else:
            self.level = logging.INFO
        self.msg = None
        self.no_logs = no_logs
        self.formatter = formatter

    def __enter__(self):
        if isinstance(self.logger_name, logging.Logger):
            logger = self.logger_name
        else:
            logger = logging.getLogger(self.logger_name)
        self.logger = logger
        handler = _CapturingHandler()
        handler.setLevel(self.level)
        handler.setFormatter(self.formatter or
                             logging.Formatter(self.LOGGING_FORMAT))
        self.watcher = handler.watcher
        self.old_handlers = logger.handlers[:]
        self.old_level = logger.level
        self.old_propagate = logger.propagate
        logger.handlers = [handler]
        logger.setLevel(self.level)
        logger.propagate = False
        if self.no_logs:
            return None
        return handler.watcher

    def __exit__(self, exc_type, exc_value, tb):
        logger = self.logger
        logger.handlers = self.old_handlers
        logger.propagate = self.old_propagate
        logger.setLevel(self.old_level)
        if exc_type is not None:
            return False
        if self.no_logs:
            if self.watcher.records:
                self._raiseFailure('Unexpected logs found: {!r}'.format(
                    self.watcher.output))
        elif not self.watcher.records:
            self._raiseFailure(
                'no logs of level {} or higher triggered on {}'.format(
                    logging.getLevelName(self.level), logger.name))
