CREATE TABLE spools (
    id INTEGER PRIMARY KEY,
    material TEXT NOT NULL,
    brand TEXT NOT NULL,
    color_name TEXT NOT NULL,
    color_hex TEXT NOT NULL,
    owner_id INTEGER REFERENCES users (id) ON DELETE SET NULL,
    price_cents INTEGER,
    initial_grams REAL NOT NULL,
    remaining_grams REAL NOT NULL,
    notes TEXT NOT NULL,
    archived INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0, 1)),
    created_at INTEGER NOT NULL
) STRICT;

-- Both sides are unique: a tray holds one spool and a spool sits in one tray.
CREATE TABLE tray_bindings (
    canvas_id INTEGER NOT NULL,
    tray_id INTEGER NOT NULL,
    spool_id INTEGER NOT NULL UNIQUE REFERENCES spools (id) ON DELETE CASCADE,
    bound_at INTEGER NOT NULL,
    PRIMARY KEY (canvas_id, tray_id)
) STRICT;

-- The ledger behind `spools.remaining_grams`. Positive grams were used up; a weigh-in that
-- finds more filament than recorded is negative.
CREATE TABLE consumption (
    id INTEGER PRIMARY KEY,
    spool_id INTEGER NOT NULL REFERENCES spools (id) ON DELETE CASCADE,
    user_id INTEGER REFERENCES users (id) ON DELETE SET NULL,
    kind TEXT NOT NULL CHECK (kind IN ('print', 'estimate', 'weigh_in')),
    grams REAL NOT NULL,
    note TEXT NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;

CREATE INDEX consumption_by_spool ON consumption (spool_id, created_at);
