#![recursion_limit = "256"]

mod common;

use async_graphql::{Request, Variables};
use common::TestContext;
use scryer_application::{
    SettingsRepository, TotpCredentialRecord, TotpRepository, UserRepository,
    testing::AppUseCaseTestExt,
};
use scryer_domain::{AppPermissionMask, User};
use scryer_infrastructure_acquisition::indexers::client_store::IndexerClientStore;
use scryer_infrastructure_identity::users::totp_store::TotpStore;
use scryer_interface::context::{AuthlessDefaultSession, InteractiveSession, MfaVerification};
use serde_json::{Value, json};
use std::sync::Arc;

const CREATE: &str = "mutation { createIndexerClient(input: {name: \"Tool fixture\", enabled: true,
    protocols: [NEWZNAB, TORZNAB], grants: []}) { apiKey client { id name revision keyActive } } }";

fn request(
    query: &str,
    actor: &User,
    interactive: bool,
    bypass: bool,
    until: Option<i64>,
) -> Request {
    let mut request = Request::new(query)
        .data(actor.clone())
        .data(MfaVerification {
            step_up_verified_until: until,
            ..Default::default()
        });
    if interactive {
        request = request.data(InteractiveSession);
    }
    if bypass {
        request = request.data(AuthlessDefaultSession);
    }
    request
}

fn error_field(response: &async_graphql::Response, field: &str) -> String {
    assert_eq!(response.errors.len(), 1, "{response:?}");
    serde_json::to_value(&response.errors[0]).unwrap()["extensions"][field]
        .as_str()
        .expect("typed error field")
        .to_owned()
}

fn data(response: async_graphql::Response) -> Value {
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    response.data.into_json().unwrap()
}

async fn seed_experimental_setting(ctx: &TestContext) {
    ctx.settings_store
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
}

#[tokio::test]
async fn client_management_graphql_requires_interactive_full_permissions_and_mfa() {
    let ctx = TestContext::new().await;
    seed_experimental_setting(&ctx).await;
    let store = Arc::new(IndexerClientStore::new(ctx.db.datastore()));
    let app = ctx
        .app
        .with_test_overrides(|builder| builder.with_indexer_client_repository(store));
    let schema = scryer_interface::build_schema(app, ctx.auth_runtime.clone());
    let mut actor = User::new_admin("client-operator");
    actor.authorization.app = AppPermissionMask::ALL;
    actor.authorization.loaded = true;
    ctx.users.create(actor.clone()).await.unwrap();
    for query in ["{ indexerClients { id } }", CREATE] {
        let response = schema
            .execute(request(query, &actor, true, false, Some(i64::MAX)))
            .await;
        assert_eq!(
            error_field(&response, "reason"),
            "INDEXER_CLIENTS_EXPERIMENTAL_DISABLED"
        );
    }
    ctx.settings_store
        .upsert_setting_json(
            scryer_application::SETTINGS_SCOPE_SYSTEM,
            scryer_application::EXPERIMENTAL_FEATURES_ENABLED_KEY,
            None,
            "true".into(),
            "test",
            None,
        )
        .await
        .unwrap();
    for (interactive, bypass) in [(false, false), (false, true), (true, true)] {
        let response = schema
            .execute(request(CREATE, &actor, interactive, bypass, Some(i64::MAX)))
            .await;
        assert_eq!(
            error_field(&response, "reason"),
            "INDEXER_CLIENTS_INTERACTIVE_SESSION_REQUIRED"
        );
    }
    let mut restricted = actor.clone();
    restricted.authorization.app = AppPermissionMask::NONE;
    let response = schema
        .execute(request(CREATE, &restricted, true, false, Some(i64::MAX)))
        .await;
    assert_eq!(
        error_field(&response, "reason"),
        "INDEXER_CLIENTS_ALL_PERMISSIONS_REQUIRED"
    );
    let response = schema
        .execute(request(CREATE, &actor, true, false, Some(i64::MAX)))
        .await;
    assert_eq!(error_field(&response, "code"), "TOTP_ENROLLMENT_REQUIRED");

    let now = chrono::Utc::now();
    TotpStore::new(ctx.db.datastore(), ctx.db.encryption_key_state())
        .upsert_credential(TotpCredentialRecord {
            id: "client-factor".into(),
            user_id: actor.id.clone(),
            secret_base32: "JBSWY3DPEHPK3PXP".into(),
            algorithm: "SHA1".into(),
            digits: 6,
            period_seconds: 30,
            last_accepted_step: None,
            created_at: now.to_rfc3339(),
            updated_at: now.to_rfc3339(),
            last_used_at: None,
        })
        .await
        .unwrap();
    for until in [None, Some(0)] {
        let response = schema
            .execute(request(CREATE, &actor, true, false, until))
            .await;
        assert_eq!(error_field(&response, "code"), "MFA_STEP_UP_REQUIRED");
    }
    let issued = data(
        schema
            .execute(request(CREATE, &actor, true, false, Some(i64::MAX)))
            .await,
    );
    assert!(
        issued["createIndexerClient"]["apiKey"]
            .as_str()
            .unwrap()
            .starts_with("sck_")
    );
    assert_eq!(issued["createIndexerClient"]["client"]["revision"], 1);
}

