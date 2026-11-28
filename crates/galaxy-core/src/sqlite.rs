//! SQLite database tabs: list tables and views, page through rows, edit
//! cells, insert and delete rows, and run SQL.
//!
//! Rows of a table are addressed by a key: `rowid` for ordinary tables and
//! the primary key columns for `WITHOUT ROWID` tables. Views have no key and
//! are read-only. Every write is a single statement or one transaction, so a
//! failed edit leaves the file unchanged.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::types::{ToSqlOutput, ValueRef};
use rusqlite::{Connection, OpenFlags, ToSql};

/// First 16 bytes of every SQLite 3 database file.
pub const HEADER: &[u8; 16] = b"SQLite format 3\0";

/// Blob cells shown in a page keep at most this many bytes; the rest is
/// only counted. Blob keys are always kept whole.
const BLOB_PREVIEW_BYTES: usize = 256;

/// Whether `path` is a SQLite 3 database, judged by its header. An empty
/// file is not, even though SQLite would accept it.
pub fn is_database_file(path: &Path) -> bool {
    use std::io::Read;
    let mut header = [0u8; 16];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut header))
        .is_ok_and(|()| &header == HEADER)
}

/// One SQLite value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    /// `bytes` may be a prefix of the stored blob; `len` is the full size.
    Blob {
        len: u64,
        bytes: Vec<u8>,
    },
}

impl Value {
    fn from_ref(value: ValueRef<'_>, preview: bool) -> Self {
        match value {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(value) => Value::Integer(value),
            ValueRef::Real(value) => Value::Real(value),
            ValueRef::Text(text) => Value::Text(String::from_utf8_lossy(text).into_owned()),
            ValueRef::Blob(bytes) => Value::Blob {
                len: bytes.len() as u64,
                bytes: if preview {
                    bytes[..bytes.len().min(BLOB_PREVIEW_BYTES)].to_vec()
                } else {
                    bytes.to_vec()
                },
            },
        }
    }

    /// The value for `text` typed over a cell that held `previous`: a
    /// number stays a number when the text parses as one. Otherwise the
    /// column's type affinity decides, as for any SQL text literal.
    pub fn from_input(text: &str, previous: &Value) -> Self {
        match previous {
            Value::Integer(_) => text
                .trim()
                .parse()
                .map(Value::Integer)
                .unwrap_or_else(|_| Value::Text(text.to_owned())),
            Value::Real(_) => text
                .trim()
                .parse()
                .map(Value::Real)
                .unwrap_or_else(|_| Value::Text(text.to_owned())),
            _ => Value::Text(text.to_owned()),
        }
    }
}

impl ToSql for Value {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(match self {
            Value::Null => ToSqlOutput::Borrowed(ValueRef::Null),
            Value::Integer(value) => ToSqlOutput::Borrowed(ValueRef::Integer(*value)),
            Value::Real(value) => ToSqlOutput::Borrowed(ValueRef::Real(*value)),
            Value::Text(text) => ToSqlOutput::Borrowed(ValueRef::Text(text.as_bytes())),
            Value::Blob { len, bytes } => {
                if *len != bytes.len() as u64 {
                    return Err(rusqlite::Error::ToSqlConversionFailure(
                        "a truncated blob preview cannot be written back".into(),
                    ));
                }
                ToSqlOutput::Borrowed(ValueRef::Blob(bytes))
            }
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Table,
    View,
}

/// A table or view from `sqlite_schema`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub name: String,
    pub kind: ObjectKind,
    /// The `CREATE` statement as stored in the schema.
    pub sql: String,
}

/// One row of `PRAGMA table_xinfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub declared_type: String,
    pub not_null: bool,
    pub default_value: Option<String>,
    /// Position in the primary key, from 1; 0 when not part of it.
    pub primary_key: u32,
    /// Generated columns cannot be written.
    pub generated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    pub name: String,
    pub unique: bool,
    pub columns: Vec<String>,
    /// `None` for indexes SQLite creates for `UNIQUE` and `PRIMARY KEY`.
    pub sql: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// Values of the page's `key_columns`; empty for a view.
    pub key: Vec<Value>,
    pub values: Vec<Value>,
}

