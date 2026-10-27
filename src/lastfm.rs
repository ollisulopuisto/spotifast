//! Scrobbling to Last.fm.
//!
//! Tracks played here are sent to Last.fm so they appear on the listener's
//! profile. A track is scrobbled once enough of it has been heard, following
//! Last.fm's rule: 30 seconds, or half the track, whichever is shorter.
//!
//! Sign-in is Last.fm's web-based flow: Spotifast asks Last.fm for a token,
//! opens an authorization URL in the browser, then exchanges the token for a
//! session key kept in the credential store. The API secret signs every
//! request and is stored with the session, never in settings and never
//! logged.

use std::time::{SystemTime, UNIX_EPOCH};

use md5::{Digest, Md5};
use reqwest::StatusCode;
use serde_json::Value;

use crate::http::Http;

const BASE_URL: &str = "https://ws.audioscrobbler.com/2.0";
const AUTH_URL: &str = "https://www.last.fm/api/auth";

/// Last.fm's rule: a play counts after 30 seconds, or half a shorter track.
const COUNTS_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

/// A track played here, ready to scrobble.
#[derive(Clone, Debug)]
pub struct Scrobble {
    pub artist: String,
    pub track: String,
    /// Seconds the track lasts.
    pub duration: u32,
    /// When the track started playing, as Unix seconds.
    pub timestamp: u64,
    pub album: Option<String>,
}

/// The current track, for a now-playing report.
#[derive(Clone, Debug)]
pub struct NowPlaying {
    pub artist: String,
    pub track: String,
    pub album: Option<String>,
    pub duration: u32,
}

/// A finished sign-in: Last.fm's public API key, the per-user session key,
/// and the API secret used to sign every request.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub api_key: String,
    pub session_key: String,
    pub secret: String,
}

impl Session {
    /// Signs request parameters: sorted by key, each key concatenated with
    /// its value, the secret appended, then MD5-hashed.
    pub fn sign(&self, params: &[(&str, &str)]) -> String {
        sign(&self.secret, params)
    }
}

/// Signs request parameters with the API secret: Last.fm sorts them by key,
/// concatenates each key with its value, appends the secret, and MD5-hashes.
pub fn sign(secret: &str, params: &[(&str, &str)]) -> String {
    let mut sorted: Vec<&(&str, &str)> = params.iter().collect();
    sorted.sort_by_key(|(key, _)| *key);
    let mut text = String::new();
    for (key, value) in sorted {
        text.push_str(key);
        text.push_str(value);
    }
    text.push_str(secret);
    let mut hasher = Md5::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Seconds since the Unix epoch, now.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Whether `duration_ms` of listening is enough for a track to count.
pub fn counts_after(duration_ms: u32) -> std::time::Duration {
    let half = std::time::Duration::from_millis(u64::from(duration_ms) / 2);
    COUNTS_AFTER.min(half).max(std::time::Duration::from_secs(1))
}

/// Why Last.fm turned a request down, for the interface to explain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignInProblem {
    /// What was pasted is not shaped like an API key.
    MalformedApiKey,
    /// Last.fm does not know the API key.
    UnknownKey,
    /// Last.fm refused the session or the scrobble.
    Refused,
}

/// Errors from the Last.fm API or the local sign-in flow.
#[derive(Clone, Debug, thiserror::Error)]
pub enum Error {
    #[error("not signed in to Last.fm")]
    NotSignedIn,
    #[error("Last.fm answered {status}")]
    Status { status: u16, message: String },
    #[error("network error: {0}")]
    Network(String),
    #[error("unexpected response from Last.fm: {0}")]
    Decode(String),
}

impl Error {
    /// A problem the sign-in screen can explain, or `None` when it is not
    /// a sign-in failure.
    pub fn sign_in_problem(&self) -> Option<SignInProblem> {
        match self {
            Self::NotSignedIn => None,
            Self::Status { status, .. } => match *status {
                4 | 8 | 9 | 10 => Some(SignInProblem::Refused),
                _ => Some(SignInProblem::Refused),
            },
            Self::Network(_) => None,
            Self::Decode(_) => None,
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        if error.is_decode() {
            Self::Decode(error.to_string())
        } else {
            Self::Network(error.to_string())
        }
    }
}

/// The Last.fm API: scrobbling and now-playing reports.
///
/// `base_url` is the API endpoint. It is separate from the HTTP client so a
/// test can point at a local listener while production keeps the real one.
#[derive(Clone)]
pub struct Scrobbler {
    http: Http,
    base_url: String,
    session: Option<Session>,
}

impl Scrobbler {
    pub fn new(http: Http) -> Self {
        Self {
            http,
            base_url: BASE_URL.to_string(),
            session: None,
        }
    }

