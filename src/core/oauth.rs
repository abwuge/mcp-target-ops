use crate::core::error::{Error, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct OAuthState {
    #[serde(default)]
    clients: BTreeMap<String, OAuthClient>,
    #[serde(default, skip)]
    codes: BTreeMap<String, AuthorizationCode>,
    #[serde(default)]
    tokens: BTreeMap<String, AccessToken>,
    #[serde(default)]
    refresh_tokens: BTreeMap<String, RefreshToken>,
    #[serde(default, skip)]
    state_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthClient {
    pub client_id: String,
    pub client_name: Option<String>,
    pub redirect_uris: Vec<String>,
    pub issued_at_unix: u64,
}

#[derive(Debug, Clone)]
pub struct AuthorizationCodeRequest {
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
    pub scopes: Vec<String>,
    pub resource: String,
}

#[derive(Debug, Clone)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
    pub scope: String,
}

#[derive(Debug, Clone, Copy)]
pub struct TokenLifetimes {
    pub access_token_secs: u64,
    pub refresh_token_secs: u64,
}

#[derive(Debug, Clone)]
pub struct OAuthError {
    pub error: &'static str,
    pub description: String,
}

#[derive(Debug, Clone)]
struct AuthorizationCode {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    code_challenge_method: String,
    scopes: Vec<String>,
    resource: String,
    expires_at: SystemTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AccessToken {
    expires_at: SystemTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RefreshToken {
    expires_at: SystemTime,
    client_id: String,
    scopes: Vec<String>,
    resource: String,
}

impl OAuthState {
    pub fn load(state_file: Option<PathBuf>) -> Result<Self> {
        let Some(path) = state_file else {
            return Ok(Self::default());
        };

        let mut state = if path.exists() {
            check_private_permissions(&path)?;
            let bytes = fs::read(&path).map_err(|err| {
                Error::Config(format!(
                    "failed to read OAuth state {}: {err}",
                    path.display()
                ))
            })?;
            serde_json::from_slice::<Self>(&bytes).map_err(|err| {
                Error::Config(format!(
                    "failed to parse OAuth state {}: {err}",
                    path.display()
                ))
            })?
        } else {
            Self::default()
        };
        state.state_file = Some(path);
        state.prune_expired(SystemTime::now());
        Ok(state)
    }

    pub fn register_client(
        &mut self,
        client_name: Option<String>,
        redirect_uris: Vec<String>,
    ) -> std::result::Result<OAuthClient, OAuthError> {
        self.prune_expired(SystemTime::now());

        let client = OAuthClient {
            client_id: format!("client_{}", random_url_token(24)),
            client_name,
            redirect_uris,
            issued_at_unix: unix_now(),
        };
        self.clients
            .insert(client.client_id.clone(), client.clone());
        self.persist_oauth()?;
        Ok(client)
    }

    pub fn client_allows_redirect(&self, client_id: &str, redirect_uri: &str) -> bool {
        self.clients
            .get(client_id)
            .map(|client| {
                client
                    .redirect_uris
                    .iter()
                    .any(|registered| registered == redirect_uri)
            })
            .unwrap_or(false)
    }

    pub fn has_client(&self, client_id: &str) -> bool {
        self.clients.contains_key(client_id)
    }

    pub fn issue_authorization_code(
        &mut self,
        request: AuthorizationCodeRequest,
        ttl_secs: u64,
    ) -> String {
        let now = SystemTime::now();
        self.prune_expired(now);

        let code = random_url_token(32);
        self.codes.insert(
            code.clone(),
            AuthorizationCode {
                client_id: request.client_id,
                redirect_uri: request.redirect_uri,
                code_challenge: request.code_challenge,
                code_challenge_method: request.code_challenge_method,
                scopes: request.scopes,
                resource: request.resource,
                expires_at: now + Duration::from_secs(ttl_secs),
            },
        );
        code
    }

    pub fn exchange_authorization_code(
        &mut self,
        code: &str,
        client_id: &str,
        redirect_uri: &str,
        code_verifier: &str,
        resource: Option<&str>,
        lifetimes: TokenLifetimes,
    ) -> std::result::Result<TokenResponse, OAuthError> {
        let now = SystemTime::now();
        self.prune_expired(now);

        let Some(code_record) = self.codes.remove(code) else {
            return Err(OAuthError::new(
                "invalid_grant",
                "authorization code is unknown or expired",
            ));
        };

        if code_record.expires_at <= now {
            return Err(OAuthError::new(
                "invalid_grant",
                "authorization code is expired",
            ));
        }

        if code_record.client_id != client_id {
            return Err(OAuthError::new(
                "invalid_grant",
                "authorization code was issued to a different client",
            ));
        }

        if code_record.redirect_uri != redirect_uri {
            return Err(OAuthError::new(
                "invalid_grant",
                "redirect_uri does not match the authorization request",
            ));
        }

        if let Some(resource) = resource {
            if resource != code_record.resource {
                return Err(OAuthError::new(
                    "invalid_target",
                    "resource does not match the authorization request",
                ));
            }
        }

        if !pkce_matches(
            &code_record.code_challenge_method,
            &code_record.code_challenge,
            code_verifier,
        ) {
            return Err(OAuthError::new(
                "invalid_grant",
                "PKCE code verifier did not match",
            ));
        }

        let response = self.issue_token_pair(
            client_id.to_string(),
            code_record.scopes,
            code_record.resource,
            lifetimes,
            now,
        );
        self.persist_oauth()?;
        Ok(response)
    }

    pub fn exchange_refresh_token(
        &mut self,
        refresh_token: &str,
        client_id: &str,
        resource: Option<&str>,
        requested_scope: Option<&str>,
        lifetimes: TokenLifetimes,
    ) -> std::result::Result<TokenResponse, OAuthError> {
        let now = SystemTime::now();
        self.prune_expired(now);

        let Some(record) = self.refresh_tokens.get(refresh_token).cloned() else {
            return Err(OAuthError::new(
                "invalid_grant",
                "refresh token is unknown or expired",
            ));
        };

        if record.client_id != client_id {
            return Err(OAuthError::new(
                "invalid_grant",
                "refresh token was issued to a different client",
            ));
        }

        if let Some(resource) = resource {
            if resource != record.resource {
                return Err(OAuthError::new(
                    "invalid_target",
                    "resource does not match the refresh token",
                ));
            }
        }

        let scopes = match requested_scope {
            Some(scope) => {
                let requested = scope
                    .split_whitespace()
                    .filter(|scope| !scope.is_empty())
                    .map(ToString::to_string)
                    .collect::<Vec<_>>();
                if requested.is_empty()
                    || requested.iter().any(|scope| !record.scopes.contains(scope))
                {
                    return Err(OAuthError::new(
                        "invalid_scope",
                        "requested scope exceeds the refresh token scope",
                    ));
                }
                requested
            }
            None => record.scopes,
        };

        self.refresh_tokens.remove(refresh_token);
        let response =
            self.issue_token_pair(record.client_id, scopes, record.resource, lifetimes, now);
        self.persist_oauth()?;
        Ok(response)
    }

    pub fn access_token_valid(&mut self, token: &str) -> bool {
        let now = SystemTime::now();
        self.prune_expired(now);
        self.tokens
            .get(token)
            .map(|record| record.expires_at > now)
            .unwrap_or(false)
    }

    fn prune_expired(&mut self, now: SystemTime) {
        self.codes.retain(|_, code| code.expires_at > now);
        self.tokens.retain(|_, token| token.expires_at > now);
        self.refresh_tokens
            .retain(|_, token| token.expires_at > now);
    }

    fn persist_oauth(&self) -> std::result::Result<(), OAuthError> {
        self.persist()
            .map_err(|err| OAuthError::new("server_error", err.to_string()))
    }

    fn persist(&self) -> Result<()> {
        let Some(path) = &self.state_file else {
            return Ok(());
        };
        let parent = path.parent().ok_or_else(|| {
            Error::Config(format!("OAuth state path {} has no parent", path.display()))
        })?;
        fs::create_dir_all(parent).map_err(|err| {
            Error::Config(format!(
                "failed to create OAuth state directory {}: {err}",
                parent.display()
            ))
        })?;

        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
        #[cfg(unix)]
        tmp.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        serde_json::to_writer(tmp.as_file_mut(), self)?;
        tmp.as_file_mut().write_all(b"\n")?;
        tmp.as_file_mut().sync_all()?;
        tmp.persist(path).map_err(|err| {
            Error::Config(format!(
                "failed to persist OAuth state {}: {}",
                path.display(),
                err.error
            ))
        })?;
        Ok(())
    }

    fn issue_token_pair(
        &mut self,
        client_id: String,
        scopes: Vec<String>,
        resource: String,
        lifetimes: TokenLifetimes,
        now: SystemTime,
    ) -> TokenResponse {
        let access_token = format!("mcp_{}", random_url_token(32));
        let refresh_token = format!("mcp_rt_{}", random_url_token(48));

        self.tokens.insert(
            access_token.clone(),
            AccessToken {
                expires_at: now + Duration::from_secs(lifetimes.access_token_secs),
            },
        );
        self.refresh_tokens.insert(
            refresh_token.clone(),
            RefreshToken {
                expires_at: now + Duration::from_secs(lifetimes.refresh_token_secs),
                client_id,
                scopes: scopes.clone(),
                resource,
            },
        );

        TokenResponse {
            access_token,
            refresh_token,
            expires_in: lifetimes.access_token_secs,
            scope: scopes.join(" "),
        }
    }
}

impl OAuthError {
    pub fn new(error: &'static str, description: impl Into<String>) -> Self {
        Self {
            error,
            description: description.into(),
        }
    }
}

fn pkce_matches(method: &str, challenge: &str, verifier: &str) -> bool {
    if method != "S256" {
        return false;
    }

    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest) == challenge
}

fn random_url_token(bytes: usize) -> String {
    let mut buf = vec![0_u8; bytes];
    OsRng.fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn check_private_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let mode = fs::metadata(path)?.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(Error::Config(format!(
                "OAuth state file {} must not be accessible by group or others; expected mode 0600",
                path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{AuthorizationCodeRequest, OAuthState, TokenLifetimes};
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    #[test]
    fn exchanges_authorization_code_with_pkce() {
        let verifier = "correct horse battery staple";
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut state = OAuthState::load(None).unwrap();

        let code = state.issue_authorization_code(
            AuthorizationCodeRequest {
                client_id: "client-1".to_string(),
                redirect_uri: "https://chatgpt.com/connector/oauth/callback".to_string(),
                code_challenge: challenge,
                code_challenge_method: "S256".to_string(),
                scopes: vec!["mcp:tools".to_string()],
                resource: "https://mcp.example.com".to_string(),
            },
            600,
        );

        let token = state
            .exchange_authorization_code(
                &code,
                "client-1",
                "https://chatgpt.com/connector/oauth/callback",
                verifier,
                Some("https://mcp.example.com"),
                TokenLifetimes {
                    access_token_secs: 3600,
                    refresh_token_secs: 2_592_000,
                },
            )
            .expect("code exchange succeeds");

        assert_eq!(token.scope, "mcp:tools");
        assert!(state.access_token_valid(&token.access_token));

        let refreshed = state
            .exchange_refresh_token(
                &token.refresh_token,
                "client-1",
                Some("https://mcp.example.com"),
                None,
                TokenLifetimes {
                    access_token_secs: 3600,
                    refresh_token_secs: 2_592_000,
                },
            )
            .expect("refresh succeeds");
        assert_ne!(refreshed.refresh_token, token.refresh_token);
        assert!(state.access_token_valid(&refreshed.access_token));

        let err = state
            .exchange_refresh_token(
                &token.refresh_token,
                "client-1",
                Some("https://mcp.example.com"),
                None,
                TokenLifetimes {
                    access_token_secs: 3600,
                    refresh_token_secs: 2_592_000,
                },
            )
            .expect_err("rotated refresh token cannot be reused");
        assert_eq!(err.error, "invalid_grant");
    }

    #[test]
    fn persists_clients_and_refresh_tokens_across_restart() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("oauth-state.json");
        let mut state = OAuthState::load(Some(path.clone())).unwrap();
        let client = state
            .register_client(
                Some("ChatGPT".to_string()),
                vec!["https://chatgpt.com/connector/oauth/callback".to_string()],
            )
            .unwrap();

        let verifier = "persistent verifier";
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let code = state.issue_authorization_code(
            AuthorizationCodeRequest {
                client_id: client.client_id.clone(),
                redirect_uri: client.redirect_uris[0].clone(),
                code_challenge: challenge,
                code_challenge_method: "S256".to_string(),
                scopes: vec!["mcp:tools".to_string()],
                resource: "https://mcp.example.com".to_string(),
            },
            600,
        );
        let token = state
            .exchange_authorization_code(
                &code,
                &client.client_id,
                &client.redirect_uris[0],
                verifier,
                Some("https://mcp.example.com"),
                TokenLifetimes {
                    access_token_secs: 3600,
                    refresh_token_secs: 2_592_000,
                },
            )
            .unwrap();
        drop(state);

        let mut restored = OAuthState::load(Some(path)).unwrap();
        assert!(restored.has_client(&client.client_id));
        assert!(restored.access_token_valid(&token.access_token));
        let refreshed = restored
            .exchange_refresh_token(
                &token.refresh_token,
                &client.client_id,
                Some("https://mcp.example.com"),
                None,
                TokenLifetimes {
                    access_token_secs: 3600,
                    refresh_token_secs: 2_592_000,
                },
            )
            .expect("refresh token survives restart");
        assert_ne!(refreshed.refresh_token, token.refresh_token);
    }

    #[test]
    fn rejects_reused_authorization_code() {
        let verifier = "verifier";
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut state = OAuthState::load(None).unwrap();
        let code = state.issue_authorization_code(
            AuthorizationCodeRequest {
                client_id: "client-1".to_string(),
                redirect_uri: "https://chatgpt.com/connector/oauth/callback".to_string(),
                code_challenge: challenge,
                code_challenge_method: "S256".to_string(),
                scopes: vec!["mcp:tools".to_string()],
                resource: "https://mcp.example.com".to_string(),
            },
            600,
        );

        let _ = state.exchange_authorization_code(
            &code,
            "client-1",
            "https://chatgpt.com/connector/oauth/callback",
            verifier,
            Some("https://mcp.example.com"),
            TokenLifetimes {
                access_token_secs: 3600,
                refresh_token_secs: 2_592_000,
            },
        );

        let err = state
            .exchange_authorization_code(
                &code,
                "client-1",
                "https://chatgpt.com/connector/oauth/callback",
                verifier,
                Some("https://mcp.example.com"),
                TokenLifetimes {
                    access_token_secs: 3600,
                    refresh_token_secs: 2_592_000,
                },
            )
            .expect_err("code cannot be reused");
        assert_eq!(err.error, "invalid_grant");
    }
}