/// A slice of a table or view.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub columns: Vec<String>,
    /// How rows are addressed for edits: `["rowid"]` (or `_rowid_`/`oid`
    /// when a column takes that name) or the primary key. Empty for views.
    pub key_columns: Vec<String>,
    pub rows: Vec<Row>,
    /// Rows in the whole table or view.
    pub total: u64,
    pub offset: u64,
}

impl Page {
    pub fn editable(&self) -> bool {
        !self.key_columns.is_empty()
    }
}

/// What a SQL run returned: the rows of the last statement that produced
/// any, and the rows changed by the others.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    /// More rows were available than `max_rows`.
    pub truncated: bool,
    pub changes: u64,
    pub statements: u32,
    /// The run may have changed the schema (`CREATE`, `DROP`, `ALTER`).
    pub schema_changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sort {
    pub column: String,
    pub descending: bool,
}

/// An open database file.
pub struct Database {
    connection: Connection,
    path: PathBuf,
    read_only: bool,
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn message(error: rusqlite::Error) -> String {
    match error {
        rusqlite::Error::SqliteFailure(_, Some(message)) => message,
        other => other.to_string(),
    }
}

type Result<T> = std::result::Result<T, String>;

impl Database {
    /// Opens an existing database for reading and writing, or read-only when
    /// the file or its folder cannot be written. Refuses anything without a
    /// SQLite header, so a stray `.db` file is never turned into a database.
    pub fn open(path: &Path) -> Result<Self> {
        if !is_database_file(path) {
            return Err("not a SQLite 3 database".to_owned());
        }
        let flags = OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let writable = Connection::open_with_flags(path, flags | OpenFlags::SQLITE_OPEN_READ_WRITE);
        let (connection, read_only) = match writable {
            Ok(connection) => (connection, false),
            Err(_) => (
                Connection::open_with_flags(path, flags | OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .map_err(message)?,
                true,
            ),
        };
        // Another process (an app server, a migration) may hold a write lock
        // for a moment; wait briefly instead of failing at once.
        connection
            .busy_timeout(Duration::from_secs(2))
            .map_err(message)?;
        let database = Self {
            connection,
            path: path.to_path_buf(),
            read_only,
        };
        // An encrypted or damaged file has a valid header but no readable schema.
        database.objects()?;
        Ok(database)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Tables and views, by name. SQLite's own `sqlite_*` tables are left out.
    pub fn objects(&self) -> Result<Vec<Object>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT name, type, coalesce(sql, '') FROM sqlite_schema \
                 WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
                 ORDER BY name COLLATE NOCASE",
            )
            .map_err(message)?;
        let rows = statement
            .query_map([], |row| {
                let kind: String = row.get(1)?;
                Ok(Object {
                    name: row.get(0)?,
                    kind: if kind == "view" {
                        ObjectKind::View
                    } else {
                        ObjectKind::Table
                    },
                    sql: row.get(2)?,
                })
            })
            .map_err(message)?;
        rows.collect::<rusqlite::Result<_>>().map_err(message)
    }

    fn object(&self, name: &str) -> Result<Object> {
        self.objects()?
            .into_iter()
            .find(|object| object.name == name)
            .ok_or_else(|| format!("no table or view named {name}"))
    }

    pub fn columns(&self, table: &str) -> Result<Vec<Column>> {
        let mut statement = self
            .connection
            .prepare(&format!("PRAGMA table_xinfo({})", quote(table)))
            .map_err(message)?;
        let rows = statement
            .query_map([], |row| {
                let hidden: i64 = row.get(6)?;
                Ok((
                    hidden,
                    Column {
                        name: row.get(1)?,
                        declared_type: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        not_null: row.get::<_, i64>(3)? != 0,
                        default_value: row.get(4)?,
                        primary_key: row.get::<_, i64>(5)? as u32,
                        generated: hidden == 2 || hidden == 3,
                    },
                ))
            })
            .map_err(message)?;
        let mut columns = Vec::new();
        for row in rows {
            let (hidden, column) = row.map_err(message)?;
            // 1 marks the hidden columns of a virtual table.
            if hidden != 1 {
                columns.push(column);
            }
        }
        Ok(columns)
    }

