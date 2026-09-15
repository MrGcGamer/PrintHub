-- Rights an admin grants to individual members; admins hold every one without a row here.
-- `permission` has no CHECK, so adding a right needs no table rebuild; names the running
-- version does not know are ignored.
CREATE TABLE user_permissions (
    user_id INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    permission TEXT NOT NULL,
    granted_by INTEGER REFERENCES users (id) ON DELETE SET NULL,
    granted_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, permission)
) STRICT;

-- The printer does not report its nozzle, so people record it. NULL until someone does;
-- PRINTER_NOZZLE is assumed until then.
ALTER TABLE printer_state ADD COLUMN nozzle TEXT CHECK (nozzle IN ('0.2', '0.4', '0.6', '0.8'));
ALTER TABLE printer_state ADD COLUMN nozzle_changed_by INTEGER REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE printer_state ADD COLUMN nozzle_changed_at INTEGER;

-- The nozzle diameter the G-code was sliced for; NULL when the file does not name one.
ALTER TABLE jobs ADD COLUMN nozzle_mm REAL;