    /// A scrobbler pointed at a different API endpoint, for tests.
    #[cfg(test)]
    pub(crate) fn with_base_url(http: Http, base_url: String) -> Self {
        Self {
            http,
            base_url,
            session: None,
        }
    }

    /// The stored session, if the listener is signed in to Last.fm.
    pub fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    /// Stores a session, for example after sign-in or after loading it from
    /// the credential store.
    pub fn set_session(&mut self, session: Session) {
        self.session = Some(session);
    }

    /// Forgets the session, as signing out does.
    pub fn clear_session(&mut self) {
        self.session = None;
    }

    /// Whether the listener is signed in to Last.fm.
    pub fn is_signed_in(&self) -> bool {
        self.session.is_some()
    }

    /// The API endpoint this scrobbler talks to.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Sends a played track to Last.fm.
    pub async fn scrobble(&self, track: &Scrobble) -> Result<(), Error> {
        let session = self.session.as_ref().ok_or(Error::NotSignedIn)?;
        let mut params: Vec<(&str, String)> = vec![
            ("method", "track.scrobble".to_string()),
            ("api_key", session.api_key.clone()),
            ("session_key", session.session_key.clone()),
            ("artist", track.artist.clone()),
            ("track", track.track.clone()),
            ("timestamp", track.timestamp.to_string()),
        ];
        if let Some(album) = &track.album {
            params.push(("album", album.clone()));
        }
        params.push(("duration", track.duration.to_string()));
        self.post(session, &params).await
    }

    /// Tells Last.fm what is playing now.
    pub async fn now_playing(&self, now: &NowPlaying) -> Result<(), Error> {
        let session = self.session.as_ref().ok_or(Error::NotSignedIn)?;
        let mut params: Vec<(&str, String)> = vec![
            ("method", "track.updateNowPlaying".to_string()),
            ("api_key", session.api_key.clone()),
            ("session_key", session.session_key.clone()),
            ("artist", now.artist.clone()),
            ("track", now.track.clone()),
        ];
        if let Some(album) = &now.album {
            params.push(("album", album.clone()));
        }
        params.push(("duration", now.duration.to_string()));
        self.post(session, &params).await
    }

    /// POSTs signed parameters to Last.fm and checks the answer.
    async fn post(&self, session: &Session, params: &[(&str, String)]) -> Result<(), Error> {
        let client = self.http.client().map_err(|error| Error::Network(error.to_string()))?;
        let signature = session.sign(&signed_params(params));
        let mut form = vec![("api_sig", signature.as_str())];
        for (key, value) in params {
            form.push((key, value.as_str()));
        }
        let response = client
            .post(&self.base_url)
            .form(&form)
            .send()
            .await
            .map_err(|error| Error::Network(error.to_string()))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(Error::Status {
                status: status.as_u16(),
                message: "Last.fm is rate limiting requests; try again in a moment".into(),
            });
        }
        if !status.is_success() {
            return Err(Error::Status {
                status: status.as_u16(),
                message: status
                    .canonical_reason()
                    .unwrap_or("request failed")
                    .to_string(),
            });
        }
        // Last.fm answers track.scrobble and track.updateNowPlaying with the
        // bare text "ok"; anything else is a JSON object (an error or, for the
        // auth calls, a success).
        if text.trim().eq_ignore_ascii_case("ok") {
            return Ok(());
        }
        let value: Value = serde_json::from_str(&text).map_err(|_| Error::Decode(text.clone()))?;
        if let Some(error) = value.get("error") {
            let code = error.as_u64().unwrap_or(0);
            let message = value
                .get("message")
                .and_then(|message| message.as_str())
                .unwrap_or("request failed")
                .to_string();
            return Err(Error::Status {
                status: code as u16,
                message,
            });
        }
        Ok(())
    }

