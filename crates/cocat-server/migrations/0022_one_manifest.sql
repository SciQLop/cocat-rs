-- A project has at most one file list.
CREATE UNIQUE INDEX document_one_manifest ON document(room_id) WHERE kind = 'manifest';
