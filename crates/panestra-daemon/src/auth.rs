use std::{collections::HashMap, time::Duration};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const BOOTSTRAP_TTL: Duration = Duration::from_secs(120);
const SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const TICKET_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
struct ExpiringToken {
    expires_at: std::time::Instant,
}

#[derive(Default)]
pub struct AuthState {
    bootstrap_tokens: Mutex<HashMap<String, ExpiringToken>>,
    browser_sessions: Mutex<HashMap<String, ExpiringToken>>,
    websocket_tickets: Mutex<HashMap<String, ExpiringToken>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapRequest {
    pub token: String,
    pub capabilities: BrowserCapabilities,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserCapabilities {
    pub web_gpu: bool,
    pub web_worker: bool,
    pub binary_web_socket: bool,
    pub text_decoder: bool,
    pub clipboard: bool,
    pub ime: bool,
}

impl BrowserCapabilities {
    pub fn is_supported(&self) -> bool {
        self.web_gpu
            && self.web_worker
            && self.binary_web_socket
            && self.text_decoder
            && self.clipboard
            && self.ime
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapResponse {
    pub credential: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketResponse {
    pub ticket: String,
    pub protocol: String,
}

impl AuthState {
    pub fn issue_bootstrap(&self) -> String {
        let token = random_token();
        self.bootstrap_tokens.lock().insert(
            token.clone(),
            ExpiringToken {
                expires_at: std::time::Instant::now() + BOOTSTRAP_TTL,
            },
        );
        token
    }

    pub fn exchange_bootstrap(&self, request: &BootstrapRequest) -> Option<String> {
        if !request.capabilities.is_supported() {
            return None;
        }
        let token = self.bootstrap_tokens.lock().remove(&request.token)?;
        if token.expires_at <= std::time::Instant::now() {
            return None;
        }
        let credential = random_token();
        self.browser_sessions.lock().insert(
            credential.clone(),
            ExpiringToken {
                expires_at: std::time::Instant::now() + SESSION_TTL,
            },
        );
        Some(credential)
    }

    pub fn validate_browser_session(&self, credential: &str) -> bool {
        let mut sessions = self.browser_sessions.lock();
        sessions.retain(|_, token| token.expires_at > std::time::Instant::now());
        sessions.contains_key(credential)
    }

    pub fn issue_websocket_ticket(&self, credential: &str) -> Option<TicketResponse> {
        if !self.validate_browser_session(credential) {
            return None;
        }
        let ticket = random_token();
        self.websocket_tickets.lock().insert(
            ticket.clone(),
            ExpiringToken {
                expires_at: std::time::Instant::now() + TICKET_TTL,
            },
        );
        Some(TicketResponse {
            protocol: format!("panestra.v2.{ticket}"),
            ticket,
        })
    }

    pub fn consume_websocket_ticket(&self, ticket: &str) -> bool {
        let Some(token) = self.websocket_tickets.lock().remove(ticket) else {
            return false;
        };
        token.expires_at > std::time::Instant::now()
    }
}

pub(crate) fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn supported(token: String) -> BootstrapRequest {
        BootstrapRequest {
            token,
            capabilities: BrowserCapabilities {
                web_gpu: true,
                web_worker: true,
                binary_web_socket: true,
                text_decoder: true,
                clipboard: true,
                ime: true,
            },
        }
    }

    #[test]
    fn bootstrap_token_is_one_time_and_requires_capabilities() {
        let auth = AuthState::default();
        let token = auth.issue_bootstrap();
        let credential = auth.exchange_bootstrap(&supported(token.clone())).unwrap();
        assert!(auth.validate_browser_session(&credential));
        assert!(auth.exchange_bootstrap(&supported(token)).is_none());
    }

    #[test]
    fn websocket_ticket_is_one_time() {
        let auth = AuthState::default();
        let token = auth.issue_bootstrap();
        let credential = auth.exchange_bootstrap(&supported(token)).unwrap();
        let ticket = auth.issue_websocket_ticket(&credential).unwrap();
        assert!(auth.consume_websocket_ticket(&ticket.ticket));
        assert!(!auth.consume_websocket_ticket(&ticket.ticket));
    }
}
