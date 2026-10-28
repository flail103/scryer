//! The mandatory authentication decision on newly initialized instances.
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use scryer_domain::{ConfigurationChangeAction, User, UserAuthorization};
use serde::{Deserialize, Serialize};

use crate::{AppError, AppResult, AppUseCase};

pub const SETTING_KEY: &str = "auth.initial_setup";
pub const SETTING_ID: &str = "initial-authentication-setup";
pub const SEED_JSON: &str = "{\"stage\":\"choice\"}";

pub mod refusal {
    /// Authentication setup has not been completed.
    pub const REQUIRED: &str = "INITIAL_AUTH_REQUIRED";
    /// The initial authentication decision has already been made.
    pub const ALREADY_COMPLETED: &str = "INITIAL_AUTH_ALREADY_COMPLETED";
    /// The operator must replace the bootstrap password instead.
    pub const PASSWORD_REPLACEMENT_REQUIRED: &str = "INITIAL_AUTH_PASSWORD_REPLACEMENT_REQUIRED";
    /// The submitted setup code is absent or incorrect.
    pub const CODE_INVALID: &str = "INITIAL_AUTH_CODE_INVALID";
    /// Too many incorrect setup codes have been submitted.
    pub const CODE_RATE_LIMITED: &str = "INITIAL_AUTH_CODE_RATE_LIMITED";
    /// The selection conflicts with an environment override.
    pub const MODE_CONFLICT: &str = "INITIAL_AUTH_MODE_CONFLICT";
    /// The no-authentication choice requires acknowledgement.
    pub const ACKNOWLEDGEMENT_REQUIRED: &str = "INITIAL_AUTH_ACKNOWLEDGEMENT_REQUIRED";
    pub const ALL: &[&str] = &[
        REQUIRED,
        ALREADY_COMPLETED,
        PASSWORD_REPLACEMENT_REQUIRED,
        CODE_INVALID,
        CODE_RATE_LIMITED,
        MODE_CONFLICT,
        ACKNOWLEDGEMENT_REQUIRED,
    ];
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum SetupState {
    Choice,
    PasswordReplacement { user_id: String },
    Complete,
}

impl SetupState {
    pub fn validate_runtime_mode(&self, form_login_enabled: bool) -> AppResult<()> {
        if matches!(self, Self::PasswordReplacement { .. }) && !form_login_enabled {
            return Err(AppError::validation_refused(
                refusal::MODE_CONFLICT,
                "temporary administrator password replacement requires form login; remove the disabled-authentication override",
            ));
        }
        Ok(())
    }

    pub fn is_pending(&self) -> bool {
        !matches!(self, Self::Complete)
    }
    pub fn stage(&self) -> &'static str {
        match self {
            Self::Choice => "choice",
            Self::PasswordReplacement { .. } => "password_replacement",
            Self::Complete => "complete",
        }
    }
}

impl AppUseCase {
    pub async fn initial_authentication_state(&self) -> AppResult<Option<SetupState>> {
        self.services
            .identity
            .users
            .initial_authentication_state()
            .await
    }

    pub async fn refresh_initial_authentication_settings(
        &self,
    ) -> AppResult<crate::SecuritySettings> {
        // Initial credentials and settings share the identity transaction, which
        // deliberately bypasses the configuration store's separate transaction.
        self.services.config.settings.invalidate_cache();
        self.security_settings().await
    }

    pub async fn complete_initial_authentication(
        &self,
        password: Option<String>,
        acknowledge_no_auth: bool,
    ) -> AppResult<User> {
        match self.initial_authentication_state().await? {
            Some(SetupState::Choice) => {}
            Some(SetupState::PasswordReplacement { .. }) => {
                return Err(AppError::validation_refused(
                    refusal::PASSWORD_REPLACEMENT_REQUIRED,
                    "sign in and replace the temporary administrator password",
                ));
            }
            _ => {
                return Err(AppError::validation_refused(
                    refusal::ALREADY_COMPLETED,
                    "authentication setup is already complete",
                ));
            }
        }
        let password_hash = if let Some(password) = password {
            self.validate_new_local_password(&password).await?;
            Some(self.hash_password(&password)?)
        } else {
            if !acknowledge_no_auth {
                return Err(AppError::validation_refused(
                    refusal::ACKNOWLEDGEMENT_REQUIRED,
                    "explicitly acknowledge running without authentication",
                ));
            }
            None
        };
        let user = User {
            id: "00000000000000000000000000000001".into(),
            username: "admin".into(),
            password_hash,
            password_change_required: false,
            account_kind: Default::default(),
            authorization: UserAuthorization::full_admin(),
        };
        let user = self
            .services
            .identity
            .users
            .complete_initial_authentication(user)
            .await?;
        self.services.config.settings.invalidate_cache();
        self.refresh_cached_jwt_signing_key(&user).await?;
        self.emit_configuration_changed_event(
            None,
            "initial_authentication",
            Some(user.id.clone()),
            ConfigurationChangeAction::Saved,
        )
        .await;
        Ok(user)
    }
}

