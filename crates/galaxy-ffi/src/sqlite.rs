//! SQLite database tabs for Swift. See `galaxy_core::sqlite`.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use galaxy_core::sqlite::{self, Database};

use crate::CoreError;

/// One cell value.
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum SqliteValue {
    Null,
    Integer {
        value: i64,
    },
    Real {
        value: f64,
    },
    Text {
        value: String,
    },
    /// `bytes` is at most a preview of the stored blob; `len` is its size.
    Blob {
        len: u64,
        bytes: Vec<u8>,
    },
}

impl From<sqlite::Value> for SqliteValue {
    fn from(value: sqlite::Value) -> Self {
        match value {
            sqlite::Value::Null => Self::Null,
            sqlite::Value::Integer(value) => Self::Integer { value },
            sqlite::Value::Real(value) => Self::Real { value },
            sqlite::Value::Text(value) => Self::Text { value },
            sqlite::Value::Blob { len, bytes } => Self::Blob { len, bytes },
        }
    }
}

impl From<SqliteValue> for sqlite::Value {
    fn from(value: SqliteValue) -> Self {
        match value {
            SqliteValue::Null => Self::Null,
            SqliteValue::Integer { value } => Self::Integer(value),
            SqliteValue::Real { value } => Self::Real(value),
            SqliteValue::Text { value } => Self::Text(value),
            SqliteValue::Blob { len, bytes } => Self::Blob { len, bytes },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SqliteObject {
    pub name: String,
    pub is_view: bool,
    pub sql: String,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SqliteColumn {
    pub name: String,
    pub declared_type: String,
    pub not_null: bool,
    pub default_value: Option<String>,
    pub primary_key: u32,
    pub generated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SqliteIndex {
    pub name: String,
    pub unique: bool,
    pub columns: Vec<String>,
    pub sql: Option<String>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SqliteRow {
    pub key: Vec<SqliteValue>,
    pub values: Vec<SqliteValue>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SqlitePage {
    pub columns: Vec<String>,
    /// Empty for a view, whose rows cannot be edited.
    pub key_columns: Vec<String>,
    pub rows: Vec<SqliteRow>,
    pub total: u64,
    pub offset: u64,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SqliteQueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<SqliteValue>>,
    pub truncated: bool,
    pub changes: u64,
    pub statements: u32,
    pub schema_changed: bool,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SqliteCellValue {
    pub column: String,
    pub value: SqliteValue,
}

fn error(message: String) -> CoreError {
    CoreError::Io { message }
}

fn values(values: Vec<SqliteValue>) -> Vec<sqlite::Value> {
    values.into_iter().map(Into::into).collect()
}

/// Whether the file starts with the SQLite 3 header.
#[uniffi::export]
pub fn is_sqlite_database(path: String) -> bool {
    sqlite::is_database_file(Path::new(&path))
}

/// The value for text typed over a cell that held `previous`.
#[uniffi::export]
pub fn sqlite_value_from_input(text: String, previous: SqliteValue) -> SqliteValue {
    sqlite::Value::from_input(&text, &previous.into()).into()
}

/// An open SQLite file. Calls are serialized; a slow query blocks the
/// others, so the app runs queries off the main thread.
#[derive(uniffi::Object)]
pub struct SqliteDatabase {
    inner: Mutex<Database>,
}

#[uniffi::export]
impl SqliteDatabase {
    #[uniffi::constructor]
    pub fn open(path: String) -> Result<Arc<Self>, CoreError> {
        let database = Database::open(Path::new(&path)).map_err(error)?;
        Ok(Arc::new(Self {
            inner: Mutex::new(database),
        }))
    }

    pub fn is_read_only(&self) -> bool {
        self.lock().is_read_only()
    }

    pub fn objects(&self) -> Result<Vec<SqliteObject>, CoreError> {
        Ok(self
            .lock()
            .objects()
            .map_err(error)?
            .into_iter()
            .map(|object| SqliteObject {
                name: object.name,
                is_view: object.kind == sqlite::ObjectKind::View,
                sql: object.sql,
            })
            .collect())
    }

    pub fn columns(&self, table: String) -> Result<Vec<SqliteColumn>, CoreError> {
        Ok(self
            .lock()
            .columns(&table)
            .map_err(error)?
            .into_iter()
            .map(|column| SqliteColumn {
                name: column.name,
                declared_type: column.declared_type,
                not_null: column.not_null,
                default_value: column.default_value,
                primary_key: column.primary_key,
                generated: column.generated,
            })
            .collect())
    }

    pub fn indexes(&self, table: String) -> Result<Vec<SqliteIndex>, CoreError> {
        Ok(self
            .lock()
            .indexes(&table)
            .map_err(error)?
            .into_iter()
            .map(|index| SqliteIndex {
                name: index.name,
                unique: index.unique,
                columns: index.columns,
                sql: index.sql,
            })
            .collect())
    }

    pub fn page(
        &self,
        table: String,
        offset: u64,
        limit: u64,
        sort_column: Option<String>,
        descending: bool,
    ) -> Result<SqlitePage, CoreError> {
        let sort = sort_column.map(|column| sqlite::Sort { column, descending });
        let page = self
            .lock()
            .page(&table, offset, limit, sort.as_ref())
            .map_err(error)?;
        Ok(SqlitePage {
            columns: page.columns,
            key_columns: page.key_columns,
            rows: page
                .rows
                .into_iter()
                .map(|row| SqliteRow {
                    key: row.key.into_iter().map(Into::into).collect(),
                    values: row.values.into_iter().map(Into::into).collect(),
                })
                .collect(),
            total: page.total,
            offset: page.offset,
        })
    }

    pub fn update_cell(
        &self,
        table: String,
        key: Vec<SqliteValue>,
        column: String,
        value: SqliteValue,
    ) -> Result<(), CoreError> {
        self.lock()
            .update_cell(&table, &values(key), &column, &value.into())
            .map_err(error)
    }

    pub fn insert_row(&self, table: String, values: Vec<SqliteCellValue>) -> Result<(), CoreError> {
        let values: Vec<_> = values
            .into_iter()
            .map(|cell| (cell.column, cell.value.into()))
            .collect();
        self.lock().insert_row(&table, &values).map_err(error)
    }

    pub fn delete_rows(
        &self,
        table: String,
        keys: Vec<Vec<SqliteValue>>,
    ) -> Result<u64, CoreError> {
        let keys: Vec<_> = keys.into_iter().map(values).collect();
        self.lock().delete_rows(&table, &keys).map_err(error)
    }

    pub fn execute(&self, sql: String, max_rows: u32) -> Result<SqliteQueryResult, CoreError> {
        let result = self
            .lock()
            .execute(&sql, max_rows as usize)
            .map_err(error)?;
        Ok(SqliteQueryResult {
            columns: result.columns,
            rows: result
                .rows
                .into_iter()
                .map(|row| row.into_iter().map(Into::into).collect())
                .collect(),
            truncated: result.truncated,
            changes: result.changes,
            statements: result.statements,
            schema_changed: result.schema_changed,
        })
    }
}

impl SqliteDatabase {
    fn lock(&self) -> MutexGuard<'_, Database> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swift_facing_calls_round_trip_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.db");
        let path_text = path.to_string_lossy().into_owned();
        assert!(
            SqliteDatabase::open(path_text.clone()).is_err(),
            "missing file"
        );
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE t (x INTEGER); INSERT INTO t VALUES (1);")
            .unwrap();
        assert!(is_sqlite_database(path_text.clone()));

        let database = SqliteDatabase::open(path_text).unwrap();
        let page = database.page("t".into(), 0, 10, None, false).unwrap();
        assert_eq!(page.key_columns, ["rowid"]);
        assert_eq!(page.rows[0].values, [SqliteValue::Integer { value: 1 }]);
        database
            .update_cell(
                "t".into(),
                page.rows[0].key.clone(),
                "x".into(),
                sqlite_value_from_input("5".into(), SqliteValue::Integer { value: 1 }),
            )
            .unwrap();
        let result = database.execute("SELECT x FROM t".into(), 10).unwrap();
        assert_eq!(result.rows, [vec![SqliteValue::Integer { value: 5 }]]);
        let CoreError::Io { message } = database
            .page("missing".into(), 0, 10, None, false)
            .unwrap_err();
        assert!(message.contains("missing"), "{message}");
    }
}
