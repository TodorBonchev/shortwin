#!/bin/sh
# Roles of the reference deployment (docs/design/INDEXER.md §12.1): `shortwin_ingest` owns the
# schema (migrations, the single writer, the rollup job); `shortwin_api` reads only.
set -eu
ingest_pw=$(cat /run/secrets/shortwin/pg_ingest)
api_pw=$(cat /run/secrets/shortwin/pg_api)
psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" <<SQL
CREATE ROLE shortwin_ingest LOGIN PASSWORD '${ingest_pw}';
CREATE ROLE shortwin_api LOGIN PASSWORD '${api_pw}';
GRANT CONNECT ON DATABASE ${POSTGRES_DB} TO shortwin_ingest, shortwin_api;
GRANT USAGE, CREATE ON SCHEMA public TO shortwin_ingest;
GRANT USAGE ON SCHEMA public TO shortwin_api;
-- Everything the ingest role creates is readable by the API role.
ALTER DEFAULT PRIVILEGES FOR ROLE shortwin_ingest IN SCHEMA public
    GRANT SELECT ON TABLES TO shortwin_api;
ALTER DEFAULT PRIVILEGES FOR ROLE shortwin_ingest IN SCHEMA public
    GRANT EXECUTE ON FUNCTIONS TO shortwin_api;
-- The API's own guard rails (§10.2).
ALTER ROLE shortwin_api SET statement_timeout = '2s';
ALTER ROLE shortwin_api SET default_transaction_read_only = on;
SQL
