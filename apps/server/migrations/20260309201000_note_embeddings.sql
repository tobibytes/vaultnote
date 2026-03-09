-- Cosine similarity between two float4 arrays of the same length.
CREATE OR REPLACE FUNCTION cosine_similarity(a float4[], b float4[])
RETURNS float4 AS $$
  SELECT
    (SELECT sum(a_i * b_i) FROM unnest(a, b) AS t(a_i, b_i)) /
    NULLIF(
      sqrt((SELECT sum(a_i * a_i) FROM unnest(a) AS t(a_i))) *
      sqrt((SELECT sum(b_i * b_i) FROM unnest(b) AS t(b_i))),
      0
    )
$$ LANGUAGE sql IMMUTABLE;

-- One embedding vector per note (model-specific; multiple rows allowed per note
-- when embeddings are regenerated with a new model).
CREATE TABLE note_embeddings (
    id         UUID    PRIMARY KEY DEFAULT uuid_generate_v4(),
    note_id    UUID    NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    embedding  float4[] NOT NULL,
    model      TEXT    NOT NULL DEFAULT 'bow-384',
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW()
);

CREATE INDEX note_embeddings_note_id_idx ON note_embeddings(note_id);