    pub fn indexes(&self, table: &str) -> Result<Vec<Index>> {
        let mut list = self
            .connection
            .prepare(&format!("PRAGMA index_list({})", quote(table)))
            .map_err(message)?;
        let entries: Vec<(String, bool)> = list
            .query_map([], |row| Ok((row.get(1)?, row.get::<_, i64>(2)? != 0)))
            .map_err(message)?
            .collect::<rusqlite::Result<_>>()
            .map_err(message)?;
        let mut indexes = Vec::new();
        for (name, unique) in entries {
            let mut info = self
                .connection
                .prepare(&format!("PRAGMA index_info({})", quote(&name)))
                .map_err(message)?;
            let columns = info
                .query_map([], |row| {
                    Ok(row
                        .get::<_, Option<String>>(2)?
                        .unwrap_or_else(|| "<expression>".to_owned()))
                })
                .map_err(message)?
                .collect::<rusqlite::Result<_>>()
                .map_err(message)?;
            let sql = self
                .connection
                .query_row(
                    "SELECT sql FROM sqlite_schema WHERE type = 'index' AND name = ?1",
                    [&name],
                    |row| row.get(0),
                )
                .unwrap_or(None);
            indexes.push(Index {
                name,
                unique,
                columns,
                sql,
            });
        }
        Ok(indexes)
    }

    /// Columns that address one row of `table`, or empty for a view.
    fn key_columns(&self, object: &Object, columns: &[Column]) -> Vec<String> {
        if object.kind == ObjectKind::View {
            return Vec::new();
        }
        let has_rowid = self
            .connection
            .prepare(&format!(
                "SELECT rowid FROM {} LIMIT 0",
                quote(&object.name)
            ))
            .is_ok();
        if has_rowid {
            // A real column may shadow `rowid`; SQLite then still answers
            // to the other two names unless they are taken as well.
            return ["rowid", "_rowid_", "oid"]
                .into_iter()
                .find(|alias| {
                    !columns
                        .iter()
                        .any(|column| column.name.eq_ignore_ascii_case(alias))
                })
                .map(|alias| vec![alias.to_owned()])
                .unwrap_or_default();
        }
        let mut primary: Vec<_> = columns
            .iter()
            .filter(|column| column.primary_key > 0)
            .collect();
        primary.sort_by_key(|column| column.primary_key);
        primary
            .into_iter()
            .map(|column| column.name.clone())
            .collect()
    }

    /// `limit` rows of `table` from `offset`, in `sort` order or by key.
    pub fn page(&self, table: &str, offset: u64, limit: u64, sort: Option<&Sort>) -> Result<Page> {
        let object = self.object(table)?;
        let columns = self.columns(table)?;
        let key_columns = self.key_columns(&object, &columns);
        let keys: Vec<String> = key_columns.iter().map(|name| quote(name)).collect();
        let selected = keys
            .iter()
            .cloned()
            .chain(std::iter::once("*".to_owned()))
            .collect::<Vec<_>>()
            .join(", ");
        let mut order: Vec<String> = Vec::new();
        if let Some(sort) = sort {
            if !columns.iter().any(|column| column.name == sort.column) {
                return Err(format!("no column named {}", sort.column));
            }
            let direction = if sort.descending { "DESC" } else { "ASC" };
            order.push(format!("{} {direction}", quote(&sort.column)));
        }
        // Ties (and the unsorted case) follow the key so paging is stable.
        order.extend(keys.iter().cloned());
        let order = if order.is_empty() {
            String::new()
        } else {
            format!(" ORDER BY {}", order.join(", "))
        };
        let sql = format!(
            "SELECT {selected} FROM {}{order} LIMIT ?1 OFFSET ?2",
            quote(table)
        );
        let mut statement = self.connection.prepare(&sql).map_err(message)?;
        let names: Vec<String> = statement
            .column_names()
            .into_iter()
            .skip(keys.len())
            .map(str::to_owned)
            .collect();
        let width = names.len() + keys.len();
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let offset_value = i64::try_from(offset).unwrap_or(i64::MAX);
        let mut rows = statement
            .query(rusqlite::params![limit, offset_value])
            .map_err(message)?;
        let mut page_rows = Vec::new();
        while let Some(row) = rows.next().map_err(message)? {
            let mut key = Vec::with_capacity(keys.len());
            let mut values = Vec::with_capacity(names.len());
            for index in 0..width {
                let value = row.get_ref(index).map_err(message)?;
                if index < keys.len() {
                    key.push(Value::from_ref(value, false));
                } else {
                    values.push(Value::from_ref(value, true));
                }
            }
            page_rows.push(Row { key, values });
        }
        let total: i64 = self
            .connection
            .query_row(
                &format!("SELECT count(*) FROM {}", quote(table)),
                [],
                |row| row.get(0),
            )
            .map_err(message)?;
        Ok(Page {
            columns: names,
            key_columns,
            rows: page_rows,
            total: total.max(0) as u64,
            offset,
        })
    }

