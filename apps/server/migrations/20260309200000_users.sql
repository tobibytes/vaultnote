CREATE TABLE users (
    id            UUID    PRIMARY KEY DEFAULT uuid_generate_v4(),
    email         TEXT    NOT NULL UNIQUE,
    password_hash TEXT    NOT NULL,
    created_at    TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW()
);
