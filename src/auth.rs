//! Auth challenge proofs and ticket redemption helpers.

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::session::{ACCESS_TOKEN_MAX_LEN, BOOTSTRAP_ID_MAX_LEN, SessionBootstrap};

pub const MAX_PENDING_AUTH_PROOFS: usize = 10;
pub const AUTH_PROOF_TTL: Duration = Duration::from_secs(60);
pub const PROTOCOL_VERSION: u8 = crate::protocol::PROTOCOL_VERSION;
pub const TICKET_LENGTH: usize = crate::protocol::TICKET_LENGTH;
pub const CHALLENGE_HEX_LENGTH: usize = crate::protocol::CHALLENGE_HEX_LENGTH;

#[derive(Debug, Clone)]
pub struct AuthProof {
    pub challenge: String,
    pub verifier: String,
    pub created_at: Instant,
}

#[derive(Debug, Default)]
pub struct PendingAuthProofs {
    proofs: HashMap<String, AuthProof>,
}

impl PendingAuthProofs {
    pub fn insert(&mut self, request_id: String, proof: AuthProof) {
        self.expire();
        if self.proofs.len() >= MAX_PENDING_AUTH_PROOFS
            && let Some(oldest) = self
                .proofs
                .iter()
                .min_by_key(|(_, p)| p.created_at)
                .map(|(k, _)| k.clone())
        {
            self.proofs.remove(&oldest);
        }
        self.proofs.insert(request_id, proof);
    }

    pub fn take(&mut self, request_id: &str) -> Option<AuthProof> {
        self.expire();
        self.proofs.remove(request_id)
    }

    pub fn clear(&mut self) {
        self.proofs.clear();
    }

    fn expire(&mut self) {
        let now = Instant::now();
        self.proofs
            .retain(|_, proof| now.duration_since(proof.created_at) <= AUTH_PROOF_TTL);
    }
}

pub fn new_auth_proof() -> AuthProof {
    let verifier = base64_url(&random_bytes());
    let challenge = hex_digest(verifier.as_bytes());
    AuthProof {
        challenge,
        verifier,
        created_at: Instant::now(),
    }
}

pub fn redemption_url(frontend_url: &str) -> Result<String, url::ParseError> {
    let parsed = url::Url::parse(frontend_url)?;
    let origin = parsed.origin().ascii_serialization();
    Ok(format!("{origin}/api/v1/desktop/auth-tickets/redeem"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthErrorCode {
    InvalidRequest,
    ServerUnreachable,
    SessionExpired,
    TicketExpired,
    TicketUsed,
    NotLinked,
    TokenInvalid,
    UnsupportedMediaServer,
    InvalidBootstrapResponse,
}

impl AuthErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::ServerUnreachable => "server_unreachable",
            Self::SessionExpired => "session_expired",
            Self::TicketExpired => "ticket_expired",
            Self::TicketUsed => "ticket_used",
            Self::NotLinked => "not_linked",
            Self::TokenInvalid => "token_invalid",
            Self::UnsupportedMediaServer => "unsupported_media_server",
            Self::InvalidBootstrapResponse => "invalid_bootstrap_response",
        }
    }

    pub fn from_server(code: Option<&str>) -> Self {
        match code {
            Some("session_expired") => Self::SessionExpired,
            Some("ticket_expired") => Self::TicketExpired,
            Some("ticket_used") => Self::TicketUsed,
            Some("not_linked") => Self::NotLinked,
            Some("token_invalid") => Self::TokenInvalid,
            Some("unsupported_media_server") => Self::UnsupportedMediaServer,
            Some("invalid_bootstrap_response") => Self::InvalidBootstrapResponse,
            Some("server_unreachable") => Self::ServerUnreachable,
            _ => Self::InvalidRequest,
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct RedemptionErrorResponse {
    code: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RedemptionBootstrapResponse {
    server_url: Option<String>,
    server_id: Option<String>,
    user_id: Option<String>,
    device_id: Option<String>,
    access_token: Option<String>,
    bootstrap_generation: Option<String>,
    fallback_server_url: Option<String>,
}

fn optional_bootstrap_url(value: Option<String>) -> Result<Option<String>, AuthErrorCode> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > crate::config::MAX_FORESEER_URL_LEN {
        return Err(AuthErrorCode::InvalidBootstrapResponse);
    }
    Ok(Some(value))
}

fn required_bootstrap_field(value: Option<String>, max: usize) -> Result<String, AuthErrorCode> {
    let value = value.ok_or(AuthErrorCode::InvalidBootstrapResponse)?;
    if value.is_empty() || value.len() > max {
        return Err(AuthErrorCode::InvalidBootstrapResponse);
    }
    Ok(value)
}

pub fn parse_redemption_bootstrap(body: &str) -> Result<SessionBootstrap, AuthErrorCode> {
    let parsed: RedemptionBootstrapResponse =
        serde_json::from_str(body).map_err(|_| AuthErrorCode::InvalidBootstrapResponse)?;
    let bootstrap = SessionBootstrap {
        server_url: required_bootstrap_field(
            parsed.server_url,
            crate::config::MAX_FORESEER_URL_LEN,
        )?,
        server_id: required_bootstrap_field(parsed.server_id, BOOTSTRAP_ID_MAX_LEN)?,
        user_id: required_bootstrap_field(parsed.user_id, BOOTSTRAP_ID_MAX_LEN)?,
        device_id: required_bootstrap_field(parsed.device_id, BOOTSTRAP_ID_MAX_LEN)?,
        access_token: required_bootstrap_field(parsed.access_token, ACCESS_TOKEN_MAX_LEN)?,
        bootstrap_generation: required_bootstrap_field(
            parsed.bootstrap_generation,
            BOOTSTRAP_ID_MAX_LEN,
        )?,
        fallback_server_url: optional_bootstrap_url(parsed.fallback_server_url)?,
    };
    bootstrap
        .validate_shape()
        .map_err(|_| AuthErrorCode::InvalidBootstrapResponse)?;
    Ok(bootstrap)
}

pub fn map_http_error_body(body: &str) -> AuthErrorCode {
    serde_json::from_str::<RedemptionErrorResponse>(body)
        .ok()
        .map(|r| AuthErrorCode::from_server(r.code.as_deref()))
        .unwrap_or(AuthErrorCode::InvalidRequest)
}

pub fn redeem_ticket(
    agent: &ureq::Agent,
    redeem_url: &str,
    ticket: &str,
    verifier: &str,
) -> Result<SessionBootstrap, AuthErrorCode> {
    let mut response = match agent.post(redeem_url).send_json(serde_json::json!({
        "ticket": ticket,
        "verifier": verifier,
        "protocolVersion": PROTOCOL_VERSION,
    })) {
        Ok(r) => r,
        Err(_) => return Err(AuthErrorCode::ServerUnreachable),
    };
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let body = response.body_mut().read_to_string().unwrap_or_default();
        return Err(map_http_error_body(&body));
    }
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|_| AuthErrorCode::InvalidBootstrapResponse)?;
    parse_redemption_bootstrap(&body)
}

pub fn random_bytes() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("getrandom");
    bytes
}

