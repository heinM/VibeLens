#!/usr/bin/env python3
"""Seed demo Iceberg tables with pyiceberg.

Two targets:
  * local REST catalog + MinIO from docker-compose.dev.yml (default):
        python3 scripts/seed/iceberg-seed.py
  * a self-contained SQL (SQLite) catalog with a file:// warehouse (used by Rust tests):
        python3 scripts/seed/iceberg-seed.py --sql /tmp/ice/catalog.db --warehouse /tmp/ice/warehouse

Creates namespace `shop` with:
  orders   (partitioned by day(placed_at), 3 snapshots: 2 appends + 1 overwrite) for time travel
  customers (unpartitioned), events (bucket(8, customer_id) + identity(kind), nested struct/list/map)
and namespace `history`, two tables whose history is not a straight line, for time travel by
timestamp (the snapshot that was current then is not always the newest one from before then):
  rolled_back  100 rows, +50, rolled back to the first snapshot, +10 on top of it  -> 110 rows
  branched     100 rows, +50 on branch `audit` only, +10 on main                   -> 110 rows
Requires: pip install "pyiceberg[sql-sqlite,pyarrow,s3fs,pyiceberg-core]==0.12.0"
(pyiceberg 0.11+ needs pyiceberg-core for the bucket transform used by shop.events)
"""
import argparse
import datetime as dt
import decimal
import random
import time

import pyarrow as pa
from pyiceberg.catalog import load_catalog
from pyiceberg.exceptions import NamespaceAlreadyExistsError, NoSuchTableError
from pyiceberg.partitioning import PartitionField, PartitionSpec
from pyiceberg.schema import Schema
from pyiceberg.transforms import BucketTransform, DayTransform, IdentityTransform
from pyiceberg.types import (
    BooleanType, DecimalType, DoubleType, IntegerType, ListType, LongType, MapType, NestedField,
    StringType, StructType, TimestamptzType,
)


def catalog(args):
    if args.sql:
        return load_catalog("vg_test", **{"type": "sql", "uri": f"sqlite:///{args.sql}", "warehouse": f"file://{args.warehouse}"})
    return load_catalog("rest", **{
        "type": "rest", "uri": args.rest_uri,
        "s3.endpoint": args.s3_endpoint, "s3.access-key-id": "admin", "s3.secret-access-key": "password",
        "s3.region": "us-east-1", "s3.path-style-access": "true",
    })


def ensure_bucket(endpoint, bucket="warehouse", timeout_s=60):
    """Creates the S3 bucket the REST fixture's warehouse points at, waiting for the store to come up."""
    import time
    from urllib.parse import urlparse

    from pyarrow import fs as pafs

    u = urlparse(endpoint)
    s3 = pafs.S3FileSystem(
        access_key="admin", secret_key="password", region="us-east-1",
        endpoint_override=u.netloc, scheme=u.scheme or "http", allow_bucket_creation=True,
    )
    deadline = time.monotonic() + timeout_s
    while True:
        try:
            if s3.get_file_info(bucket).type == pafs.FileType.NotFound:
                s3.create_dir(bucket)
            return
        except OSError as e:
            if time.monotonic() > deadline:
                raise SystemExit(f"S3 store at {endpoint} not reachable or bucket not creatable: {e}")
            time.sleep(1)


