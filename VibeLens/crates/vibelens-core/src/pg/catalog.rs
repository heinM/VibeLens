//! Catalog introspection. Everything is set-based: schema metadata for N tables costs a fixed
//! number of queries (tables, columns, indexes, foreign keys), never N+1.

use std::cmp::Ordering;
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
            "SELECT c.relname::text, c.relkind::text, obj_description(c.oid, 'pg_class'), {EST_ROWS}, \
                    c.relispartition, pn.nspname::text, pc.relname::text \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             LEFT JOIN pg_inherits i ON c.relispartition AND i.inhrelid = c.oid \
             LEFT JOIN pg_class pc ON pc.oid = i.inhparent \
             LEFT JOIN pg_namespace pn ON pn.oid = pc.relnamespace \
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
                is_partition: get(&r, 4)?,
                parent_schema: get(&r, 5)?,
                parent_name: get(&r, 6)?,
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
                is_partition: false,
                parent_schema: None,
                parent_name: None,
            });
        }
        Ok(out)
    }

    /// Partition details of one partitioned table: its key, its direct child partitions (any
    /// schema, default partition last, at most [`PARTITION_CAP`]) and, when pg_partman manages
    /// it, that configuration.
    pub async fn list_partitions(&self, schema: &str, table: &str) -> Result<PartitionListing> {
        let (c, _g) = self.client().await?;
        let parent = query(
            &c,
            "SELECT c.oid, pg_get_partkeydef(c.oid) \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind = 'p'",
            &[&schema, &table],
        )
        .await?;
        let parent = parent
            .first()
            .ok_or_else(|| CoreError::NotFound(format!("partitioned table {schema}.{table}")))?;
        let parent_oid: u32 = get(parent, 0)?;
        let partition_key: Option<String> = get(parent, 1)?;

        let rows = query(
            &c,
            "SELECT c.oid, cn.nspname::text, c.relname::text, COALESCE(pg_get_expr(c.relpartbound, c.oid), ''), c.relkind = 'p' \
             FROM pg_inherits i \
             JOIN pg_class c ON c.oid = i.inhrelid \
             JOIN pg_namespace cn ON cn.oid = c.relnamespace \
             WHERE i.inhparent = $1 AND c.relispartition",
            &[&parent_oid],
        )
        .await?;
        let mut heads = Vec::with_capacity(rows.len());
        for r in &rows {
            heads.push((
                get::<u32>(r, 0)?,
                get::<String>(r, 1)?,
                get::<String>(r, 2)?,
                get::<String>(r, 3)?,
                get::<bool>(r, 4)?,
            ));
        }
        // The server cannot order bounds as text sensibly (`FROM (10)` sorts before `FROM (9)`),
        // so order here, then cap, then ask for sizes of the kept partitions only.
        heads.sort_by(|a, b| {
            let (da, db) = (a.3 == "DEFAULT", b.3 == "DEFAULT");
            da.cmp(&db)
                .then_with(|| natural_cmp(&a.3, &b.3))
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.2.cmp(&b.2))
        });
        let truncated = heads.len() > PARTITION_CAP;
        heads.truncate(PARTITION_CAP);

        let oids: Vec<u32> = heads.iter().map(|h| h.0).collect();
        let mut stats: HashMap<u32, (Option<i64>, Option<i64>)> = HashMap::new();
        if !oids.is_empty() {
            let sql = format!(
                "SELECT c.oid, {EST_ROWS}, \
                        CASE WHEN c.relkind = 'f' THEN NULL \
                             WHEN c.relkind = 'p' THEN (SELECT sum(pg_total_relation_size(t.relid))::bigint \
                                                        FROM pg_partition_tree(c.oid) t WHERE t.isleaf) \
                             ELSE pg_total_relation_size(c.oid) END \
                 FROM pg_class c WHERE c.oid = ANY ($1)"
            );
            for r in query(&c, &sql, &[&oids]).await? {
                stats.insert(get(&r, 0)?, (get(&r, 1)?, get(&r, 2)?));
            }
        }
        let partitions = heads
            .into_iter()
            .map(|(oid, schema, name, bound, is_partitioned)| {
                let (estimated_rows, size_bytes) = stats.get(&oid).copied().unwrap_or_default();
                PartitionInfo {
                    schema,
                    name,
                    is_default: bound == "DEFAULT",
                    bound,
                    is_partitioned,
                    estimated_rows,
                    size_bytes,
                }
            })
            .collect();
        let partman = partman_config(&c, schema, table).await;
        Ok(PartitionListing {
            partition_key,
            partitions,
            truncated,
            partman,
        })
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

/// Partitions returned for one parent; more are reported through `truncated`.
const PARTITION_CAP: usize = 1000;

