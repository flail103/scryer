use crate::middleware::AuthState;
use axum::{
    Json,
    body::Body,
    extract::{MatchedPath, State},
    http::{Method, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use scryer_application::initial_auth::{SetupState, refusal};

#[derive(Clone)]
pub(crate) struct PendingSetup;

#[derive(Clone)]
pub(crate) struct SetupAdmission(pub Option<SetupState>);

pub(crate) fn refused(reason: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({"errors": [{
            "message": "Complete initial authentication setup before continuing.",
            "extensions": {"code": "VALIDATION_ERROR", "reason": reason}
        }]})),
    )
        .into_response()
}

pub(crate) async fn setup_state(
    state: &AuthState,
) -> scryer_application::AppResult<Option<SetupState>> {
    let setup = state.app.initial_authentication_state().await?;
    // A concurrent completion can commit before its resolver publishes runtime
    // settings. Do not admit an authless administrator in that interval.
    if matches!(setup, Some(SetupState::Complete))
        && state.auth_runtime.initial_authentication.is_available()
    {
        let settings = state.app.refresh_initial_authentication_settings().await?;
        state.auth_runtime.apply_saved_security_settings(
            settings.form_login_enabled,
            settings.skip_login_for_local_ips,
        );
        state.auth_runtime.initial_authentication.consume();
    }
    Ok(setup)
}

