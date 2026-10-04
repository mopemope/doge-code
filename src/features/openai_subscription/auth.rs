use super::credentials::{Account, CredentialStore, Tokens};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use rand::TryRng;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

pub const ISSUER: &str = "https://auth.openai.com";
pub const RESOURCE: &str = "https://api.openai.com/v1";
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

#[derive(Clone)]
pub struct AuthHandle {
    pub store: CredentialStore,
    pub account: String,
    pub(crate) http: reqwest::Client,
    pub(crate) issuer: String,
    pub(crate) resource: String,
    refresh: Arc<tokio::sync::Mutex<()>>,
}
impl std::fmt::Debug for AuthHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthHandle")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    token_type: String,
    expires_in: u64,
    scope: Option<String>,
    earliest_refresh_at: Option<i64>,
}
#[derive(Clone, Deserialize)]
pub(crate) struct Claims {
    pub sub: String,
    pub nonce: Option<String>,
    pub email: Option<String>,
}
#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    jwks_uri: String,
    revocation_endpoint: String,
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
pub(crate) fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()?)
}

pub(crate) async fn json_response(resp: reqwest::Response) -> Result<serde_json::Value> {
    let status = resp.status();
    let request_id = resp
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let mut bytes = Vec::new();
    let mut resp = resp;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("OAuth response read failed"))?
    {
        if bytes.len().saturating_add(chunk.len()) > 1024 * 1024 {
            bail!("OAuth response exceeds size limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    let body = serde_json::from_slice(&bytes).unwrap_or_else(|_| serde_json::json!({}));
    if !status.is_success() {
        return Err(
            super::ProviderError::from_body(Some(status.as_u16()), &body, request_id).into(),
        );
    }
    Ok(body)
}

impl AuthHandle {
    pub fn selected(store: CredentialStore) -> Result<Self> {
        let registry = store.load()?;
        let account = registry
            .active
            .context("No ChatGPT account selected. Run dgc auth login openai-chatgpt.")?;
        let entry = registry
            .accounts
            .iter()
            .find(|a| a.label == account)
            .context("selected ChatGPT account missing")?;
        if entry.tokens.is_none() {
            bail!(
                "ChatGPT account is signed out. Run dgc auth login openai-chatgpt --account {account}."
            );
        }
        Ok(Self {
            store,
            account,
            http: http_client()?,
            issuer: ISSUER.into(),
            resource: RESOURCE.into(),
            refresh: Arc::new(tokio::sync::Mutex::new(())),
        })
    }
    pub async fn bearer(&self, cancel: &CancellationToken) -> Result<String> {
        tokio::select! { biased; _ = cancel.cancelled() => bail!(crate::llm::LlmErrorKind::Cancelled), result = self.bearer_inner(cancel) => result }
    }
    async fn bearer_inner(&self, cancel: &CancellationToken) -> Result<String> {
        let _flight = self.refresh.lock().await;
        let _lock = self.store.lock(cancel).await?;
        let mut registry = self.store.load()?;
        let entry = registry
            .accounts
            .iter_mut()
            .find(|a| a.label == self.account)
            .context("ChatGPT account missing")?;
        let tokens = entry
            .tokens
            .as_ref()
            .context("ChatGPT account signed out; sign in again")?
            .clone();
        if tokens.renewal_uncertain {
            bail!(
                "The previous token renewal was interrupted. Sign in again; the rotating token will not be replayed."
            );
        }
        if !tokens
            .scopes
            .iter()
            .any(|s| s == "chatgpt.tokens.use.direct")
        {
            bail!("ChatGPT plan usage was not granted. Sign in and enable plan usage explicitly.");
        }
        if tokens.expires_at > now() + 60 {
            return Ok(tokens.access_token.clone());
        }
        if tokens.earliest_refresh_at.is_some_and(|t| t > now()) {
            if tokens.expires_at > now() {
                return Ok(tokens.access_token.clone());
            }
            bail!("ChatGPT token expired before refresh is permitted; sign in again");
        }
        let refresh_token = tokens
            .refresh_token
            .as_ref()
            .context("ChatGPT refresh token missing; sign in again")?;
        let client_id = entry.client_id.clone();
        // Persist the uncertain state before consuming a rotating refresh token.
        // A crash/timeout/cancellation must not replay the old token next launch.
        entry
            .tokens
            .as_mut()
            .context("token disappeared")?
            .renewal_uncertain = true;
        self.store.save(&registry)?;
        let response = self.http.post(format!("{}/api/accounts/oauth/token", self.issuer.trim_end_matches('/')))
            .form(&[("grant_type", "refresh_token"), ("client_id", &client_id), ("refresh_token", refresh_token), ("resource", &self.resource)])
            .send().await.map_err(|_| anyhow::anyhow!("Token refresh transport failed; no automatic retry of the rotating token was made. Sign in again if access cannot be recovered."))?;
        let body = match json_response(response).await {
            Ok(body) => body,
            Err(error) => {
                if error
                    .downcast_ref::<super::ProviderError>()
                    .is_some_and(|e| {
                        matches!(
                            e.code.as_str(),
                            "invalid_grant"
                                | "invalid_refresh_token"
                                | "token_expired"
                                | "refresh_token_expired"
                                | "refresh_token_invalidated"
                                | "refresh_token_reused"
                        )
                    })
                {
                    registry
                        .accounts
                        .iter_mut()
                        .find(|a| a.label == self.account)
                        .context("account disappeared")?
                        .tokens = None;
                    self.store.save(&registry)?;
                }
                return Err(error
                    .context("Token renewal failed; sign in again with the saved account label"));
            }
        };
        let response: TokenResponse = serde_json::from_value(body)
            .map_err(|_| anyhow::anyhow!("invalid refresh response"))?;
        let replacement = token_set(response, Some(&tokens), now())?;
        if replacement.refresh_token.is_none() {
            bail!("Replacement refresh token missing; sign in again");
        }
        let plan_granted = replacement
            .scopes
            .iter()
            .any(|s| s == "chatgpt.tokens.use.direct");
        let access = replacement.access_token.clone();
        registry
            .accounts
            .iter_mut()
            .find(|a| a.label == self.account)
            .context("account disappeared")?
            .tokens = Some(replacement);
        self.store
            .save(&registry)
            .context("refreshed credentials could not be persisted; sign in again")?;
        if !plan_granted {
            bail!("ChatGPT plan permission was removed; sign in again");
        }
        Ok(access)
    }
}

fn token_set(response: TokenResponse, old: Option<&Tokens>, received_at: i64) -> Result<Tokens> {
    if !response.token_type.eq_ignore_ascii_case("bearer")
        || response.access_token.is_empty()
        || response.expires_in == 0
        || response.expires_in > i64::MAX as u64
    {
        bail!("invalid OAuth token response");
    }
    Ok(Tokens {
        renewal_uncertain: false,
        access_token: response.access_token,
        refresh_token: response.refresh_token,
        id_token: response
            .id_token
            .or_else(|| old.map(|t| t.id_token.clone()))
            .context("ID token missing")?,
        scopes: response
            .scope
            .map(|s| s.split_whitespace().map(str::to_owned).collect())
            .or_else(|| old.map(|t| t.scopes.clone()))
            .unwrap_or_default(),
        expires_at: received_at
            .checked_add(response.expires_in as i64)
            .context("invalid token expiry")?,
        earliest_refresh_at: response.earliest_refresh_at,
    })
}

pub(crate) fn random_secret() -> Result<String> {
    let mut bytes = [0u8; 32];
    rand::rngs::SysRng
        .try_fill_bytes(&mut bytes)
        .context("OS entropy unavailable")?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
pub(crate) fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub(crate) fn validate_id(
    token: &str,
    jwks: &JwkSet,
    client_id: &str,
    nonce: &str,
) -> Result<Claims> {
    let header = decode_header(token).map_err(|_| anyhow::anyhow!("invalid ID token header"))?;
    if !matches!(header.alg, Algorithm::RS256 | Algorithm::ES256) {
        bail!("unsupported ID token signing algorithm");
    }
    let key = jwks
        .find(header.kid.as_deref().context("ID token key ID missing")?)
        .context("ID token signing key unavailable")?;
    let mut validation = Validation::new(header.alg);
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[client_id]);
    validation.validate_nbf = true;
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    let claims = decode::<Claims>(token, &DecodingKey::from_jwk(key)?, &validation)
        .map_err(|_| anyhow::anyhow!("ID token signature or claims validation failed"))?
        .claims;
    if claims.nonce.as_deref() != Some(nonce) || claims.sub.is_empty() {
        bail!("ID token nonce or identity mismatch");
    }
    Ok(claims)
}

fn allowed_auth_url(value: &str) -> Result<()> {
    let url = url::Url::parse(value)?;
    if url.scheme() != "https"
        || url.host_str() != Some("auth.openai.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("untrusted OpenAI authentication endpoint");
    }
    Ok(())
}

/// Login dependencies are kept outside project configuration. Production always uses OpenAI.
pub(crate) struct AuthService {
    pub(crate) store: CredentialStore,
    pub(crate) http: reqwest::Client,
    pub(crate) auth_base: String,
    pub(crate) clock: fn() -> i64,
}
impl AuthService {
    fn production(store: CredentialStore) -> Result<Self> {
        Ok(Self {
            store,
            http: http_client()?,
            auth_base: ISSUER.into(),
            clock: now,
        })
    }
    fn check_endpoint(&self, value: &str) -> Result<()> {
        #[cfg(test)]
        if self.auth_base != ISSUER {
            let base = url::Url::parse(&self.auth_base)?;
            let endpoint = url::Url::parse(value)?;
            anyhow::ensure!(
                endpoint.origin() == base.origin(),
                "untrusted test auth endpoint"
            );
            return Ok(());
        }
        allowed_auth_url(value)
    }
    pub(crate) async fn login<F, Fut>(
        &self,
        account: Option<&str>,
        enable_plan_usage: bool,
        mut open: F,
        cancel: &CancellationToken,
    ) -> Result<String>
    where
        F: FnMut(url::Url) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!(crate::llm::LlmErrorKind::Cancelled),
            result = self.login_attempt(account, enable_plan_usage, &mut open, cancel, None) => result,
        }
    }
    async fn login_attempt<F, Fut>(
        &self,
        account: Option<&str>,
        enable_plan_usage: bool,
        open: &mut F,
        cancel: &CancellationToken,
        retry_client: Option<String>,
    ) -> Result<String>
    where
        F: FnMut(url::Url) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let store = &self.store;
        let http = &self.http;
        let (host_id, previous) = {
            let _lock = store.lock(cancel).await?;
            let mut registry = store.load()?;
            if registry.host_id.is_empty() {
                registry.host_id = format!("urn:uuid:{}", uuid::Uuid::new_v4());
                store.save(&registry)?;
            }
            let previous = account
                .map(|label| {
                    registry
                        .accounts
                        .iter()
                        .find(|a| a.label == label)
                        .cloned()
                        .context("unknown ChatGPT account label")
                })
                .transpose()?;
            (registry.host_id, previous)
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let redirect = format!(
            "http://127.0.0.1:{}/auth/callback",
            listener.local_addr()?.port()
        );
        let state = random_secret()?;
        let nonce = random_secret()?;
        let verifier = random_secret()?;
        let mut url = url::Url::parse(&format!("{}/api/accounts/authorize", self.auth_base))?;
        {
            let mut query = url.query_pairs_mut();
            query.extend_pairs([
                (
                    "client_id",
                    previous
                        .as_ref()
                        .map(|a| a.client_id.as_str())
                        .or(retry_client.as_deref())
                        .unwrap_or("dynamic_agent_client"),
                ),
                ("ext_agent_host_id", &host_id),
                ("response_type", "code"),
                ("redirect_uri", &redirect),
                ("scope", SCOPE),
                ("resource", RESOURCE),
                ("state", &state),
                ("nonce", &nonce),
                ("code_challenge_method", "S256"),
                ("code_challenge", &challenge(&verifier)),
            ]);
            if enable_plan_usage {
                query.append_pair("prompt", "consent");
            }
            if let Some(previous) = &previous {
                if let Some(tokens) = &previous.tokens {
                    query.append_pair("id_token_hint", &tokens.id_token);
                }
            } else if retry_client.is_none() {
                query.append_pair("agent_name_hint", "doge-code");
            }
        }
        tokio::select! { _ = cancel.cancelled() => bail!(crate::llm::LlmErrorKind::Cancelled), result = open(url) => result? };
        let callback = tokio::select! { _ = cancel.cancelled() => bail!(crate::llm::LlmErrorKind::Cancelled), result = tokio::time::timeout(Duration::from_secs(300), callback(listener, &state)) => result.context("ChatGPT login timed out")?? };
        let registered = previous
            .as_ref()
            .map(|p| p.client_id.as_str())
            .or(retry_client.as_deref());
        let client_id = match registered {
            Some(saved) => {
                if callback.client_id.as_ref().is_some_and(|id| id != saved) {
                    bail!("callback client ID mismatch");
                }
                saved.to_owned()
            }
            None => callback
                .client_id
                .filter(|id| id != "dynamic_agent_client" && !id.is_empty())
                .context("issued client ID missing")?,
        };
        let response = http
            .post(format!("{}/api/accounts/oauth/token", self.auth_base))
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", &client_id),
                ("code", &callback.code),
                ("code_verifier", &verifier),
                ("redirect_uri", &redirect),
                ("resource", RESOURCE),
            ])
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("OAuth code exchange failed; restart sign-in"))?;
        let body = match json_response(response).await {
            Ok(body) => body,
            Err(error)
                if retry_client.is_none()
                    && error
                        .downcast_ref::<super::ProviderError>()
                        .is_some_and(|e| e.code == "invalid_grant") =>
            {
                // The code is consumed or expired. Keep its issued registration ID,
                // but use a fresh state/nonce/verifier/listener/code exactly once.
                eprintln!(
                    "Authorization code expired; restarting sign-in with the issued registration."
                );
                return Box::pin(self.login_attempt(
                    account,
                    enable_plan_usage,
                    open,
                    cancel,
                    Some(client_id),
                ))
                .await;
            }
            Err(error) => return Err(error),
        };
        let tokens = token_set(
            serde_json::from_value(body).map_err(|_| anyhow::anyhow!("invalid token response"))?,
            None,
            (self.clock)(),
        )?;
        let discovery: Discovery = serde_json::from_value(
            json_response(
                http.get(format!(
                    "{}/.well-known/openid-configuration",
                    self.auth_base
                ))
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("OpenAI discovery unavailable"))?,
            )
            .await?,
        )?;
        if discovery.issuer != ISSUER {
            bail!("OIDC issuer mismatch");
        }
        self.check_endpoint(&discovery.jwks_uri)?;
        let jwks: JwkSet = serde_json::from_value(
            json_response(
                http.get(discovery.jwks_uri)
                    .send()
                    .await
                    .map_err(|_| anyhow::anyhow!("OpenAI signing keys unavailable"))?,
            )
            .await?,
        )?;
        let claims = validate_id(&tokens.id_token, &jwks, &client_id, &nonce)?;
        if previous.as_ref().is_some_and(|p| p.subject != claims.sub) {
            bail!("reauthorization returned a different account");
        }
        let _lock = store.lock(cancel).await?;
        let mut registry = store.load()?;
        // Do not overwrite credentials changed by another login/logout while the browser was open.
        if let Some(p) = &previous {
            let current = registry
                .accounts
                .iter()
                .find(|a| a.label == p.label)
                .context("account changed during login")?;
            if serde_json::to_vec(current)? != serde_json::to_vec(p)? {
                bail!("account changed during login; restart sign-in");
            }
        }
        let label = previous
            .as_ref()
            .map(|a| a.label.clone())
            .unwrap_or_else(|| format!("account-{}", uuid::Uuid::new_v4().simple()));
        registry.accounts.retain(|a| a.label != label);
        registry.accounts.push(Account {
            label: label.clone(),
            subject: claims.sub,
            client_id,
            email: claims.email,
            tokens: Some(tokens),
        });
        registry.active = Some(label.clone());
        store.save(&registry)?;
        Ok(label)
    }
}

