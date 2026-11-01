"""Produce an SQL text dump of an SQLite database (Connection.iterdump)."""


def _quote_name(name):
    return '"{}"'.format(name.replace('"', '""'))


def _quote_value(value):
    return "'{}'".format(value.replace("'", "''"))


def _iterdump(connection, *, filter=None):
    """Yield the SQL statements that recreate the database of `connection`.

    `filter` is an optional LIKE pattern restricting the dumped objects
    (tables, indexes, triggers and views) by name.
    """
    cu = connection.cursor()
    # the dump must not depend on the connection's row factory
    cu.row_factory = None

    if filter is None:
        name_clause = ""
        params = ()
    else:
        name_clause = 'AND "name" LIKE ?'
        params = (filter,)

    tables = cu.execute(
        'SELECT "name", "type", "sql" FROM "sqlite_master" '
        f"""WHERE "sql" NOT NULL AND "type" == 'table' {name_clause} """
        'ORDER BY "name"', params).fetchall()

    # Disable foreign key enforcement while restoring if any table uses them.
    for name, _, _ in tables:
        if name.startswith("sqlite_"):
            continue
        fks = cu.execute(
            "SELECT COUNT(*) FROM pragma_foreign_key_list(?)", (name,)).fetchone()
        if fks and fks[0]:
            yield "PRAGMA foreign_keys=OFF;"
            break

    yield "BEGIN TRANSACTION;"

    writable_schema = False
    sequences = []
    for name, _, sql in tables:
        if name == "sqlite_sequence":
            sequences = cu.execute(
                'SELECT "name", "seq" FROM "sqlite_sequence"').fetchall()
            continue
        if name == "sqlite_stat1":
            yield 'ANALYZE "sqlite_master";'
            continue
        if name.startswith("sqlite_"):
            continue
        if sql.startswith("CREATE VIRTUAL TABLE"):
            if not writable_schema:
                writable_schema = True
                yield "PRAGMA writable_schema=ON;"
            yield ("INSERT INTO sqlite_master(type,name,tbl_name,rootpage,sql)"
                   "VALUES('table',{0},{0},0,{1});".format(
                       _quote_value(name), _quote_value(sql)))
            continue
        yield f"{sql};"

        # the rows, rendered by SQLite's quote() function
        columns = [info[1] for info in
                   cu.execute(f"PRAGMA table_info({_quote_name(name)})").fetchall()]
        if not columns:
            continue
        values = "||','||".join(f"quote({_quote_name(c)})" for c in columns)
        query = ("SELECT 'INSERT INTO ' || {0} || ' VALUES(' || {1} || ')' "
                 "FROM {2}").format(
                     _quote_value(_quote_name(name)), values, _quote_name(name))
        for (statement,) in cu.execute(query).fetchall():
            yield f"{statement};"

    if writable_schema:
        yield "PRAGMA writable_schema=OFF;"

    if sequences:
        yield 'DELETE FROM "sqlite_sequence";'
        for name, seq in sequences:
            yield ('INSERT INTO "sqlite_sequence" VALUES({},{});'
                   .format(_quote_value(name), seq))

    others = cu.execute(
        'SELECT "name", "type", "sql" FROM "sqlite_master" '
        f"""WHERE "sql" NOT NULL AND "type" IN ('index', 'trigger', 'view') {name_clause}""",
        params).fetchall()
    for _, _, sql in others:
        yield f"{sql};"

    cu.close()
    yield "COMMIT;"