    fn ensure_writable(&self) -> Result<()> {
        if self.read_only {
            Err("the database is open read-only".to_owned())
        } else {
            Ok(())
        }
    }

    fn row_filter(key_columns: &[String], first_parameter: usize) -> String {
        key_columns
            .iter()
            .enumerate()
            .map(|(index, name)| format!("{} IS ?{}", quote(name), first_parameter + index))
            .collect::<Vec<_>>()
            .join(" AND ")
    }

    fn table_key(&self, table: &str) -> Result<Vec<String>> {
        let object = self.object(table)?;
        let key = self.key_columns(&object, &self.columns(table)?);
        if key.is_empty() {
            return Err(match object.kind {
                ObjectKind::View => format!("{table} is a view and cannot be edited"),
                ObjectKind::Table => format!("rows of {table} cannot be addressed"),
            });
        }
        Ok(key)
    }

    /// Sets one cell of the row whose key is `key`.
    pub fn update_cell(
        &self,
        table: &str,
        key: &[Value],
        column: &str,
        value: &Value,
    ) -> Result<()> {
        self.ensure_writable()?;
        let key_columns = self.table_key(table)?;
        if key.len() != key_columns.len() {
            return Err("the row key does not match the table".to_owned());
        }
        let sql = format!(
            "UPDATE {} SET {} = ?1 WHERE {}",
            quote(table),
            quote(column),
            Self::row_filter(&key_columns, 2)
        );
        let mut parameters: Vec<&dyn ToSql> = vec![value];
        parameters.extend(key.iter().map(|value| value as &dyn ToSql));
        let changed = self
            .connection
            .execute(&sql, parameters.as_slice())
            .map_err(message)?;
        match changed {
            1 => Ok(()),
            0 => Err("the row no longer exists; refresh the table".to_owned()),
            _ => Err(format!("{changed} rows matched the key")),
        }
    }

    /// Inserts a row. Columns not listed take their default (or NULL).
    pub fn insert_row(&self, table: &str, values: &[(String, Value)]) -> Result<()> {
        self.ensure_writable()?;
        self.table_key(table)?;
        let sql = if values.is_empty() {
            format!("INSERT INTO {} DEFAULT VALUES", quote(table))
        } else {
            let names: Vec<String> = values.iter().map(|(name, _)| quote(name)).collect();
            let placeholders: Vec<String> = (1..=values.len())
                .map(|index| format!("?{index}"))
                .collect();
            format!(
                "INSERT INTO {} ({}) VALUES ({})",
                quote(table),
                names.join(", "),
                placeholders.join(", ")
            )
        };
        let parameters: Vec<&dyn ToSql> = values
            .iter()
            .map(|(_, value)| value as &dyn ToSql)
            .collect();
        self.connection
            .execute(&sql, parameters.as_slice())
            .map(|_| ())
            .map_err(message)
    }

