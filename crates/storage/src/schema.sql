CREATE TABLE workspaces (
  id TEXT PRIMARY KEY, root TEXT NOT NULL UNIQUE, name TEXT NOT NULL,
  created_ms INTEGER NOT NULL
);
CREATE TABLE sessions (
  id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id),
  provider TEXT NOT NULL, native_id TEXT NOT NULL, native_version TEXT NOT NULL,
  reserved INTEGER NOT NULL CHECK(reserved IN (0,1)), created_ms INTEGER NOT NULL,
  UNIQUE(provider, native_id)
);
CREATE TABLE tasks (
  id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id),
  request_key TEXT NOT NULL, prompt TEXT NOT NULL, native_id TEXT,
  state TEXT NOT NULL CHECK(state IN ('submitting','running','unknown','completed','failed','cancelled')),
  created_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL,
  UNIQUE(session_id, request_key), UNIQUE(session_id, native_id)
);
CREATE UNIQUE INDEX one_active_task ON tasks(session_id)
  WHERE state IN ('submitting','running','unknown');
CREATE TABLE events (
  id INTEGER PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id),
  task_id TEXT REFERENCES tasks(id), sequence INTEGER NOT NULL,
  stream_id TEXT NOT NULL, provider_sequence INTEGER NOT NULL,
  payload TEXT NOT NULL, created_ms INTEGER NOT NULL,
  UNIQUE(session_id, sequence), UNIQUE(session_id, stream_id, provider_sequence)
);
CREATE INDEX event_cursor ON events(session_id, sequence);
PRAGMA user_version = 1;
