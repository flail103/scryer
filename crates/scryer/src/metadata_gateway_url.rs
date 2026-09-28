//! Resolves the metadata gateway (SMG) GraphQL endpoint once at startup.
//!
//! SMG is a single centrally hosted service and self-hosted metadata gateways are
//! not supported, so every path here ends at the official endpoint: the
//! environment value when it is usable, the URL the build was compiled with, and
//! otherwise the production endpoint itself. Compose files routinely quote the
//! environment value, and those quotes used to survive into the URL, which left
//! every metadata operation failing with an opaque transport error and nothing
//! naming the setting at fault. Resolution here is forgiving about the quoting an
//! operator actually writes, but it says so in the log, and it refuses to hand a
//! value that is not an absolute http(s) URL downstream.

use url::Url;

use crate::SMG_GRAPHQL_URL;

pub(crate) const METADATA_GATEWAY_URL_ENV: &str = "SCRYER_METADATA_GATEWAY_GRAPHQL_URL";
/// The build-time variable `build.rs` reads to fill in the compiled-in default.
const METADATA_GATEWAY_BUILD_ENV: &str = "SCRYER_SMG_GRAPHQL_URL";
/// The endpoint every release is compiled with, and the last resort for a build
/// that carries no default of its own.
///
/// There is no local endpoint to fall back to: a development build that
/// configures nothing talks to the official service like a release build does,
/// and one that wants a mock points the environment setting at it explicitly.
const PRODUCTION_SMG_URL: &str = "https://smg.scryer.media/graphql";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Source {
    Environment,
    CompiledIn,
}

impl Source {
    /// The setting whose name the operator can actually go and fix.
    fn setting(self) -> &'static str {
        match self {
            Self::Environment => METADATA_GATEWAY_URL_ENV,
            Self::CompiledIn => METADATA_GATEWAY_BUILD_ENV,
        }
    }
}

/// The single metadata gateway endpoint this process uses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MetadataGatewayUrl {
    value: String,
}

impl MetadataGatewayUrl {
    pub(crate) fn from_env() -> Self {
        let explicit = std::env::var(METADATA_GATEWAY_URL_ENV).ok();
        Self::resolve(explicit.as_deref(), SMG_GRAPHQL_URL)
    }

    /// Resolution order: the explicit environment value, then the compiled-in
    /// default, then the production endpoint.
    ///
    /// A setting that is *set but unusable* is reported and then treated as
    /// unset. SMG is a single centrally hosted service, so the chain ends at the
    /// official endpoint: a typo costs the operator their log line and nothing
    /// else, instead of leaving metadata down against a local endpoint that
    /// refuses every connection.
    pub(crate) fn resolve(explicit: Option<&str>, compiled_in: Option<&str>) -> Self {
        Self::from_setting(explicit, Source::Environment)
            .or_else(|| Self::from_setting(compiled_in, Source::CompiledIn))
            .unwrap_or_else(Self::production_default)
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.value
    }

    pub(crate) fn into_string(self) -> String {
        self.value
    }

    fn production_default() -> Self {
        Self {
            value: PRODUCTION_SMG_URL.to_string(),
        }
    }

    /// An unset or empty setting is not a configuration error — it means "use
    /// the next source". A value that is set but unusable is reported before it
    /// is dropped, because the alternative is what issue #166 hit: an operator
    /// watching metadata fail with no way to tell which setting did it. The
    /// value itself never reaches the log, since a URL can carry credentials in
    /// its userinfo or query.
    fn from_setting(raw: Option<&str>, source: Source) -> Option<Self> {
        let trimmed = raw.unwrap_or_default().trim();
        if trimmed.is_empty() {
            return None;
        }

        let unquoted = strip_surrounding_quotes(trimmed).trim();
        if unquoted.is_empty() {
            return None;
        }
        if unquoted.len() != trimmed.len() {
            tracing::warn!(
                setting = source.setting(),
                "metadata gateway URL is wrapped in quotes; using the unquoted value"
            );
        }

        match validated(unquoted) {
            Ok(value) => Some(Self { value }),
            Err(reason) => {
                tracing::error!(
                    setting = source.setting(),
                    reason,
                    "metadata gateway URL is unusable; ignoring it"
                );
                None
            }
        }
    }
}

/// Returns the operator's own string when it is an absolute http(s) URL, so a
/// working configuration reaches the gateway client byte for byte as before,
/// and the reason it was refused otherwise.
fn validated(candidate: &str) -> Result<String, &'static str> {
    let url = Url::parse(candidate).map_err(|_| "the value is not an absolute URL")?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("the scheme is not http or https");
    }
    if url.host_str().is_none() {
        return Err("the URL has no host");
    }
    Ok(candidate.to_string())
}

