CREATE TABLE documents (
    id           UUID    PRIMARY KEY DEFAULT uuid_generate_v4(),
    note_id      UUID    NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    filename     TEXT    NOT NULL,
    content      BYTEA   NOT NULL,
    size_bytes   BIGINT  NOT NULL DEFAULT 0,
    created_at   TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW()
);
