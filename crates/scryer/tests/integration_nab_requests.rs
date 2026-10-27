#![recursion_limit = "256"]

#[path = "integration_nab_requests/admission.rs"]
mod admission;
mod common;
#[path = "../src/nab_api/mod.rs"]
mod nab_api;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use common::TestContext;
use scryer_application::{SettingsRepository, indexer_clients::*, testing::AppUseCaseTestExt};
use scryer_infrastructure_acquisition::indexers::client_store::IndexerClientStore;
use scryer_infrastructure_crypto::{EncryptionKey, artifact_reference::ArtifactReferenceCipher};
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;

async fn fixture() -> (TestContext, Router, String, Arc<IndexerClientStore>) {
    let context = TestContext::new().await;
    context
        .settings_store
        .batch_ensure_setting_definitions(vec![
            scryer_infrastructure_sql::types::SettingDefinitionSeed {
                category: "ui".into(),
                scope: scryer_application::SETTINGS_SCOPE_SYSTEM.into(),
                key_name: scryer_application::EXPERIMENTAL_FEATURES_ENABLED_KEY.into(),
                data_type: "boolean".into(),
                default_value_json: "false".into(),
                is_sensitive: false,
                validation_json: None,
            },
        ])
        .await
        .unwrap();
    let store = Arc::new(IndexerClientStore::new(context.db.datastore()));
    let key = issue_indexer_client_key("test-salt").unwrap();
    let now = chrono::Utc::now();
    assert!(
        store
            .save(
                &IndexerClientRecord {
                    id: "http-client".into(),
                    configuration: IndexerClientConfiguration {
                        name: "Consumer fixture".into(),
                        enabled: true,
                        protocols: vec![
                            IndexerClientProtocol::Newznab,
                            IndexerClientProtocol::Torznab
                        ],
                        grants: vec![],
                    },
                    lookup_id: Some(key.lookup_id),
                    secret_hash: Some(key.secret_hash),
                    key_generation: 1,
                    revision: 1,
                    last_used_at: None,
                    last_outcome_code: None,
                    created_at: now,
                    updated_at: now,
                },
                None
            )
            .await
            .unwrap()
    );
    let app = context
        .app
        .with_test_overrides(|builder| builder.with_indexer_client_repository(store.clone()));
    let router = nab_api::router(nab_api::NabState {
        app,
        cipher: Arc::new(ArtifactReferenceCipher::new(&EncryptionKey::from_bytes(
            [42; 32],
        ))),
        base_path: String::new(),
        tls: false,
    });
    (context, router, key.raw_key, store)
}

async fn enable(context: &TestContext, enabled: bool) {
    context
        .settings_store
        .upsert_setting_json(
            scryer_application::SETTINGS_SCOPE_SYSTEM,
            scryer_application::EXPERIMENTAL_FEATURES_ENABLED_KEY,
            None,
            enabled.to_string(),
            "test".into(),
            None,
        )
        .await
        .unwrap();
}

