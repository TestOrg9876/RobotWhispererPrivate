use rusqlite::{Connection, Transaction};

use crate::{CoreError, CoreResult};

const MIGRATIONS: &[fn(&Transaction<'_>) -> rusqlite::Result<()>] = &[
    migration_1,
    migration_2,
    migration_3,
    migration_4,
    migration_5,
    migration_6,
    migration_7,
];

pub(super) fn run(conn: &mut Connection) -> CoreResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY)",
    )
    .map_err(|err| CoreError::Storage(err.to_string()))?;

    let current: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .map_err(|err| CoreError::Storage(err.to_string()))?;

    for (offset, migration) in MIGRATIONS.iter().enumerate() {
        let version = (offset as i64) + 1;
        if version <= current {
            continue;
        }
        let tx = conn
            .transaction()
            .map_err(|err| CoreError::Storage(err.to_string()))?;
        migration(&tx)
            .map_err(|err| CoreError::Storage(format!("migration {version} failed: {err}")))?;
        tx.execute(
            "INSERT INTO schema_migrations (version) VALUES (?1)",
            [version],
        )
        .map_err(|err| CoreError::Storage(err.to_string()))?;
        tx.commit()
            .map_err(|err| CoreError::Storage(err.to_string()))?;
    }
    Ok(())
}