#[tokio::test]
async fn graphql_client_rotation_revocation_and_disable_invalidate_authenticated_requests() {
    let ctx = TestContext::new().await;
    seed_experimental_setting(&ctx).await;
    ctx.settings_store
        .upsert_setting_json(
            scryer_application::SETTINGS_SCOPE_SYSTEM,
            scryer_application::EXPERIMENTAL_FEATURES_ENABLED_KEY,
            None,
            "true".into(),
            "test",
            None,
        )
        .await
        .unwrap();
    let store = Arc::new(IndexerClientStore::new(ctx.db.datastore()));
    let app = ctx
        .app
        .with_test_overrides(|builder| builder.with_indexer_client_repository(store));
    let schema = scryer_interface::build_schema(app.clone(), ctx.auth_runtime.clone());
    let mut actor = User::new_admin("client-operator");
    actor.authorization.app = AppPermissionMask::ALL;
    actor.authorization.loaded = true;
    ctx.users.create(actor.clone()).await.unwrap();
    let now = chrono::Utc::now();
    TotpStore::new(ctx.db.datastore(), ctx.db.encryption_key_state())
        .upsert_credential(TotpCredentialRecord {
            id: "client-factor".into(),
            user_id: actor.id.clone(),
            secret_base32: "JBSWY3DPEHPK3PXP".into(),
            algorithm: "SHA1".into(),
            digits: 6,
            period_seconds: 30,
            last_accepted_step: None,
            created_at: now.to_rfc3339(),
            updated_at: now.to_rfc3339(),
            last_used_at: None,
        })
        .await
        .unwrap();
    let until = Some(i64::MAX);
    let issued = data(
        schema
            .execute(request(CREATE, &actor, true, false, until))
            .await,
    );
    let id = issued["createIndexerClient"]["client"]["id"]
        .as_str()
        .unwrap();
    let key = issued["createIndexerClient"]["apiKey"].as_str().unwrap();
    let authenticated = app.authenticate_indexer_client(key).await.unwrap();
    let rotate = "mutation($id: ID!, $rev: Long!) { rotateIndexerClientKey(id: $id, expectedRevision: $rev) { apiKey client { revision } } }";
    let variables = Variables::from_json(json!({"id": id, "rev": 1}));
    let rotated = data(
        schema
            .execute(request(rotate, &actor, true, false, until).variables(variables.clone()))
            .await,
    );
    let new_key = rotated["rotateIndexerClientKey"]["apiKey"]
        .as_str()
        .unwrap();
    assert!(app.authenticate_indexer_client(key).await.is_err());
    assert!(app.revalidate_indexer_client(&authenticated).await.is_err());
    let authenticated = app.authenticate_indexer_client(new_key).await.unwrap();
    let stale = schema
        .execute(request(rotate, &actor, true, false, until).variables(variables))
        .await;
    assert_eq!(
        error_field(&stale, "reason"),
        "INDEXER_CLIENTS_CONFIGURATION_CHANGED"
    );

    let disable = "mutation($id: ID!) { updateIndexerClient(id: $id, expectedRevision: 2, input: {
        name: \"Disabled client\", enabled: false, protocols: [NEWZNAB], grants: [] }) { revision enabled } }";
    data(
        schema
            .execute(
                request(disable, &actor, true, false, until)
                    .variables(Variables::from_json(json!({"id": id}))),
            )
            .await,
    );
    assert!(app.authenticate_indexer_client(new_key).await.is_err());
    assert!(app.revalidate_indexer_client(&authenticated).await.is_err());
    let revoke = "mutation($id: ID!) { revokeIndexerClientKey(id: $id, expectedRevision: 3) { revision keyActive } }";
    let revoked = data(
        schema
            .execute(
                request(revoke, &actor, true, false, until)
                    .variables(Variables::from_json(json!({"id": id}))),
            )
            .await,
    );
    assert_eq!(revoked["revokeIndexerClientKey"]["keyActive"], false);

    let public = data(
        schema
            .execute(request(
                "{ indexerClients { id revision keyActive name } }",
                &actor,
                true,
                false,
                None,
            ))
            .await,
    );
    assert_eq!(public["indexerClients"][0]["revision"], 4);
    assert!(!public.to_string().contains(key) && !public.to_string().contains(new_key));
    let sdl = schema.sdl();
    let client_type = sdl
        .split("type IndexerClientPayload {")
        .nth(1)
        .unwrap()
        .split('}')
        .next()
        .unwrap();
    assert!(
        !client_type.contains("secretHash")
            && !client_type.contains("lookupId")
            && !client_type.contains("apiKey")
    );
}
