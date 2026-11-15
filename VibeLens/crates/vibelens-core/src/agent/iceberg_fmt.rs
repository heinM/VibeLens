//! Compact text for an Iceberg table's metadata, for the agent's `describe_table`.
//!
//! The columns come with their field ids and Iceberg types; the partition spec and sort order are
//! resolved to column names; the snapshot history is summarised (count, current snapshot and the few
//! newest) instead of dumped. Table properties, locations and manifest paths are left out on purpose:
//! this text goes to the model and none of them help it write SQL.
//!
//! Table properties are omitted entirely (they can carry credentials or owners); snapshot summaries
//! contribute only their numeric record counts.

use chrono::{DateTime, Utc};

use crate::model::{IcebergField, IcebergSnapshot, IcebergTableInfo};

/// How many of the newest snapshots are listed individually.
const SNAPSHOTS_SHOWN: usize = 5;

fn when(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| ms.to_string())
}

fn field_line(f: &IcebergField, depth: usize) -> String {
    let mut s = format!(
        "{}{} {}{} (id {})",
        "  ".repeat(depth + 1),
        f.name,
        f.type_name,
        if f.required { " NOT NULL" } else { "" },
        f.id
    );
    if let Some(d) = f.doc.as_deref().filter(|d| !d.is_empty()) {
        s.push_str(&format!(" -- {d}"));
    }
    s.push('\n');
    s
}

fn snapshot_line(s: &IcebergSnapshot) -> String {
    let mut line = format!(
        "  {} {} {}",
        s.snapshot_id,
        when(s.timestamp_ms),
        s.operation.as_deref().unwrap_or("unknown")
    );
    let num = |k: &str| {
        s.summary
            .get(k)
            .filter(|v| v.chars().all(|c| c.is_ascii_digit()))
            .cloned()
    };
    if let Some(n) = num("added-records") {
        line.push_str(&format!(", +{n} rows"));
    }
    if let Some(n) = num("total-records") {
        line.push_str(&format!(", {n} total"));
    }
    line.push('\n');
    line
}

