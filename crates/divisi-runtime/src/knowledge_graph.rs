//! A knowledge-graph memory backend — entities, their observations, and
//! typed relations between them — stored in the same SQLite database as
//! everything else in `divisi-runtime` (spec section 9: "Knowledge" is
//! one of the named memory categories, separate from the scoped
//! working/project/task memory already in `memory.rs`).
//!
//! The shape (entity name + type + observations; relation from/to/type)
//! deliberately mirrors the MCP `@modelcontextprotocol/server-memory`
//! convention, which is already configured and in real use on this
//! project's own reference machine — a proven, real-world pattern to
//! converge on rather than inventing a new graph schema.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use divisi_protocol::{KgEntity, KgRelation, KnowledgeGraphSnapshot};

pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS kg_entities (
            name TEXT PRIMARY KEY,
            entity_type TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS kg_observations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            entity_name TEXT NOT NULL REFERENCES kg_entities(name) ON DELETE CASCADE,
            content TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS kg_relations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            from_entity TEXT NOT NULL REFERENCES kg_entities(name) ON DELETE CASCADE,
            to_entity TEXT NOT NULL REFERENCES kg_entities(name) ON DELETE CASCADE,
            relation_type TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        PRAGMA foreign_keys = ON;",
    )?;
    Ok(())
}

