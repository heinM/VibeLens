//! Mapping from `iceberg::spec` metadata to the IPC model ([`IcebergTableInfo`], [`TableDescription`]).
//!
//! Pure functions, no I/O: everything here is unit-tested against hand-built metadata and, in the
//! integration tests, against tables written by pyiceberg.

use std::collections::BTreeMap;

use iceberg::spec::{NestedField, Operation, SnapshotRef, TableMetadata, Type};
use iceberg::table::Table;
use serde_json::Value;

use crate::model::{
    ColumnInfo, IcebergField, IcebergObjectKind, IcebergPartitionField, IcebergRef,
    IcebergSnapshot, IcebergSortField, IcebergTableInfo, ObjectKind, TableDescription,
};

/// Iceberg type as text: `long`, `decimal(12, 2)`, `struct<a: int, b: string>`, `list<string>`, `map<string, long>`.
pub(crate) fn type_string(ty: &Type) -> String {
    match ty {
        Type::Primitive(p) => p.to_string(),
        Type::Struct(s) => {
            let inner: Vec<String> = s
                .fields()
                .iter()
                .map(|f| format!("{}: {}", f.name, type_string(&f.field_type)))
                .collect();
            format!("struct<{}>", inner.join(", "))
        }
        Type::List(l) => format!("list<{}>", type_string(&l.element_field.field_type)),
        Type::Map(m) => format!(
            "map<{}, {}>",
            type_string(&m.key_field.field_type),
            type_string(&m.value_field.field_type)
        ),
        Type::Variant(_) => "variant".to_string(),
    }
}

/// Maps a schema field, recursing into struct members, list elements (`element`) and map entries (`key`, `value`).
pub(crate) fn map_field(f: &NestedField) -> IcebergField {
    let children = match f.field_type.as_ref() {
        Type::Struct(s) => s.fields().iter().map(|c| map_field(c)).collect(),
        Type::List(l) => vec![named(map_field(&l.element_field), "element")],
        Type::Map(m) => vec![
            named(map_field(&m.key_field), "key"),
            named(map_field(&m.value_field), "value"),
        ],
        Type::Primitive(_) | Type::Variant(_) => Vec::new(),
    };
    IcebergField {
        id: f.id,
        name: f.name.clone(),
        type_name: type_string(&f.field_type),
        required: f.required,
        doc: f.doc.clone(),
        children,
    }
}

fn named(mut f: IcebergField, name: &str) -> IcebergField {
    f.name = name.to_string();
    f
}

fn map_snapshot(s: &SnapshotRef) -> IcebergSnapshot {
    let summary: BTreeMap<String, String> = s
        .summary()
        .additional_properties
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    IcebergSnapshot {
        snapshot_id: s.snapshot_id().to_string(),
        parent_snapshot_id: s.parent_snapshot_id().map(|p| p.to_string()),
        sequence_number: Some(s.sequence_number().to_string()),
        timestamp_ms: s.timestamp_ms(),
        operation: Some(operation_str(&s.summary().operation).to_string()),
        summary,
        manifest_list: Some(s.manifest_list().to_string()),
        schema_id: s.schema_id(),
    }
}

fn operation_str(op: &Operation) -> &str {
    op.as_str()
}

/// Branches and tags. `TableMetadata` keeps its refs private, so they are read from its JSON form
/// (which is the on-disk format, so stable). A table with a current snapshot but no explicit
/// `main` ref (format v1) gets a synthetic `main`.
pub(crate) fn refs_of(md: &TableMetadata) -> Vec<IcebergRef> {
    let mut out = Vec::new();
    if let Ok(Value::Object(root)) = serde_json::to_value(md)
        && let Some(Value::Object(refs)) = root.get("refs")
    {
        for (name, r) in refs {
            let Some(id) = r.get("snapshot-id").and_then(Value::as_i64) else {
                continue;
            };
            let kind = r
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("branch")
                .to_string();
            out.push(IcebergRef {
                name: name.clone(),
                snapshot_id: id.to_string(),
                kind,
            });
        }
    }
    if !out.iter().any(|r| r.name == "main")
        && let Some(id) = md.current_snapshot_id()
    {
        out.push(IcebergRef {
            name: "main".into(),
            snapshot_id: id.to_string(),
            kind: "branch".into(),
        });
    }
    // `main` first, then branches, then tags, each alphabetical: stable for the UI.
    out.sort_by(|a, b| {
        let rank = |r: &IcebergRef| (r.name != "main", r.kind != "branch", r.name.clone());
        rank(a).cmp(&rank(b))
    });
    out
}