    /// Deletes the rows with these keys in one transaction: all or none.
    pub fn delete_rows(&self, table: &str, keys: &[Vec<Value>]) -> Result<u64> {
        self.ensure_writable()?;
        let key_columns = self.table_key(table)?;
        let sql = format!(
            "DELETE FROM {} WHERE {}",
            quote(table),
            Self::row_filter(&key_columns, 1)
        );
        let transaction = self.connection.unchecked_transaction().map_err(message)?;
        let mut deleted = 0;
        {
            let mut statement = transaction.prepare(&sql).map_err(message)?;
            for key in keys {
                if key.len() != key_columns.len() {
                    return Err("the row key does not match the table".to_owned());
                }
                let parameters: Vec<&dyn ToSql> =
                    key.iter().map(|value| value as &dyn ToSql).collect();
                deleted += statement.execute(parameters.as_slice()).map_err(message)? as u64;
            }
        }
        transaction.commit().map_err(message)?;
        Ok(deleted)
    }

    /// Runs one or more `;`-separated statements in order and stops at the
    /// first error. Statements already run stay applied, as in the
    /// `sqlite3` shell; wrap them in `BEGIN`/`COMMIT` for all-or-nothing.
    pub fn execute(&self, sql: &str, max_rows: usize) -> Result<QueryResult> {
        let mut result = QueryResult::default();
        let mut batch = rusqlite::Batch::new(&self.connection, sql);
        while let Some(mut statement) = batch.next().map_err(message)? {
            result.statements += 1;
            if !statement.readonly() {
                self.ensure_writable()?;
            }
            let text = statement.expanded_sql().unwrap_or_default();
            let leading = text.trim_start().to_ascii_uppercase();
            if ["CREATE", "DROP", "ALTER"]
                .iter()
                .any(|word| leading.starts_with(word))
            {
                result.schema_changed = true;
            }
            if statement.column_count() == 0 {
                let changes_before = self.connection.total_changes();
                statement.raw_execute().map_err(message)?;
                result.changes += self
                    .connection
                    .total_changes()
                    .saturating_sub(changes_before);
                continue;
            }
            let columns: Vec<String> = statement
                .column_names()
                .into_iter()
                .map(str::to_owned)
                .collect();
            let width = columns.len();
            let changes_before = self.connection.total_changes();
            let mut rows = statement.raw_query();
            let mut collected = Vec::new();
            let mut truncated = false;
            while let Some(row) = rows.next().map_err(message)? {
                if collected.len() == max_rows {
                    truncated = true;
                    break;
                }
                let mut values = Vec::with_capacity(width);
                for index in 0..width {
                    values.push(Value::from_ref(row.get_ref(index).map_err(message)?, true));
                }
                collected.push(values);
            }
            drop(rows);
            // `INSERT .. RETURNING` both changes rows and returns them.
            result.changes += self
                .connection
                .total_changes()
                .saturating_sub(changes_before);
            result.columns = columns;
            result.rows = collected;
            result.truncated = truncated;
        }
        if result.statements == 0 {
            return Err("no SQL statement to run".to_owned());
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL, avatar BLOB);
                 INSERT INTO users (name, score, avatar) VALUES ('ada', 1.5, x'0102'), ('bob', NULL, NULL), ('cy', 3.0, NULL);
                 CREATE TABLE tags (slug TEXT PRIMARY KEY, label TEXT) WITHOUT ROWID;
                 INSERT INTO tags VALUES ('b', 'Beta'), ('a', 'Alpha');
                 CREATE INDEX users_name ON users (name);
                 CREATE VIEW top AS SELECT name FROM users WHERE score > 1;",
            )
            .unwrap();
        (dir, path)
    }

    #[test]
    fn only_files_with_the_sqlite_header_open() {
        let (dir, path) = fixture();
        assert!(is_database_file(&path));
        let text = dir.path().join("notes.db");
        std::fs::write(&text, "not a database").unwrap();
        assert!(!is_database_file(&text));
        assert_eq!(
            Database::open(&text).err().as_deref(),
            Some("not a SQLite 3 database")
        );
        let empty = dir.path().join("empty.db");
        std::fs::write(&empty, "").unwrap();
        assert!(Database::open(&empty).is_err());
    }

