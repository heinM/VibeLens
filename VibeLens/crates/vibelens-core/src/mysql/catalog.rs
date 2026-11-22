//! Navigator and schema metadata from `information_schema`. Names chosen in the UI or by the
//! agent are passed as hex literals (`CONVERT(X'…' USING utf8mb4)`), which mean the same under
//! every `sql_mode`, so no name can change the query. Every listing is bounded.

use std::collections::BTreeMap;

use super::MysqlHandle;
use super::check_database_name;
use crate::error::{CoreError, Result};
use crate::model::*;

/// Most databases a listing returns.
const MAX_DATABASES: usize = 10_000;
/// Most objects (tables, views, routines) one database listing returns.
const MAX_OBJECTS: usize = 20_000;
/// Most column, index or key rows read for one metadata call.
const MAX_DETAIL_ROWS: usize = 200_000;

/// `name` as a SQL string literal that no `sql_mode` can read differently.
pub(super) fn literal(name: &str) -> String {
    if name.is_empty() {
        return "''".into();
    }
    format!("CONVERT(X'{}' USING utf8mb4)", hex::encode(name.as_bytes()))
}

fn kind_of(table_type: &str) -> ObjectKind {
    match table_type {
        "VIEW" | "SYSTEM VIEW" => ObjectKind::View,
        _ => ObjectKind::Table,
    }
}

fn opt(v: Option<&Option<String>>) -> Option<String> {
    v.cloned().flatten()
}

fn text(v: Option<&Option<String>>) -> String {
    opt(v).unwrap_or_default()
}

fn comment(v: Option<&Option<String>>) -> Option<String> {
    opt(v).filter(|c| !c.is_empty() && c != "VIEW")
}

impl MysqlHandle {
    /// Databases (schemas) the user can see, sorted by name.
    pub async fn list_databases(&self) -> Result<Vec<String>> {
        let rows = self
            .catalog_rows(
                "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA ORDER BY SCHEMA_NAME",
                MAX_DATABASES,
            )
            .await?;
        Ok(rows.into_iter().filter_map(|r| opt(r.first())).collect())
    }

    /// Tables, views, functions and procedures of `database`.
    pub async fn list_objects(&self, database: &str) -> Result<Vec<SchemaObject>> {
        check_database_name(database)?;
        let db = literal(database);
        let tables = self
            .catalog_rows(
                &format!(
                    "SELECT TABLE_NAME, TABLE_TYPE, TABLE_COMMENT, TABLE_ROWS \
                     FROM information_schema.TABLES WHERE TABLE_SCHEMA = {db} \
                     ORDER BY TABLE_NAME"
                ),
                MAX_OBJECTS,
            )
            .await?;
        let routines = self
            .catalog_rows(
                &format!(
                    "SELECT ROUTINE_NAME, ROUTINE_TYPE FROM information_schema.ROUTINES \
                     WHERE ROUTINE_SCHEMA = {db} ORDER BY ROUTINE_NAME"
                ),
                MAX_OBJECTS,
            )
            .await?;
        let mut out: Vec<SchemaObject> = tables
            .iter()
            .map(|r| SchemaObject {
                schema: database.to_string(),
                name: text(r.first()),
                kind: kind_of(&text(r.get(1))),
                comment: comment(r.get(2)),
                estimated_rows: opt(r.get(3)).and_then(|n| n.parse().ok()),
                signature: None,
                is_partition: false,
                parent_schema: None,
                parent_name: None,
            })
            .collect();
        out.extend(routines.iter().map(|r| SchemaObject {
            schema: database.to_string(),
            name: text(r.first()),
            kind: if text(r.get(1)) == "PROCEDURE" {
                ObjectKind::Procedure
            } else {
                ObjectKind::Function
            },
            comment: None,
            estimated_rows: None,
            signature: None,
            is_partition: false,
            parent_schema: None,
            parent_name: None,
        }));
        Ok(out)
    }

    /// Columns, indexes and foreign keys of one table or view.
    pub async fn describe_table(&self, database: &str, table: &str) -> Result<TableDescription> {
        let meta = self.metadata(database, Some(table), 1).await?;
        meta.tables
            .into_iter()
            .next()
            .ok_or_else(|| CoreError::NotFound(format!("table {database}.{table}")))
    }

    /// Tables and views of `database` (this pool's database when `None`) with columns, keys
    /// and indexes, capped at `max_tables`.
    pub async fn schema_metadata(
        &self,
        database: Option<&str>,
        max_tables: usize,
    ) -> Result<SchemaMetadata> {
        let db = database
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .unwrap_or(self.database())
            .to_string();
        self.metadata(&db, None, max_tables).await
    }