/// Orders text so digit runs compare as numbers (`p9` < `p10`, `2024-02` < `2024-10`).
fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn chunk(s: &str) -> (bool, &str, &str) {
        let digit = s.as_bytes()[0].is_ascii_digit();
        let end = s
            .bytes()
            .position(|b| b.is_ascii_digit() != digit)
            .unwrap_or(s.len());
        (digit, &s[..end], &s[end..])
    }
    let (mut a, mut b) = (a, b);
    while !a.is_empty() && !b.is_empty() {
        let (ad, ac, ar) = chunk(a);
        let (bd, bc, br) = chunk(b);
        let ord = if ad && bd {
            let (at, bt) = (ac.trim_start_matches('0'), bc.trim_start_matches('0'));
            at.len().cmp(&bt.len()).then_with(|| at.cmp(bt))
        } else {
            ac.cmp(bc)
        };
        if ord != Ordering::Equal {
            return ord;
        }
        (a, b) = (ar, br);
    }
    a.len().cmp(&b.len())
}

/// pg_partman's settings for `schema.table`. Absent when the extension is not installed, does
/// not manage the table, or `part_config` is unreadable for this role: none of those is an error.
async fn partman_config(c: &Client, schema: &str, table: &str) -> Option<PartmanConfig> {
    // The extension need not live in a schema called `partman`. `quote_ident` makes the name
    // from the catalog safe to splice into the statement below.
    //
    // `part_config` is the one relation the navigator reads outside pg_catalog, so it is read
    // only when it is the extension's own plain table without row-level security: a view or a
    // policy put in its place would run its owner's functions with the browsing user's rights.
    let rows = query(
        c,
        "SELECT quote_ident(n.nspname) FROM pg_extension e \
         JOIN pg_namespace n ON n.oid = e.extnamespace \
         JOIN pg_class t ON t.relnamespace = n.oid AND t.relname = 'part_config' \
              AND t.relkind = 'r' AND NOT t.relrowsecurity \
         JOIN pg_depend d ON d.classid = 'pg_class'::regclass AND d.objid = t.oid \
              AND d.refclassid = 'pg_extension'::regclass AND d.refobjid = e.oid AND d.deptype = 'e' \
         WHERE e.extname = 'pg_partman'",
        &[],
    )
    .await
    .ok()?;
    let ext_schema: String = rows.first()?.try_get(0).ok()?;
    read_part_config(c, &ext_schema, schema, table).await
}

/// Reads the `part_config` row of `schema.table` from the (already quoted) `ext_schema`. The row
/// goes through `to_jsonb` so that partman 4 and 5, whose column sets differ, read alike.
async fn read_part_config(
    c: &Client,
    ext_schema: &str,
    schema: &str,
    table: &str,
) -> Option<PartmanConfig> {
    // partman keeps `parent_table` as text, quoted only where needed (`format('%I.%I')`).
    let sql = format!(
        "SELECT to_jsonb(p)::text FROM {ext_schema}.part_config p \
         WHERE p.parent_table IN ($1::text, format('%I.%I', $2::text, $3::text)) LIMIT 1"
    );
    let plain = format!("{schema}.{table}");
    let rows = query(c, &sql, &[&plain, &schema, &table]).await.ok()?;
    let raw: String = rows.first()?.try_get(0).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some(partman_from_json(&value))
}