    #[test]
    fn objects_columns_and_indexes_describe_the_schema() {
        let (_dir, path) = fixture();
        let database = Database::open(&path).unwrap();
        let objects = database.objects().unwrap();
        assert_eq!(
            objects
                .iter()
                .map(|object| (object.name.as_str(), object.kind))
                .collect::<Vec<_>>(),
            [
                ("tags", ObjectKind::Table),
                ("top", ObjectKind::View),
                ("users", ObjectKind::Table)
            ]
        );
        assert!(objects[2].sql.starts_with("CREATE TABLE users"));
        let columns = database.columns("users").unwrap();
        assert_eq!(columns[0].name, "id");
        assert_eq!(columns[0].primary_key, 1);
        assert!(columns[1].not_null);
        assert_eq!(columns[2].declared_type, "REAL");
        let indexes = database.indexes("users").unwrap();
        assert_eq!(indexes.len(), 1);
        assert_eq!(indexes[0].columns, ["name"]);
        assert!(indexes[0].sql.as_deref().unwrap().contains("users_name"));
    }

    #[test]
    fn pages_are_keyed_sorted_and_counted() {
        let (_dir, path) = fixture();
        let database = Database::open(&path).unwrap();
        let page = database.page("users", 1, 1, None).unwrap();
        assert_eq!(page.columns, ["id", "name", "score", "avatar"]);
        assert_eq!(page.key_columns, ["rowid"]);
        assert_eq!(page.total, 3);
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].key, [Value::Integer(2)]);
        assert_eq!(page.rows[0].values[1], Value::Text("bob".into()));
        assert_eq!(page.rows[0].values[2], Value::Null);

        let sort = Sort {
            column: "name".into(),
            descending: true,
        };
        let sorted = database.page("users", 0, 10, Some(&sort)).unwrap();
        let names: Vec<_> = sorted
            .rows
            .iter()
            .map(|row| row.values[1].clone())
            .collect();
        assert_eq!(
            names,
            [
                Value::Text("cy".into()),
                Value::Text("bob".into()),
                Value::Text("ada".into())
            ]
        );
        assert_eq!(
            sorted.rows[2].values[3],
            Value::Blob {
                len: 2,
                bytes: vec![1, 2]
            }
        );

        let tags = database.page("tags", 0, 10, None).unwrap();
        assert_eq!(tags.key_columns, ["slug"]);
        assert_eq!(tags.rows[0].key, [Value::Text("a".into())]);

        let view = database.page("top", 0, 10, None).unwrap();
        assert!(!view.editable());
        assert_eq!(view.rows.len(), 2);
    }

    #[test]
    fn a_column_named_rowid_moves_the_key_to_another_alias() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("odd.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE t (rowid TEXT); INSERT INTO t VALUES ('x');")
            .unwrap();
        let database = Database::open(&path).unwrap();
        let page = database.page("t", 0, 10, None).unwrap();
        assert_eq!(page.key_columns, ["_rowid_"]);
        assert_eq!(page.columns, ["rowid"]);
        database
            .update_cell("t", &page.rows[0].key, "rowid", &Value::Text("y".into()))
            .unwrap();
        let page = database.page("t", 0, 10, None).unwrap();
        assert_eq!(page.rows[0].values, [Value::Text("y".into())]);
    }

    #[test]
    fn cells_rows_and_deletes_are_written() {
        let (_dir, path) = fixture();
        let database = Database::open(&path).unwrap();
        let key = [Value::Integer(2)];
        database
            .update_cell(
                "users",
                &key,
                "score",
                &Value::from_input("7", &Value::Null),
            )
            .unwrap();
        database
            .update_cell("users", &key, "name", &Value::Text("bobby".into()))
            .unwrap();
        let error = database
            .update_cell("users", &key, "name", &Value::Null)
            .unwrap_err();
        assert!(error.contains("NOT NULL"), "{error}");
        assert!(
            database
                .update_cell(
                    "users",
                    &[Value::Integer(99)],
                    "name",
                    &Value::Text("x".into())
                )
                .unwrap_err()
                .contains("no longer exists")
        );
        let row = &database.page("users", 1, 1, None).unwrap().rows[0];
        // REAL affinity turns the text "7" into a number.
        assert_eq!(
            row.values[1..3],
            [Value::Text("bobby".into()), Value::Real(7.0)]
        );

        assert!(
            database
                .insert_row("users", &[])
                .unwrap_err()
                .contains("NOT NULL")
        );
        database
            .insert_row("users", &[("name".into(), Value::Text("dee".into()))])
            .unwrap();
        database
            .update_cell(
                "tags",
                &[Value::Text("a".into())],
                "label",
                &Value::Text("A".into()),
            )
            .unwrap();
        assert!(
            database
                .update_cell("top", &[], "name", &Value::Null)
                .unwrap_err()
                .contains("view")
        );

        let deleted = database
            .delete_rows("users", &[vec![Value::Integer(1)], vec![Value::Integer(3)]])
            .unwrap();
        assert_eq!(deleted, 2);
        let page = database.page("users", 0, 10, None).unwrap();
        assert_eq!(page.total, 2);
        drop(database);
        // The changes are in the file, not only in this connection.
        let reopened = Database::open(&path).unwrap();
        assert_eq!(reopened.page("users", 0, 10, None).unwrap().total, 2);
    }

    #[test]
    fn typed_input_keeps_numbers_numeric() {
        assert_eq!(
            Value::from_input("12", &Value::Integer(1)),
            Value::Integer(12)
        );
        assert_eq!(
            Value::from_input("1.25", &Value::Real(0.0)),
            Value::Real(1.25)
        );
        assert_eq!(
            Value::from_input("abc", &Value::Integer(1)),
            Value::Text("abc".into())
        );
        assert_eq!(
            Value::from_input("12", &Value::Null),
            Value::Text("12".into())
        );
    }

    #[test]
    fn execute_runs_every_statement_and_returns_the_last_rows() {
        let (_dir, path) = fixture();
        let database = Database::open(&path).unwrap();
        let result = database
            .execute(
                "UPDATE users SET score = 0; SELECT name FROM users ORDER BY id;",
                2,
            )
            .unwrap();
        assert_eq!(result.statements, 2);
        assert_eq!(result.changes, 3);
        assert_eq!(result.columns, ["name"]);
        assert_eq!(result.rows.len(), 2);
        assert!(result.truncated);
        assert!(!result.schema_changed);

        let ddl = database.execute("CREATE TABLE extra (x)", 10).unwrap();
        assert!(ddl.schema_changed);
        assert!(
            database
                .objects()
                .unwrap()
                .iter()
                .any(|object| object.name == "extra")
        );

        let error = database
            .execute("DELETE FROM users WHERE id = 1; SELECT * FROM missing;", 10)
            .unwrap_err();
        assert!(error.contains("missing"), "{error}");
        // The statement before the error stays applied.
        assert_eq!(database.page("users", 0, 10, None).unwrap().total, 2);
        assert!(database.execute("  ", 10).is_err());
    }

    #[test]
    fn a_read_only_file_opens_but_refuses_writes() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, path) = fixture();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let database = Database::open(&path).unwrap();
        let read_only = database.is_read_only();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        // Root ignores file modes; the check only means something otherwise.
        if read_only {
            assert!(
                database
                    .update_cell(
                        "users",
                        &[Value::Integer(1)],
                        "name",
                        &Value::Text("x".into())
                    )
                    .unwrap_err()
                    .contains("read-only")
            );
            assert!(
                database
                    .execute("DELETE FROM users", 10)
                    .unwrap_err()
                    .contains("read-only")
            );
            assert_eq!(database.execute("SELECT 1", 10).unwrap().rows.len(), 1);
        }
    }
}
