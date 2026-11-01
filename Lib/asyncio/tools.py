"""Inspect the asyncio tasks of another process ('python -m asyncio ps' and
'pstree').

The raw data comes from _remote_debugging.RemoteUnwinder(pid)
.get_all_awaited_by(): per thread, the pending tasks with their coroutine
stacks and the (task, call stack) pairs awaiting them.  build_task_table()
and build_async_tree() turn it into rows and tree lines.
"""

import sys

__all__ = ('CycleFoundException', 'build_async_tree', 'build_task_table',
           'display_awaited_by_tasks_table', 'display_awaited_by_tasks_tree')


class CycleFoundException(Exception):
    """The await graph has cycles (lists of task ids, the first id
    repeated at the end), so it cannot be drawn as a tree."""

    def __init__(self, cycles, id2name):
        super().__init__(cycles, id2name)
        self.cycles = cycles
        self.id2name = id2name


def _tasks(result):
    # (thread id, task info) in input order
    for awaited_info in result:
        for task in awaited_info.awaited_by:
            yield awaited_info.thread_id, task


def _frame_text(frame):
    text = frame.funcname
    filename = frame.filename
    if filename and filename != "~":
        text = f"{text} {filename}"
        location = frame.location
        lineno = getattr(location, "lineno", None) if location is not None \
            else None
        if lineno:
            text = f"{text}:{lineno}"
    return text


def _chain(call_stack):
    return " -> ".join(frame.funcname for frame in call_stack)


def build_task_table(result):
    """One row per (task, awaiter): [thread id, task id (hex), task name,
    coroutine stack, awaiter call chain, awaiter name, awaiter id (hex)].
    A task nobody awaits gets one row with empty awaiter columns and
    awaiter id '0x0'."""
    id2name = {task.task_id: task.task_name for _, task in _tasks(result)}
    rows = []
    for thread_id, task in _tasks(result):
        coro_stack = " -> ".join(
            _chain(coro.call_stack) for coro in task.coroutine_stack)
        head = [thread_id, hex(task.task_id), task.task_name, coro_stack]
        if not task.awaited_by:
            rows.append(head + ["", "", "0x0"])
            continue
        for coro in task.awaited_by:
            awaiter = coro.task_name
            rows.append(head + [_chain(coro.call_stack),
                                id2name.get(awaiter, "Unknown"),
                                hex(awaiter) if isinstance(awaiter, int)
                                else str(awaiter)])
    return rows


def _find_cycles(edges):
    # depth-first search; edges: awaiter id -> [awaited ids] (ordered)
    cycles = []
    state = {}          # 1: on the current path, 2: finished
    path = []

    def visit(node):
        state[node] = 1
        path.append(node)
        for nxt in edges.get(node, ()):
            mark = state.get(nxt)
            if mark == 1:
                cycles.append(path[path.index(nxt):] + [nxt])
            elif mark is None:
                visit(nxt)
        path.pop()
        state[node] = 2

    for node in list(edges):
        if node not in state:
            visit(node)
    return cycles


class _Node:
    __slots__ = ('label', 'children')

    def __init__(self, label):
        self.label = label
        self.children = {}      # key -> _Node, in insertion order

    def child(self, key, label):
        node = self.children.get(key)
        if node is None:
            node = self.children[key] = _Node(label)
        return node


def _render(node, prefix, last, lines):
    lines.append(prefix + ("└── " if last else "├── ") + node.label)
    inner = prefix + ("    " if last else "│   ")
    kids = list(node.children.values())
    for i, kid in enumerate(kids):
        _render(kid, inner, i == len(kids) - 1, lines)


def build_async_tree(result, task_emoji="(T)", cor_emoji=""):
    """One list of tree lines per task that nothing awaits.

    Below a task come the coroutine frames (outermost first) through
    which it awaits other tasks, then those tasks, and so on; a task
    awaited from several places appears under each of them.  Raises
    CycleFoundException if tasks await each other in a cycle.
    """
    tasks = list(_tasks(result))
    id2name = {task.task_id: task.task_name for _, task in tasks}
    edges = {}          # awaiter id -> [(call stack, awaited task id)]
    for _, task in tasks:
        for coro in task.awaited_by:
            edges.setdefault(coro.task_name, []).append(
                (coro.call_stack, task.task_id))
    cycles = _find_cycles({awaiter: [child for _, child in pairs]
                           for awaiter, pairs in edges.items()})
    if cycles:
        raise CycleFoundException(cycles, id2name)
    own_stacks = {task.task_id: task.coroutine_stack for _, task in tasks}

    def task_label(task_id):
        return f"{task_emoji} {id2name.get(task_id, task_id)}"

    def expand(node, task_id):
        for coro in own_stacks.get(task_id, ()):
            parent = node
            for frame in reversed(coro.call_stack):
                text = _frame_text(frame)
                parent = parent.child(('frame', text),
                                      f"{cor_emoji} {text}")
        for call_stack, child in edges.get(task_id, ()):
            parent = node
            for frame in reversed(call_stack):
                text = _frame_text(frame)
                parent = parent.child(('frame', text),
                                      f"{cor_emoji} {text}")
            sub = parent.child(('task', child), task_label(child))
            expand(sub, child)

    trees = []
    for _, task in tasks:
        if task.awaited_by:
            continue
        root = _Node(task_label(task.task_id))
        expand(root, task.task_id)
        lines = []
        _render(root, "", True, lines)
        trees.append(lines)
    return trees


def _get_awaited_by(pid):
    import _remote_debugging
    unwinder = _remote_debugging.RemoteUnwinder(pid)
    return unwinder.get_all_awaited_by()


def _fail(message, exc=None):
    if exc is not None:
        message = f"{message}: {exc}"
    print(message, file=sys.stderr)
    sys.exit(1)


def _awaited_by_or_exit(pid):
    try:
        return _get_awaited_by(pid)
    except PermissionError as exc:
        _fail(f"Permission denied while inspecting process {pid}; run "
              f"with elevated privileges", exc)
    except ProcessLookupError as exc:
        _fail(f"No process with PID {pid}", exc)
    except (RuntimeError, OSError, ImportError) as exc:
        _fail(f"Error retrieving tasks of process {pid}", exc)


def display_awaited_by_tasks_table(pid):
    """Print a table of the pending tasks of process *pid*."""
    rows = build_task_table(_awaited_by_or_exit(pid))
    headers = ("tid", "task id", "task name", "coroutine stack",
               "awaiter chain", "awaiter name", "awaiter id")
    table = [list(map(str, headers))] + [list(map(str, row)) for row in rows]
    widths = [max(len(row[i]) for row in table) for i in range(len(headers))]
    for n, row in enumerate(table):
        print("  ".join(cell.ljust(width)
                        for cell, width in zip(row, widths)).rstrip())
        if n == 0:
            print("  ".join("-" * width for width in widths))


def display_awaited_by_tasks_tree(pid):
    """Print the await trees of the pending tasks of process *pid*."""
    result = _awaited_by_or_exit(pid)
    try:
        trees = build_async_tree(result)
    except CycleFoundException as exc:
        print("ERROR: await-graph contains cycles - cannot print a tree!",
              file=sys.stderr)
        for cycle in exc.cycles:
            names = " → ".join(str(exc.id2name.get(tid, hex(tid)))
                               for tid in cycle)
            print(f"cycle: {names}", file=sys.stderr)
        sys.exit(1)
    for lines in trees:
        print("\n".join(lines))
