//! Protocol v1 command/event envelopes.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u8 = 1;
pub const HOST_NAME: &str = "foreseer-desktop";
pub const EVENT_NAME: &str = "foreseer:native-event";
pub const REQUEST_ID_MAX_LENGTH: usize = 64;
pub const ITEM_ID_MAX_LENGTH: usize = 128;
pub const TICKET_LENGTH: usize = 43;
pub const CHALLENGE_HEX_LENGTH: usize = 64;
pub const SETUP_MESSAGE_MAX_LENGTH: usize = 256;
pub const MAX_PAYLOAD_BYTES: usize = 16 * 1024;

fn valid_id(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub fn valid_request_id(request_id: &str) -> bool {
    valid_id(request_id, REQUEST_ID_MAX_LENGTH)
}

pub fn valid_item_id(item_id: &str) -> bool {
    valid_id(item_id, ITEM_ID_MAX_LENGTH)
}

pub fn valid_ticket(ticket: &str) -> bool {
    ticket.len() == TICKET_LENGTH
        && ticket
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum NativeCommandV1 {
    #[serde(rename = "auth.challenge")]
    AuthChallenge { id: String },
    #[serde(rename = "auth.complete")]
    AuthComplete { id: String, ticket: String },
    #[serde(rename = "session.clear")]
    SessionClear { id: String },
    #[serde(rename = "play.item")]
    PlayItem {
        id: String,
        #[serde(rename = "itemId")]
        item_id: String,
    },
    #[serde(rename = "setup.check")]
    SetupCheck {
        id: String,
        url: String,
        #[serde(rename = "allowHttp")]
        allow_http: bool,
    },
    #[serde(rename = "setup.save")]
    SetupSave {
        id: String,
        url: String,
        #[serde(rename = "allowHttp")]
        allow_http: bool,
    },
    #[serde(rename = "setup.standalone")]
    SetupStandalone { id: String },
    #[serde(rename = "browser-cache.clear")]
    BrowserCacheClear { id: String, ticket: String },
    #[serde(rename = "runtime.retry")]
    RuntimeRetry { id: String },
    #[serde(rename = "runtime.open-logs")]
    RuntimeOpenLogs { id: String },
    #[serde(rename = "runtime.open-setup")]
    RuntimeOpenSetup { id: String },
    #[serde(rename = "window.minimize")]
    WindowMinimize { id: String },
    #[serde(rename = "window.toggle-maximize")]
    WindowToggleMaximize { id: String },
    #[serde(rename = "window.toggle-fullscreen")]
    WindowToggleFullscreen { id: String },
    #[serde(rename = "app.quit")]
    AppQuit { id: String },
}

impl NativeCommandV1 {
    pub fn id(&self) -> &str {
        match self {
            Self::AuthChallenge { id }
            | Self::AuthComplete { id, .. }
            | Self::SessionClear { id }
            | Self::PlayItem { id, .. }
            | Self::SetupCheck { id, .. }
            | Self::SetupSave { id, .. }
            | Self::SetupStandalone { id }
            | Self::BrowserCacheClear { id, .. }
            | Self::RuntimeRetry { id }
            | Self::RuntimeOpenLogs { id }
            | Self::RuntimeOpenSetup { id }
            | Self::WindowMinimize { id }
            | Self::WindowToggleMaximize { id }
            | Self::WindowToggleFullscreen { id }
            | Self::AppQuit { id } => id,
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if !valid_request_id(self.id()) {
            return Err("invalid_request_id");
        }
        match self {
            Self::AuthComplete { ticket, .. } | Self::BrowserCacheClear { ticket, .. }
                if !valid_ticket(ticket) =>
            {
                Err("invalid_ticket")
            }
            Self::PlayItem { item_id, .. } if !valid_item_id(item_id) => Err("invalid_item_id"),
            Self::SetupCheck { url, .. } | Self::SetupSave { url, .. }
                if url.is_empty() || url.len() > crate::config::MAX_FORESEER_URL_LEN =>
            {
                Err("invalid_url")
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeEventV1 {
    pub protocol_version: u8,
    pub id: String,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "errorCode")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

impl NativeEventV1 {
    pub fn new(id: impl Into<String>, event_type: impl Into<String>) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            id: id.into(),
            event_type: event_type.into(),
            challenge: None,
            error_code: None,
            status: None,
            message: None,
            payload: None,
        }
    }

    pub fn with_challenge(mut self, challenge: impl Into<String>) -> Self {
        self.challenge = Some(challenge.into());
        self
    }

    pub fn with_error(mut self, code: impl Into<String>) -> Self {
        self.error_code = Some(code.into());
        self
    }

    pub fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        let mut msg = message.into();
        if msg.len() > SETUP_MESSAGE_MAX_LENGTH {
            msg.truncate(SETUP_MESSAGE_MAX_LENGTH);
        }
        self.message = Some(msg);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    NonUtf8,
    Oversized,
    Json,
    Validation(&'static str),
}

pub fn parse_command(bytes: &[u8]) -> Result<NativeCommandV1, ParseError> {
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(ParseError::Oversized);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ParseError::NonUtf8)?;
    let command: NativeCommandV1 = serde_json::from_str(text).map_err(|_| ParseError::Json)?;
    command
        .validate()
        .map_err(ParseError::Validation)
        .map(|_| command)
}

pub fn serialize_event(event: &NativeEventV1) -> Result<Vec<u8>, ParseError> {
    let text = serde_json::to_string(event).map_err(|_| ParseError::Json)?;
    if text.len() > MAX_PAYLOAD_BYTES {
        return Err(ParseError::Oversized);
    }
    Ok(text.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_play_item_and_denies_unknown_fields() {
        let ok = br#"{"id":"req-1","type":"play.item","itemId":"abc123"}"#;
        let cmd = parse_command(ok).unwrap();
        assert!(matches!(cmd, NativeCommandV1::PlayItem { .. }));

        let bad = br#"{"id":"req-1","type":"play.item","itemId":"abc123","extra":true}"#;
        assert_eq!(parse_command(bad), Err(ParseError::Json));
    }

    #[test]
    fn rejects_oversized_and_non_utf8() {
        let huge = vec![b'a'; MAX_PAYLOAD_BYTES + 1];
        assert_eq!(parse_command(&huge), Err(ParseError::Oversized));
        assert_eq!(parse_command(&[0xff, 0xfe]), Err(ParseError::NonUtf8));
    }

    #[test]
    fn rejects_invalid_ticket_and_item_id() {
        let bad_ticket = br#"{"id":"req-1","type":"auth.complete","ticket":"short"}"#;
        assert!(matches!(
            parse_command(bad_ticket),
            Err(ParseError::Validation("invalid_ticket"))
        ));
        let bad_item = br#"{"id":"req-1","type":"play.item","itemId":""}"#;
        assert!(matches!(
            parse_command(bad_item),
            Err(ParseError::Validation("invalid_item_id"))
        ));
    }

    #[test]
    fn parses_browser_cache_clear_ticket() {
        let ticket = "a".repeat(TICKET_LENGTH);
        let text =
            format!(r#"{{"id":"cache-1","type":"browser-cache.clear","ticket":"{ticket}"}}"#);
        assert!(matches!(
            parse_command(text.as_bytes()),
            Ok(NativeCommandV1::BrowserCacheClear { .. })
        ));
    }

    #[test]
    fn parses_runtime_retry_without_optional_fields() {
        let command = br#"{"id":"recovery-1","type":"runtime.retry"}"#;
        assert!(matches!(
            parse_command(command),
            Ok(NativeCommandV1::RuntimeRetry { .. })
        ));
    }

    #[test]
    fn parses_runtime_open_logs_without_optional_fields() {
        let command = br#"{"id":"recovery-1","type":"runtime.open-logs"}"#;
        assert!(matches!(
            parse_command(command),
            Ok(NativeCommandV1::RuntimeOpenLogs { .. })
        ));
    }

    #[test]
    fn parses_runtime_open_setup_without_optional_fields() {
        let command = br#"{"id":"recovery-1","type":"runtime.open-setup"}"#;
        assert!(matches!(
            parse_command(command),
            Ok(NativeCommandV1::RuntimeOpenSetup { .. })
        ));
    }

    #[test]
    fn fixture_matches_package_version_and_limits() {
        let fixture = include_str!("../protocol/protocol-v1.json");
        let value: Value = serde_json::from_str(fixture).unwrap();
        assert_eq!(value["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(value["host"]["name"], HOST_NAME);
        assert_eq!(value["eventName"], EVENT_NAME);
        assert_eq!(value["limits"]["maxPayloadBytes"], MAX_PAYLOAD_BYTES);
        assert_eq!(value["host"]["versionSource"], "package-metadata");
        let version = env!("CARGO_PKG_VERSION");
        assert!(!version.is_empty());
    }

    #[test]
    fn serialize_event_never_includes_secrets_fields_by_default() {
        let event = NativeEventV1::new("r1", "error").with_error("ticket_expired");
        let bytes = serialize_event(&event).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("accessToken"));
        assert!(!text.contains("\"ticket\""));
        assert!(!text.contains("verifier"));
        assert!(text.contains("ticket_expired"));
    }
}