/// Maps one `part_config` row (as JSON) to the settings the tree shows. Missing, null and
/// oddly typed values become `None` rather than failing the whole listing.
fn partman_from_json(row: &serde_json::Value) -> PartmanConfig {
    use serde_json::Value;
    let text = |k: &str| match row.get(k) {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    };
    let flag = |k: &str| match row.get(k) {
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "t" | "true" | "on" | "yes" | "1" => Some(true),
            "f" | "false" | "off" | "no" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    };
    PartmanConfig {
        control: text("control"),
        partition_interval: text("partition_interval"),
        premake: match row.get("premake") {
            Some(Value::Number(n)) => n.as_i64(),
            Some(Value::String(s)) => s.trim().parse().ok(),
            _ => None,
        },
        retention: text("retention"),
        retention_keep_table: flag("retention_keep_table"),
        // `text` in partman 5 (`on`/`off`), a boolean in some 4.x releases.
        automatic_maintenance: flag("automatic_maintenance")
            .map(|on| if on { "on" } else { "off" }.to_string())
            .or_else(|| text("automatic_maintenance")),
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

    #[test]
    fn natural_order_compares_digit_runs_as_numbers() {
        let mut v = vec![
            "FOR VALUES FROM (10) TO (20)",
            "FOR VALUES FROM (9) TO (10)",
            "FOR VALUES FROM ('2024-10-01') TO ('2024-11-01')",
            "FOR VALUES FROM ('2024-02-01') TO ('2024-03-01')",
            "FOR VALUES FROM (100) TO (200)",
        ];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            v,
            [
                "FOR VALUES FROM (9) TO (10)",
                "FOR VALUES FROM (10) TO (20)",
                "FOR VALUES FROM (100) TO (200)",
                "FOR VALUES FROM ('2024-02-01') TO ('2024-03-01')",
                "FOR VALUES FROM ('2024-10-01') TO ('2024-11-01')",
            ]
        );
        assert_eq!(natural_cmp("a007", "a7"), Ordering::Equal);
        assert_eq!(natural_cmp("a", "a1"), Ordering::Less);
        assert_eq!(natural_cmp("", ""), Ordering::Equal);
    }

    #[test]
    fn partman_rows_map_across_versions() {
        // partman 5: text maintenance flag, interval as text.
        let v5 = serde_json::json!({
            "parent_table": "public.events", "control": "created_at",
            "partition_interval": "1 day", "premake": 4, "retention": "90 days",
            "retention_keep_table": false, "automatic_maintenance": "on",
            "partition_type": "range"
        });
        assert_eq!(
            partman_from_json(&v5),
            PartmanConfig {
                control: Some("created_at".into()),
                partition_interval: Some("1 day".into()),
                premake: Some(4),
                retention: Some("90 days".into()),
                retention_keep_table: Some(false),
                automatic_maintenance: Some("on".into()),
            }
        );
        // partman 4: no automatic_maintenance, id-based set with a numeric interval and no retention.
        let v4 = serde_json::json!({
            "parent_table": "public.t", "control": "id", "partition_interval": 100000,
            "premake": "3", "retention": null, "retention_keep_table": true
        });
        assert_eq!(
            partman_from_json(&v4),
            PartmanConfig {
                control: Some("id".into()),
                partition_interval: Some("100000".into()),
                premake: Some(3),
                retention: None,
                retention_keep_table: Some(true),
                automatic_maintenance: None,
            }
        );
        // Booleans for the maintenance flag, junk values, and an empty row.
        let odd =
            serde_json::json!({"automatic_maintenance": false, "premake": "many", "control": " "});
        let m = partman_from_json(&odd);
        assert_eq!(m.automatic_maintenance.as_deref(), Some("off"));
        assert_eq!((m.premake, m.control), (None, None));
        assert_eq!(
            partman_from_json(&serde_json::json!({})),
            PartmanConfig::default()
        );
    }

    async fn raw_client() -> Option<tokio_postgres::Client> {
        let raw = std::env::var("VG_TEST_PG").ok()?;
        let mut cfg = tokio_postgres::Config::new();
        for kv in raw.split_whitespace() {
            let (k, v) = kv.split_once('=')?;
            match k {
                "host" => cfg.host(v),
                "port" => cfg.port(v.parse().ok()?),
                "user" => cfg.user(v),
                "password" => cfg.password(v),
                "dbname" => cfg.dbname(v),
                _ => &mut cfg,
            };
        }
        let (client, conn) = cfg.connect(tokio_postgres::NoTls).await.ok()?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        Some(client)
    }

    /// pg_partman is not installed on the test server, so a schema with a `part_config` table
    /// of its own (partman 5 columns, plus a column the code does not know) stands in for it.
    #[tokio::test]
    async fn part_config_is_read_from_the_extension_schema() {
        let Some(c) = raw_client().await else {
            crate::test_services::skip("pg", "VG_TEST_PG is not set");
            return;
        };
        let tag = uuid::Uuid::new_v4().simple().to_string();
        let ext = format!("vg_pm_{}", &tag[..10]);
        // A name that needs quoting, to check both spellings of `parent_table` are matched.
        let odd_table = "Mixed Case";
        c.batch_execute(&format!(
            "CREATE SCHEMA {ext};
             CREATE TABLE {ext}.part_config (
                 parent_table text PRIMARY KEY, control text NOT NULL,
                 partition_interval text NOT NULL, premake int, retention text,
                 retention_keep_table boolean NOT NULL DEFAULT true,
                 automatic_maintenance text NOT NULL DEFAULT 'on', epoch text);
             INSERT INTO {ext}.part_config VALUES
                 ('{ext}.events', 'created_at', '1 day', 4, '90 days', false, 'on', 'none'),
                 ('{ext}.\"{odd_table}\"', 'ts', '1 mon', 2, NULL, true, 'off', 'none');"
        ))
        .await
        .expect("stand-in setup");
        let quoted = format!("\"{ext}\"");

        let plain = read_part_config(&c, &quoted, &ext, "events").await;
        let odd = read_part_config(&c, &quoted, &ext, odd_table).await;
        let missing_row = read_part_config(&c, &quoted, &ext, "nope").await;
        let missing_schema = read_part_config(&c, "\"vg_no_such_schema\"", &ext, "events").await;
        // Drop before asserting, so a failed assertion does not leak the schema.
        c.batch_execute(&format!("DROP SCHEMA {ext} CASCADE"))
            .await
            .expect("drop stand-in");

        let plain = plain.expect("row for the plain name");
        assert_eq!(plain.control.as_deref(), Some("created_at"));
        assert_eq!(plain.partition_interval.as_deref(), Some("1 day"));
        assert_eq!(plain.premake, Some(4));
        assert_eq!(plain.retention.as_deref(), Some("90 days"));
        assert_eq!(plain.retention_keep_table, Some(false));
        assert_eq!(plain.automatic_maintenance.as_deref(), Some("on"));
        let odd = odd.expect("row for the quoted name");
        assert_eq!(odd.control.as_deref(), Some("ts"));
        assert_eq!(odd.retention, None);
        assert_eq!(odd.automatic_maintenance.as_deref(), Some("off"));
        assert_eq!(missing_row, None, "no row means no partman info");
        assert_eq!(missing_schema, None, "an unreadable table is not an error");
    }
}
