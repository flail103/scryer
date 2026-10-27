use async_graphql::{Context, Enum, ID, InputObject, Object, Result as GqlResult, SimpleObject};
use scryer_application::indexer_clients::{
    IndexerClientConfiguration, IndexerClientGrant, IndexerClientProtocol, IndexerClientRecord,
};
use scryer_interface_core::{
    AuthlessDefaultSession, InteractiveSession, actor_from_ctx, app_from_ctx,
    mfa_verification_from_ctx, to_gql_error,
};

#[derive(Clone, Copy, PartialEq, Eq, Enum)]
/// Compatibility protocol a client may use.
enum IndexerClientProtocolValue {
    /// Newznab feeds for Usenet releases.
    Newznab,
    /// Torznab feeds for torrent releases.
    Torznab,
}

impl From<IndexerClientProtocolValue> for IndexerClientProtocol {
    fn from(value: IndexerClientProtocolValue) -> Self {
        match value {
            IndexerClientProtocolValue::Newznab => Self::Newznab,
            IndexerClientProtocolValue::Torznab => Self::Torznab,
        }
    }
}

impl From<IndexerClientProtocol> for IndexerClientProtocolValue {
    fn from(value: IndexerClientProtocol) -> Self {
        match value {
            IndexerClientProtocol::Newznab => Self::Newznab,
            IndexerClientProtocol::Torznab => Self::Torznab,
        }
    }
}

#[derive(InputObject)]
/// Explicit access to one existing source, including a managed child.
struct IndexerClientGrantInput {
    /// Existing indexer configuration identity.
    indexer_id: ID,
    /// Null permits all source categories; an empty list permits none.
    categories: Option<Vec<String>>,
}

#[derive(InputObject)]
/// Operator configuration for a client sharing Scryer's existing indexers.
struct IndexerClientConfigurationInput {
    /// Operator-visible client name.
    name: String,
    /// Whether this client may authenticate.
    enabled: bool,
    /// Compatibility protocols this key is allowed to access.
    protocols: Vec<IndexerClientProtocolValue>,
    /// Explicit source and category grants; an empty list grants no sources.
    grants: Vec<IndexerClientGrantInput>,
}

impl From<IndexerClientConfigurationInput> for IndexerClientConfiguration {
    fn from(value: IndexerClientConfigurationInput) -> Self {
        Self {
            name: value.name,
            enabled: value.enabled,
            protocols: value.protocols.into_iter().map(Into::into).collect(),
            grants: value
                .grants
                .into_iter()
                .map(|grant| IndexerClientGrant {
                    indexer_id: grant.indexer_id.to_string(),
                    categories: grant.categories,
                })
                .collect(),
        }
    }
}

#[derive(SimpleObject)]
/// Effective access to one existing indexer.
struct IndexerClientGrantPayload {
    /// Existing indexer configuration identity.
    indexer_id: ID,
    /// Null permits all categories; an empty list permits none.
    categories: Option<Vec<String>>,
}

