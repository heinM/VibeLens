"""Rows of the demo Iceberg tables, recomputed without a catalog.

scripts/seed/iceberg-seed.py builds shop.customers and shop.orders from random.Random(42). This
replays the same calls in the same order, so the aggregates below are what a query over the seeded
tables returns. Prints JSON for refresh-data.mjs.
"""
import datetime as dt
import decimal
import json
import random

rnd = random.Random(42)
base = dt.datetime(2026, 10, 1, tzinfo=dt.timezone.utc)

# shop.customers: 500 rows, one rnd.choice per country
countries = [rnd.choice(["ZA", "NA", "BW", "KE", "GB", "NL"]) for _ in range(500)]


def batch(start, n, pool):
    # Same order of rnd calls as the seed's batch(): customer ids, then statuses, then totals.
    cust = [rnd.randint(1, 500) for _ in range(n)]
    status = [rnd.choice(pool) for _ in range(n)]
    total = [decimal.Decimal(rnd.randint(500, 99999)) / 100 for _ in range(n)]
    placed = [base + dt.timedelta(minutes=37 * i) for i in range(start, start + n)]
    return list(zip(range(start, start + n), cust, status, total, placed))


batch(1, 2000, ["pending", "paid"])  # snapshot 1 (replaced by the overwrite)
batch(2001, 1500, ["paid", "shipped"])  # snapshot 2 (replaced by the overwrite)
orders = batch(1, 3000, ["paid", "shipped", "cancelled"])  # snapshot 3: the current table

by_country = {}
for _id, cust, status, total, _at in orders:
    if status == "cancelled":
        continue
    c = countries[cust - 1]
    n, s = by_country.get(c, (0, decimal.Decimal(0)))
    by_country[c] = (n + 1, s + total)

rows = [[c, str(n), str(s)] for c, (n, s) in sorted(by_country.items(), key=lambda kv: -kv[1][1])]

print(
    json.dumps(
        {
            "revenueByCountry": {
                "sql": "SELECT c.country, count(*) AS orders, sum(o.total) AS revenue\nFROM shop.orders o\nJOIN shop.customers c ON c.id = o.customer_id\nWHERE o.status <> 'cancelled'\nGROUP BY c.country\nORDER BY revenue DESC;",
                # Trino's own type names; bigint and decimal arrive as strings (remote/trino.rs).
                "columns": [
                    {"name": "country", "typeName": "varchar", "typeOid": 0},
                    {"name": "orders", "typeName": "bigint", "typeOid": 0},
                    {"name": "revenue", "typeName": "decimal(38,2)", "typeOid": 0},
                ],
                "rows": rows,
            },
            "orderCount": len(orders),
            "customerCount": len(countries),
        },
        indent=1,
    )
)