pub fn base64_url(bytes: &[u8]) -> String {
    base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, bytes)
}

pub fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_is_sha256_of_verifier_string() {
        let proof = new_auth_proof();
        assert_eq!(proof.challenge.len(), CHALLENGE_HEX_LENGTH);
        assert_eq!(proof.verifier.len(), TICKET_LENGTH);
        assert_eq!(proof.challenge, hex_digest(proof.verifier.as_bytes()));
    }

    #[test]
    fn proofs_are_request_correlated_and_expire() {
        let mut pending = PendingAuthProofs::default();
        let proof = AuthProof {
            challenge: "a".repeat(CHALLENGE_HEX_LENGTH),
            verifier: "v".into(),
            created_at: Instant::now() - AUTH_PROOF_TTL - Duration::from_secs(1),
        };
        pending.insert("old".into(), proof);
        assert!(pending.take("old").is_none());

        let fresh = new_auth_proof();
        assert_eq!(fresh.challenge.len(), CHALLENGE_HEX_LENGTH);
        pending.insert("req".into(), fresh);
        assert!(pending.take("req").is_some());
        assert!(pending.take("req").is_none());
    }

    #[test]
    fn bootstrap_parser_redacts_token_paths() {
        let body = r#"{
            "serverUrl":"https://jellyfin.example/",
            "serverId":"s",
            "userId":"u",
            "deviceId":"d",
            "accessToken":"secret-token",
            "bootstrapGeneration":"g1"
        }"#;
        let bootstrap = parse_redemption_bootstrap(body).unwrap();
        assert_eq!(bootstrap.access_token, "secret-token");
        let err = parse_redemption_bootstrap(r#"{"code":"ticket_used"}"#).unwrap_err();
        assert_eq!(err, AuthErrorCode::InvalidBootstrapResponse);
        assert_eq!(
            map_http_error_body(r#"{"code":"ticket_used"}"#),
            AuthErrorCode::TicketUsed
        );
    }

    #[test]
    fn redemption_endpoint_uses_frontend_origin_only() {
        let url = redemption_url("https://foreseer.example:8443/discover?x=1").unwrap();
        assert_eq!(
            url,
            "https://foreseer.example:8443/api/v1/desktop/auth-tickets/redeem"
        );
    }
}