/// This projection intentionally has no lookup identifier or verification hash.
#[derive(SimpleObject)]
struct IndexerClientPayload {
    /// Stable client identity.
    id: ID,
    /// Operator-visible name.
    name: String,
    /// Whether this client may authenticate.
    enabled: bool,
    /// Allowed compatibility protocols.
    protocols: Vec<IndexerClientProtocolValue>,
    /// Explicit source and category grants.
    grants: Vec<IndexerClientGrantPayload>,
    /// Whether the client has a key that has not been revoked.
    key_active: bool,
    /// Opaque concurrency token, represented as a string to avoid JS integer loss.
    revision: String,
    /// Most recent request recorded for this client.
    last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Stable outcome code for the most recently recorded request.
    last_outcome_code: Option<String>,
    /// When this client was created.
    created_at: chrono::DateTime<chrono::Utc>,
    /// When its configuration or key last changed.
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<IndexerClientRecord> for IndexerClientPayload {
    fn from(value: IndexerClientRecord) -> Self {
        Self {
            id: value.id.into(),
            name: value.configuration.name,
            enabled: value.configuration.enabled,
            protocols: value
                .configuration
                .protocols
                .into_iter()
                .map(Into::into)
                .collect(),
            grants: value
                .configuration
                .grants
                .into_iter()
                .map(|grant| IndexerClientGrantPayload {
                    indexer_id: grant.indexer_id.into(),
                    categories: grant.categories,
                })
                .collect(),
            key_active: value.secret_hash.is_some(),
            revision: value.revision.to_string(),
            last_used_at: value.last_used_at,
            last_outcome_code: value.last_outcome_code,
            created_at: value.created_at,
            updated_at: value.updated_at,
        }
    }
}

#[derive(SimpleObject)]
/// A newly issued client key and its public management record.
struct IssuedIndexerClientKeyPayload {
    /// Current client configuration without key verification material.
    client: IndexerClientPayload,
    /// Returned once. Scryer does not persist this secret.
    api_key: String,
}

fn interactive(ctx: &Context<'_>) -> bool {
    ctx.data_opt::<InteractiveSession>().is_some()
        && ctx.data_opt::<AuthlessDefaultSession>().is_none()
}

fn revision(value: &str) -> GqlResult<u64> {
    value
        .parse::<u64>()
        .ok()
        .filter(|revision| *revision > 0 && *revision <= i64::MAX as u64)
        .ok_or_else(|| {
            to_gql_error(scryer_application::AppError::validation_refused(
                scryer_application::error_reasons::indexer_clients::CONFIGURATION_CHANGED,
                "Indexer client revision is invalid; reload it",
            ))
        })
}

#[derive(Default)]
pub struct IndexerClientQueries;

#[Object]
impl IndexerClientQueries {
    /// Client administration is available only to fully privileged interactive sessions.
    async fn indexer_clients(&self, ctx: &Context<'_>) -> GqlResult<Vec<IndexerClientPayload>> {
        let app = app_from_ctx(ctx)?;
        let actor = actor_from_ctx(ctx)?;
        Ok(app
            .list_indexer_clients(&actor, interactive(ctx))
            .await
            .map_err(to_gql_error)?
            .into_iter()
            .map(Into::into)
            .collect())
    }
}

#[derive(Default)]
pub struct IndexerClientMutations;

#[Object]
impl IndexerClientMutations {
    /// Create an indexer client and reveal its key once; requires fresh MFA.
    async fn create_indexer_client(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "Name, protocols and explicit source/category grants.")]
        input: IndexerClientConfigurationInput,
    ) -> GqlResult<IssuedIndexerClientKeyPayload> {
        let app = app_from_ctx(ctx)?;
        let actor = actor_from_ctx(ctx)?;
        let issued = app
            .create_indexer_client(
                &actor,
                interactive(ctx),
                mfa_verification_from_ctx(ctx).step_up_verified_until,
                input.into(),
            )
            .await
            .map_err(to_gql_error)?;
        Ok(IssuedIndexerClientKeyPayload {
            client: issued.client.into(),
            api_key: issued.raw_key,
        })
    }

    /// Replace a client's configuration if its revision is still current; requires fresh MFA.
    async fn update_indexer_client(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "Client identity to update.")] id: ID,
        #[graphql(desc = "Revision observed when opening the client for editing.")]
        expected_revision: String,
        #[graphql(desc = "Complete replacement configuration and grants.")]
        input: IndexerClientConfigurationInput,
    ) -> GqlResult<IndexerClientPayload> {
        let app = app_from_ctx(ctx)?;
        let actor = actor_from_ctx(ctx)?;
        app.update_indexer_client(
            &actor,
            interactive(ctx),
            mfa_verification_from_ctx(ctx).step_up_verified_until,
            id.as_str(),
            revision(&expected_revision)?,
            input.into(),
        )
        .await
        .map(Into::into)
        .map_err(to_gql_error)
    }

    /// Invalidate the previous key and reveal a new key once; requires fresh MFA.
    async fn rotate_indexer_client_key(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "Client whose key is rotated.")] id: ID,
        #[graphql(desc = "Current client revision.")] expected_revision: String,
    ) -> GqlResult<IssuedIndexerClientKeyPayload> {
        let app = app_from_ctx(ctx)?;
        let actor = actor_from_ctx(ctx)?;
        let issued = app
            .rotate_indexer_client_key(
                &actor,
                interactive(ctx),
                mfa_verification_from_ctx(ctx).step_up_verified_until,
                id.as_str(),
                revision(&expected_revision)?,
            )
            .await
            .map_err(to_gql_error)?;
        Ok(IssuedIndexerClientKeyPayload {
            client: issued.client.into(),
            api_key: issued.raw_key,
        })
    }

    /// Revoke a client's key and derived retrieval access; requires fresh MFA.
    async fn revoke_indexer_client_key(
        &self,
        ctx: &Context<'_>,
        #[graphql(desc = "Client whose key is revoked.")] id: ID,
        #[graphql(desc = "Current client revision.")] expected_revision: String,
    ) -> GqlResult<IndexerClientPayload> {
        let app = app_from_ctx(ctx)?;
        let actor = actor_from_ctx(ctx)?;
        app.revoke_indexer_client_key(
            &actor,
            interactive(ctx),
            mfa_verification_from_ctx(ctx).step_up_verified_until,
            id.as_str(),
            revision(&expected_revision)?,
        )
        .await
        .map(Into::into)
        .map_err(to_gql_error)
    }
}
