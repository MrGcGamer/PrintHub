-- Uniform scale applied when slicing, in percent. NULL on every job sliced before this, and on
-- G-code jobs, which are never scaled here.
ALTER TABLE jobs ADD COLUMN scale_percent REAL CHECK (scale_percent > 0);
