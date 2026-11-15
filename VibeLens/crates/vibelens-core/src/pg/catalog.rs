//! Catalog introspection. Everything is set-based: schema metadata for N tables costs a fixed
//! number of queries (tables, columns, indexes, foreign keys), never N+1.

use std::collections::HashMap;

use tokio_postgres::Client;
use tokio_postgres::types::FromSql;

use super::PgHandle;
use super::error::map_pg_error;
use crate::error::{CoreError, Result};
use crate::model::*;

fn get<'a, T: FromSql<'a>>(row: &'a tokio_postgres::Row, idx: usize) -> Result<T> {
    row.try_get(idx).map_err(|e| map_pg_error(&e))
}

async fn query(
    c: &Client,
    sql: &str,
    params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
) -> Result<Vec<tokio_postgres::Row>> {
    c.query(sql, params).await.map_err(|e| map_pg_error(&e))
}

fn kind_of(relkind: &str) -> Option<ObjectKind> {
    Some(match relkind {
        "r" => ObjectKind::Table,
        "p" => ObjectKind::PartitionedTable,
        "v" => ObjectKind::View,
        "m" => ObjectKind::MaterializedView,
        "f" => ObjectKind::ForeignTable,
        "S" => ObjectKind::Sequence,
        _ => return None,
    })
}

/// Schemas Postgres creates for itself.
pub(super) fn is_system_schema(name: &str) -> bool {
    name == "pg_catalog"
        || name == "information_schema"
        || name.starts_with("pg_toast")
        || name.starts_with("pg_temp")
}

/// `reltuples` is -1 (PG14+) or 0 (older) before the first ANALYZE; only relations with storage
/// carry a meaningful estimate.
const EST_ROWS: &str =
    "CASE WHEN c.relkind IN ('r','p','m','f') AND c.reltuples >= 0 THEN c.reltuples::bigint END";

impl PgHandle {
    /// Lists databases on the server (templates included, flagged).
    pub async fn list_databases(&self) -> Result<Vec<DatabaseInfo>> {
        let (c, _g) = self.client().await?;
        let rows = query(
            &c,
            "SELECT d.datname::text, d.datistemplate, d.datallowconn, \
                    CASE WHEN has_database_privilege(d.oid, 'CONNECT') THEN pg_database_size(d.oid) END \
             FROM pg_database d ORDER BY d.datname",
            &[],
        )
        .await?;
        rows.iter()
            .map(|r| {
                Ok(DatabaseInfo {
                    name: get(r, 0)?,
                    is_template: get(r, 1)?,
                    allow_connections: get(r, 2)?,
                    size_bytes: get(r, 3)?,
                })
            })
            .collect()
    }

    /// Lists schemas; `pg_catalog`, `information_schema`, `pg_toast*` and `pg_temp*` are flagged.
    pub async fn list_schemas(&self) -> Result<Vec<SchemaInfo>> {
        let (c, _g) = self.client().await?;
        let rows = query(&c, "SELECT n.nspname::text, pg_get_userbyid(n.nspowner)::text FROM pg_namespace n ORDER BY n.nspname", &[]).await?;
        rows.iter()
            .map(|r| {
                let name: String = get(r, 0)?;
                Ok(SchemaInfo {
                    is_system: is_system_schema(&name),
                    owner: get(r, 1)?,
                    name,
                })
            })
            .collect()
    }

