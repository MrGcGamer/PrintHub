-- The build plate type the G-code heats the bed for, as OrcaSlicer names it
-- (`Textured PEI Plate`); NULL when the file does not name one.
ALTER TABLE jobs ADD COLUMN plate TEXT;