    /// Asks Last.fm for a request token, the first step of signing in.
    pub async fn request_token(
        http: &Http,
        base_url: &str,
        api_key: &str,
        secret: &str,
    ) -> Result<String, Error> {
        let params = [("api_key", api_key)];
        let signature = sign(secret, &params);
        let client = http
            .client()
            .map_err(|error| Error::Network(error.to_string()))?;
        let response = client
            .get(base_url)
            .query(&[
                ("method", "auth.getToken"),
                ("api_key", api_key),
                ("api_sig", &signature),
            ])
            .send()
            .await
            .map_err(|error| Error::Network(error.to_string()))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(Error::Status {
                status: status.as_u16(),
                message: status
                    .canonical_reason()
                    .unwrap_or("request failed")
                    .to_string(),
            });
        }
        let value: Value = serde_json::from_str(&text).map_err(|_| Error::Decode(text.clone()))?;
        let token = value
            .get("token")
            .and_then(|token| token.as_str())
            .ok_or_else(|| Error::Decode(text.clone()))?;
        Ok(token.to_string())
    }

    /// Exchanges a token for a session key. The secret signs the request.
    pub async fn session_for_token(
        http: &Http,
        base_url: &str,
        api_key: &str,
        secret: &str,
        token: &str,
    ) -> Result<Session, Error> {
        let params = [
            ("api_key", api_key),
            ("token", token),
        ];
        let signature = sign(secret, &params);
        let client = http
            .client()
            .map_err(|error| Error::Network(error.to_string()))?;
        let response = client
            .get(base_url)
            .query(&[
                ("method", "auth.getSession"),
                ("api_key", api_key),
                ("token", token),
                ("api_sig", &signature),
            ])
            .send()
            .await
            .map_err(|error| Error::Network(error.to_string()))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(Error::Status {
                status: status.as_u16(),
                message: status
                    .canonical_reason()
                    .unwrap_or("request failed")
                    .to_string(),
            });
        }
        let value: Value = serde_json::from_str(&text).map_err(|_| Error::Decode(text.clone()))?;
        if let Some(error) = value.get("error") {
            let code = error.as_u64().unwrap_or(0);
            let message = value
                .get("message")
                .and_then(|message| message.as_str())
                .unwrap_or("request failed")
                .to_string();
            return Err(Error::Status {
                status: code as u16,
                message,
            });
        }
        let session_key = value
            .get("session")
            .and_then(|session| session.get("key"))
            .and_then(|key| key.as_str())
            .ok_or_else(|| Error::Decode(text.clone()))?;
        Ok(Session {
            api_key: api_key.to_string(),
            session_key: session_key.to_string(),
            secret: secret.to_string(),
        })
    }

    /// The authorization URL to open in the browser, once a token is held.
    pub fn auth_url(api_key: &str, token: &str) -> String {
        format!(
            "{AUTH_URL}?api_key={}&token={}",
            urlencoding::encode(api_key),
            urlencoding::encode(token)
        )
    }
}