    /// Tables, views, materialised views, foreign tables, sequences, functions and procedures
    /// in `schema`.
    pub async fn list_objects(&self, schema: &str) -> Result<Vec<SchemaObject>> {
        let (c, _g) = self.client().await?;
        let rel_sql = format!(
            "SELECT c.relname::text, c.relkind::text, obj_description(c.oid, 'pg_class'), {EST_ROWS} \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relkind IN ('r','p','v','m','f','S') \
             ORDER BY c.relname"
        );
        let mut out = Vec::new();
        for r in query(&c, &rel_sql, &[&schema]).await? {
            let relkind: String = get(&r, 1)?;
            let Some(kind) = kind_of(&relkind) else {
                continue;
            };
            out.push(SchemaObject {
                schema: schema.to_string(),
                name: get(&r, 0)?,
                kind,
                comment: get(&r, 2)?,
                estimated_rows: get(&r, 3)?,
                signature: None,
            });
        }
        // Functions owned by extensions (pg_trgm, postgis, ...) are noise; hide them.
        let fn_sql = "SELECT p.proname::text, p.prokind::text, pg_get_function_identity_arguments(p.oid), obj_description(p.oid, 'pg_proc') \
                      FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
                      WHERE n.nspname = $1 AND p.prokind IN ('f','p') \
                        AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid AND d.deptype = 'e') \
                      ORDER BY p.proname, 3";
        for r in query(&c, fn_sql, &[&schema]).await? {
            let prokind: String = get(&r, 1)?;
            let args: String = get(&r, 2)?;
            out.push(SchemaObject {
                schema: schema.to_string(),
                name: get(&r, 0)?,
                kind: if prokind == "p" {
                    ObjectKind::Procedure
                } else {
                    ObjectKind::Function
                },
                comment: get(&r, 3)?,
                estimated_rows: None,
                signature: Some(format!("({args})")),
            });
        }
        Ok(out)
    }

    /// Columns, indexes and foreign keys of one relation.
    pub async fn describe_table(&self, schema: &str, table: &str) -> Result<TableDescription> {
        let (c, _g) = self.client().await?;
        let sql = format!(
            "SELECT c.oid, c.relkind::text, obj_description(c.oid, 'pg_class'), {EST_ROWS} \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('r','p','v','m','f','S')"
        );
        let rows = query(&c, &sql, &[&schema, &table]).await?;
        let row = rows
            .first()
            .ok_or_else(|| CoreError::NotFound(format!("relation {schema}.{table}")))?;
        let oid: u32 = get(row, 0)?;
        let relkind: String = get(row, 1)?;
        let kind = kind_of(&relkind)
            .ok_or_else(|| CoreError::Database(format!("unsupported relkind {relkind}")))?;
        let oids = [oid];
        let mut columns = load_columns(&c, &oids).await?;
        let mut indexes = load_indexes(&c, &oids).await?;
        let mut fks = load_foreign_keys(&c, &oids).await?;
        Ok(TableDescription {
            schema: schema.to_string(),
            name: table.to_string(),
            kind,
            comment: get(row, 2)?,
            estimated_rows: get(row, 3)?,
            columns: columns.remove(&oid).unwrap_or_default(),
            indexes: indexes.remove(&oid).unwrap_or_default(),
            foreign_keys: fks.remove(&oid).unwrap_or_default(),
        })
    }

    /// Compact catalog dump for the agent: user schemas (or just `schema_filter`), at most
    /// `max_tables` relations (partitions are skipped), with `truncated` set when more exist.
    pub async fn schema_metadata(
        &self,
        schema_filter: Option<&str>,
        max_tables: usize,
    ) -> Result<SchemaMetadata> {
        let version = self.server_version().await.ok();
        let (c, _g) = self.client().await?;
        let limit = i64::try_from(max_tables)
            .unwrap_or(i64::MAX - 1)
            .saturating_add(1);
        let sql = format!(
            "SELECT c.oid, n.nspname::text, c.relname::text, c.relkind::text, obj_description(c.oid, 'pg_class'), {EST_ROWS} \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relkind IN ('r','p','v','m','f') AND NOT c.relispartition \
               AND CASE WHEN $1::text IS NULL \
                        THEN n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname !~ '^pg_(toast|temp)' \
                        ELSE n.nspname = $1::text END \
             ORDER BY n.nspname, c.relname LIMIT $2"
        );
        let rows = query(&c, &sql, &[&schema_filter, &limit]).await?;
        let truncated = rows.len() > max_tables;
        let mut heads = Vec::new();
        for r in rows.iter().take(max_tables) {
            let relkind: String = get(r, 3)?;
            let Some(kind) = kind_of(&relkind) else {
                continue;
            };
            heads.push((
                get::<u32>(r, 0)?,
                get::<String>(r, 1)?,
                get::<String>(r, 2)?,
                kind,
                get::<Option<String>>(r, 4)?,
                get::<Option<i64>>(r, 5)?,
            ));
        }
        let oids: Vec<u32> = heads.iter().map(|h| h.0).collect();
        let (mut columns, mut indexes, mut fks) = if oids.is_empty() {
            (HashMap::new(), HashMap::new(), HashMap::new())
        } else {
            (
                load_columns(&c, &oids).await?,
                load_indexes(&c, &oids).await?,
                load_foreign_keys(&c, &oids).await?,
            )
        };
        let tables = heads
            .into_iter()
            .map(
                |(oid, schema, name, kind, comment, estimated_rows)| TableDescription {
                    schema,
                    name,
                    kind,
                    comment,
                    estimated_rows,
                    columns: columns.remove(&oid).unwrap_or_default(),
                    indexes: indexes.remove(&oid).unwrap_or_default(),
                    foreign_keys: fks.remove(&oid).unwrap_or_default(),
                },
            )
            .collect();
        Ok(SchemaMetadata {
            database: self.database().to_string(),
            server_version: version,
            tables,
            truncated,
        })
    }
}

