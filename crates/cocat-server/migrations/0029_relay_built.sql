-- The fingerprint of the lists a provider has built into the server, as of its last run that
-- worked: a server built with other lists runs the provider at start.
ALTER TABLE relay_provider ADD COLUMN built TEXT;
