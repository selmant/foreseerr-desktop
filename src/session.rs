//! Private Jellyfin session bootstrap correlation.

use crate::config::validate_bootstrap_server_url;

pub const BOOTSTRAP_ID_MAX_LEN: usize = 256;
pub const ACCESS_TOKEN_MAX_LEN: usize = 8192;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBootstrap {
    pub server_url: String,
    pub server_id: String,
    pub user_id: String,
    pub device_id: String,
    pub access_token: String,
    pub bootstrap_generation: String,
    pub fallback_server_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedSession {
    pub server_id: String,
    pub user_id: String,
    pub bootstrap_generation: String,
    pub server_origin: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMatchError {
    ServerMismatch,
    UserMismatch,
    GenerationMismatch,
    OriginMismatch,
}

impl SessionBootstrap {
    pub fn validate_shape(&self) -> Result<(), &'static str> {
        validate_bootstrap_server_url(&self.server_url)
            .map_err(|_| "invalid_bootstrap_response")?;
        if let Some(fallback) = &self.fallback_server_url {
            validate_bootstrap_server_url(fallback)
                .map_err(|_| "invalid_bootstrap_response")?;
        }
        self.validate_ids()
    }

    fn validate_ids(&self) -> Result<(), &'static str> {
        for (value, name) in [
            (&self.server_id, "server_id"),
            (&self.user_id, "user_id"),
            (&self.device_id, "device_id"),
            (&self.bootstrap_generation, "bootstrap_generation"),
        ] {
            if value.is_empty() || value.len() > BOOTSTRAP_ID_MAX_LEN {
                let _ = name;
                return Err("invalid_bootstrap_response");
            }
        }
        if self.access_token.is_empty() || self.access_token.len() > ACCESS_TOKEN_MAX_LEN {
            return Err("invalid_bootstrap_response");
        }
        Ok(())
    }

    pub fn expected(&self) -> Result<ExpectedSession, &'static str> {
        self.validate_ids()?;
        let origin = url::Url::parse(&self.server_url)
            .map(|u| u.origin().ascii_serialization())
            .map_err(|_| "invalid_bootstrap_response")?;
        Ok(ExpectedSession {
            server_id: self.server_id.clone(),
            user_id: self.user_id.clone(),
            bootstrap_generation: self.bootstrap_generation.clone(),
            server_origin: origin,
        })
    }
}

impl ExpectedSession {
    pub fn matches(
        &self,
        server_id: &str,
        user_id: &str,
        generation: &str,
    ) -> Result<(), SessionMatchError> {
        if self.server_id != server_id {
            return Err(SessionMatchError::ServerMismatch);
        }
        if self.user_id != user_id {
            return Err(SessionMatchError::UserMismatch);
        }
        if self.bootstrap_generation != generation {
            return Err(SessionMatchError::GenerationMismatch);
        }
        Ok(())
    }
}

/// Redact secrets from diagnostic strings.
pub fn redact_secrets(input: &str) -> String {
    // Never echo raw tokens; callers should pass already-safe summaries.
    if input.contains("accessToken") || input.contains("eyJ") {
        return "[redacted]".to_string();
    }
    input.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SessionBootstrap {
        SessionBootstrap {
            server_url: "https://jellyfin.example/".into(),
            server_id: "srv".into(),
            user_id: "user".into(),
            device_id: "dev".into(),
            access_token: "tok".into(),
            bootstrap_generation: "gen-1".into(),
            fallback_server_url: None,
        }
    }

    #[test]
    fn bootstrap_match_and_mismatch() {
        let expected = sample().expected().unwrap();
        assert!(expected.matches("srv", "user", "gen-1").is_ok());
        assert_eq!(
            expected.matches("other", "user", "gen-1").unwrap_err(),
            SessionMatchError::ServerMismatch
        );
        assert_eq!(
            expected.matches("srv", "other", "gen-1").unwrap_err(),
            SessionMatchError::UserMismatch
        );
        assert_eq!(
            expected.matches("srv", "user", "gen-2").unwrap_err(),
            SessionMatchError::GenerationMismatch
        );
    }

    #[test]
    fn accepts_http_bootstrap_from_url_scheme() {
        let mut http = sample();
        http.server_url = "http://jellyfin.example/".into();
        assert!(http.validate_shape().is_ok());
        assert_eq!(redact_secrets(r#"{"accessToken":"secret"}"#), "[redacted]");
    }

    #[test]
    fn accepts_private_http_bootstrap() {
        let mut lan = sample();
        lan.server_url = "http://192.168.40.3:8096".into();
        assert!(lan.validate_shape().is_ok());
    }
}
