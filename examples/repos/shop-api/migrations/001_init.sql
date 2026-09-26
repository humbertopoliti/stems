-- shop-api schema: one table, applied by the hello-shop workspace's postgres seed.
CREATE TABLE IF NOT EXISTS products (
    id          SERIAL PRIMARY KEY,
    name        TEXT    NOT NULL,
    price_cents INTEGER NOT NULL CHECK (price_cents >= 0)
);

INSERT INTO products (name, price_cents) VALUES
    ('Espresso cup', 900),
    ('Pour-over kettle', 4500),
    ('Burr grinder', 12900)
ON CONFLICT DO NOTHING;
