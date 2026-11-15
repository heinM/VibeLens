-- Sample data for trying VibeLens locally. Loaded automatically by docker-compose.dev.yml
-- on the Postgres container's first start (docker-entrypoint-initdb.d).
CREATE SCHEMA IF NOT EXISTS shop;
COMMENT ON SCHEMA shop IS 'Demo e-commerce schema for VibeLens';

CREATE TABLE shop.customers (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    email       text NOT NULL UNIQUE,
    full_name   text NOT NULL,
    country     char(2) NOT NULL DEFAULT 'ZA',
    created_at  timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE shop.customers IS 'People who buy things';

CREATE TABLE shop.products (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    sku         text NOT NULL UNIQUE,
    name        text NOT NULL,
    price       numeric(12,2) NOT NULL CHECK (price >= 0),
    attributes  jsonb NOT NULL DEFAULT '{}'
);

CREATE TYPE shop.order_status AS ENUM ('pending', 'paid', 'shipped', 'cancelled');

CREATE TABLE shop.orders (
    id           bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    customer_id  bigint NOT NULL REFERENCES shop.customers(id),
    status       shop.order_status NOT NULL DEFAULT 'pending',
    total        numeric(14,2) NOT NULL DEFAULT 0,
    placed_at    timestamptz NOT NULL DEFAULT now(),
    notes        text
);
CREATE INDEX orders_customer_idx ON shop.orders (customer_id, placed_at DESC);

CREATE TABLE shop.order_items (
    order_id    bigint NOT NULL REFERENCES shop.orders(id) ON DELETE CASCADE,
    product_id  bigint NOT NULL REFERENCES shop.products(id),
    qty         int NOT NULL CHECK (qty > 0),
    unit_price  numeric(12,2) NOT NULL,
    PRIMARY KEY (order_id, product_id)
);

INSERT INTO shop.customers (email, full_name, country, created_at)
SELECT 'customer' || g || '@example.com',
       (ARRAY['Thandi','Pieter','Aisha','Johan','Lerato','Sipho','Anke','Ravi'])[1 + g % 8] || ' ' ||
       (ARRAY['Nkosi','van Wyk','Patel','Botha','Mokoena','Smith','Naidoo','Dlamini'])[1 + (g / 8) % 8],
       (ARRAY['ZA','ZA','ZA','NA','BW','KE','GB','NL'])[1 + g % 8],
       now() - (g || ' hours')::interval
FROM generate_series(1, 2000) g;

INSERT INTO shop.products (sku, name, price, attributes)
SELECT 'SKU-' || lpad(g::text, 4, '0'),
       (ARRAY['Rooibos tea','Biltong','Wine glass','Hiking boots','Kettle','Braai tongs','Notebook','Coffee beans'])[1 + g % 8] || ' #' || g,
       round((5 + random() * 995)::numeric, 2),
       jsonb_build_object('colour', (ARRAY['red','green','blue','black'])[1 + g % 4], 'weight_g', 100 + g * 7)
FROM generate_series(1, 200) g;

INSERT INTO shop.orders (customer_id, status, placed_at, notes)
SELECT 1 + (g * 7919) % 2000,
       (ARRAY['pending','paid','paid','shipped','shipped','cancelled']::shop.order_status[])[1 + g % 6],
       now() - (g || ' minutes')::interval,
       CASE WHEN g % 11 = 0 THEN 'gift wrap please' END
FROM generate_series(1, 25000) g;

INSERT INTO shop.order_items (order_id, product_id, qty, unit_price)
SELECT o.id, 1 + (o.id * k * 31) % 200, 1 + (o.id + k) % 4, p.price
FROM shop.orders o
CROSS JOIN generate_series(1, 3) k
JOIN shop.products p ON p.id = 1 + (o.id * k * 31) % 200
ON CONFLICT DO NOTHING;

UPDATE shop.orders o SET total = s.total
FROM (SELECT order_id, sum(qty * unit_price) AS total FROM shop.order_items GROUP BY order_id) s
WHERE s.order_id = o.id;

CREATE VIEW shop.v_open_orders AS
SELECT o.id, c.email, o.status, o.total, o.placed_at
FROM shop.orders o JOIN shop.customers c ON c.id = o.customer_id
WHERE o.status IN ('pending', 'paid');

CREATE MATERIALIZED VIEW shop.mv_daily_revenue AS
SELECT date_trunc('day', placed_at) AS day, count(*) AS orders, sum(total) AS revenue
FROM shop.orders WHERE status <> 'cancelled' GROUP BY 1;

CREATE FUNCTION shop.customer_lifetime_value(p_customer bigint) RETURNS numeric
LANGUAGE sql STABLE AS $$
    SELECT coalesce(sum(total), 0) FROM shop.orders WHERE customer_id = p_customer AND status <> 'cancelled'
$$;

-- A read-only role, as the README recommends for agent use.
CREATE ROLE vibelens_agent LOGIN PASSWORD 'agent';
GRANT USAGE ON SCHEMA shop TO vibelens_agent;
GRANT SELECT ON ALL TABLES IN SCHEMA shop TO vibelens_agent;
ALTER ROLE vibelens_agent SET default_transaction_read_only = on;
ANALYZE;
