//! Initialize and validate the current development database schema.
use super::storage;
use crate::error::{Error, Result};
use rusqlite::Connection;

fn unsupported() -> Error {
    Error::new(
        "DATABASE_SCHEMA",
        "这份运行数据的结构不受支持；原数据未改写。请使用受支持版本的数据目录。",
    )
}

pub(super) fn initialize(db: &Connection) -> Result<()> {
    let version: u32 = db
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(storage)?;
    if version > 7 {
        return Err(Error::new(
            "DATABASE_VERSION",
            "数据库版本高于当前程序，请使用对应的新版本。",
        ));
    }
    let populated: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%')", [], |r|r.get(0)).map_err(storage)?;
    if !populated {
        let tx = db.unchecked_transaction().map_err(storage)?;
        tx.execute_batch(include_str!("schema.sql"))
            .map_err(storage)?;
        return tx.commit().map_err(storage);
    }
    if version != 7 {
        return Err(unsupported());
    }
    for (table, columns) in [
        ("settings", "id,document"),
        (
            "requests",
            "id,started_at,finished_at,kind,action,account_id,account_name,group_id,group_name,status,duration_ms,error_code,wait_ms,input_tokens,output_tokens,cached_input_tokens,key_id,native_thread_id,native_turn_id,model,response_model,response_service_tier,model_outcome,failure_stage,cleanup_error_code,first_delta_ms,delivery_status,key_name,reasoning_effort,user_agent,streaming,parameter_handling,scheduling,subagent_kind,service_tier,request_content,response_content,endpoint,client_transport,upstream_transport",
        ),
        (
            "api_keys",
            "id,name,prefix,secret_hash,group_id,models,state,created_at,expires_at,last_authenticated_at,revision,max_concurrency,secret,fast_mode",
        ),
        ("native_capabilities", "account_id,document"),
        ("account_quotas", "account_id,document"),
        ("account_reset_automation", "account_id,document"),
        (
            "response_affinities",
            "owner,group_id,model,fingerprint,account,binding,expires",
        ),
    ] {
        db.prepare(&format!("SELECT {columns} FROM {table} LIMIT 0"))
            .map_err(|_| unsupported())?;
    }
    let obsolete: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name IN ('audit','account_tasks'))",
            [],
            |r| r.get(0),
        )
        .map_err(storage)?;
    let triggers: i64=db.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name IN ('response_key_delete','response_key_revoke')",[],|r|r.get(0)).map_err(storage)?;
    let missing_secrets: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM api_keys WHERE secret IS NULL OR secret='')",
            [],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if obsolete || triggers != 2 || missing_secrets {
        return Err(unsupported());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn current() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(include_str!("schema.sql")).unwrap();
        db.execute_batch("INSERT INTO requests(id,started_at,kind,action,status) VALUES('old',1,'inference','responses','succeeded');").unwrap();
        db
    }
    #[test]
    fn initializes_an_empty_database() {
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        initialize(&db).unwrap();
        db.prepare("SELECT service_tier,response_service_tier FROM requests")
            .unwrap();
        assert_eq!(
            db.pragma_query_value::<u32, _>(None, "user_version", |r| r.get(0))
                .unwrap(),
            7
        );
    }

    #[test]
    fn current_schema_is_validated_without_modifying_rows() {
        let db = current();
        initialize(&db).unwrap();
        initialize(&db).unwrap();
        assert_eq!(
            db.pragma_query_value::<u32, _>(None, "user_version", |r| r.get(0))
                .unwrap(),
            7
        );
        let record: (String, Option<String>) = db
            .query_row(
                "SELECT status,scheduling FROM requests WHERE id='old'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(record, ("succeeded".into(), None));
    }
    #[test]
    fn invalid_or_future_schemas_are_not_modified() {
        let db = current();
        db.execute_batch("DROP TRIGGER response_key_delete;")
            .unwrap();
        assert_eq!(initialize(&db).unwrap_err().code, "DATABASE_SCHEMA");
        assert_eq!(
            db.pragma_query_value::<u32, _>(None, "user_version", |r| r.get(0))
                .unwrap(),
            7
        );
        assert!(
            db.prepare("SELECT request_content,response_content FROM requests")
                .is_ok()
        );
        db.execute_batch("PRAGMA user_version=8;").unwrap();
        assert_eq!(initialize(&db).unwrap_err().code, "DATABASE_VERSION");
    }

    #[test]
    fn missing_actual_service_tier_is_rejected_without_rebuilding_data() {
        let db = current();
        db.execute_batch("ALTER TABLE requests DROP COLUMN response_service_tier;")
            .unwrap();
        assert_eq!(initialize(&db).unwrap_err().code, "DATABASE_SCHEMA");
        assert_eq!(
            db.query_row("SELECT status FROM requests WHERE id='old'", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
            "succeeded"
        );
        assert!(
            db.prepare("SELECT response_service_tier FROM requests")
                .is_err()
        );
    }
}
