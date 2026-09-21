-- Each account's colour theme, picked from the set compiled into the app.
ALTER TABLE users ADD COLUMN theme TEXT NOT NULL DEFAULT 'printhub'
    CHECK (theme IN ('printhub', 'github', 'docker'));