fn migration_1(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE collections (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            parent_id INTEGER REFERENCES collections(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE connections (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            transport_kind TEXT NOT NULL CHECK (transport_kind IN ('foxglove_ws','rosbridge','native_ros2')),
            config_json TEXT NOT NULL,
            auto_connect INTEGER NOT NULL DEFAULT 0,
            color TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX idx_connections_name ON connections(name);

        CREATE TABLE requests (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            collection_id INTEGER REFERENCES collections(id) ON DELETE SET NULL,
            connection_id INTEGER REFERENCES connections(id) ON DELETE SET NULL,
            name TEXT NOT NULL,
            kind TEXT NOT NULL CHECK (kind IN ('topic','service','action')),
            target TEXT NOT NULL,
            schema_name TEXT,
            schema_hash TEXT,
            input_json TEXT NOT NULL DEFAULT '{}',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX idx_requests_collection ON requests(collection_id);
        CREATE INDEX idx_requests_connection ON requests(connection_id);

        CREATE TABLE schemas (
            hash TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            definition TEXT NOT NULL,
            parsed_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE INDEX idx_schemas_name ON schemas(name);
        "#,
    )?;
    Ok(())
}

fn migration_2(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch("DELETE FROM schemas;")?;
    Ok(())
}

fn migration_3(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE connections_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            transport_kind TEXT NOT NULL CHECK (transport_kind IN ('foxglove_ws','rosbridge','native_ros2','dummy')),
            config_json TEXT NOT NULL,
            auto_connect INTEGER NOT NULL DEFAULT 0,
            color TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        INSERT INTO connections_new SELECT * FROM connections;
        DROP TABLE connections;
        ALTER TABLE connections_new RENAME TO connections;
        CREATE INDEX idx_connections_name ON connections(name);
        "#,
    )?;
    Ok(())
}

fn migration_4(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch("ALTER TABLE requests ADD COLUMN visualization_json TEXT;")?;
    Ok(())
}

/// Lets a recording be stored as a connection.
///
/// SQLite cannot widen a `CHECK` in place, so the table is rebuilt — the same
/// dance migration 3 did to add `dummy`. Columns are named rather than starred
/// so a future column added between these two cannot silently transpose them.
fn migration_5(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE connections_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            transport_kind TEXT NOT NULL CHECK (transport_kind IN ('foxglove_ws','rosbridge','native_ros2','dummy','replay')),
            config_json TEXT NOT NULL,
            auto_connect INTEGER NOT NULL DEFAULT 0,
            color TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        INSERT INTO connections_new
            (id, name, transport_kind, config_json, auto_connect, color, created_at, updated_at)
        SELECT id, name, transport_kind, config_json, auto_connect, color, created_at, updated_at
        FROM connections;
        DROP TABLE connections;
        ALTER TABLE connections_new RENAME TO connections;
        CREATE INDEX idx_connections_name ON connections(name);
        "#,
    )?;
    Ok(())
}

/// Dashboards: a named arrangement of live views, saved like a request is.
fn migration_6(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE dashboards (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            layout_json TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX idx_dashboards_name ON dashboards(name);
        "#,
    )?;
    Ok(())
}

/// Lets a request address a node's parameters.
///
/// ROS 2 parameters are ordinary services on the node, so a parameter request
/// stores exactly what the other kinds store — only the `CHECK` has to learn
/// the word. SQLite cannot widen one in place, so the table is rebuilt the way
/// migration 5 rebuilt `connections`: columns named rather than starred, and
/// the two indexes recreated, because `DROP TABLE` takes them with it.
fn migration_7(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE requests_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            collection_id INTEGER REFERENCES collections(id) ON DELETE SET NULL,
            connection_id INTEGER REFERENCES connections(id) ON DELETE SET NULL,
            name TEXT NOT NULL,
            kind TEXT NOT NULL CHECK (kind IN ('topic','service','action','param')),
            target TEXT NOT NULL,
            schema_name TEXT,
            schema_hash TEXT,
            input_json TEXT NOT NULL DEFAULT '{}',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            visualization_json TEXT
        );
        INSERT INTO requests_new
            (id, collection_id, connection_id, name, kind, target, schema_name,
             schema_hash, input_json, created_at, updated_at, visualization_json)
        SELECT id, collection_id, connection_id, name, kind, target, schema_name,
               schema_hash, input_json, created_at, updated_at, visualization_json
        FROM requests;
        DROP TABLE requests;
        ALTER TABLE requests_new RENAME TO requests;
        CREATE INDEX idx_requests_collection ON requests(collection_id);
        CREATE INDEX idx_requests_connection ON requests(connection_id);
        "#,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rebuilding a table to widen a `CHECK` is the one migration shape that
    /// can silently lose or transpose data, and the fresh-database tests never
    /// see it — they create the widened table directly. So build a database
    /// that stopped at version 6, put a row in it, and step it forward.
    fn database_at_version_6() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY)",
        )
        .expect("bootstrap");
        for (offset, migration) in MIGRATIONS.iter().take(6).enumerate() {
            let version = (offset as i64) + 1;
            let tx = conn.transaction().expect("begin");
            migration(&tx).expect("migrate");
            tx.execute(
                "INSERT INTO schema_migrations (version) VALUES (?1)",
                [version],
            )
            .expect("record");
            tx.commit().expect("commit");
        }
        conn
    }

    #[test]
    fn a_request_written_before_the_parameter_kind_survives_the_rebuild() {
        let mut conn = database_at_version_6();
        conn.execute_batch(
            r#"
            INSERT INTO collections (id, parent_id, name, created_at)
            VALUES (1, NULL, 'Nav', '2026-01-01T00:00:00Z');
            INSERT INTO requests
                (id, collection_id, connection_id, name, kind, target, schema_name,
                 schema_hash, input_json, created_at, updated_at, visualization_json)
            VALUES (7, 1, NULL, 'Scan', 'topic', '/scan', 'sensor_msgs/LaserScan',
                    'hash', '{"a":1}', '2026-01-01T00:00:00Z', '2026-01-02T00:00:00Z',
                    '{"view":"plot"}');
            "#,
        )
        .expect("seed");

        run(&mut conn).expect("upgrade");

        /// Every column of the row, in the order the rebuild lists them.
        type Row = (
            i64,
            Option<i64>,
            String,
            String,
            String,
            Option<String>,
            String,
            String,
            Option<String>,
        );

        let row: Row = conn
            .query_row(
                "SELECT id, collection_id, name, kind, target, schema_name, input_json,
                        updated_at, visualization_json
                 FROM requests WHERE id = 7",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                    ))
                },
            )
            .expect("the row came through");

        assert_eq!(
            row,
            (
                7,
                Some(1),
                "Scan".to_string(),
                "topic".to_string(),
                "/scan".to_string(),
                Some("sensor_msgs/LaserScan".to_string()),
                r#"{"a":1}"#.to_string(),
                "2026-01-02T00:00:00Z".to_string(),
                Some(r#"{"view":"plot"}"#.to_string()),
            )
        );
    }

    #[test]
    fn an_upgraded_database_accepts_a_parameter_request_and_still_refuses_a_nonsense_one() {
        let mut conn = database_at_version_6();
        run(&mut conn).expect("upgrade");

        conn.execute(
            "INSERT INTO requests (name, kind, target, input_json, created_at, updated_at)
             VALUES ('Planner', 'param', '/planner', '{}', 'now', 'now')",
            [],
        )
        .expect("the constraint learned the parameter kind");

        let nonsense = conn.execute(
            "INSERT INTO requests (name, kind, target, input_json, created_at, updated_at)
             VALUES ('Nope', 'parameter', '/planner', '{}', 'now', 'now')",
            [],
        );
        assert!(nonsense.is_err(), "the constraint is still a constraint");
    }

    /// `DROP TABLE` takes the table's indexes with it, so a rebuild that forgets
    /// to recreate them leaves every later lookup scanning.
    #[test]
    fn the_rebuild_leaves_the_indexes_it_found() {
        let mut conn = database_at_version_6();
        run(&mut conn).expect("upgrade");

        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'requests' ORDER BY name")
            .expect("prepare");
        let names: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("collect");

        assert_eq!(
            names,
            ["idx_requests_collection", "idx_requests_connection"]
        );
    }
}