pub(crate) async fn admit_http(
    State(state): State<AuthState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let setup = match setup_state(&state).await {
        Ok(setup) => setup,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let pending = setup.as_ref().is_some_and(|state| state.is_pending());
    request.extensions_mut().insert(SetupAdmission(setup));
    if pending {
        let route = request
            .extensions()
            .get::<MatchedPath>()
            .map(|path| path.as_str());
        // nest_service strips the configured prefix from the request URI,
        // while MatchedPath still includes it.
        let allowed = match request.uri().path() {
            "/health" | "/health/ready" => request.method() == Method::GET,
            "/graphql" => request.method() == Method::POST,
            _ if route.is_none() => request.method() == Method::GET,
            _ => false,
        };
        if !allowed {
            return refused(refusal::REQUIRED);
        }
        request.extensions_mut().insert(PendingSetup);
    }
    next.run(request).await
}

pub(crate) fn allows_graphql(request: &async_graphql::Request, stage: &SetupState) -> bool {
    use async_graphql::parser::{
        parse_query,
        types::{ExecutableDocument, OperationType, Selection, SelectionSet},
    };
    let Ok(document) = parse_query(&request.query) else {
        return false;
    };
    let selected = match request.operation_name.as_deref() {
        Some(name) => document
            .operations
            .iter()
            .find(|(n, _)| n.is_some_and(|n| n.as_str() == name)),
        None => {
            let mut iter = document.operations.iter();
            let first = iter.next();
            if iter.next().is_some() {
                return false;
            }
            first
        }
    };
    let Some((_, operation)) = selected else {
        return false;
    };
    if operation.node.ty == OperationType::Subscription {
        return false;
    }
    fn allowed(
        set: &SelectionSet,
        document: &ExecutableDocument,
        stage: &SetupState,
        mutation: bool,
        depth: usize,
        budget: &mut usize,
    ) -> bool {
        if depth > 32 || set.items.is_empty() {
            return false;
        }
        for item in &set.items {
            if *budget == 0 {
                return false;
            }
            *budget -= 1;
            let ok = match &item.node {
                Selection::Field(field) => match (mutation, field.node.name.node.as_str(), stage) {
                    (false, "authRuntimeState", _) => true,
                    (true, "completeInitialAuthentication", SetupState::Choice) => true,
                    (
                        true,
                        "login" | "completeRequiredPasswordChange",
                        SetupState::PasswordReplacement { .. },
                    ) => true,
                    _ => false,
                },
                Selection::InlineFragment(fragment) => allowed(
                    &fragment.node.selection_set.node,
                    document,
                    stage,
                    mutation,
                    depth + 1,
                    budget,
                ),
                Selection::FragmentSpread(spread) => document
                    .fragments
                    .get(&spread.node.fragment_name.node)
                    .is_some_and(|fragment| {
                        allowed(
                            &fragment.node.selection_set.node,
                            document,
                            stage,
                            mutation,
                            depth + 1,
                            budget,
                        )
                    }),
            };
            if !ok {
                return false;
            }
        }
        true
    }
    allowed(
        &operation.node.selection_set.node,
        &document,
        stage,
        operation.node.ty == OperationType::Mutation,
        0,
        &mut 1024,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::{self, integration_test_common::TestContext};
    use scryer_application::UserRepository;
    use tower::ServiceExt;

    #[test]
    fn initial_auth_graphql_admission_checks_actual_fields_and_stage() {
        let choice = SetupState::Choice;
        assert!(allows_graphql(
            &async_graphql::Request::new(
                "query { alias: authRuntimeState { initialAuthenticationStage } }"
            ),
            &choice
        ));
        assert!(allows_graphql(
            &async_graphql::Request::new(
                "query { ...F } fragment F on Query { authRuntimeState { passwordMinLength } }"
            ),
            &choice
        ));
        for query in [
            "query { authRuntimeState { passwordMinLength } users { id } }",
            "query { ...F } fragment F on Query { alias: users { id } }",
            "mutation { completeSetup { completed } }",
            "subscription { events { id } }",
            "query A { authRuntimeState { passwordMinLength } } query B { users { id } }",
            "query { ...F } fragment F on Query { ...F }",
            "mutation { login(input: {username: \"admin\", password: \"test\"}) { token } }",
        ] {
            assert!(
                !allows_graphql(&async_graphql::Request::new(query), &choice),
                "{query}"
            );
        }
        let replacement = SetupState::PasswordReplacement {
            user_id: "admin".into(),
        };
        assert!(allows_graphql(
            &async_graphql::Request::new(
                "mutation { completeRequiredPasswordChange(input: {password: \"replacement\"}) { token } }"
            ),
            &replacement
        ));
        assert!(!allows_graphql(
            &async_graphql::Request::new(
                "mutation { completeInitialAuthentication(input: {}) { completed } }"
            ),
            &replacement
        ));
    }

    async fn fixture() -> TestContext {
        let context = tokio::time::timeout(std::time::Duration::from_secs(60), TestContext::new())
            .await
            .unwrap();
        sqlx::query("UPDATE settings_values SET value_json = '{\"stage\":\"choice\"}' WHERE id = 'initial-authentication-setup'")
            .execute(&context.db.pool).await.unwrap();
        crate::settings_bootstrap::seed_service_setting_definitions(context.settings_store.clone())
            .await
            .unwrap();
        context
    }

    #[tokio::test]
    async fn initial_auth_choice_is_atomic_and_only_one_submission_wins() {
        let context = fixture().await;
        assert_eq!(
            context.app.initial_authentication_state().await.unwrap(),
            Some(SetupState::Choice)
        );
        let (first, second) = tokio::join!(
            context
                .app
                .complete_initial_authentication(Some("first-password".into()), false),
            context
                .app
                .complete_initial_authentication(Some("second-password".into()), false),
        );
        assert_ne!(first.is_ok(), second.is_ok());
        assert_eq!(
            context.app.initial_authentication_state().await.unwrap(),
            Some(SetupState::Complete)
        );
        let settings = context.app.security_settings().await.unwrap();
        assert!(settings.form_login_enabled);
        assert!(!settings.skip_login_for_local_ips);
        assert!(
            context
                .users
                .get_by_username("admin")
                .await
                .unwrap()
                .unwrap()
                .password_hash
                .is_some()
        );
        assert!(
            context
                .app
                .complete_initial_authentication(None, true)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn initial_auth_failed_write_preserves_pending_choice_and_credentials() {
        let context = fixture().await;
        sqlx::query("CREATE TRIGGER fail_initial_auth BEFORE UPDATE OF password_hash ON users BEGIN SELECT RAISE(ABORT, 'fixture write failure'); END")
            .execute(&context.db.pool).await.unwrap();
        assert!(
            context
                .app
                .complete_initial_authentication(Some("test-password".into()), false)
                .await
                .is_err()
        );
        assert_eq!(
            context.app.initial_authentication_state().await.unwrap(),
            Some(SetupState::Choice)
        );
        assert!(
            context
                .users
                .get_by_username("admin")
                .await
                .unwrap()
                .unwrap()
                .password_hash
                .is_none()
        );
        sqlx::query("DROP TRIGGER fail_initial_auth")
            .execute(&context.db.pool)
            .await
            .unwrap();
        context
            .app
            .complete_initial_authentication(Some("test-password".into()), false)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn initial_auth_no_auth_requires_acknowledgement_and_blocks_generic_writes() {
        let context = fixture().await;
        assert!(
            context
                .app
                .complete_initial_authentication(None, false)
                .await
                .is_err()
        );
        use scryer_application::SettingsRepository;
        assert!(
            context
                .settings_store
                .delete_setting_value(
                    "system",
                    scryer_application::initial_auth::SETTING_KEY,
                    None
                )
                .await
                .is_err()
        );
        assert!(
            context
                .settings_store
                .upsert_setting_json(
                    "system",
                    scryer_application::initial_auth::SETTING_KEY,
                    None,
                    "{\"stage\":\"complete\"}".into(),
                    "fixture",
                    None
                )
                .await
                .is_err()
        );
        context
            .app
            .complete_initial_authentication(None, true)
            .await
            .unwrap();
        assert!(
            !context
                .app
                .security_settings()
                .await
                .unwrap()
                .form_login_enabled
        );
        assert!(
            context
                .users
                .get_by_username("admin")
                .await
                .unwrap()
                .unwrap()
                .password_hash
                .is_none()
        );
    }

    #[tokio::test]
    async fn initial_auth_bootstrap_can_resume_with_an_initialized_passwordless_admin() {
        let context = fixture().await;
        let admin = context.app.find_or_create_default_user().await.unwrap();
        context
            .app
            .configure_bootstrap_admin(None, Some("temporary-password"), false, false)
            .await
            .unwrap();
        assert_eq!(
            context.app.initial_authentication_state().await.unwrap(),
            Some(SetupState::PasswordReplacement { user_id: admin.id })
        );
        assert!(
            context
                .app
                .security_settings()
                .await
                .unwrap()
                .form_login_enabled
        );
        assert!(
            context
                .app
                .complete_initial_authentication(None, true)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn initial_auth_bootstrap_requires_replacement_and_preserves_it_on_restart() {
        let context = fixture().await;
        context
            .app
            .configure_bootstrap_admin(Some("operator"), Some("temporary-password"), false, true)
            .await
            .unwrap();
        let user = context
            .users
            .get_by_username("operator")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            context.app.initial_authentication_state().await.unwrap(),
            Some(SetupState::PasswordReplacement {
                user_id: user.id.clone()
            })
        );
        assert!(
            context
                .app
                .complete_initial_authentication(None, true)
                .await
                .is_err()
        );
        let version = context.users.auth_session_version(&user.id).await.unwrap();
        assert!(
            context
                .app
                .complete_required_password_change(&user, "temporary-password".into(), &version)
                .await
                .is_err()
        );
        let (saved, _) = context
            .app
            .complete_required_password_change(&user, "permanent-password".into(), &version)
            .await
            .unwrap();
        assert_eq!(
            context.app.initial_authentication_state().await.unwrap(),
            Some(SetupState::Complete)
        );
        context
            .app
            .configure_bootstrap_admin(Some("operator"), Some("temporary-password"), false, true)
            .await
            .unwrap();
        let restarted = context.users.get_by_id(&user.id).await.unwrap().unwrap();
        assert_eq!(restarted.password_hash, saved.password_hash);
        assert!(!restarted.password_change_required);
    }

    fn http_state(context: &TestContext) -> middleware::AuthState {
        middleware::AuthState {
            app: context.app.clone(),
            schema: context.schema.clone(),
            auth_runtime: context.auth_runtime.clone(),
            rate_limiter: crate::rate_limit::ScryerRateLimiter::from_env(Default::default()),
            ws_origin_policy: middleware::WebSocketOriginPolicy::default(),
            list_account_origin_policy: Default::default(),
            authless_web_client_proof: middleware::AuthlessWebClientProofState::new(),
        }
    }

    fn http_router(context: &TestContext) -> axum::Router {
        let state = http_state(context);
        axum::Router::new()
            .route(
                "/graphql",
                axum::routing::post(middleware::graphql_handler).with_state(state.clone()),
            )
            .fallback(axum::routing::get(|| async { "web shell" }))
            .layer(axum::middleware::from_fn_with_state(state, admit_http))
    }

    async fn graphql(
        router: axum::Router,
        path: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        graphql_with_token(router, path, body, None).await
    }

    async fn graphql_with_token(
        router: axum::Router,
        path: &str,
        body: serde_json::Value,
        token: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let mut request = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        if let Some(token) = token {
            request
                .headers_mut()
                .insert("authorization", format!("Bearer {token}").parse().unwrap());
        }
        request.extensions_mut().insert(axum::extract::ConnectInfo(
            "127.0.0.1:12345".parse::<std::net::SocketAddr>().unwrap(),
        ));
        let response =
            tokio::time::timeout(std::time::Duration::from_secs(60), router.oneshot(request))
                .await
                .unwrap()
                .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn initial_auth_http_gate_blocks_routes_and_mixed_graphql_batches() {
        let context = fixture().await;
        context
            .auth_runtime
            .initial_authentication
            .initialize()
            .unwrap();
        let state = http_state(&context);
        let router = axum::Router::new()
            .route(
                "/graphql",
                axum::routing::post(middleware::graphql_handler).with_state(state.clone()),
            )
            .route(
                "/graphql/ws",
                axum::routing::get(|| async { "must not reach" }),
            )
            .route(
                "/api/test",
                axum::routing::get(|| async { "must not reach" }),
            )
            .fallback(axum::routing::get(|| async { "web shell" }))
            .layer(axum::middleware::from_fn_with_state(state, admit_http));
        for path in ["/graphql/ws", "/api/test"] {
            let response = router
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/setup?step=99")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut request = Request::builder().method("POST").uri("/graphql").header("content-type", "application/json")
            .body(Body::from(r#"[{"query":"{ authRuntimeState { passwordMinLength } }"},{"query":"{ users { id } }"}]"#)).unwrap();
        request.extensions_mut().insert(axum::extract::ConnectInfo(
            "127.0.0.1:12345".parse::<std::net::SocketAddr>().unwrap(),
        ));
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn initial_auth_loopback_http_choices_require_code_and_survive_prefixed_routes() {
        for password_mode in [true, false] {
            let context = fixture().await;
            let code = context
                .auth_runtime
                .initial_authentication
                .initialize()
                .unwrap();
            let router = crate::base_path::mount_router(
                http_router(&context),
                &crate::base_path::BasePath::from_raw(Some("/scryer")),
            );
            let shell = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/scryer/setup?skip=true")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(shell.status(), StatusCode::OK);
            let (status, metadata) = graphql(router.clone(), "/scryer/graphql", serde_json::json!({"query":"{ authRuntimeState { initialAuthenticationStage initialAuthenticationCodeRequired passwordMinLength } }"})).await;
            assert_eq!(status, StatusCode::OK, "{metadata}");
            assert_eq!(
                metadata["data"]["authRuntimeState"]["initialAuthenticationStage"],
                "choice"
            );
            assert_eq!(
                metadata["data"]["authRuntimeState"]["initialAuthenticationCodeRequired"],
                true
            );
            assert!(!metadata.to_string().contains(&code));
            let query = "mutation Decide($input: CompleteInitialAuthenticationInput!) { completeInitialAuthentication(input: $input) { completed login { token } } }";
            let mut input = serde_json::json!({"mode": if password_mode { "PASSWORD" } else { "NO_AUTHENTICATION" },
                "password": password_mode.then_some("permanent-password"), "acknowledgeNoAuthentication": !password_mode, "setupCode": "invalid"});
            // The loopback peer has no forwarding headers, exactly as with a
            // reverse proxy that relays remote requests without adding them.
            for supplied_code in [serde_json::Value::Null, "invalid".into()] {
                input["setupCode"] = supplied_code;
                let (_, failed) = graphql(
                    router.clone(),
                    "/scryer/graphql",
                    serde_json::json!({"query":query,"variables":{"input":input}}),
                )
                .await;
                assert_eq!(
                    failed["errors"][0]["extensions"]["reason"],
                    refusal::CODE_INVALID,
                    "{failed}"
                );
                assert_eq!(
                    context.app.initial_authentication_state().await.unwrap(),
                    Some(SetupState::Choice)
                );
            }
            input["setupCode"] = code.into();
            let (_, result) = graphql(
                router.clone(),
                "/scryer/graphql",
                serde_json::json!({"query":query,"variables":{"input":input}}),
            )
            .await;
            assert_eq!(
                result["data"]["completeInitialAuthentication"]["completed"], true,
                "{result}"
            );
            assert_eq!(
                result["data"]["completeInitialAuthentication"]["login"].is_object(),
                password_mode
            );
            assert_eq!(
                context.auth_runtime.snapshot().effective_form_login_enabled,
                password_mode
            );
            assert!(!context.auth_runtime.initial_authentication.is_available());
            let (status, stale) = graphql(
                router,
                "/scryer/graphql",
                serde_json::json!({"query":query,"variables":{"input":input}}),
            )
            .await;
            if password_mode {
                assert_eq!(
                    stale["errors"][0]["extensions"]["reason"],
                    refusal::ALREADY_COMPLETED,
                    "{stale}"
                );
            } else {
                // Ordinary authless admission again requires the web-client proof.
                assert_eq!(status, StatusCode::FORBIDDEN, "{stale}");
            }
        }
    }

    #[tokio::test]
    async fn initial_auth_admission_publishes_committed_password_mode_before_access() {
        let context = fixture().await;
        context
            .auth_runtime
            .initial_authentication
            .initialize()
            .unwrap();
        context
            .auth_runtime
            .apply_saved_security_settings(false, false);
        assert!(
            !context
                .app
                .security_settings()
                .await
                .unwrap()
                .form_login_enabled
        );
        context
            .app
            .complete_initial_authentication(Some("permanent-password".into()), false)
            .await
            .unwrap();
        assert!(!context.auth_runtime.snapshot().effective_form_login_enabled);
        assert_eq!(
            setup_state(&http_state(&context)).await.unwrap(),
            Some(SetupState::Complete)
        );
        assert!(context.auth_runtime.snapshot().effective_form_login_enabled);
    }

    #[tokio::test]
    async fn initial_auth_http_bootstrap_replacement_uses_a_restricted_session() {
        let context = fixture().await;
        context
            .app
            .configure_bootstrap_admin(Some("operator"), Some("temporary-password"), false, true)
            .await
            .unwrap();
        context
            .auth_runtime
            .apply_saved_security_settings(true, false);
        let router = http_router(&context);
        let (_, metadata) = graphql_with_token(
            router.clone(),
            "/graphql",
            serde_json::json!({"query":"{ authRuntimeState { initialAuthenticationStage } }"}),
            Some("stale-token"),
        )
        .await;
        assert_eq!(
            metadata["data"]["authRuntimeState"]["initialAuthenticationStage"],
            "password_replacement",
            "{metadata}"
        );
        let (_, login) = graphql(router.clone(), "/graphql", serde_json::json!({"query":"mutation { login(input: {username: \"operator\", password: \"temporary-password\"}) { token passwordChangeRequired } }"})).await;
        assert_eq!(
            login["data"]["login"]["passwordChangeRequired"], true,
            "{login}"
        );
        let token = login["data"]["login"]["token"].as_str().unwrap();
        let (status, _) = graphql_with_token(
            router.clone(),
            "/graphql",
            serde_json::json!({"query":"{ me { id } }"}),
            Some(token),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        context
            .auth_runtime
            .apply_saved_security_settings(false, false);
        let (_, refused) = graphql_with_token(router.clone(), "/graphql", serde_json::json!({"query":"mutation { completeRequiredPasswordChange(input: {password: \"permanent-password\"}) { token } }"}), Some(token)).await;
        assert_eq!(
            refused["errors"][0]["extensions"]["reason"],
            refusal::MODE_CONFLICT,
            "{refused}"
        );
        assert!(matches!(
            context.app.initial_authentication_state().await.unwrap(),
            Some(SetupState::PasswordReplacement { .. })
        ));
        context
            .auth_runtime
            .apply_saved_security_settings(true, false);
        let (_, changed) = graphql_with_token(router.clone(), "/graphql", serde_json::json!({"query":"mutation { completeRequiredPasswordChange(input: {password: \"permanent-password\"}) { token passwordChangeRequired } }"}), Some(token)).await;
        assert_eq!(
            changed["data"]["completeRequiredPasswordChange"]["passwordChangeRequired"], false,
            "{changed}"
        );
        assert_eq!(
            context.app.initial_authentication_state().await.unwrap(),
            Some(SetupState::Complete)
        );
        let token = changed["data"]["completeRequiredPasswordChange"]["token"]
            .as_str()
            .unwrap();
        let (_, me) = graphql_with_token(
            router,
            "/graphql",
            serde_json::json!({"query":"{ me { username } }"}),
            Some(token),
        )
        .await;
        assert_eq!(me["data"]["me"]["username"], "operator", "{me}");
    }

    #[tokio::test]
    async fn initial_auth_recovery_requires_replacement_and_preserves_atomic_admission() {
        for bootstrap_first in [false, true] {
            let context = fixture().await;
            if bootstrap_first {
                context
                    .app
                    .configure_bootstrap_admin(
                        Some("operator"),
                        Some("lost-temporary-password"),
                        false,
                        true,
                    )
                    .await
                    .unwrap();
            }
            let previous = context.app.initial_authentication_state().await.unwrap();
            context.app.set_recovery_admin_login_enabled(true);
            sqlx::query("CREATE TRIGGER fail_recovery_selection BEFORE UPDATE OF password_change_required ON users WHEN NEW.username = 'recovery-admin' AND NEW.password_change_required = 1 BEGIN SELECT RAISE(ABORT, 'fixture write failure'); END")
                .execute(&context.db.pool).await.unwrap();
            assert!(
                context
                    .app
                    .recover_reserved_admin_access("recovery-temporary-password")
                    .await
                    .is_err()
            );
            assert_eq!(
                context.app.initial_authentication_state().await.unwrap(),
                previous
            );
            sqlx::query("DROP TRIGGER fail_recovery_selection")
                .execute(&context.db.pool)
                .await
                .unwrap();
            let recovered = context
                .app
                .recover_reserved_admin_access("recovery-temporary-password")
                .await
                .unwrap();
            assert!(recovered.password_change_required);
            assert_eq!(
                context.app.initial_authentication_state().await.unwrap(),
                Some(SetupState::PasswordReplacement {
                    user_id: recovered.id.clone()
                })
            );
            let settings = context.app.security_settings().await.unwrap();
            assert!(settings.form_login_enabled);
            assert!(!settings.skip_login_for_local_ips);
            context
                .auth_runtime
                .apply_saved_security_settings(true, false);
            let router = http_router(&context);
            let (_, login) = graphql(router.clone(), "/graphql", serde_json::json!({"query":"mutation { login(input: {username: \"recovery-admin\", password: \"recovery-temporary-password\"}) { token passwordChangeRequired } }"})).await;
            assert_eq!(
                login["data"]["login"]["passwordChangeRequired"], true,
                "{login}"
            );
            let token = login["data"]["login"]["token"].as_str().unwrap();
            let (status, _) = graphql_with_token(
                router.clone(),
                "/graphql",
                serde_json::json!({"query":"{ me { id } }"}),
                Some(token),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            let (_, unchanged) = graphql_with_token(router.clone(), "/graphql", serde_json::json!({"query":"mutation { completeRequiredPasswordChange(input: {password: \"recovery-temporary-password\"}) { token } }"}), Some(token)).await;
            assert!(unchanged["errors"].is_array(), "{unchanged}");
            let (_, changed) = graphql_with_token(router.clone(), "/graphql", serde_json::json!({"query":"mutation { completeRequiredPasswordChange(input: {password: \"permanent-password\"}) { token passwordChangeRequired } }"}), Some(token)).await;
            assert_eq!(
                changed["data"]["completeRequiredPasswordChange"]["passwordChangeRequired"], false,
                "{changed}"
            );
            assert_eq!(
                context.app.initial_authentication_state().await.unwrap(),
                Some(SetupState::Complete)
            );
            let token = changed["data"]["completeRequiredPasswordChange"]["token"]
                .as_str()
                .unwrap();
            let (_, me) = graphql_with_token(
                router,
                "/graphql",
                serde_json::json!({"query":"{ me { username } }"}),
                Some(token),
            )
            .await;
            assert_eq!(me["data"]["me"]["username"], "recovery-admin", "{me}");
            // Explicit recovery on an already-complete installation retains its
            // existing full-session behavior, rather than reopening setup.
            let recovered_again = context
                .app
                .recover_reserved_admin_access("another-recovery-password")
                .await
                .unwrap();
            assert!(!recovered_again.password_change_required);
            assert_eq!(
                context.app.initial_authentication_state().await.unwrap(),
                Some(SetupState::Complete)
            );
        }
    }

    #[tokio::test]
    async fn initial_auth_existing_unfinished_database_is_not_reclassified_on_restart() {
        let context = fixture().await;
        sqlx::query("DELETE FROM settings_values WHERE id = 'initial-authentication-setup'")
            .execute(&context.db.pool)
            .await
            .unwrap();
        scryer_infrastructure_datastore::migrations::run_migrations(
            &context.db.pool,
            scryer_infrastructure_datastore::MigrationMode::Apply,
        )
        .await
        .unwrap();
        assert_eq!(
            context.app.initial_authentication_state().await.unwrap(),
            None
        );
        assert!(
            context
                .app
                .complete_initial_authentication(None, true)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn initial_auth_restore_replaces_destination_choice_with_backup_state() {
        use scryer_infrastructure_datastore::SqliteServices;
        use scryer_infrastructure_runtime::sqlite_backup::{
            export_backup_bundle_from_sqlite, restore_backup_bundle_into_sqlite_pool,
        };
        let context = fixture().await;
        context
            .app
            .complete_initial_authentication(Some("destination-password".into()), false)
            .await
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("source.db");
        let source = SqliteServices::new(source_path.to_str().unwrap())
            .await
            .unwrap();
        // An older, deliberately unauthenticated backup has no first-run state.
        sqlx::query("DELETE FROM settings_values WHERE id = 'initial-authentication-setup'")
            .execute(&source.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM settings_definitions WHERE id = 'initial-authentication-setup'")
            .execute(&source.pool)
            .await
            .unwrap();
        let bundle = directory.path().join("backup.tar.zst");
        export_backup_bundle_from_sqlite(
            source_path.to_str().unwrap(),
            scryer_application::BackupBundleExportRequest {
                output_path: bundle.clone(),
                passphrase: "fixture-passphrase".into(),
                source_migration_key: None,
                source_scryer_version: "fixture".into(),
                source_engine: "sqlite".into(),
                secrets: scryer_application::BackupExportSecrets {
                    encryption_master_key: "fixture-master-key".into(),
                    jwt_signing_secret: "fixture-jwt-secret".into(),
                    smg_registration_secret: None,
                    smg_gateway_url: None,
                },
            },
        )
        .await
        .unwrap();
        restore_backup_bundle_into_sqlite_pool(
            &context.db.pool,
            &bundle,
            Some("fixture-passphrase"),
        )
        .await
        .unwrap();
        scryer_infrastructure_datastore::migrations::run_migrations(
            &context.db.pool,
            scryer_infrastructure_datastore::MigrationMode::Apply,
        )
        .await
        .unwrap();
        assert_eq!(
            context.app.initial_authentication_state().await.unwrap(),
            None
        );
        assert!(
            context
                .users
                .get_by_username("admin")
                .await
                .unwrap()
                .unwrap()
                .password_hash
                .is_none()
        );
        source.pool.close().await;
    }
}
