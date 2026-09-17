-- The spool's owner and the filament's value as they were when the entry was written, so that
-- reassigning a spool or correcting its price does not rewrite who used whose filament.
-- `spool_owner_id` NULL means a shared spool; `value_cents` NULL means the spool had no price.
ALTER TABLE consumption ADD COLUMN spool_owner_id INTEGER REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE consumption ADD COLUMN value_cents REAL;

-- Earlier entries get the spool as it is now, the closest record there is.
UPDATE consumption SET
    spool_owner_id = (SELECT s.owner_id FROM spools s WHERE s.id = consumption.spool_id),
    value_cents = (SELECT consumption.grams * s.price_cents / s.initial_grams FROM spools s
                   WHERE s.id = consumption.spool_id
                     AND s.price_cents IS NOT NULL AND s.initial_grams > 0);

CREATE INDEX consumption_by_time ON consumption (created_at);

-- Money handed over for filament; it offsets what the balances say is owed.
CREATE TABLE settlements (
    id INTEGER PRIMARY KEY,
    from_user INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    to_user INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    amount_cents INTEGER NOT NULL CHECK (amount_cents > 0),
    recorded_by INTEGER REFERENCES users (id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL,
    CHECK (from_user != to_user)
) STRICT;
