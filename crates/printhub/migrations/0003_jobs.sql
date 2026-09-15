CREATE TABLE jobs (
    id INTEGER PRIMARY KEY,
    owner_id INTEGER REFERENCES users (id) ON DELETE SET NULL,
    -- The uploaded file's name, for display only; files on disk and on the printer use the id.
    name TEXT NOT NULL,
    source TEXT NOT NULL CHECK (source IN ('stl', 'gcode')),
    state TEXT NOT NULL CHECK (state IN (
        'slicing', 'awaiting_confirm', 'queued', 'uploading', 'printing', 'done', 'failed', 'cancelled'
    )),
    -- Queue order among queued jobs, lowest first.
    position INTEGER NOT NULL,
    -- Slice settings, set for STL jobs only.
    process_profile TEXT,
    filament_profile TEXT,
    supports INTEGER CHECK (supports IN (0, 1)),
    infill_percent INTEGER CHECK (infill_percent BETWEEN 0 AND 100),
    estimated_seconds INTEGER,
    layers INTEGER,
    printer_task_uuid TEXT,
    progress INTEGER NOT NULL DEFAULT 0 CHECK (progress BETWEEN 0 AND 100),
    error TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    started_at INTEGER,
    finished_at INTEGER
) STRICT;

CREATE INDEX jobs_by_state ON jobs (state, position);

-- One row per slicer filament the G-code uses. `spool_id` is chosen when the job is confirmed;
-- the tray is looked up from the bindings when the print starts.
CREATE TABLE job_tools (
    job_id INTEGER NOT NULL REFERENCES jobs (id) ON DELETE CASCADE,
    tool_index INTEGER NOT NULL,
    material TEXT NOT NULL,
    color_hex TEXT NOT NULL,
    grams REAL NOT NULL,
    spool_id INTEGER REFERENCES spools (id) ON DELETE SET NULL,
    canvas_id INTEGER,
    tray_id INTEGER,
    PRIMARY KEY (job_id, tool_index)
) STRICT;

CREATE TABLE schedule_rules (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('allow', 'deny')),
    -- Bit 0 is Monday, bit 6 Sunday.
    days INTEGER NOT NULL CHECK (days BETWEEN 1 AND 127),
    start_minute INTEGER NOT NULL CHECK (start_minute BETWEEN 0 AND 1439),
    -- At or before `start_minute`, the window ends on the following day.
    end_minute INTEGER NOT NULL CHECK (end_minute BETWEEN 0 AND 1439),
    must_finish_before INTEGER NOT NULL CHECK (must_finish_before IN (0, 1)),
    label TEXT NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;

-- A single row. The bed starts out not clear: nobody has looked at it yet.
CREATE TABLE printer_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    bed_clear INTEGER NOT NULL CHECK (bed_clear IN (0, 1)),
    bed_changed_by INTEGER REFERENCES users (id) ON DELETE SET NULL,
    bed_changed_at INTEGER NOT NULL
) STRICT;

INSERT INTO printer_state (id, bed_clear, bed_changed_at) VALUES (1, 0, 0);

ALTER TABLE consumption ADD COLUMN job_id INTEGER REFERENCES jobs (id) ON DELETE SET NULL;
