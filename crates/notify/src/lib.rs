//! Webhook dispatch: HTTP POST with an HMAC-SHA256 signature over the
//! raw request body - the same model Stripe/GitHub webhooks use (see
//! the `webhooks` table's own migration comment in `rebalancer-db`).
//! The receiving endpoint recomputes the signature with its own copy of
//! `secret` and rejects anything that doesn't match, so a leaked webhook
//! URL alone doesn't let an attacker forge events.
//!
//! Email notifications aren't built - PROJECT.md calls for "email +
//! webhook alerts", but email needs a real provider (SendGrid, Postmark,
//! SES, ...) and an API key, which is a decision only the project owner
//! can make; this crate covers webhooks only.

use hmac::{Hmac, Mac};
use sha2::Sha256;

pub struct WebhookClient {
    http: reqwest::Client,
}

#[derive(Debug)]
pub enum WebhookError {
    Request(reqwest::Error),
    /// The receiving endpoint responded, just not with 2xx - still
    /// useful to distinguish from a network-level failure in logs.
    NonSuccessStatus(u16),
}

impl std::fmt::Display for WebhookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(e) => write!(f, "webhook request failed: {e}"),
            Self::NonSuccessStatus(code) => write!(f, "webhook endpoint returned status {code}"),
        }
    }
}

impl std::error::Error for WebhookError {}

impl Default for WebhookClient {
    fn default() -> Self {
        Self::new()
    }
}

impl WebhookClient {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
        }
    }

    /// Sends `body` (already-serialized JSON bytes) to `url`, signed with
    /// `secret` via `X-Rebalancer-Signature: sha256=<hex hmac>` computed
    /// over the exact bytes sent - the receiver must verify against the
    /// raw body it received, not a re-serialized version of it, or the
    /// signature won't match on any JSON key-ordering/whitespace
    /// difference between the two.
    pub async fn send(&self, url: &str, secret: &str, body: &[u8]) -> Result<(), WebhookError> {
        let signature = sign(secret, body);
        let response = self
            .http
            .post(url)
            .header("Content-Type", "application/json")
            .header("X-Rebalancer-Signature", format!("sha256={signature}"))
            .body(body.to_vec())
            .send()
            .await
            .map_err(WebhookError::Request)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(WebhookError::NonSuccessStatus(response.status().as_u16()))
        }
    }
}

/// Split out from `send` so the signature computation is testable
/// against a known HMAC-SHA256 vector without any network access.
fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC-SHA256 accepts a key of any length");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod test;