pub async fn login(
    store: CredentialStore,
    account: Option<&str>,
    no_browser: bool,
    enable_plan_usage: bool,
    cancel: &CancellationToken,
) -> Result<String> {
    AuthService::production(store)?.login(account, enable_plan_usage, |url| async move {
    // Only this explicit login command exposes the ephemeral authorization URL.
    if no_browser {
        eprintln!("Continue with ChatGPT. Open this URL in your browser (do not share it):\n{url}");
    } else {
        let program = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let spec = crate::execution::runner::ManagedProcessSpec::new(
            program,
            vec![url.to_string()],
            std::env::current_dir()?,
        );
        let result = crate::execution::runner::run_managed_process(
            spec,
            crate::execution::runner::ManagedRunOptions::new(Some(Duration::from_secs(15)))
                .with_cancellation(Some(cancel.clone())),
        )
        .await?;
        if !result.success() {
            bail!("could not open browser; rerun with --no-browser");
        }
        eprintln!("Continue with ChatGPT in your browser.");
    }
        Ok(())
    }, cancel).await
}

pub(crate) struct Callback {
    pub code: String,
    pub client_id: Option<String>,
}
pub(crate) fn parse_callback(target: &str, state: &str) -> Result<Callback> {
    let url = url::Url::parse(&format!("http://127.0.0.1{target}"))?;
    if url.path() != "/auth/callback" {
        bail!("invalid callback path");
    }
    let mut params = std::collections::HashMap::new();
    for (key, value) in url.query_pairs() {
        if params
            .insert(key.into_owned(), value.into_owned())
            .is_some()
        {
            bail!("duplicate callback parameter");
        }
    }
    if params.get("state").map(String::as_str) != Some(state) {
        bail!("OAuth state mismatch");
    }
    if params.get("iss").is_some_and(|issuer| issuer != ISSUER) {
        bail!("callback issuer mismatch");
    }
    if params.contains_key("error") {
        bail!("ChatGPT sign-in was declined or failed");
    }
    let code = params
        .remove("code")
        .filter(|s| !s.is_empty())
        .context("callback code missing")?;
    Ok(Callback {
        code,
        client_id: params.remove("client_id"),
    })
}
async fn callback(listener: tokio::net::TcpListener, state: &str) -> Result<Callback> {
    let expected_host = format!("127.0.0.1:{}", listener.local_addr()?.port());
    loop {
        let (mut stream, _) = listener.accept().await?;
        let mut bytes = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let mut buf = [0; 1024];
                let n = stream.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buf[..n]);
                if bytes.len() > 8192 {
                    bail!("callback request too large");
                }
                if bytes.windows(4).any(|b| b == b"\r\n\r\n") {
                    break;
                }
            }
            Ok::<_, anyhow::Error>(())
        })
        .await;
        if !matches!(read, Ok(Ok(()))) {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        let hosts: Vec<_> = text
            .lines()
            .skip(1)
            .filter_map(|line| line.split_once(':'))
            .filter(|(name, _)| name.eq_ignore_ascii_case("host"))
            .map(|(_, value)| value.trim())
            .collect();
        if hosts.as_slice() != [expected_host.as_str()] {
            continue;
        }
        let mut line = text.lines().next().unwrap_or("").split_whitespace();
        if line.next() != Some("GET") {
            continue;
        }
        let target = line.next().unwrap_or("");
        if !target.starts_with("/auth/callback?") {
            let _ = stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            continue;
        }
        let result = parse_callback(target, state);
        let body = if result.is_ok() {
            "Return to doge-code to finish sign-in."
        } else {
            "Sign-in failed. Return to doge-code."
        };
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        return result;
    }
}