fn signed_params<'a>(params: &'a [(&str, String)]) -> Vec<(&'a str, &'a str)> {
    params
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_sorts_parameters_and_hashes_with_the_secret() {
        let signature = sign("secret", &[("b", "2"), ("a", "1")]);
        let text = "a1b2secret";
        let digest = md5::Md5::digest(text.as_bytes());
        assert_eq!(signature, format!("{digest:x}"));
    }

    #[test]
    fn signing_is_deterministic_and_secret_dependent() {
        let params = [("artist", "Bonobo"), ("track", "Rosewood")];
        let one = sign("s3cret", &params);
        let two = sign("s3cret", &params);
        assert_eq!(one, two);
        let other = sign("other-secret", &params);
        assert_ne!(one, other);
    }

    #[test]
    fn now_is_unix_seconds() {
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let value = now();
        let after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(before <= value && value <= after);
    }

    #[test]
    fn counts_after_uses_thirty_seconds_or_half() {
        assert_eq!(
            counts_after(60_000),
            std::time::Duration::from_secs(30)
        );
        assert_eq!(
            counts_after(40_000),
            std::time::Duration::from_secs(20)
        );
        assert_eq!(
            counts_after(2_000),
            std::time::Duration::from_secs(1)
        );
    }

    #[test]
    fn auth_url_encodes_api_key_and_token() {
        let url = Scrobbler::auth_url("abc-key", "tok-en");
        assert!(url.contains("api_key=abc-key"));
        assert!(url.contains("token=tok-en"));
        let encoded = Scrobbler::auth_url("a b", "t&k");
        assert!(!encoded.contains("a b"));
        assert!(!encoded.contains("t&k"));
    }

    #[test]
    fn sign_in_problems_classify_statuses() {
        assert_eq!(
            Error::Status {
                status: 4,
                message: "Invalid API key".into()
            }
            .sign_in_problem(),
            Some(SignInProblem::Refused)
        );
        assert_eq!(
            Error::Status {
                status: 9,
                message: "Invalid session key".into()
            }
            .sign_in_problem(),
            Some(SignInProblem::Refused)
        );
        assert_eq!(Error::NotSignedIn.sign_in_problem(), None);
        assert_eq!(Error::Network("down".into()).sign_in_problem(), None);
    }

    #[test]
    fn session_sign_matches_standalone_sign() {
        let session = Session {
            api_key: "key".into(),
            session_key: "sess".into(),
            secret: "secret".into(),
        };
        let params = [("a", "1"), ("b", "2")];
        assert_eq!(session.sign(&params), sign("secret", &params));
    }

    mod http_tests {
        use super::*;

        /// A minimal Last.fm-shaped JSON error.
        fn error_json(code: u64, message: &str) -> Value {
            serde_json::json!({"error": code, "message": message})
        }

        #[tokio::test]
        async fn request_token_parses_the_token() {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = [0u8; 4096];
                assert!(socket.read(&mut bytes).await.unwrap() > 0);
                let body = serde_json::to_string(&serde_json::json!({"token": "dummy-token"})).unwrap();
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            });
            let http = Http::new(reqwest::Client::builder().no_proxy().build().unwrap());
            let api = format!("http://{address}");
            let token = Scrobbler::request_token(&http, &api, "dummy-key", "dummy-secret")
                .await
                .unwrap();
            assert_eq!(token, "dummy-token");
            server.await.unwrap();
        }

        #[tokio::test]
        async fn session_for_token_parses_the_session_key() {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = [0u8; 4096];
                assert!(socket.read(&mut bytes).await.unwrap() > 0);
                let body = serde_json::to_string(&serde_json::json!({
                    "session": {"key": "dummy-session-key"}
                }))
                .unwrap();
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            });
            let http = Http::new(reqwest::Client::builder().no_proxy().build().unwrap());
            let api = format!("http://{address}");
            let session = Scrobbler::session_for_token(&http, &api, "dummy-key", "dummy-secret", "dummy-token")
                .await
                .unwrap();
            assert_eq!(session.session_key, "dummy-session-key");
            assert_eq!(session.api_key, "dummy-key");
            assert_eq!(session.secret, "dummy-secret");
            server.await.unwrap();
        }

        #[tokio::test]
        async fn scrobble_sends_signed_params_and_succeeds_on_ok() {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = [0u8; 8192];
                assert!(socket.read(&mut bytes).await.unwrap() > 0);
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                    .await;
            });
            let http = Http::new(reqwest::Client::builder().no_proxy().build().unwrap());
            let api = format!("http://{address}");
            let mut scrobbler = Scrobbler::with_base_url(http, api);
            scrobbler.set_session(Session {
                api_key: "key".into(),
                session_key: "sess".into(),
                secret: "secret".into(),
            });
            let result = scrobbler
                .scrobble(&Scrobble {
                    artist: "Bonobo".into(),
                    track: "Rosewood".into(),
                    duration: 245,
                    timestamp: now(),
                    album: None,
                })
                .await;
            assert!(result.is_ok());
            server.await.unwrap();
        }

        #[tokio::test]
        async fn scrobble_without_session_is_not_signed_in() {
            let http = Http::new(reqwest::Client::new());
            let scrobbler = Scrobbler::new(http);
            let error = scrobbler
                .scrobble(&Scrobble {
                    artist: "Bonobo".into(),
                    track: "Rosewood".into(),
                    duration: 245,
                    timestamp: now(),
                    album: None,
                })
                .await
                .unwrap_err();
            assert!(matches!(error, Error::NotSignedIn));
        }

        #[tokio::test]
        async fn now_playing_succeeds_on_ok() {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = [0u8; 4096];
                assert!(socket.read(&mut bytes).await.unwrap() > 0);
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                    .await;
            });
            let http = Http::new(reqwest::Client::builder().no_proxy().build().unwrap());
            let api = format!("http://{address}");
            let mut scrobbler = Scrobbler::with_base_url(http, api);
            scrobbler.set_session(Session {
                api_key: "key".into(),
                session_key: "sess".into(),
                secret: "secret".into(),
            });
            let result = scrobbler
                .now_playing(&NowPlaying {
                    artist: "Bonobo".into(),
                    track: "Rosewood".into(),
                    album: None,
                    duration: 245,
                })
                .await;
            assert!(result.is_ok());
            server.await.unwrap();
        }

        #[tokio::test]
        async fn last_fm_errors_are_reported_without_leaking_bodies() {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = [0u8; 4096];
                assert!(socket.read(&mut bytes).await.unwrap() > 0);
                let body = serde_json::to_string(&error_json(9, "dummy-session-secret")).unwrap();
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            });
            let http = Http::new(reqwest::Client::builder().no_proxy().build().unwrap());
            let api = format!("http://{address}");
            let mut scrobbler = Scrobbler::with_base_url(http, api);
            scrobbler.set_session(Session {
                api_key: "key".into(),
                session_key: "sess".into(),
                secret: "secret".into(),
            });
            let error = scrobbler
                .scrobble(&Scrobble {
                    artist: "Bonobo".into(),
                    track: "Rosewood".into(),
                    duration: 245,
                    timestamp: now(),
                    album: None,
                })
                .await
                .unwrap_err();
            let text = format!("{error}");
            assert!(!text.contains("dummy-session-secret"));
            assert_eq!(
                error.sign_in_problem(),
                Some(SignInProblem::Refused)
            );
            server.await.unwrap();
        }
    }
}