/// Renders `info` as the `describe_table` result for an Iceberg table.
pub(super) fn render_table_info(info: &IcebergTableInfo) -> String {
    let ns = info.namespace.join(".");
    let mut s = format!(
        "{ns}.{} [iceberg {}, format v{}]\n",
        info.name,
        match info.kind {
            crate::model::IcebergObjectKind::Table => "table",
            crate::model::IcebergObjectKind::View => "view",
        },
        info.format_version
    );
    s.push_str("columns (field id in parentheses; nested fields indented):\n");
    fn walk(out: &mut String, fields: &[IcebergField], depth: usize) {
        for f in fields {
            out.push_str(&field_line(f, depth));
            walk(out, &f.children, depth + 1);
        }
    }
    walk(&mut s, &info.fields, 0);

    let source = |id: i32| {
        info.fields
            .iter()
            .find(|f| f.id == id)
            .map(|f| f.name.clone())
            .unwrap_or_else(|| format!("field {id}"))
    };
    if info.partition_spec.is_empty() {
        s.push_str("partitioning: unpartitioned\n");
    } else {
        let parts: Vec<String> = info
            .partition_spec
            .iter()
            .map(|p| {
                if p.transform == "identity" {
                    source(p.source_id)
                } else {
                    format!("{}({})", p.transform, source(p.source_id))
                }
            })
            .collect();
        s.push_str(&format!(
            "partitioning: {} (filter on these columns to prune files)\n",
            parts.join(", ")
        ));
    }
    if !info.sort_order.is_empty() {
        let keys: Vec<String> = info
            .sort_order
            .iter()
            .map(|k| {
                let col = source(k.source_id);
                let col = if k.transform == "identity" {
                    col
                } else {
                    format!("{}({col})", k.transform)
                };
                format!("{col} {}", k.direction)
            })
            .collect();
        s.push_str(&format!("sort order: {}\n", keys.join(", ")));
    }
    if info.schema_count > 1 {
        s.push_str(&format!(
            "schema evolution: {} schema versions (current id {})\n",
            info.schema_count, info.current_schema_id
        ));
    }

    let n = info.snapshots.len();
    s.push_str(&format!(
        "snapshots: {n}{}\n",
        match &info.current_snapshot_id {
            Some(id) => format!(", current {id}"),
            None => ", no current snapshot (empty table)".into(),
        }
    ));
    s.push_str("  (newest first; time travel reads ns.\"table@<snapshot_id>\")\n");
    for snap in info.snapshots.iter().take(SNAPSHOTS_SHOWN) {
        s.push_str(&snapshot_line(snap));
    }
    if n > SNAPSHOTS_SHOWN {
        s.push_str(&format!(
            "  ... {} older snapshots not shown\n",
            n - SNAPSHOTS_SHOWN
        ));
    }
    if !info.refs.is_empty() {
        let refs: Vec<String> = info
            .refs
            .iter()
            .map(|r| format!("{} {} -> {}", r.kind, r.name, r.snapshot_id))
            .collect();
        s.push_str(&format!("refs: {}\n", refs.join("; ")));
    }
    s
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::model::{
        IcebergField, IcebergObjectKind, IcebergPartitionField, IcebergRef, IcebergSortField,
    };

    fn field(id: i32, name: &str, ty: &str, required: bool) -> IcebergField {
        IcebergField {
            id,
            name: name.into(),
            type_name: ty.into(),
            required,
            doc: None,
            children: vec![],
        }
    }

    fn snap(id: &str, ts: i64, op: &str, added: &str, total: &str) -> IcebergSnapshot {
        IcebergSnapshot {
            snapshot_id: id.into(),
            parent_snapshot_id: None,
            sequence_number: None,
            timestamp_ms: ts,
            operation: Some(op.into()),
            summary: BTreeMap::from([
                ("added-records".to_string(), added.to_string()),
                ("total-records".to_string(), total.to_string()),
                ("committer".to_string(), "alice@bank.example".to_string()),
            ]),
            manifest_list: Some("s3://secret-bucket/m.avro".into()),
            schema_id: Some(0),
        }
    }

    fn info() -> IcebergTableInfo {
        let mut tags = field(5, "tags", "list<string>", false);
        tags.children = vec![field(6, "element", "string", true)];
        let mut customer = field(1, "customer_id", "long", true);
        customer.doc = Some("who ordered".into());
        IcebergTableInfo {
            namespace: vec!["shop".into()],
            name: "orders".into(),
            kind: IcebergObjectKind::Table,
            location: "s3://secret-bucket/shop/orders".into(),
            metadata_location: Some("s3://secret-bucket/shop/orders/metadata/v9.json".into()),
            format_version: 2,
            table_uuid: None,
            last_updated_ms: None,
            current_snapshot_id: Some("300".into()),
            current_schema_id: 1,
            fields: vec![
                customer,
                field(2, "placed_at", "timestamptz", true),
                field(3, "total", "decimal(12, 2)", false),
                tags,
            ],
            schema_count: 2,
            partition_spec: vec![
                IcebergPartitionField {
                    source_id: 2,
                    field_id: 1000,
                    name: "placed_at_day".into(),
                    transform: "day".into(),
                },
                IcebergPartitionField {
                    source_id: 1,
                    field_id: 1001,
                    name: "customer_id".into(),
                    transform: "identity".into(),
                },
            ],
            sort_order: vec![IcebergSortField {
                source_id: 1,
                transform: "identity".into(),
                direction: "asc".into(),
                null_order: "nulls-first".into(),
            }],
            properties: BTreeMap::from([("owner".to_string(), "secret-team".to_string())]),
            snapshots: (0..8)
                .rev()
                .map(|i| {
                    snap(
                        &format!("{}", 100 + i * 100),
                        1_700_000_000_000 + i * 86_400_000,
                        if i == 3 { "overwrite" } else { "append" },
                        "10",
                        &format!("{}", (i + 1) * 10),
                    )
                })
                .collect(),
            refs: vec![IcebergRef {
                name: "main".into(),
                snapshot_id: "300".into(),
                kind: "branch".into(),
            }],
        }
    }

    #[test]
    fn renders_columns_partitioning_and_a_snapshot_summary() {
        let s = render_table_info(&info());
        assert!(
            s.starts_with("shop.orders [iceberg table, format v2]"),
            "{s}"
        );
        assert!(s.contains("customer_id long NOT NULL (id 1) -- who ordered"));
        assert!(s.contains("total decimal(12, 2) (id 3)"));
        assert!(
            s.contains("    element string NOT NULL (id 6)"),
            "nested fields are indented: {s}"
        );
        assert!(
            s.contains("partitioning: day(placed_at), customer_id"),
            "{s}"
        );
        assert!(s.contains("sort order: customer_id asc"));
        assert!(s.contains("2 schema versions"));
        assert!(s.contains("snapshots: 8, current 300"));
        assert!(s.contains("table@<snapshot_id>"));
        assert!(s.contains("... 3 older snapshots not shown"), "{s}");
        // Newest first, capped.
        let shown = s
            .lines()
            .filter(|l| l.starts_with("  ") && l.contains("20"))
            .count();
        assert!(shown >= SNAPSHOTS_SHOWN, "{s}");
        assert!(s.contains("700 ") && !s.contains("  200 "), "{s}");
        assert!(s.contains("+10 rows, 80 total"));
        assert!(s.contains("refs: branch main -> 300"));
    }

    #[test]
    fn leaves_out_locations_properties_and_free_text_summary_values() {
        let s = render_table_info(&info());
        for hidden in [
            "secret-bucket",
            "secret-team",
            "alice@bank.example",
            "manifest",
            "committer",
        ] {
            assert!(!s.contains(hidden), "{hidden} leaked: {s}");
        }
    }

    #[test]
    fn unpartitioned_empty_table() {
        let mut i = info();
        i.partition_spec.clear();
        i.sort_order.clear();
        i.snapshots.clear();
        i.current_snapshot_id = None;
        i.schema_count = 1;
        i.refs.clear();
        let s = render_table_info(&i);
        assert!(s.contains("partitioning: unpartitioned"));
        assert!(s.contains("snapshots: 0, no current snapshot"));
        assert!(
            !s.contains("sort order") && !s.contains("schema evolution") && !s.contains("refs:")
        );
    }
}