impl AuthService {
    pub(crate) async fn logout(
        &self,
        label: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<bool> {
        let store = &self.store;
        let _lock = store.lock(cancel).await?;
        let mut registry = store.load()?;
        let label = label
            .map(str::to_owned)
            .or_else(|| registry.active.clone())
            .context("no selected account")?;
        let entry = registry
            .accounts
            .iter_mut()
            .find(|a| a.label == label)
            .context("unknown account")?;
        let mut confirmed = entry.tokens.is_none();
        if let Some(refresh) = entry.tokens.as_ref().and_then(|t| t.refresh_token.as_ref()) {
            let http = &self.http;
            let revoke = async {
                let d: Discovery = serde_json::from_value(
                    json_response(
                        http.get(format!(
                            "{}/.well-known/openid-configuration",
                            self.auth_base
                        ))
                        .send()
                        .await?,
                    )
                    .await?,
                )?;
                if d.issuer != ISSUER {
                    bail!("OIDC issuer mismatch");
                }
                self.check_endpoint(&d.revocation_endpoint)?;
                for attempt in 0..3 {
                    match http
                        .post(&d.revocation_endpoint)
                        .form(&[
                            ("token", refresh.as_str()),
                            ("token_type_hint", "refresh_token"),
                            ("client_id", entry.client_id.as_str()),
                        ])
                        .send()
                        .await
                    {
                        Ok(response) if response.status() == reqwest::StatusCode::OK => {
                            return Ok(true);
                        }
                        Ok(response)
                            if !response.status().is_server_error()
                                && response.status().as_u16() != 429 =>
                        {
                            return Ok(false);
                        }
                        _ if attempt < 2 => {
                            tokio::time::sleep(Duration::from_millis(250 * (1 << attempt))).await
                        }
                        _ => return Ok(false),
                    }
                }
                Ok::<bool, anyhow::Error>(false)
            };
            confirmed = tokio::select! { _ = cancel.cancelled() => false, result = revoke => result.unwrap_or(false) };
        }
        entry.tokens = None;
        store.save(&registry)?;
        Ok(confirmed)
    }
}

pub async fn logout(
    store: &CredentialStore,
    label: Option<&str>,
    cancel: &CancellationToken,
) -> Result<bool> {
    AuthService::production(store.clone())?
        .logout(label, cancel)
        .await
}