async fn call(router: &Router, uri: &str, key: Option<(&str, &str)>) -> (StatusCode, String) {
    let mut request = Request::builder().uri(uri).header("host", "scryer.example");
    if let Some((name, value)) = key {
        request = request.header(name, value);
    }
    let response = tokio::time::timeout(
        Duration::from_secs(60),
        router.clone().oneshot(request.body(Body::empty()).unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    let status = response.status();
    let bytes = tokio::time::timeout(
        Duration::from_secs(60),
        to_bytes(response.into_body(), 1024 * 1024),
    )
    .await
    .unwrap()
    .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn shared_routes_require_the_experimental_gate_and_a_client_key_for_caps_and_search() {
    let (context, router, key, _) = fixture().await;
    for path in ["/newznab/api", "/torznab/api"] {
        let (status, body) = call(
            &router,
            &format!("{path}?t=caps"),
            Some(("x-api-key", &key)),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(body.contains("910"));
    }
    enable(&context, true).await;
    for path in ["/newznab/api", "/torznab/api"] {
        for operation in ["caps", "search"] {
            let (status, body) = call(&router, &format!("{path}?t={operation}&o=json"), None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert!(body.contains("100"));
        }
        let (status, xml) = call(&router, &format!("{path}?t=caps&apikey={key}"), None).await;
        assert_eq!(status, StatusCode::OK, "{xml}");
        assert!(xml.contains("<caps>") && xml.contains("default=\"100\""));
        let (status, json) = call(
            &router,
            &format!("{path}?t=caps&o=json"),
            Some(("authorization", &format!("Bearer {key}"))),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        serde_json::from_str::<serde_json::Value>(&json).unwrap();
        let (status, xml) = call(
            &router,
            &format!("{path}?t=search&q=fixture"),
            Some(("x-api-key", &key)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{xml}");
        assert!(xml.contains("<rss") && !xml.contains("<item>"));
        assert!(!xml.contains(&key));
    }
}

#[tokio::test]
async fn authenticated_outcomes_are_recorded_without_stamping_invalid_keys() {
    let (context, router, key, store) = fixture().await;
    enable(&context, true).await;
    assert!(
        store
            .get_by_id("http-client")
            .await
            .unwrap()
            .unwrap()
            .last_used_at
            .is_none()
    );

    let (status, _) = call(
        &router,
        "/newznab/api?t=caps",
        Some(("x-api-key", "invalid")),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        store
            .get_by_id("http-client")
            .await
            .unwrap()
            .unwrap()
            .last_used_at
            .is_none()
    );

    for (operation, outcome) in [("caps", usage::CAPS_READY), ("search", usage::SEARCH_READY)] {
        let (status, _) = call(
            &router,
            &format!("/newznab/api?t={operation}"),
            Some(("x-api-key", &key)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let record = store.get_by_id("http-client").await.unwrap().unwrap();
        assert!(record.last_used_at.is_some());
        assert_eq!(record.last_outcome_code.as_deref(), Some(outcome));
    }

    let mut record = store.get_by_id("http-client").await.unwrap().unwrap();
    record.configuration.protocols = vec![IndexerClientProtocol::Newznab];
    record.revision += 1;
    assert!(store.save(&record, Some(1)).await.unwrap());
    let (status, _) = call(&router, "/torznab/api?t=caps", Some(("x-api-key", &key))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let refused = store.get_by_id("http-client").await.unwrap().unwrap();
    assert_eq!(refused.last_outcome_code.as_deref(), Some(usage::REFUSED));

    let (status, _) = call(
        &router,
        "/newznab/api?t=caps",
        Some(("x-api-key", "invalid")),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let unchanged = store.get_by_id("http-client").await.unwrap().unwrap();
    assert_eq!(unchanged.last_used_at, refused.last_used_at);
    assert_eq!(unchanged.last_outcome_code, refused.last_outcome_code);
}

#[tokio::test]
async fn keys_do_not_cross_protocol_grants_and_revocation_applies_to_both_routes() {
    let (context, router, key, store) = fixture().await;
    enable(&context, true).await;
    let (status, _) = call(
        &router,
        "/newznab/api?t=caps",
        Some(("x-api-key", "spk_personal.fixture")),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let mut record = store.get_by_id("http-client").await.unwrap().unwrap();
    record.configuration.protocols = vec![IndexerClientProtocol::Newznab];
    record.revision += 1;
    assert!(store.save(&record, Some(1)).await.unwrap());
    let (status, _) = call(&router, "/torznab/api?t=caps", Some(("x-api-key", &key))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = call(
        &router,
        "/newznab/api?t=get&id=https%3A%2F%2Fupstream.example%2Fsecret",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!body.contains("upstream.example") && !body.contains("secret"));
    record.secret_hash = None;
    record.lookup_id = None;
    record.revision += 1;
    assert!(store.save(&record, Some(2)).await.unwrap());
    let (status, _) = call(&router, "/newznab/api?t=caps", Some(("x-api-key", &key))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
