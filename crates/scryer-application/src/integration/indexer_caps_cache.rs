use crate::{AppError, AppResult, RateLimitCooldownAction};
use chrono::{DateTime, Utc};
use scryer_domain::IndexerConfig;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, Weak};

const CAPS_TTL: chrono::Duration = chrono::Duration::days(7);
const FETCHED_AT: &str = "cache_fetched_at";

pub(crate) fn connection_identity(config: &IndexerConfig) -> serde_json::Value {
    serde_json::json!({
        "provider": config.provider_type.trim().to_ascii_lowercase(),
        "endpoint": config.base_url.trim().trim_end_matches('/'),
        "config": config.config_json.as_deref().and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok()),
        "secret": config.api_key_encrypted,
        "proxy": config.proxy_config_id,
    })
}

pub(crate) fn fresh_snapshot(config: &IndexerConfig, now: DateTime<Utc>) -> Option<String> {
    let raw = config.caps_snapshot_json.as_ref()?;
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let fetched = DateTime::parse_from_rfc3339(value.get(FETCHED_AT)?.as_str()?)
        .ok()?
        .with_timezone(&Utc);
    if fetched > now || now.signed_duration_since(fetched) >= CAPS_TTL {
        return None;
    }
    serde_json::from_value::<scryer_domain::IndexerCapsSnapshot>(value).ok()?;
    Some(raw.clone())
}

/// A Prowlarr managed child with no readable caps snapshot, fresh or not, on
/// the config or in its managed metadata. Automatic search leaves it out until
/// its caps are fetched.
pub(crate) fn awaits_managed_caps(config: &IndexerConfig) -> bool {
    #[derive(serde::Deserialize)]
    struct MetadataCaps {
        #[serde(default)]
        caps_snapshot: Option<scryer_domain::IndexerCapsSnapshot>,
    }
    if !config.is_prowlarr_nab_proxy() {
        return false;
    }
    let on_config = config
        .caps_snapshot_json
        .as_deref()
        .is_some_and(|raw| serde_json::from_str::<scryer_domain::IndexerCapsSnapshot>(raw).is_ok());
    let in_metadata = config
        .managed_metadata_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<MetadataCaps>(raw).ok())
        .is_some_and(|metadata| metadata.caps_snapshot.is_some());
    !on_config && !in_metadata
}

pub(crate) fn serialize_snapshot(
    snapshot: &scryer_domain::IndexerCapsSnapshot,
    now: DateTime<Utc>,
) -> AppResult<String> {
    let mut value =
        serde_json::to_value(snapshot).map_err(|error| AppError::Repository(error.to_string()))?;
    value[FETCHED_AT] = serde_json::Value::String(now.to_rfc3339());
    serde_json::to_string(&value).map_err(|error| AppError::Repository(error.to_string()))
}

/// Keeps the stored capabilities searchable but drops their fetch time, so
/// the next caps pass treats them as due. Used when the connection they were
/// fetched over changes.
pub(crate) fn mark_snapshot_stale(raw: &str) -> Option<String> {
    let mut value: serde_json::Value = serde_json::from_str(raw).ok()?;
    value.as_object_mut()?.remove(FETCHED_AT)?;
    serde_json::to_string(&value).ok()
}

/// Whether two stored snapshots describe the same capabilities, ignoring when
/// each was fetched.
pub(crate) fn same_snapshot_content(left: Option<&str>, right: Option<&str>) -> bool {
    fn content(raw: Option<&str>) -> Option<serde_json::Value> {
        let mut value: serde_json::Value = serde_json::from_str(raw?).ok()?;
        if let Some(object) = value.as_object_mut() {
            object.remove(FETCHED_AT);
        }
        Some(value)
    }
    match (left, right) {
        (None, None) => true,
        (Some(_), Some(_)) => {
            let (left_content, right_content) = (content(left), content(right));
            if left_content.is_some() && right_content.is_some() {
                left_content == right_content
            } else {
                left == right
            }
        }
        _ => false,
    }
}

type SharedFetch = tokio::sync::Mutex<Option<Result<Option<String>, AppError>>>;