/// Process-local proof for initial setup. The durable state is authoritative.
pub struct SetupChallenge {
    state: Mutex<ChallengeState>,
}
struct ChallengeState {
    verifier: Option<blake3::Hash>,
    failures: VecDeque<Instant>,
    available: bool,
}
impl Default for SetupChallenge {
    fn default() -> Self {
        Self {
            state: Mutex::new(ChallengeState {
                verifier: None,
                failures: VecDeque::new(),
                available: false,
            }),
        }
    }
}
impl SetupChallenge {
    pub fn initialize(&self) -> AppResult<String> {
        const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
        let mut code = String::new();
        while code.len() < 6 {
            let mut bytes = [0u8; 16];
            SystemRandom::new()
                .fill(&mut bytes)
                .map_err(|_| AppError::Repository("setup code generation failed".into()))?;
            for byte in bytes {
                if byte < 248 && code.len() < 6 {
                    code.push(ALPHABET[usize::from(byte) % 31] as char);
                }
            }
        }
        let mut state = self.state.lock().expect("setup challenge lock poisoned");
        state.verifier = Some(blake3::hash(code.as_bytes()));
        state.available = true;
        state.failures.clear();
        Ok(format!("{}-{}", &code[..3], &code[3..]))
    }
    pub fn code_required(&self) -> bool {
        self.state
            .lock()
            .expect("setup challenge lock poisoned")
            .verifier
            .is_some()
    }
    pub fn verify(&self, code: Option<&str>) -> AppResult<()> {
        self.verify_at(code, Instant::now())
    }
    pub fn is_available(&self) -> bool {
        self.state
            .lock()
            .expect("setup challenge lock poisoned")
            .available
    }
    fn verify_at(&self, code: Option<&str>, now: Instant) -> AppResult<()> {
        let mut state = self.state.lock().expect("setup challenge lock poisoned");
        if !state.available {
            return Err(AppError::validation_refused(
                refusal::ALREADY_COMPLETED,
                "setup proof is not available",
            ));
        }
        let Some(verifier) = state.verifier else {
            return Ok(());
        };
        while state
            .failures
            .front()
            .is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(60))
        {
            state.failures.pop_front();
        }
        if state.failures.len() >= 5 {
            return Err(AppError::validation_refused_with_facts(
                refusal::CODE_RATE_LIMITED,
                "too many incorrect setup codes; try again later",
                vec![crate::RefusalFact::new("retryAfterSeconds", "60")],
            ));
        }
        let normalized: String = code
            .unwrap_or_default()
            .trim()
            .chars()
            .filter(|c| *c != '-')
            .flat_map(char::to_uppercase)
            .collect();
        if normalized.len() != 6 || blake3::hash(normalized.as_bytes()) != verifier {
            state.failures.push_back(now);
            return Err(AppError::validation_refused(
                refusal::CODE_INVALID,
                "enter the setup code from the server console",
            ));
        }
        Ok(())
    }
    pub fn consume(&self) {
        let mut state = self.state.lock().expect("setup challenge lock poisoned");
        state.available = false;
        state.verifier = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_auth_code_is_retryable_until_commit_and_accepts_display_variants() {
        let challenge = SetupChallenge::default();
        let code = challenge.initialize().unwrap();
        assert_eq!(code.len(), 7);
        challenge.verify(Some(&code)).unwrap();
        challenge
            .verify(Some(&code.to_lowercase().replace('-', "")))
            .unwrap();
        challenge.verify(Some(&format!(" {code} "))).unwrap();
        challenge.consume();
        assert_eq!(
            challenge
                .verify(Some(&code))
                .unwrap_err()
                .validation_reason(),
            Some(refusal::ALREADY_COMPLETED)
        );
    }

    #[test]
    fn initial_auth_code_limit_uses_explicit_time_and_counts_missing_codes() {
        let challenge = SetupChallenge::default();
        let code = challenge.initialize().unwrap();
        let now = Instant::now();
        for _ in 0..5 {
            assert_eq!(
                challenge
                    .verify_at(None, now)
                    .unwrap_err()
                    .validation_reason(),
                Some(refusal::CODE_INVALID)
            );
        }
        assert_eq!(
            challenge
                .verify_at(Some(&code), now)
                .unwrap_err()
                .validation_reason(),
            Some(refusal::CODE_RATE_LIMITED)
        );
        challenge
            .verify_at(Some(&code), now + Duration::from_secs(60))
            .unwrap();
    }

    #[test]
    fn initial_auth_challenge_always_requires_a_code_and_is_single_use() {
        let challenge = SetupChallenge::default();
        assert!(challenge.verify(None).is_err());
        let code = challenge.initialize().unwrap();
        assert!(challenge.code_required());
        assert_eq!(
            challenge.verify(None).unwrap_err().validation_reason(),
            Some(refusal::CODE_INVALID)
        );
        challenge.verify(Some(&code)).unwrap();
        challenge.consume();
        assert!(challenge.verify(None).is_err());
    }

    #[test]
    fn initial_auth_pending_replacement_rejects_disabled_form_login() {
        let pending = SetupState::PasswordReplacement {
            user_id: "operator".into(),
        };
        assert_eq!(
            pending
                .validate_runtime_mode(false)
                .unwrap_err()
                .validation_reason(),
            Some(refusal::MODE_CONFLICT)
        );
        pending.validate_runtime_mode(true).unwrap();
        for state in [SetupState::Choice, SetupState::Complete] {
            state.validate_runtime_mode(false).unwrap();
            state.validate_runtime_mode(true).unwrap();
        }
    }

    #[test]
    fn initial_auth_refusal_codes_are_distinct_and_prefixed() {
        let mut seen = std::collections::HashSet::new();
        for code in refusal::ALL {
            assert!(code.starts_with("INITIAL_AUTH_"));
            assert!(code.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'));
            assert!(seen.insert(code));
        }
    }
}