async fn load_columns(c: &Client, oids: &[u32]) -> Result<HashMap<u32, Vec<ColumnInfo>>> {
    let rows = query(
        c,
        "SELECT a.attrelid, a.attname::text, format_type(a.atttypid, a.atttypmod), NOT a.attnotnull, \
                CASE WHEN a.attidentity = 'a' THEN 'GENERATED ALWAYS AS IDENTITY' \
                     WHEN a.attidentity = 'd' THEN 'GENERATED BY DEFAULT AS IDENTITY' \
                     WHEN a.attgenerated = 's' THEN 'GENERATED ALWAYS AS (' || pg_get_expr(d.adbin, d.adrelid) || ') STORED' \
                     ELSE pg_get_expr(d.adbin, d.adrelid) END, \
                EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = a.attrelid AND i.indisprimary AND a.attnum = ANY (i.indkey)), \
                a.attnum::int4, col_description(a.attrelid, a.attnum) \
         FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
         WHERE a.attrelid = ANY ($1) AND a.attnum > 0 AND NOT a.attisdropped \
         ORDER BY a.attrelid, a.attnum",
        &[&oids],
    )
    .await?;
    let mut out: HashMap<u32, Vec<ColumnInfo>> = HashMap::new();
    for r in &rows {
        out.entry(get(r, 0)?).or_default().push(ColumnInfo {
            name: get(r, 1)?,
            data_type: get(r, 2)?,
            nullable: get(r, 3)?,
            default: get(r, 4)?,
            is_primary_key: get(r, 5)?,
            ordinal: get(r, 6)?,
            comment: get(r, 7)?,
        });
    }
    Ok(out)
}

async fn load_indexes(c: &Client, oids: &[u32]) -> Result<HashMap<u32, Vec<IndexInfo>>> {
    let rows = query(
        c,
        "SELECT i.indrelid, ic.relname::text, pg_get_indexdef(i.indexrelid), i.indisunique, i.indisprimary \
         FROM pg_index i JOIN pg_class ic ON ic.oid = i.indexrelid \
         WHERE i.indrelid = ANY ($1) \
         ORDER BY i.indrelid, i.indisprimary DESC, ic.relname",
        &[&oids],
    )
    .await?;
    let mut out: HashMap<u32, Vec<IndexInfo>> = HashMap::new();
    for r in &rows {
        out.entry(get(r, 0)?).or_default().push(IndexInfo {
            name: get(r, 1)?,
            definition: get(r, 2)?,
            is_unique: get(r, 3)?,
            is_primary: get(r, 4)?,
        });
    }
    Ok(out)
}

/// Foreign keys with both column lists resolved in constraint order (`WITH ORDINALITY`).
async fn load_foreign_keys(c: &Client, oids: &[u32]) -> Result<HashMap<u32, Vec<ForeignKeyInfo>>> {
    let rows = query(
        c,
        "SELECT con.conrelid, con.conname::text, \
                ARRAY(SELECT a.attname::text FROM unnest(con.conkey) WITH ORDINALITY AS k(attnum, ord) \
                      JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.attnum ORDER BY k.ord), \
                fn.nspname::text, fc.relname::text, \
                ARRAY(SELECT a.attname::text FROM unnest(con.confkey) WITH ORDINALITY AS k(attnum, ord) \
                      JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = k.attnum ORDER BY k.ord) \
         FROM pg_constraint con \
         JOIN pg_class fc ON fc.oid = con.confrelid \
         JOIN pg_namespace fn ON fn.oid = fc.relnamespace \
         WHERE con.conrelid = ANY ($1) AND con.contype = 'f' \
         ORDER BY con.conrelid, con.conname",
        &[&oids],
    )
    .await?;
    let mut out: HashMap<u32, Vec<ForeignKeyInfo>> = HashMap::new();
    for r in &rows {
        out.entry(get(r, 0)?).or_default().push(ForeignKeyInfo {
            name: get(r, 1)?,
            columns: get(r, 2)?,
            referenced_schema: get(r, 3)?,
            referenced_table: get(r, 4)?,
            referenced_columns: get(r, 5)?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_schema_detection() {
        for s in [
            "pg_catalog",
            "information_schema",
            "pg_toast",
            "pg_toast_temp_1",
            "pg_temp_3",
        ] {
            assert!(is_system_schema(s), "{s}");
        }
        for s in ["public", "app", "pgcrypto", "my_pg_toast"] {
            assert!(!is_system_schema(s), "{s}");
        }
    }
}