/// Creates the entity if it doesn't exist yet; a no-op (not an error) if
/// it already does, since callers typically want "make sure this entity
/// exists" rather than a strict create.
pub fn create_entity(conn: &Connection, name: &str, entity_type: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO kg_entities (name, entity_type, created_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(name) DO NOTHING",
        params![name, entity_type, chrono::Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

pub fn add_observation(conn: &Connection, entity_name: &str, content: &str) -> Result<i64> {
    if !entity_exists(conn, entity_name)? {
        anyhow::bail!("no such entity: {entity_name}");
    }
    conn.execute(
        "INSERT INTO kg_observations (entity_name, content, created_at) VALUES (?1, ?2, ?3)",
        params![entity_name, content, chrono::Utc::now().to_rfc3339()],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn create_relation(conn: &Connection, from: &str, to: &str, relation_type: &str) -> Result<i64> {
    if !entity_exists(conn, from)? {
        anyhow::bail!("no such entity: {from}");
    }
    if !entity_exists(conn, to)? {
        anyhow::bail!("no such entity: {to}");
    }
    conn.execute(
        "INSERT INTO kg_relations (from_entity, to_entity, relation_type, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![from, to, relation_type, chrono::Utc::now().to_rfc3339()],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn delete_entity(conn: &Connection, name: &str) -> Result<bool> {
    let affected = conn.execute("DELETE FROM kg_entities WHERE name = ?1", params![name])?;
    Ok(affected > 0)
}

fn entity_exists(conn: &Connection, name: &str) -> Result<bool> {
    conn.query_row("SELECT 1 FROM kg_entities WHERE name = ?1", params![name], |_| Ok(()))
        .optional_bool()
}

trait OptionalBool {
    fn optional_bool(self) -> Result<bool>;
}
impl<T> OptionalBool for rusqlite::Result<T> {
    fn optional_bool(self) -> Result<bool> {
        match self {
            Ok(_) => Ok(true),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(e) => Err(e).context("checking entity existence"),
        }
    }
}

pub fn get_entity(conn: &Connection, name: &str) -> Result<Option<KgEntity>> {
    let Some(created_at) = conn
        .query_row("SELECT created_at FROM kg_entities WHERE name = ?1", params![name], |r| r.get::<_, String>(0))
        .ok()
    else {
        return Ok(None);
    };
    let entity_type: String = conn.query_row("SELECT entity_type FROM kg_entities WHERE name = ?1", params![name], |r| r.get(0))?;
    let observations = observations_for(conn, name)?;
    Ok(Some(KgEntity { name: name.to_string(), entity_type, observations, created_at }))
}

fn observations_for(conn: &Connection, entity_name: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT content FROM kg_observations WHERE entity_name = ?1 ORDER BY created_at ASC")?;
    let rows = stmt.query_map(params![entity_name], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Substring search over entity name, type, and observation content —
/// same "not semantic search" honesty as `memory.rs::search`.
pub fn query(conn: &Connection, term: &str) -> Result<Vec<KgEntity>> {
    let pattern = format!("%{term}%");
    let mut stmt = conn.prepare(
        "SELECT DISTINCT e.name, e.entity_type, e.created_at FROM kg_entities e
         LEFT JOIN kg_observations o ON o.entity_name = e.name
         WHERE e.name LIKE ?1 OR e.entity_type LIKE ?1 OR o.content LIKE ?1
         ORDER BY e.created_at DESC",
    )?;
    let names: Vec<(String, String, String)> = stmt
        .query_map(params![pattern], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    names
        .into_iter()
        .map(|(name, entity_type, created_at)| {
            let observations = observations_for(conn, &name)?;
            Ok(KgEntity { name, entity_type, observations, created_at })
        })
        .collect()
}

/// Full graph dump, mirroring `@modelcontextprotocol/server-memory`'s
/// `read_graph` — every entity with its observations, and every relation.
pub fn read_graph(conn: &Connection) -> Result<KnowledgeGraphSnapshot> {
    let mut stmt = conn.prepare("SELECT name FROM kg_entities ORDER BY name ASC")?;
    let names: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let entities = names.into_iter().map(|n| get_entity(conn, &n).map(|e| e.unwrap())).collect::<Result<Vec<_>>>()?;

    let mut rel_stmt = conn.prepare("SELECT from_entity, to_entity, relation_type, created_at FROM kg_relations ORDER BY created_at ASC")?;
    let relations = rel_stmt
        .query_map([], |r| Ok(KgRelation { from_entity: r.get(0)?, to_entity: r.get(1)?, relation_type: r.get(2)?, created_at: r.get(3)? }))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(KnowledgeGraphSnapshot { entities, relations })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        ensure_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn create_entity_is_idempotent() {
        let conn = test_conn();
        create_entity(&conn, "alice", "person").unwrap();
        create_entity(&conn, "alice", "person").unwrap(); // no error
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM kg_entities", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn observations_accumulate_in_order() {
        let conn = test_conn();
        create_entity(&conn, "alice", "person").unwrap();
        add_observation(&conn, "alice", "likes rust").unwrap();
        add_observation(&conn, "alice", "works on divisi").unwrap();
        let entity = get_entity(&conn, "alice").unwrap().unwrap();
        assert_eq!(entity.observations, vec!["likes rust", "works on divisi"]);
    }

    #[test]
    fn add_observation_fails_for_unknown_entity() {
        let conn = test_conn();
        assert!(add_observation(&conn, "ghost", "x").is_err());
    }

    #[test]
    fn relations_require_both_entities_to_exist() {
        let conn = test_conn();
        create_entity(&conn, "alice", "person").unwrap();
        assert!(create_relation(&conn, "alice", "bob", "knows").is_err());
        create_entity(&conn, "bob", "person").unwrap();
        assert!(create_relation(&conn, "alice", "bob", "knows").is_ok());
    }

    #[test]
    fn deleting_entity_cascades_observations_and_relations() {
        let conn = test_conn();
        create_entity(&conn, "alice", "person").unwrap();
        create_entity(&conn, "bob", "person").unwrap();
        add_observation(&conn, "alice", "x").unwrap();
        create_relation(&conn, "alice", "bob", "knows").unwrap();

        assert!(delete_entity(&conn, "alice").unwrap());
        assert!(!delete_entity(&conn, "alice").unwrap());

        let obs_count: i64 = conn.query_row("SELECT COUNT(*) FROM kg_observations", [], |r| r.get(0)).unwrap();
        let rel_count: i64 = conn.query_row("SELECT COUNT(*) FROM kg_relations", [], |r| r.get(0)).unwrap();
        assert_eq!(obs_count, 0);
        assert_eq!(rel_count, 0);
    }

    #[test]
    fn query_matches_name_type_and_observation_content() {
        let conn = test_conn();
        create_entity(&conn, "divisi", "project").unwrap();
        create_entity(&conn, "alice", "person").unwrap();
        add_observation(&conn, "alice", "maintains divisi").unwrap();

        assert_eq!(query(&conn, "divisi").unwrap().len(), 2); // matches entity name AND the observation mentioning it
        assert_eq!(query(&conn, "project").unwrap().len(), 1);
    }

    #[test]
    fn read_graph_returns_full_snapshot() {
        let conn = test_conn();
        create_entity(&conn, "alice", "person").unwrap();
        create_entity(&conn, "bob", "person").unwrap();
        create_relation(&conn, "alice", "bob", "knows").unwrap();

        let snapshot = read_graph(&conn).unwrap();
        assert_eq!(snapshot.entities.len(), 2);
        assert_eq!(snapshot.relations.len(), 1);
        assert_eq!(snapshot.relations[0].relation_type, "knows");
    }
}