/// Removes exactly one matching pair of surrounding quotes, which is the shape
/// `KEY="value"` produces once the shell has handed the value over.
fn strip_surrounding_quotes(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() < 2 {
        return value;
    }

    let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
    let matched_pair = (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'');
    if matched_pair {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::MetadataGatewayUrl;
    use super::PRODUCTION_SMG_URL;
    use super::validated;

    const GATEWAY: &str = "https://smg.example.test/graphql";

    #[test]
    fn the_production_default_is_an_absolute_https_url() {
        assert!(validated(PRODUCTION_SMG_URL).is_ok());
    }

    #[test]
    fn no_resolution_path_lands_on_a_loopback_endpoint() {
        let cases = [
            MetadataGatewayUrl::resolve(None, None),
            MetadataGatewayUrl::resolve(Some(""), Some("")),
            MetadataGatewayUrl::resolve(Some("not a url"), None),
            MetadataGatewayUrl::resolve(Some("file:///tmp/graphql"), None),
        ];

        for resolved in cases {
            assert!(!resolved.as_str().contains("127.0.0.1"), "{resolved:?}");
        }
    }

    #[test]
    fn keeps_a_bare_environment_value() {
        assert_eq!(
            MetadataGatewayUrl::resolve(Some(GATEWAY), None).as_str(),
            GATEWAY
        );
    }

    #[test]
    fn strips_surrounding_double_quotes() {
        assert_eq!(
            MetadataGatewayUrl::resolve(Some(&format!("\"{GATEWAY}\"")), None).as_str(),
            GATEWAY
        );
    }

    #[test]
    fn strips_surrounding_single_quotes() {
        assert_eq!(
            MetadataGatewayUrl::resolve(Some(&format!("'{GATEWAY}'")), None).as_str(),
            GATEWAY
        );
    }

    #[test]
    fn trims_whitespace_around_and_inside_the_quotes() {
        let quoted = format!("  \"  {GATEWAY}  \"  ");
        assert_eq!(
            MetadataGatewayUrl::resolve(Some(&quoted), None).as_str(),
            GATEWAY
        );
    }

    #[test]
    fn strips_quotes_from_the_compiled_in_default_too() {
        let quoted = format!("\"{GATEWAY}\"");
        assert_eq!(
            MetadataGatewayUrl::resolve(None, Some(&quoted)).as_str(),
            GATEWAY
        );
    }

    #[test]
    fn unset_environment_value_uses_the_compiled_in_default() {
        let compiled_in = "http://127.0.0.1:9000/graphql";
        assert_eq!(
            MetadataGatewayUrl::resolve(None, Some(compiled_in)).as_str(),
            compiled_in
        );
        assert_eq!(
            MetadataGatewayUrl::resolve(Some("   "), Some(compiled_in)).as_str(),
            compiled_in
        );
    }

    #[test]
    fn empty_environment_value_falls_back_to_the_production_default() {
        assert_eq!(
            MetadataGatewayUrl::resolve(Some(""), None).as_str(),
            PRODUCTION_SMG_URL
        );
        assert_eq!(
            MetadataGatewayUrl::resolve(Some("\"\""), None).as_str(),
            PRODUCTION_SMG_URL
        );
    }

    #[test]
    fn relative_value_falls_back_instead_of_panicking() {
        assert_eq!(
            MetadataGatewayUrl::resolve(Some("smg.example.test/graphql"), None).as_str(),
            PRODUCTION_SMG_URL
        );
    }

    #[test]
    fn non_http_scheme_falls_back() {
        assert_eq!(
            MetadataGatewayUrl::resolve(Some("file:///tmp/graphql"), None).as_str(),
            PRODUCTION_SMG_URL
        );
    }

    #[test]
    fn unmatched_quotes_are_left_alone_and_rejected() {
        let mismatched = format!("\"{GATEWAY}'");
        assert_eq!(
            MetadataGatewayUrl::resolve(Some(&mismatched), None).as_str(),
            PRODUCTION_SMG_URL
        );
    }

    #[test]
    fn text_after_a_closing_quote_is_not_stripped() {
        let trailing = format!("\"{GATEWAY}\"extra");
        assert_eq!(
            MetadataGatewayUrl::resolve(Some(&trailing), None).as_str(),
            PRODUCTION_SMG_URL
        );
    }

    #[test]
    fn invalid_environment_value_falls_back_to_the_compiled_in_default() {
        let compiled_in = "http://127.0.0.1:9000/graphql";
        let mismatched_quotes = format!("\"{GATEWAY}'");
        let long_value = "x".repeat(400);

        for invalid in ["not a url", mismatched_quotes.as_str(), long_value.as_str()] {
            assert_eq!(
                MetadataGatewayUrl::resolve(Some(invalid), Some(compiled_in)).as_str(),
                compiled_in
            );
        }
    }

    #[test]
    fn unusable_compiled_in_value_falls_back_to_the_production_default() {
        let mismatched_quotes = format!("\"{GATEWAY}'");

        for invalid in ["not a url", mismatched_quotes.as_str()] {
            assert_eq!(
                MetadataGatewayUrl::resolve(None, Some(invalid)).as_str(),
                PRODUCTION_SMG_URL
            );
        }
    }
}