def recreate(cat, ident, schema, spec=PartitionSpec()):
    try:
        cat.drop_table(ident)
    except NoSuchTableError:
        pass
    return cat.create_table(ident, schema=schema, partition_spec=spec, properties={"write.format.default": "parquet"})


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sql", help="SQLite catalog file path (enables SQL catalog mode)")
    ap.add_argument("--warehouse", default="/tmp/vibelens-iceberg/warehouse")
    ap.add_argument("--rest-uri", default="http://localhost:8181")
    ap.add_argument("--s3-endpoint", default="http://localhost:9000")
    args = ap.parse_args()
    if args.sql:
        import os
        os.makedirs(args.warehouse, exist_ok=True)
        os.makedirs(os.path.dirname(args.sql) or ".", exist_ok=True)
    if not args.sql:
        ensure_bucket(args.s3_endpoint)
    cat = catalog(args)
    try:
        cat.create_namespace("shop", {"owner": "vibelens-demo", "comment": "Demo e-commerce lakehouse"})
    except NamespaceAlreadyExistsError:
        pass
    rnd = random.Random(42)
    base = dt.datetime(2026, 10, 1, tzinfo=dt.timezone.utc)

    customers = recreate(cat, "shop.customers", Schema(
        NestedField(1, "id", LongType(), required=True),
        NestedField(2, "email", StringType(), required=True),
        NestedField(3, "country", StringType()),
        NestedField(4, "vip", BooleanType()),
        identifier_field_ids=[1],
    ))
    customers.append(pa.table({
        "id": pa.array(range(1, 501), pa.int64()),
        "email": [f"customer{i}@example.com" for i in range(1, 501)],
        "country": [rnd.choice(["ZA", "NA", "BW", "KE", "GB", "NL"]) for _ in range(500)],
        "vip": [i % 17 == 0 for i in range(1, 501)],
    }, schema=customers.schema().as_arrow()))

    order_schema = Schema(
        NestedField(1, "id", LongType(), required=True),
        NestedField(2, "customer_id", LongType(), required=True),
        NestedField(3, "status", StringType()),
        NestedField(4, "total", DecimalType(12, 2)),
        NestedField(5, "placed_at", TimestamptzType(), required=True),
    )
    orders = recreate(cat, "shop.orders", order_schema, PartitionSpec(PartitionField(5, 1000, DayTransform(), "placed_day")))

    def batch(start, n, status_pool):
        return pa.table({
            "id": pa.array(range(start, start + n), pa.int64()),
            "customer_id": pa.array([rnd.randint(1, 500) for _ in range(n)], pa.int64()),
            "status": [rnd.choice(status_pool) for _ in range(n)],
            "total": [decimal.Decimal(rnd.randint(500, 99999)) / 100 for _ in range(n)],
            "placed_at": [base + dt.timedelta(minutes=37 * i) for i in range(start, start + n)],
        }, schema=orders.schema().as_arrow())

    orders.append(batch(1, 2000, ["pending", "paid"]))          # snapshot 1
    orders.append(batch(2001, 1500, ["paid", "shipped"]))       # snapshot 2
    orders.overwrite(batch(1, 3000, ["paid", "shipped", "cancelled"]))  # snapshot 3 (overwrite)

    events_schema = Schema(
        NestedField(1, "event_id", LongType(), required=True),
        NestedField(2, "customer_id", LongType()),
        NestedField(3, "kind", StringType()),
        NestedField(4, "score", DoubleType()),
        NestedField(5, "ctx", StructType(NestedField(6, "device", StringType()), NestedField(7, "app_version", IntegerType()))),
        NestedField(8, "tags", ListType(9, StringType())),
        NestedField(10, "attrs", MapType(11, StringType(), 12, StringType())),
    )
    events = recreate(cat, "shop.events", events_schema, PartitionSpec(
        PartitionField(2, 1001, BucketTransform(8), "customer_bucket"), PartitionField(3, 1002, IdentityTransform(), "kind"),
    ))
    n = 800
    events.append(pa.table({
        "event_id": pa.array(range(1, n + 1), pa.int64()),
        "customer_id": pa.array([rnd.randint(1, 500) for _ in range(n)], pa.int64()),
        "kind": [rnd.choice(["view", "cart", "checkout"]) for _ in range(n)],
        "score": [rnd.random() for _ in range(n)],
        "ctx": [{"device": rnd.choice(["ios", "android", "web"]), "app_version": rnd.randint(100, 140)} for _ in range(n)],
        "tags": [[rnd.choice(["a", "b", "c"]) for _ in range(rnd.randint(0, 3))] for _ in range(n)],
        "attrs": [[("ref", f"r{i}"), ("utm", "mail")] for i in range(n)],
    }, schema=events.schema().as_arrow()))

    # Two histories that are not a straight line. The pauses put the commits (and the rollback,
    # which makes no snapshot) some milliseconds apart, so a test can name a time between them.
    try:
        cat.create_namespace("history", {"comment": "Tables whose history is not a straight line"})
    except NamespaceAlreadyExistsError:
        pass
    id_schema = Schema(NestedField(1, "id", LongType(), required=True))

    def ids(table, start, n):
        return pa.table({"id": pa.array(range(start, start + n), pa.int64())}, schema=table.schema().as_arrow())

    def pause():
        time.sleep(0.05)

    rolled_back = recreate(cat, "history.rolled_back", id_schema)
    rolled_back.append(ids(rolled_back, 1, 100))                 # snapshot 1: 100 rows
    first = rolled_back.current_snapshot().snapshot_id
    pause()
    rolled_back.append(ids(rolled_back, 101, 50))                # snapshot 2: 150 rows
    pause()
    rolled_back.manage_snapshots().rollback_to_snapshot(first).commit()  # back at snapshot 1
    pause()
    rolled_back.append(ids(rolled_back, 1001, 10))               # snapshot 3, on top of 1: 110 rows

    branched = recreate(cat, "history.branched", id_schema)
    branched.append(ids(branched, 1, 100))                       # snapshot 1 (main): 100 rows
    branched.manage_snapshots().create_branch(branched.current_snapshot().snapshot_id, "audit").commit()
    pause()
    branched.append(ids(branched, 101, 50), branch="audit")      # snapshot 2 (audit only): 150 rows
    pause()
    branched.append(ids(branched, 1001, 10))                     # snapshot 3 (main), on top of 1: 110 rows

    for t in ("shop.customers", "shop.orders", "shop.events", "history.rolled_back", "history.branched"):
        tbl = cat.load_table(t)
        print(f"✓ {t}: {len(tbl.snapshots())} snapshot(s), current {tbl.current_snapshot().snapshot_id}")


if __name__ == "__main__":
    main()