/// Full table description for the explorer.
pub(crate) fn describe(table: &Table, namespace: Vec<String>, name: &str) -> IcebergTableInfo {
    let md = table.metadata();
    let schema = md.current_schema();
    let spec = md.default_partition_spec();
    let mut snapshots: Vec<IcebergSnapshot> = md.snapshots().map(map_snapshot).collect();
    // Newest first; the sequence number breaks ties between snapshots committed in the same millisecond.
    snapshots.sort_by(|a, b| {
        b.timestamp_ms.cmp(&a.timestamp_ms).then_with(|| {
            let seq = |s: &IcebergSnapshot| {
                s.sequence_number
                    .as_deref()
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(0)
            };
            seq(b).cmp(&seq(a))
        })
    });
    IcebergTableInfo {
        namespace,
        name: name.to_string(),
        kind: IcebergObjectKind::Table,
        location: md.location().to_string(),
        metadata_location: table.metadata_location().map(str::to_string),
        format_version: md.format_version() as u8,
        table_uuid: Some(md.uuid().to_string()),
        last_updated_ms: Some(md.last_updated_ms()),
        current_snapshot_id: md.current_snapshot_id().map(|i| i.to_string()),
        current_schema_id: md.current_schema_id(),
        fields: schema
            .as_struct()
            .fields()
            .iter()
            .map(|f| map_field(f))
            .collect(),
        schema_count: md.schemas_iter().len(),
        partition_spec: spec
            .fields()
            .iter()
            .map(|p| IcebergPartitionField {
                source_id: p.source_id,
                field_id: p.field_id,
                name: p.name.clone(),
                transform: p.transform.to_string(),
            })
            .collect(),
        sort_order: md
            .default_sort_order()
            .fields
            .iter()
            .map(|s| IcebergSortField {
                source_id: s.source_id,
                transform: s.transform.to_string(),
                direction: match s.direction {
                    iceberg::spec::SortDirection::Ascending => "asc",
                    iceberg::spec::SortDirection::Descending => "desc",
                }
                .to_string(),
                null_order: match s.null_order {
                    iceberg::spec::NullOrder::First => "nulls-first",
                    iceberg::spec::NullOrder::Last => "nulls-last",
                }
                .to_string(),
            })
            .collect(),
        properties: md
            .properties()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        snapshots,
        refs: refs_of(md),
    }
}

/// One table as the editor/agent sees it: `schema` = namespace levels joined by `.`, top-level columns only.
pub(crate) fn table_description(
    table: &Table,
    namespace: &[String],
    name: &str,
) -> TableDescription {
    let md = table.metadata();
    let schema = md.current_schema();
    let pk: std::collections::HashSet<i32> = schema.identifier_field_ids().collect();
    let columns = schema
        .as_struct()
        .fields()
        .iter()
        .enumerate()
        .map(|(i, f)| ColumnInfo {
            name: f.name.clone(),
            data_type: type_string(&f.field_type),
            nullable: !f.required,
            default: None,
            is_primary_key: pk.contains(&f.id),
            ordinal: i32::try_from(i + 1).unwrap_or(i32::MAX),
            comment: f.doc.clone(),
        })
        .collect();
    let estimated_rows = md.current_snapshot().and_then(|s| {
        s.summary()
            .additional_properties
            .get("total-records")
            .and_then(|v| v.parse::<i64>().ok())
    });
    TableDescription {
        schema: namespace.join("."),
        name: name.to_string(),
        kind: if md.default_partition_spec().is_unpartitioned() {
            ObjectKind::Table
        } else {
            ObjectKind::PartitionedTable
        },
        comment: md.properties().get("comment").cloned(),
        estimated_rows,
        columns,
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use iceberg::spec::{ListType, MapType, NestedField, PrimitiveType, StructType, Type};

    use super::*;

    fn prim(p: PrimitiveType) -> Box<Type> {
        Box::new(Type::Primitive(p))
    }

    fn nested() -> NestedField {
        // struct<device: string, versions: list<int>, attrs: map<string, decimal(12, 2)>>
        NestedField::optional(
            1,
            "ctx",
            Type::Struct(StructType::new(vec![
                Arc::new(NestedField::optional(
                    2,
                    "device",
                    *prim(PrimitiveType::String),
                )),
                Arc::new(NestedField::optional(
                    3,
                    "versions",
                    Type::List(ListType::new(Arc::new(NestedField::required(
                        4,
                        "element",
                        *prim(PrimitiveType::Int),
                    )))),
                )),
                Arc::new(NestedField::optional(
                    5,
                    "attrs",
                    Type::Map(MapType::new(
                        Arc::new(NestedField::required(
                            6,
                            "key",
                            *prim(PrimitiveType::String),
                        )),
                        Arc::new(NestedField::optional(
                            7,
                            "value",
                            *prim(PrimitiveType::Decimal {
                                precision: 12,
                                scale: 2,
                            }),
                        )),
                    )),
                )),
            ])),
        )
    }

    #[test]
    fn type_strings() {
        let f = nested();
        assert_eq!(
            type_string(&f.field_type),
            "struct<device: string, versions: list<int>, attrs: map<string, decimal(12, 2)>>"
        );
        assert_eq!(
            type_string(&Type::Primitive(PrimitiveType::Timestamptz)),
            "timestamptz"
        );
        assert_eq!(
            type_string(&Type::Primitive(PrimitiveType::Fixed(16))),
            "fixed(16)"
        );
    }

    #[test]
    fn nested_children_carry_field_ids_and_canonical_names() {
        let m = map_field(&nested());
        assert_eq!(m.id, 1);
        assert!(!m.required);
        assert_eq!(
            m.children
                .iter()
                .map(|c| (c.id, c.name.as_str()))
                .collect::<Vec<_>>(),
            vec![(2, "device"), (3, "versions"), (5, "attrs")]
        );
        let versions = &m.children[1];
        assert_eq!(versions.type_name, "list<int>");
        assert_eq!(versions.children.len(), 1);
        assert_eq!(
            (
                versions.children[0].id,
                versions.children[0].name.as_str(),
                versions.children[0].required
            ),
            (4, "element", true)
        );
        let attrs = &m.children[2];
        assert_eq!(
            attrs
                .children
                .iter()
                .map(|c| (c.id, c.name.as_str()))
                .collect::<Vec<_>>(),
            vec![(6, "key"), (7, "value")]
        );
        assert_eq!(attrs.children[1].type_name, "decimal(12, 2)");
    }
}