/// Only overlapping callers share a result. Durable snapshots own the seven-day cache.
#[derive(Default)]
pub(crate) struct CapsRequestCache {
    flights: Mutex<HashMap<String, Weak<SharedFetch>>>,
}

impl CapsRequestCache {
    pub(crate) async fn fetch(
        &self,
        config: &IndexerConfig,
        request: impl Future<Output = AppResult<Option<String>>>,
    ) -> AppResult<Option<String>> {
        let identity = format!("{}:{}", config.id, connection_identity(config));
        let key = crate::helpers::blake3_identity_hex(
            crate::helpers::HashDomain::IndexerSecret,
            &identity,
        );
        let flight = {
            let mut flights = self.flights.lock().expect("caps request map lock poisoned");
            flights.retain(|_, flight| flight.strong_count() > 0);
            if let Some(flight) = flights.get(&key).and_then(Weak::upgrade) {
                flight
            } else {
                if flights.len() >= 128 {
                    return Err(AppError::TemporaryUnavailable {
                        reason: None,
                        message: "Too many concurrent indexer caps refreshes; try again later"
                            .into(),
                        retry_after: None,
                        rate_limit_cooldown: RateLimitCooldownAction::None,
                    });
                }
                let flight = Arc::new(tokio::sync::Mutex::new(None));
                flights.insert(key, Arc::downgrade(&flight));
                flight
            }
        };
        let mut result = flight.lock().await;
        if result.is_none() {
            *result = Some(request.await.map_err(std::convert::identity));
        }
        result
            .as_ref()
            .expect("caps fetch completed")
            .clone()
            .map_err(std::convert::identity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::Poll;

    fn config() -> IndexerConfig {
        serde_json::from_value(serde_json::json!({
            "id": "cache-test", "name": "Synthetic indexer",
            "provider_type": "newznab", "base_url": "https://indexer.example.test",
            "config_json": "{\"api_key\":\"synthetic\"}",
            "is_enabled": true, "enable_interactive_search": true, "enable_auto_search": true,
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"
        }))
        .unwrap()
    }

    #[test]
    fn freshness_is_seven_days_from_success_and_survives_serialization() {
        let mut config = config();
        let fetched = config.created_at;
        config.caps_snapshot_json = Some(
            serialize_snapshot(&scryer_domain::IndexerCapsSnapshot::default(), fetched).unwrap(),
        );
        let restored: IndexerConfig =
            serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        assert!(fresh_snapshot(&restored, fetched).is_some());
        assert!(
            fresh_snapshot(
                &restored,
                fetched + CAPS_TTL - chrono::Duration::nanoseconds(1)
            )
            .is_some()
        );
        assert!(fresh_snapshot(&restored, fetched + CAPS_TTL).is_none());
        assert!(fresh_snapshot(&restored, fetched - chrono::Duration::nanoseconds(1)).is_none());
        for raw in ["{}", "invalid", r#"{"cache_fetched_at":"invalid"}"#] {
            config.caps_snapshot_json = Some(raw.into());
            assert!(fresh_snapshot(&config, fetched).is_none());
        }
    }

    #[test]
    fn only_a_managed_child_without_any_snapshot_awaits_caps() {
        let mut child = config();
        assert!(!awaits_managed_caps(&child), "a direct indexer never waits");
        child.managed_parent_config_id = Some("parent".into());
        assert!(awaits_managed_caps(&child));
        let raw = serialize_snapshot(
            &scryer_domain::IndexerCapsSnapshot::default(),
            child.created_at,
        )
        .unwrap();
        // An expired or stale snapshot still counts: it keeps the child searchable.
        child.caps_snapshot_json = mark_snapshot_stale(&raw);
        assert!(!awaits_managed_caps(&child));
    }

    #[test]
    fn stale_marking_keeps_content_but_makes_the_snapshot_due() {
        let mut config = config();
        let fetched = config.created_at;
        let raw =
            serialize_snapshot(&scryer_domain::IndexerCapsSnapshot::default(), fetched).unwrap();
        let stale = mark_snapshot_stale(&raw).unwrap();
        assert!(same_snapshot_content(Some(&raw), Some(&stale)));
        config.caps_snapshot_json = Some(stale.clone());
        assert!(fresh_snapshot(&config, fetched).is_none());
        assert!(mark_snapshot_stale(&stale).is_none());
        let later = serialize_snapshot(
            &scryer_domain::IndexerCapsSnapshot::default(),
            fetched + chrono::Duration::days(1),
        )
        .unwrap();
        assert_ne!(raw, later);
        assert!(same_snapshot_content(Some(&raw), Some(&later)));
        assert!(!same_snapshot_content(Some(&raw), None));
        assert!(same_snapshot_content(None, None));
    }

    #[test]
    fn cache_metadata_and_local_preferences_do_not_change_search_or_connection_identity() {
        let original = config();
        let mut updated = original.clone();
        updated.caps_snapshot_json = Some(
            serialize_snapshot(
                &scryer_domain::IndexerCapsSnapshot::default(),
                updated.created_at,
            )
            .unwrap(),
        );
        let mut later = updated.clone();
        later.caps_snapshot_json = Some(
            serialize_snapshot(
                &scryer_domain::IndexerCapsSnapshot::default(),
                updated.created_at + CAPS_TTL,
            )
            .unwrap(),
        );
        assert_eq!(
            crate::indexer_search_identity(&updated, None),
            crate::indexer_search_identity(&later, None)
        );
        updated.name = "Renamed".into();
        updated.is_enabled = false;
        updated.rate_limit_seconds = Some(60);
        updated.download_client_id = Some("client".into());
        assert_eq!(
            connection_identity(&original),
            connection_identity(&updated)
        );
        for change in 0..4 {
            let mut changed = original.clone();
            match change {
                0 => changed.provider_type = "other".into(),
                1 => changed.base_url = "https://other.example.test".into(),
                2 => changed.config_json = Some(r#"{"api_key":"changed"}"#.into()),
                _ => changed.proxy_config_id = Some("proxy".into()),
            }
            assert_ne!(
                connection_identity(&original),
                connection_identity(&changed)
            );
        }
    }

    #[tokio::test]
    async fn overlapping_fetches_share_results_but_changed_connections_do_not() {
        let cache = CapsRequestCache::default();
        let config = config();
        let (release, waiting) = tokio::sync::oneshot::channel::<()>();
        let first = cache.fetch(&config, async {
            waiting.await.unwrap();
            Ok(Some("snapshot".into()))
        });
        let second = cache.fetch(&config, async { panic!("duplicate outbound request") });
        tokio::pin!(first, second);
        assert!(matches!(
            first
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop())),
            Poll::Pending
        ));
        assert!(matches!(
            second
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop())),
            Poll::Pending
        ));
        let mut changed = config.clone();
        changed.proxy_config_id = Some("different-proxy".into());
        assert_eq!(
            cache
                .fetch(&changed, async { Ok(Some("other".into())) })
                .await
                .unwrap()
                .as_deref(),
            Some("other")
        );
        release.send(()).unwrap();
        let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(first, second)
        })
        .await
        .unwrap();
        assert_eq!(first.unwrap(), second.unwrap());
    }

    #[tokio::test]
    async fn failed_shared_fetch_preserves_quota_error_and_allows_next_refresh() {
        let cache = CapsRequestCache::default();
        let config = config();
        let (release, waiting) = tokio::sync::oneshot::channel::<()>();
        let first = cache.fetch(&config, async {
            waiting.await.unwrap();
            Err(AppError::NewznabQuotaExceeded {
                code: 500,
                message: "quota".into(),
            })
        });
        let second = cache.fetch(&config, async { panic!("duplicate outbound request") });
        tokio::pin!(first, second);
        assert!(matches!(
            first
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop())),
            Poll::Pending
        ));
        assert!(matches!(
            second
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop())),
            Poll::Pending
        ));
        release.send(()).unwrap();
        let results = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(first, second)
        })
        .await
        .unwrap();
        for result in [results.0, results.1] {
            assert!(matches!(
                result,
                Err(AppError::NewznabQuotaExceeded { code: 500, .. })
            ));
        }
        assert_eq!(
            cache
                .fetch(&config, async { Ok(Some("recovered".into())) })
                .await
                .unwrap()
                .as_deref(),
            Some("recovered")
        );
    }
}
