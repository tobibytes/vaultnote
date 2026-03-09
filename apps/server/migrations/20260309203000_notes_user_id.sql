-- Optional owner association; NULL means the note was created anonymously.
ALTER TABLE notes ADD COLUMN user_id UUID REFERENCES users(id) ON DELETE SET NULL;
