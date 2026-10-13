//! Temporary access to an owner-approved account for Store certification.

use std::{fmt, time::Duration};

use anyhow::{Context as _, Result, bail};
use chrono::Utc;
use rspotify::Token;
use serde::Deserialize;

use crate::spotify::valid_client_id;

const SESSION_URL: &str = "https://spotic.qvq.cz/v1/session";
const MAX_RESPONSE_BYTES: usize = 24 * 1024;

/// A bearer credential. Never include its value in diagnostics or saved settings.
#[derive(Clone)]
pub struct ReviewAccess {
    code: String,
    http: oauth2_reqwest::Client,
}

impl fmt::Debug for ReviewAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ReviewAccess([redacted])")
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewSession {
    pub client_id: String,
    pub account_id: String,
    pub library: AccessToken,
    pub playback: AccessToken,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccessToken {
    pub access_token: String,
    expires_in: u32,
    scope: String,
}

impl AccessToken {
    pub(crate) fn into_library_token(self) -> Token {
        // Check the code's validity every five minutes, even while Spotify's
        // access token remains valid. This does not shorten Spotify's lifetime.
        let expires_in = chrono::Duration::seconds(i64::from(self.expires_in.min(300)));
        Token {
            access_token: self.access_token,
            expires_in,
            expires_at: Some(Utc::now() + expires_in),
            refresh_token: None,
            scopes: self.scope.split_whitespace().map(str::to_owned).collect(),
        }
    }
}

impl ReviewAccess {
    pub fn new(code: &str) -> Result<Self> {
        let code = code.trim();
        if !code.strip_prefix("review-").is_some_and(|secret| {
            secret.len() == 64 && secret.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            bail!("Enter the review code supplied for certification.");
        }
        Ok(Self {
            code: code.to_owned(),
            http: oauth2_reqwest::Client::builder()
                .timeout(Duration::from_secs(40))
                .redirect(oauth2_reqwest::redirect::Policy::none())
                .build()
                .context("Could not create the review access client")?,
        })
    }

    pub(crate) async fn session(&self) -> Result<ReviewSession> {
        let response = self
            .http
            .post(SESSION_URL)
            .bearer_auth(&self.code)
            .send()
            .await
            .context("Could not reach the review access service")?;
        match response.status().as_u16() {
            200 => {}
            401 | 403 => bail!("Review access is unavailable or expired."),
            _ => bail!("Review access is temporarily unavailable. Try again later."),
        }
        let mut response = response;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                bail!("The review access response is too large.");
            }
            body.extend_from_slice(&chunk);
        }
        parse_session(&body)
    }
}

fn parse_session(body: &[u8]) -> Result<ReviewSession> {
    let session: ReviewSession = serde_json::from_slice(body)
        .map_err(|_| anyhow::anyhow!("Invalid review access response."))?;
    if !valid_client_id(&session.client_id)
        || session.account_id.is_empty()
        || [&session.library, &session.playback]
            .iter()
            .any(|token| token.access_token.is_empty() || !(1..=3600).contains(&token.expires_in))
    {
        bail!("Invalid review access response.");
    }
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_codes_are_validated_and_redacted() {
        let code = format!("review-{}", "a".repeat(64));
        let access = ReviewAccess::new(&code).unwrap();
        assert!(!format!("{access:?}").contains(&code));
        assert!(ReviewAccess::new("review-short").is_err());
        assert!(ReviewAccess::new(&format!("review-{}", "g".repeat(64))).is_err());
    }

    #[test]
    fn review_tokens_expire_and_cannot_supply_a_refresh_token() {
        let mut body = serde_json::json!({
            "client_id": "a".repeat(32), "account_id": "owner",
            "library": {"access_token":"library", "expires_in":300,"scope":"user-library-read"},
            "playback": {"access_token":"playback", "expires_in":300,"scope":"streaming"}
        });
        let session = parse_session(&serde_json::to_vec(&body).unwrap()).unwrap();
        let token = session.library.into_library_token();
        assert!(token.refresh_token.is_none());
        assert!(!token.is_expired());
        assert!(token.scopes.contains("user-library-read"));
        body["library"]["refresh_token"] = "must-stay-on-server".into();
        assert!(parse_session(&serde_json::to_vec(&body).unwrap()).is_err());
        body["library"]
            .as_object_mut()
            .unwrap()
            .remove("refresh_token");
        body["playback"]["expires_in"] = 0.into();
        assert!(parse_session(&serde_json::to_vec(&body).unwrap()).is_err());
    }
}