    async fn metadata(
        &self,
        database: &str,
        only: Option<&str>,
        max_tables: usize,
    ) -> Result<SchemaMetadata> {
        check_database_name(database)?;
        let db = literal(database);
        let filter = match only {
            Some(t) => format!(" AND TABLE_NAME = {}", literal(t)),
            None => String::new(),
        };
        let max_tables = max_tables.max(1);
        let tables = self
            .catalog_rows(
                &format!(
                    "SELECT TABLE_NAME, TABLE_TYPE, TABLE_COMMENT, TABLE_ROWS \
                     FROM information_schema.TABLES WHERE TABLE_SCHEMA = {db}{filter} \
                     ORDER BY TABLE_NAME LIMIT {}",
                    max_tables + 1
                ),
                max_tables + 1,
            )
            .await?;
        let truncated = tables.len() > max_tables;
        let mut by_name: BTreeMap<String, TableDescription> = BTreeMap::new();
        for r in tables.iter().take(max_tables) {
            let name = text(r.first());
            by_name.insert(
                name.clone(),
                TableDescription {
                    schema: database.to_string(),
                    name,
                    kind: kind_of(&text(r.get(1))),
                    comment: comment(r.get(2)),
                    estimated_rows: opt(r.get(3)).and_then(|n| n.parse().ok()),
                    columns: Vec::new(),
                    indexes: Vec::new(),
                    foreign_keys: Vec::new(),
                },
            );
        }
        if by_name.is_empty() {
            return Ok(SchemaMetadata {
                database: database.to_string(),
                server_version: self.server_version(),
                tables: Vec::new(),
                truncated,
            });
        }
        let columns = self
            .catalog_rows(
                &format!(
                    "SELECT TABLE_NAME, COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, \
                     COLUMN_KEY, ORDINAL_POSITION, COLUMN_COMMENT, EXTRA \
                     FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = {db}{filter} \
                     ORDER BY TABLE_NAME, ORDINAL_POSITION"
                ),
                MAX_DETAIL_ROWS,
            )
            .await?;
        for r in &columns {
            let Some(t) = by_name.get_mut(&text(r.first())) else {
                continue;
            };
            let extra = text(r.get(8));
            let mut data_type = text(r.get(2));
            if !extra.is_empty() && extra.contains("auto_increment") {
                data_type.push_str(" auto_increment");
            }
            t.columns.push(ColumnInfo {
                name: text(r.get(1)),
                data_type,
                nullable: text(r.get(3)) == "YES",
                default: opt(r.get(4)),
                is_primary_key: text(r.get(5)) == "PRI",
                ordinal: opt(r.get(6)).and_then(|n| n.parse().ok()).unwrap_or(0),
                comment: opt(r.get(7)).filter(|c| !c.is_empty()),
            });
        }
        let stats = self
            .catalog_rows(
                &format!(
                    "SELECT TABLE_NAME, INDEX_NAME, NON_UNIQUE, COLUMN_NAME, INDEX_TYPE, \
                     SEQ_IN_INDEX FROM information_schema.STATISTICS \
                     WHERE TABLE_SCHEMA = {db}{filter} \
                     ORDER BY TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX"
                ),
                MAX_DETAIL_ROWS,
            )
            .await?;
        // (table, index) → (unique, type, columns)
        let mut indexes: BTreeMap<(String, String), (bool, String, Vec<String>)> = BTreeMap::new();
        for r in &stats {
            let key = (text(r.first()), text(r.get(1)));
            let e = indexes
                .entry(key)
                .or_insert_with(|| (text(r.get(2)) == "0", text(r.get(4)), Vec::new()));
            // An expression index has no column name.
            e.2.push(
                opt(r.get(3))
                    .map(|c| format!("`{}`", c.replace('`', "``")))
                    .unwrap_or_else(|| "(expression)".into()),
            );
        }
        for ((table, name), (unique, kind, cols)) in indexes {
            let Some(t) = by_name.get_mut(&table) else {
                continue;
            };
            let primary = name == "PRIMARY";
            let head = if primary {
                "PRIMARY KEY".to_string()
            } else if unique {
                format!("UNIQUE INDEX `{}`", name.replace('`', "``"))
            } else {
                format!("INDEX `{}`", name.replace('`', "``"))
            };
            let using = if kind.is_empty() {
                String::new()
            } else {
                format!(" USING {kind}")
            };
            t.indexes.push(IndexInfo {
                definition: format!("{head} ({}){using}", cols.join(", ")),
                name,
                is_unique: unique || primary,
                is_primary: primary,
            });
        }
        let fks = self
            .catalog_rows(
                &format!(
                    "SELECT TABLE_NAME, CONSTRAINT_NAME, COLUMN_NAME, REFERENCED_TABLE_SCHEMA, \
                     REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME \
                     FROM information_schema.KEY_COLUMN_USAGE \
                     WHERE TABLE_SCHEMA = {db}{filter} AND REFERENCED_TABLE_NAME IS NOT NULL \
                     ORDER BY TABLE_NAME, CONSTRAINT_NAME, ORDINAL_POSITION"
                ),
                MAX_DETAIL_ROWS,
            )
            .await?;
        let mut keys: BTreeMap<(String, String), ForeignKeyInfo> = BTreeMap::new();
        for r in &fks {
            let key = (text(r.first()), text(r.get(1)));
            let e = keys.entry(key).or_insert_with(|| ForeignKeyInfo {
                name: text(r.get(1)),
                columns: Vec::new(),
                referenced_schema: text(r.get(3)),
                referenced_table: text(r.get(4)),
                referenced_columns: Vec::new(),
            });
            e.columns.push(text(r.get(2)));
            e.referenced_columns.push(text(r.get(5)));
        }
        for ((table, _), fk) in keys {
            if let Some(t) = by_name.get_mut(&table) {
                t.foreign_keys.push(fk);
            }
        }
        Ok(SchemaMetadata {
            database: database.to_string(),
            server_version: self.server_version(),
            tables: by_name.into_values().collect(),
            truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_become_hex_literals() {
        assert_eq!(literal("shop"), "CONVERT(X'73686f70' USING utf8mb4)");
        // Quotes, backslashes and comment markers cannot leave the literal.
        let l = literal("x' OR 1=1 -- \\");
        assert!(l.starts_with("CONVERT(X'") && !l.contains("OR"), "{l}");
        assert_eq!(literal(""), "''");
    }
}
