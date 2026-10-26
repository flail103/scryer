use sqlx::Row;

#[tokio::test]
async fn reason_code_migration_preserves_text_and_backfills_legacy_conditions() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::raw_sql(
        "CREATE TABLE indexers (id TEXT NOT NULL PRIMARY KEY, last_error_message TEXT);
         CREATE TABLE rule_sets (id TEXT NOT NULL PRIMARY KEY, disabled_reason TEXT);
         INSERT INTO indexers VALUES ('caps', 'caps refresh failed: old diagnostic'), ('other', 'search failed'), ('caps-upper', 'Caps refresh failed: operator text');
         INSERT INTO rule_sets VALUES ('retired', 'uses retired input.release.guide_facts; update the rule source before enabling it.'), ('other', 'operator disabled');",
    ).execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../scryer/src/db/migrations/0312_error_reason_codes.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let caps =
        sqlx::query("SELECT last_error_code, last_error_message FROM indexers WHERE id = 'caps'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        caps.get::<String, _>("last_error_code"),
        "INDEXER_CAPS_REFRESH_FAILED"
    );
    assert_eq!(
        caps.get::<String, _>("last_error_message"),
        "caps refresh failed: old diagnostic"
    );
    let retired: String =
        sqlx::query_scalar("SELECT disabled_reason_code FROM rule_sets WHERE id = 'retired'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(retired, "RULE_INPUT_RETIRED");
    let unrelated: Option<String> =
        sqlx::query_scalar("SELECT last_error_code FROM indexers WHERE id = 'other'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(unrelated.is_none());
    let different_case: Option<String> =
        sqlx::query_scalar("SELECT last_error_code FROM indexers WHERE id = 'caps-upper'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(different_case.is_none());
}
