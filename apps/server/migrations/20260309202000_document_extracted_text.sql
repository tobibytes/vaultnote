-- Stores text extracted from uploaded documents (e.g. PDF, plain-text).
-- NULL means extraction has not been attempted yet or the file has no
-- extractable text.
ALTER TABLE documents ADD COLUMN extracted_text TEXT;
