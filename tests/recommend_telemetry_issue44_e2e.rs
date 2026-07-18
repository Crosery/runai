#![cfg(not(target_os = "windows"))]

use runai::core::db::{Database, RouterEvent};

fn base_event() -> RouterEvent {
    RouterEvent {
        id: None,
        ts: 1,
        provider: "mock".into(),
        model: "mock".into(),
        prompt_tokens: 1,
        completion_tokens: 1,
        reasoning_tokens: 0,
        total_tokens: 2,
        cache_hit_tokens: 0,
        cache_miss_tokens: 0,
        latency_ms: 3,
        chosen_skills_json: "[]".into(),
        candidate_count: 1,
        status: "ok".into(),
        error_msg: None,
        session_id: "rnai_sess_test".into(),
        mode: "exclusive".into(),
        user_prompt: "test".into(),
        cwd: "/tmp".into(),
        bm25_kept: 1,
        llm_raw_response: "NONE".into(),
        hook_output: String::new(),
        llm_input: "test".into(),
        intent_llm_input: String::new(),
        intent_llm_output: String::new(),
        intent_status: String::new(),
        intent_error_msg: None,
        bm25_candidates_json: r#"["C01"]"#.into(),
        user_id: None,
    }
}

#[test]
fn issue44_router_event_schema_roundtrips_routing_and_empty_attribution() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("runai.db")).unwrap();
    let columns: Vec<String> = {
        let conn = rusqlite::Connection::open(tmp.path().join("runai.db")).unwrap();
        let mut stmt = conn.prepare("PRAGMA table_info(router_events)").unwrap();
        stmt.query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    for required in [
        "routing_mode",
        "empty_reason",
        "retrieval_query",
        "parsed_candidates_json",
        "filtered_candidates_json",
        "parser_recovery",
        "llm_call_count",
    ] {
        assert!(
            columns.iter().any(|column| column == required),
            "缺少遥测列 {required}"
        );
    }

    db.insert_router_event(&base_event()).unwrap();
    let event = db.router_recent_events(1).unwrap().pop().unwrap();
    assert_eq!(event.status, "ok");
    assert_eq!(event.chosen_skills_json, "[]");
}
