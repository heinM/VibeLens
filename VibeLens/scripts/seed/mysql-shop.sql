-- Demo `shop` database for the MySQL service in docker-compose.dev.yml (run once, on first
-- start, by the image's entrypoint). Mirrors 01-shop.sql and adds the MySQL types the grid maps
-- specially (BIGINT UNSIGNED, DECIMAL, BIT, JSON, GEOMETRY, BLOB, zero dates).
--
-- Users:
--   vibelens / vibelens      all privileges on `shop` (a normal application user)
--   vibelens_ro / vibelens   SELECT and SHOW VIEW on `shop` only, no EXECUTE (what an agent
--                            connection should use: it cannot call stored functions)

CREATE DATABASE IF NOT EXISTS shop CHARACTER SET utf8mb4 COLLATE utf8mb4_0900_ai_ci;
USE shop;

-- Zero dates need a sql_mode without NO_ZERO_DATE for the insert below.
SET SESSION sql_mode = 'ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES,ERROR_FOR_DIVISION_BY_ZERO,NO_ENGINE_SUBSTITUTION';

CREATE TABLE customers (
  id          BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  email       VARCHAR(255) NOT NULL UNIQUE,
  full_name   VARCHAR(200) NOT NULL,
  country     CHAR(2) NOT NULL DEFAULT 'ZA',
  created_at  DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
  last_login  DATE NULL COMMENT 'zero date for customers who never logged in',
  flags       BIT(4) NOT NULL DEFAULT b'0000',
  profile     JSON NULL
) COMMENT = 'People who buy things';

CREATE TABLE products (
  id          INT NOT NULL AUTO_INCREMENT PRIMARY KEY,
  sku         VARCHAR(40) NOT NULL UNIQUE,
  name        VARCHAR(200) NOT NULL,
  price       DECIMAL(12, 2) NOT NULL,
  weight_kg   DOUBLE NULL,
  thumbnail   BLOB NULL,
  warehouse   POINT NULL SRID 4326
);

CREATE TABLE orders (
  id           BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
  customer_id  BIGINT UNSIGNED NOT NULL,
  status       ENUM('new', 'paid', 'shipped', 'cancelled') NOT NULL DEFAULT 'new',
  total        DECIMAL(65, 30) NOT NULL DEFAULT 0,
  placed_at    TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
  INDEX idx_orders_customer (customer_id, placed_at),
  CONSTRAINT fk_orders_customer FOREIGN KEY (customer_id) REFERENCES customers (id)
);

CREATE TABLE order_items (
  order_id    BIGINT NOT NULL,
  product_id  INT NOT NULL,
  quantity    SMALLINT UNSIGNED NOT NULL,
  unit_price  DECIMAL(12, 2) NOT NULL,
  PRIMARY KEY (order_id, product_id),
  CONSTRAINT fk_items_order FOREIGN KEY (order_id) REFERENCES orders (id),
  CONSTRAINT fk_items_product FOREIGN KEY (product_id) REFERENCES products (id)
);

INSERT INTO customers (email, full_name, country, last_login, flags, profile) VALUES
  ('ada@example.com', 'Ada Lovelace', 'GB', '2026-11-01', b'0101', '{"tier": "gold", "tags": ["early", "vip"]}'),
  ('alan@example.com', 'Alan Turing', 'GB', '0000-00-00', b'0001', NULL),
  ('grace@example.com', 'Grace Hopper', 'US', NULL, b'1000', '{"tier": "silver"}'),
  ('nomsa@example.com', 'Nomsa Dlamini', 'ZA', '2026-10-30', b'0000', '{"tier": "bronze", "note": "ünïcödé ✓"}');

INSERT INTO products (sku, name, price, weight_kg, thumbnail, warehouse) VALUES
  ('KB-01', 'Keyboard', 1299.00, 0.9, X'89504E470D0A1A0A', ST_GeomFromText('POINT(-33.92 18.42)', 4326)),
  ('MS-02', 'Mouse', 349.50, 0.1, NULL, NULL),
  ('MN-27', 'Monitor 27"', 5499.99, 6.5, NULL, ST_GeomFromText('POINT(-26.20 28.04)', 4326));

INSERT INTO orders (customer_id, status, total) VALUES
  (1, 'paid', 1648.50),
  (1, 'shipped', 5499.99),
  (3, 'new', 349.50);

INSERT INTO order_items (order_id, product_id, quantity, unit_price) VALUES
  (1, 1, 1, 1299.00), (1, 2, 1, 349.50), (2, 3, 1, 5499.99), (3, 2, 1, 349.50);

CREATE VIEW customer_totals AS
  SELECT c.id, c.full_name, COUNT(o.id) AS orders, COALESCE(SUM(o.total), 0) AS spent
  FROM customers c LEFT JOIN orders o ON o.customer_id = c.id
  GROUP BY c.id, c.full_name;

CREATE FUNCTION order_count(cid BIGINT UNSIGNED) RETURNS INT
  READS SQL DATA
  RETURN (SELECT COUNT(*) FROM orders WHERE customer_id = cid);

CREATE PROCEDURE recent_orders(IN n INT)
  READS SQL DATA
  SELECT * FROM orders ORDER BY placed_at DESC LIMIT n;

CREATE USER IF NOT EXISTS 'vibelens'@'%' IDENTIFIED BY 'vibelens';
GRANT ALL PRIVILEGES ON shop.* TO 'vibelens'@'%';
CREATE USER IF NOT EXISTS 'vibelens_ro'@'%' IDENTIFIED BY 'vibelens';
GRANT SELECT, SHOW VIEW ON shop.* TO 'vibelens_ro'@'%';
