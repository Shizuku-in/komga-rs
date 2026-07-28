//! OAuth2/OIDC configuration and process-local authorization transactions.

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    net::IpAddr,
    path::Path as FilePath,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::{Path, Request, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Redirect, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use dashmap::DashMap;
use futures_util::StreamExt;
use komga_core::User;
use komga_storage::StorageError;
use oauth2::{
    AsyncHttpClient, AuthType, AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, HttpRequest,
    HttpResponse, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse as OAuth2TokenResponse,
    TokenUrl, basic::BasicClient,
};
use openidconnect::{
    ClaimsVerificationError, ClientId as OidcClientId, ClientSecret as OidcClientSecret, IssuerUrl, Nonce,
    RedirectUrl as OidcRedirectUrl, Scope as OidcScope, SignatureVerificationError, TokenResponse as OidcTokenResponse,
    core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata, CoreUserInfoClaims},
};
use rand::Rng;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::{Mutex, RwLock};
use url::Url;

use crate::{
    AppState,
    auth::{CookieSettings, append_cookie, authentication_request_metadata, record_oauth_activity, session_cookie},
    error::ApiError,
    webpub_routes::ManifestUrls,
};

const MAX_HTTP_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_HTTP_REQUEST_BYTES: usize = 64 * 1024;
const MAX_TRANSACTIONS: usize = 4096;
const TRANSACTION_TTL: Duration = Duration::from_secs(10 * 60);
const OAUTH_BINDING_COOKIE: &str = "KOMGA-OAUTH2-STATE";

#[derive(Debug, Error)]
pub(crate) enum OAuthConfigError {
    #[error("OAuth configuration file does not exist: {0}")]
    Missing(String),
    #[error("could not read OAuth configuration: {0}")]
    Read(#[from] std::io::Error),
    #[error("invalid OAuth configuration JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid OAuth configuration: {0}")]
    Invalid(String),
    #[error("could not initialize OAuth HTTP client: {0}")]
    HttpClient(#[from] reqwest::Error),
    #[error("OIDC discovery failed for {provider}: {message}")]
    Discovery { provider: String, message: String },
}

#[derive(Debug, Error)]
pub(crate) enum OAuthHttpError {
    #[error("OAuth HTTP request URL is not allowed")]
    BlockedUrl,
    #[error("OAuth HTTP request exceeded {MAX_HTTP_REQUEST_BYTES} bytes")]
    RequestTooLarge,
    #[error("OAuth HTTP request failed")]
    Request(#[from] reqwest::Error),
    #[error("OAuth HTTP response was invalid")]
    Response(#[from] http::Error),
    #[error("OAuth HTTP response exceeded {MAX_HTTP_RESPONSE_BYTES} bytes")]
    ResponseTooLarge,
}

#[derive(Clone)]
pub(crate) struct LimitedHttpClient {
    client: reqwest::Client,
}

impl LimitedHttpClient {
    fn new() -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(10))
                .user_agent(concat!("komga-rs/", env!("CARGO_PKG_VERSION")))
                .build()?,
        })
    }
}

impl<'client> AsyncHttpClient<'client> for LimitedHttpClient {
    type Error = OAuthHttpError;
    type Future = Pin<Box<dyn Future<Output = Result<HttpResponse, Self::Error>> + Send + 'client>>;

    fn call(&'client self, request: HttpRequest) -> Self::Future {
        Box::pin(async move {
            let scheme = request.uri().scheme_str();
            let host = request.uri().host();
            if scheme != Some("https") && !(scheme == Some("http") && host.is_some_and(loopback_host)) {
                return Err(OAuthHttpError::BlockedUrl);
            }
            let request_size = request.body().len()
                + request.uri().to_string().len()
                + request
                    .headers()
                    .iter()
                    .map(|(name, value)| name.as_str().len() + value.as_bytes().len())
                    .sum::<usize>();
            if request_size > MAX_HTTP_REQUEST_BYTES {
                return Err(OAuthHttpError::RequestTooLarge);
            }
            let response = self.client.execute(request.try_into()?).await?;
            if response
                .content_length()
                .is_some_and(|length| length > MAX_HTTP_RESPONSE_BYTES as u64)
            {
                return Err(OAuthHttpError::ResponseTooLarge);
            }
            let status = response.status();
            let version = response.version();
            let headers = response.headers().clone();
            let mut body = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                if body.len().saturating_add(chunk.len()) > MAX_HTTP_RESPONSE_BYTES {
                    return Err(OAuthHttpError::ResponseTooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            let mut builder = http::Response::builder().status(status).version(version);
            for (name, value) in &headers {
                builder = builder.header(name, value);
            }
            Ok(builder.body(body)?)
        })
    }
}

#[derive(Clone)]
pub(crate) struct OAuthService {
    inner: Option<Arc<OAuthInner>>,
}

struct OAuthInner {
    settings: OAuthSettings,
    providers: BTreeMap<String, OAuthProvider>,
    provider_order: Vec<String>,
    http: LimitedHttpClient,
    transactions: DashMap<String, OAuthTransaction>,
    transaction_lock: Mutex<()>,
}

struct OAuthTransaction {
    registration_id: String,
    pkce_verifier: String,
    nonce: Option<String>,
    browser_binding: String,
    redirect_uri: String,
    created_at: Instant,
}

#[derive(Clone)]
enum OAuthProvider {
    Oidc(Arc<OidcProvider>),
    OAuth2(Arc<OAuth2Provider>),
}

struct OidcProvider {
    common: ResolvedProvider,
    issuer_uri: Url,
    metadata: RwLock<CoreProviderMetadata>,
}

struct OAuth2Provider {
    common: ResolvedProvider,
    authorization_uri: Url,
    token_uri: Url,
    user_info_uri: Url,
    email_attribute: String,
}

struct ResolvedProvider {
    registration_id: String,
    name: String,
    client_id: String,
    client_secret: Option<String>,
    client_authentication_method: ClientAuthenticationMethod,
    scopes: Vec<String>,
}

struct OAuthSettings {
    public_base_url: Option<Url>,
    account_creation: bool,
    oidc_email_verification: bool,
}

impl OAuthService {
    pub(crate) fn disabled() -> Self {
        Self { inner: None }
    }

    pub(crate) async fn load(path: &FilePath, explicit: bool) -> Result<Self, OAuthConfigError> {
        if !path.exists() {
            return if explicit {
                Err(OAuthConfigError::Missing(path.display().to_string()))
            } else {
                Ok(Self::disabled())
            };
        }
        let bytes = tokio::fs::read(path).await?;
        let config: OAuthFileConfig = serde_json::from_slice(&bytes)?;
        Self::from_config(config, |name| std::env::var(name).ok()).await
    }

    async fn from_config(
        config: OAuthFileConfig,
        environment: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, OAuthConfigError> {
        let http = LimitedHttpClient::new()?;
        let settings = OAuthSettings {
            public_base_url: config
                .public_base_url
                .map(|url| validate_url("publicBaseUrl", url, true))
                .transpose()?,
            account_creation: config.account_creation,
            oidc_email_verification: config.oidc_email_verification,
        };
        let mut providers = BTreeMap::new();
        let mut provider_order = Vec::new();
        for provider in config.providers {
            let (registration_id, provider) = match provider {
                ProviderConfig::Oidc(provider) => {
                    let common = resolve_common(provider.common, &environment, true)?;
                    let issuer_uri = validate_url("issuerUri", provider.issuer_uri, false)?;
                    let issuer = IssuerUrl::new(issuer_uri.to_string())
                        .map_err(|error| OAuthConfigError::Invalid(format!("invalid issuerUri: {error}")))?;
                    let metadata = CoreProviderMetadata::discover_async(issuer, &http)
                        .await
                        .map_err(|error| OAuthConfigError::Discovery {
                            provider: common.registration_id.clone(),
                            message: error.to_string(),
                        })?;
                    validate_discovered_urls(&common.registration_id, &metadata)?;
                    (
                        common.registration_id.clone(),
                        OAuthProvider::Oidc(Arc::new(OidcProvider {
                            common,
                            issuer_uri,
                            metadata: RwLock::new(metadata),
                        })),
                    )
                }
                ProviderConfig::Oauth2(provider) => {
                    let common = resolve_common(provider.common, &environment, false)?;
                    if provider.email_attribute.trim().is_empty() || provider.email_attribute.len() > 128 {
                        return Err(OAuthConfigError::Invalid(format!(
                            "provider {} has a blank emailAttribute",
                            common.registration_id
                        )));
                    }
                    let authorization_uri = validate_url("authorizationUri", provider.authorization_uri, false)?;
                    let token_uri = validate_url("tokenUri", provider.token_uri, false)?;
                    let user_info_uri = validate_url("userInfoUri", provider.user_info_uri, false)?;
                    (
                        common.registration_id.clone(),
                        OAuthProvider::OAuth2(Arc::new(OAuth2Provider {
                            common,
                            authorization_uri,
                            token_uri,
                            user_info_uri,
                            email_attribute: provider.email_attribute,
                        })),
                    )
                }
            };
            if providers.insert(registration_id.clone(), provider).is_some() {
                return Err(OAuthConfigError::Invalid(format!(
                    "duplicate provider registrationId: {registration_id}"
                )));
            }
            provider_order.push(registration_id);
        }
        Ok(Self {
            inner: Some(Arc::new(OAuthInner {
                settings,
                providers,
                provider_order,
                http,
                transactions: DashMap::new(),
                transaction_lock: Mutex::new(()),
            })),
        })
    }

    pub(crate) fn providers(&self) -> Vec<OAuthProviderDto> {
        self.inner
            .as_ref()
            .map(|inner| {
                inner
                    .provider_order
                    .iter()
                    .filter_map(|registration_id| inner.providers.get(registration_id))
                    .map(|provider| {
                        let common = provider.common();
                        OAuthProviderDto {
                            name: common.name.clone(),
                            registration_id: common.registration_id.clone(),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

pub(crate) async fn provider_list(State(state): State<crate::AppState>) -> Json<Vec<OAuthProviderDto>> {
    Json(state.oauth.providers())
}

pub(crate) async fn authorize(
    State(state): State<AppState>,
    Path(registration_id): Path<String>,
    request: Request,
) -> Result<Response, ApiError> {
    let inner = state
        .oauth
        .inner
        .as_ref()
        .ok_or_else(|| ApiError::not_found("OAuth provider not found"))?;
    let provider = inner
        .providers
        .get(&registration_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("OAuth provider not found"))?;
    let base_url = public_base_url(inner, request.headers(), request.uri())?;
    let cookie_settings = CookieSettings::from_public_url(&base_url);
    let redirect_uri = url_with_segments(&base_url, &["login", "oauth2", "code", &registration_id]);
    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let (authorization_url, state_token, nonce) = match &provider {
        OAuthProvider::Oidc(provider) => {
            let metadata = provider.metadata.read().await.clone();
            let client = CoreClient::from_provider_metadata(
                metadata,
                OidcClientId::new(provider.common.client_id.clone()),
                provider.common.client_secret.clone().map(OidcClientSecret::new),
            )
            .set_auth_type(provider.common.client_authentication_method.auth_type())
            .set_redirect_uri(
                OidcRedirectUrl::new(redirect_uri.to_string())
                    .map_err(|_| ApiError::internal("OAuth redirect URI is invalid"))?,
            );
            let (url, state, nonce) = client
                .authorize_url(
                    CoreAuthenticationFlow::AuthorizationCode,
                    CsrfToken::new_random,
                    Nonce::new_random,
                )
                .add_scopes(
                    provider
                        .common
                        .scopes
                        .iter()
                        .filter(|scope| scope.as_str() != "openid")
                        .cloned()
                        .map(OidcScope::new),
                )
                .set_pkce_challenge(pkce_challenge)
                .url();
            (url, state.secret().clone(), Some(nonce.secret().clone()))
        }
        OAuthProvider::OAuth2(provider) => {
            let client = oauth2_client(provider, &redirect_uri)?;
            let (url, state) = client
                .authorize_url(CsrfToken::new_random)
                .add_scopes(provider.common.scopes.iter().cloned().map(Scope::new))
                .set_pkce_challenge(pkce_challenge)
                .url();
            (url, state.secret().clone(), None)
        }
    };
    let browser_binding = random_secret();
    insert_transaction(
        inner,
        state_token,
        OAuthTransaction {
            registration_id,
            pkce_verifier: pkce_verifier.secret().clone(),
            nonce,
            browser_binding: browser_binding.clone(),
            redirect_uri: redirect_uri.to_string(),
            created_at: Instant::now(),
        },
    )
    .await?;
    let mut response = Redirect::temporary(authorization_url.as_str()).into_response();
    append_cookie(
        response.headers_mut(),
        &binding_cookie(&browser_binding, &cookie_settings),
    );
    Ok(response)
}

pub(crate) async fn callback(
    State(state): State<AppState>,
    Path(registration_id): Path<String>,
    request: Request,
) -> Response {
    let metadata = authentication_request_metadata(&request);
    let Some(inner) = state.oauth.inner.as_ref() else {
        return ApiError::not_found("OAuth provider not found").into_response();
    };
    let Some(provider) = inner.providers.get(&registration_id).cloned() else {
        return ApiError::not_found("OAuth provider not found").into_response();
    };
    let base_url = match public_base_url(inner, request.headers(), request.uri()) {
        Ok(url) => url,
        Err(error) => return error.into_response(),
    };
    let cookie_settings = CookieSettings::from_public_url(&base_url);
    let query = callback_query(request.uri().query());
    let result = match (query.state.as_deref(), binding_cookie_value(request.headers())) {
        (Some(state_token), Some(binding)) if state_token.len() <= 512 && binding.len() <= 512 => {
            match take_transaction(inner, state_token, &registration_id, &binding) {
                Ok(transaction) => {
                    if query.error.is_some() {
                        Err(CallbackFailure::generic())
                    } else if let Some(code) = query.code {
                        complete_callback(&state, inner, &provider, transaction, code).await
                    } else {
                        Err(CallbackFailure::generic())
                    }
                }
                Err(failure) => Err(failure),
            }
        }
        _ => Err(CallbackFailure::generic()),
    };
    let mut response = match result {
        Ok((user, email)) => {
            record_oauth_activity(
                &state,
                &metadata,
                Some(&user),
                email,
                &provider.common().name,
                true,
                None,
            )
            .await;
            let session_id = state.sessions.create(&user.id);
            let mut response = Redirect::to(success_redirect(&base_url).as_str()).into_response();
            append_cookie(response.headers_mut(), &session_cookie(&session_id, &cookie_settings));
            response
        }
        Err(failure) => {
            record_oauth_activity(
                &state,
                &metadata,
                None,
                failure.email.clone(),
                &provider.common().name,
                false,
                Some(failure.audit_error),
            )
            .await;
            Redirect::to(failure_redirect(&base_url, failure.client_error).as_str()).into_response()
        }
    };
    append_cookie(response.headers_mut(), &expired_binding_cookie(&cookie_settings));
    response
}

async fn complete_callback(
    state: &AppState,
    inner: &OAuthInner,
    provider: &OAuthProvider,
    transaction: OAuthTransaction,
    code: String,
) -> Result<(User, Option<String>), CallbackFailure> {
    if code.is_empty() || code.len() > 4096 {
        return Err(CallbackFailure::generic());
    }
    let email = match provider {
        OAuthProvider::OAuth2(provider) => oauth2_email(inner, provider, &transaction, code).await?,
        OAuthProvider::Oidc(provider) => oidc_email(inner, provider, &transaction, code).await?,
    };
    let user = bind_user(state, &email, inner.settings.account_creation)
        .await
        .map_err(|code| CallbackFailure::code(code, Some(email.clone())))?;
    Ok((user, Some(email)))
}

async fn oauth2_email(
    inner: &OAuthInner,
    provider: &OAuth2Provider,
    transaction: &OAuthTransaction,
    code: String,
) -> Result<String, CallbackFailure> {
    let redirect_uri = Url::parse(&transaction.redirect_uri).map_err(|_| CallbackFailure::generic())?;
    let client = oauth2_client(provider, &redirect_uri).map_err(|_| CallbackFailure::generic())?;
    let token = client
        .exchange_code(AuthorizationCode::new(code))
        .set_pkce_verifier(PkceCodeVerifier::new(transaction.pkce_verifier.clone()))
        .request_async(&inner.http)
        .await
        .map_err(|_| CallbackFailure::generic())?;
    let user_info = get_json(&inner.http, &provider.user_info_uri, token.access_token().secret()).await?;
    let direct = user_info
        .get(&provider.email_attribute)
        .and_then(serde_json::Value::as_str)
        .filter(|email| !email.trim().is_empty())
        .map(str::to_owned);
    if let Some(email) = direct {
        return Ok(email);
    }
    if provider.common.registration_id.eq_ignore_ascii_case("github") {
        let mut emails_uri = provider.user_info_uri.clone();
        emails_uri.set_path(&format!("{}/emails", emails_uri.path().trim_end_matches('/')));
        let emails = get_json(&inner.http, &emails_uri, token.access_token().secret()).await?;
        if let Some(email) = emails.as_array().and_then(|emails| {
            emails.iter().find_map(|entry| {
                (entry.get("primary").and_then(serde_json::Value::as_bool) == Some(true)
                    && entry.get("verified").and_then(serde_json::Value::as_bool) == Some(true))
                .then(|| entry.get("email").and_then(serde_json::Value::as_str))
                .flatten()
                .map(str::to_owned)
            })
        }) {
            return Ok(email);
        }
    }
    Err(CallbackFailure::code("ERR_1024", None))
}

async fn oidc_email(
    inner: &OAuthInner,
    provider: &OidcProvider,
    transaction: &OAuthTransaction,
    code: String,
) -> Result<String, CallbackFailure> {
    let metadata = provider.metadata.read().await.clone();
    let redirect_uri =
        OidcRedirectUrl::new(transaction.redirect_uri.clone()).map_err(|_| CallbackFailure::generic())?;
    let client = CoreClient::from_provider_metadata(
        metadata,
        OidcClientId::new(provider.common.client_id.clone()),
        provider.common.client_secret.clone().map(OidcClientSecret::new),
    )
    .set_auth_type(provider.common.client_authentication_method.auth_type())
    .set_redirect_uri(redirect_uri.clone());
    let token = client
        .exchange_code(openidconnect::AuthorizationCode::new(code))
        .map_err(|_| CallbackFailure::generic())?
        .set_pkce_verifier(openidconnect::PkceCodeVerifier::new(transaction.pkce_verifier.clone()))
        .request_async(&inner.http)
        .await
        .map_err(|_| CallbackFailure::generic())?;
    let id_token = token.id_token().cloned().ok_or_else(CallbackFailure::generic)?;
    let nonce = Nonce::new(transaction.nonce.clone().ok_or_else(CallbackFailure::generic)?);
    let (mut email, mut verified, subject) = match id_token.claims(&client.id_token_verifier(), &nonce) {
        Ok(claims) => (
            claims.email().map(|email| email.as_str().to_owned()),
            claims.email_verified(),
            claims.subject().clone(),
        ),
        Err(error) if unknown_signing_key(&error) => {
            let issuer = IssuerUrl::new(provider.issuer_uri.to_string()).map_err(|_| CallbackFailure::generic())?;
            let refreshed = CoreProviderMetadata::discover_async(issuer, &inner.http)
                .await
                .map_err(|_| CallbackFailure::generic())?;
            validate_discovered_urls(&provider.common.registration_id, &refreshed)
                .map_err(|_| CallbackFailure::generic())?;
            *provider.metadata.write().await = refreshed.clone();
            let refreshed_client = CoreClient::from_provider_metadata(
                refreshed,
                OidcClientId::new(provider.common.client_id.clone()),
                provider.common.client_secret.clone().map(OidcClientSecret::new),
            )
            .set_auth_type(provider.common.client_authentication_method.auth_type())
            .set_redirect_uri(redirect_uri);
            let claims = id_token
                .claims(&refreshed_client.id_token_verifier(), &nonce)
                .map_err(|_| CallbackFailure::generic())?;
            (
                claims.email().map(|email| email.as_str().to_owned()),
                claims.email_verified(),
                claims.subject().clone(),
            )
        }
        Err(_) => return Err(CallbackFailure::generic()),
    };

    let current_metadata = provider.metadata.read().await.clone();
    if current_metadata.userinfo_endpoint().is_some() {
        let user_info_client = CoreClient::from_provider_metadata(
            current_metadata,
            OidcClientId::new(provider.common.client_id.clone()),
            provider.common.client_secret.clone().map(OidcClientSecret::new),
        )
        .set_auth_type(provider.common.client_authentication_method.auth_type())
        .set_redirect_uri(
            OidcRedirectUrl::new(transaction.redirect_uri.clone()).map_err(|_| CallbackFailure::generic())?,
        );
        let user_info: CoreUserInfoClaims = user_info_client
            .user_info(token.access_token().clone(), Some(subject))
            .map_err(|_| CallbackFailure::generic())?
            .request_async(&inner.http)
            .await
            .map_err(|_| CallbackFailure::generic())?;
        email = user_info.email().map(|email| email.as_str().to_owned()).or(email);
        verified = user_info.email_verified().or(verified);
    }
    let email = email.ok_or_else(|| CallbackFailure::code("ERR_1028", None))?;
    if inner.settings.oidc_email_verification {
        match verified {
            None => return Err(CallbackFailure::code("ERR_1027", Some(email))),
            Some(false) => return Err(CallbackFailure::code("ERR_1026", Some(email))),
            Some(true) => {}
        }
    }
    Ok(email)
}

fn unknown_signing_key(error: &ClaimsVerificationError) -> bool {
    matches!(
        error,
        ClaimsVerificationError::SignatureVerification(SignatureVerificationError::NoMatchingKey)
    )
}

async fn get_json(
    client: &LimitedHttpClient,
    uri: &Url,
    access_token: &str,
) -> Result<serde_json::Value, CallbackFailure> {
    let authorization = format!("Bearer {access_token}")
        .parse::<http::HeaderValue>()
        .map_err(|_| CallbackFailure::generic())?;
    let request = http::Request::builder()
        .method(http::Method::GET)
        .uri(uri.as_str())
        .header(header::ACCEPT, "application/json")
        .header(header::AUTHORIZATION, authorization)
        .body(Vec::new())
        .map_err(|_| CallbackFailure::generic())?;
    let response = client.call(request).await.map_err(|_| CallbackFailure::generic())?;
    if !response.status().is_success() {
        return Err(CallbackFailure::generic());
    }
    serde_json::from_slice(response.body()).map_err(|_| CallbackFailure::generic())
}

async fn bind_user(state: &AppState, email: &str, account_creation: bool) -> Result<User, &'static str> {
    if let Some(user) = state
        .storage
        .find_user_by_email(email)
        .await
        .map_err(|_| "OAuth2 authentication failed")?
    {
        return Ok(user);
    }
    if !account_creation {
        return Err("ERR_1025");
    }
    if !valid_email(email) {
        return Err("OAuth2 authentication failed");
    }
    let password = random_secret();
    let password_hash = crate::auth::hash_password(password)
        .await
        .map_err(|_| "OAuth2 authentication failed")?;
    let user = User::new(email.to_owned(), password_hash);
    match state.storage.create_user(&user).await {
        Ok(()) => Ok(user),
        Err(StorageError::DuplicateEmail) => state
            .storage
            .find_user_by_email(email)
            .await
            .map_err(|_| "OAuth2 authentication failed")?
            .ok_or("OAuth2 authentication failed"),
        Err(_) => Err("OAuth2 authentication failed"),
    }
}

fn valid_email(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !email.chars().any(char::is_whitespace)
}

fn oauth2_client(
    provider: &OAuth2Provider,
    redirect_uri: &Url,
) -> Result<
    BasicClient<
        oauth2::EndpointSet,
        oauth2::EndpointNotSet,
        oauth2::EndpointNotSet,
        oauth2::EndpointNotSet,
        oauth2::EndpointSet,
    >,
    ApiError,
> {
    let mut client = BasicClient::new(ClientId::new(provider.common.client_id.clone()))
        .set_auth_uri(
            AuthUrl::new(provider.authorization_uri.to_string())
                .map_err(|_| ApiError::internal("OAuth authorization URI is invalid"))?,
        )
        .set_token_uri(
            TokenUrl::new(provider.token_uri.to_string())
                .map_err(|_| ApiError::internal("OAuth token URI is invalid"))?,
        )
        .set_redirect_uri(
            RedirectUrl::new(redirect_uri.to_string())
                .map_err(|_| ApiError::internal("OAuth redirect URI is invalid"))?,
        )
        .set_auth_type(provider.common.client_authentication_method.auth_type());
    if let Some(secret) = &provider.common.client_secret {
        client = client.set_client_secret(ClientSecret::new(secret.clone()));
    }
    Ok(client)
}

async fn insert_transaction(inner: &OAuthInner, state: String, transaction: OAuthTransaction) -> Result<(), ApiError> {
    let _guard = inner.transaction_lock.lock().await;
    inner
        .transactions
        .retain(|_, transaction| transaction.created_at.elapsed() < TRANSACTION_TTL);
    if inner.transactions.len() >= MAX_TRANSACTIONS {
        return Err(ApiError::service_unavailable("Too many OAuth authorization requests"));
    }
    inner.transactions.insert(state, transaction);
    Ok(())
}

fn take_transaction(
    inner: &OAuthInner,
    state: &str,
    registration_id: &str,
    binding: &str,
) -> Result<OAuthTransaction, CallbackFailure> {
    let (_, transaction) = inner.transactions.remove(state).ok_or_else(CallbackFailure::generic)?;
    if transaction.created_at.elapsed() >= TRANSACTION_TTL
        || transaction.registration_id != registration_id
        || !secure_equal(&transaction.browser_binding, binding)
    {
        return Err(CallbackFailure::generic());
    }
    Ok(transaction)
}

fn secure_equal(left: &str, right: &str) -> bool {
    Sha256::digest(left.as_bytes()) == Sha256::digest(right.as_bytes())
}

fn random_secret() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn binding_cookie(value: &str, settings: &CookieSettings) -> String {
    format!("{OAUTH_BINDING_COOKIE}={value}; Max-Age=600; {}", settings.attributes())
}

fn expired_binding_cookie(settings: &CookieSettings) -> String {
    format!("{OAUTH_BINDING_COOKIE}=; Max-Age=0; {}", settings.attributes())
}

fn binding_cookie_value(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|cookie| cookie.trim().split_once('='))
        .find_map(|(name, value)| (name == OAUTH_BINDING_COOKIE).then(|| value.to_owned()))
}

#[derive(Default)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

fn callback_query(query: Option<&str>) -> CallbackQuery {
    let mut result = CallbackQuery::default();
    for (name, value) in url::form_urlencoded::parse(query.unwrap_or_default().as_bytes()) {
        match name.as_ref() {
            "code" if result.code.is_none() => result.code = Some(value.into_owned()),
            "state" if result.state.is_none() => result.state = Some(value.into_owned()),
            "error" if result.error.is_none() => result.error = Some(value.into_owned()),
            _ => {}
        }
    }
    result
}

struct CallbackFailure {
    client_error: &'static str,
    audit_error: &'static str,
    email: Option<String>,
}

impl CallbackFailure {
    fn generic() -> Self {
        Self::code("OAuth2 authentication failed", None)
    }

    fn code(code: &'static str, email: Option<String>) -> Self {
        Self {
            client_error: code,
            audit_error: code,
            email,
        }
    }
}

fn public_base_url(inner: &OAuthInner, headers: &HeaderMap, uri: &http::Uri) -> Result<Url, ApiError> {
    let mut url = if let Some(url) = &inner.settings.public_base_url {
        url.clone()
    } else {
        Url::parse(&ManifestUrls::from_request_with_prefix(headers, uri, &[])?.absolute_path(&[]))
            .map_err(|_| ApiError::bad_request("Invalid public request URL"))?
    };
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

fn url_with_segments(base: &Url, segments: &[&str]) -> Url {
    let mut url = base.clone();
    let mut path = url.path_segments_mut().expect("HTTP URLs support path segments");
    path.pop_if_empty();
    for segment in segments {
        path.push(segment);
    }
    drop(path);
    url
}

fn success_redirect(base: &Url) -> Url {
    let mut url = base.clone();
    url.query_pairs_mut().append_pair("server_redirect", "Y");
    url
}

fn failure_redirect(base: &Url, error: &str) -> Url {
    let mut url = url_with_segments(base, &["login"]);
    url.query_pairs_mut()
        .append_pair("server_redirect", "Y")
        .append_pair("error", error);
    url
}

impl OAuthProvider {
    fn common(&self) -> &ResolvedProvider {
        match self {
            Self::Oidc(provider) => &provider.common,
            Self::OAuth2(provider) => &provider.common,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OAuthProviderDto {
    name: String,
    registration_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OAuthFileConfig {
    public_base_url: Option<Url>,
    #[serde(default)]
    account_creation: bool,
    #[serde(default = "default_true")]
    oidc_email_verification: bool,
    #[serde(default)]
    providers: Vec<ProviderConfig>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ProviderConfig {
    Oidc(OidcProviderConfig),
    Oauth2(OAuth2ProviderConfig),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OidcProviderConfig {
    #[serde(flatten)]
    common: CommonProviderConfig,
    issuer_uri: Url,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OAuth2ProviderConfig {
    #[serde(flatten)]
    common: CommonProviderConfig,
    authorization_uri: Url,
    token_uri: Url,
    user_info_uri: Url,
    email_attribute: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CommonProviderConfig {
    registration_id: String,
    name: String,
    client_id: String,
    client_secret: Option<String>,
    client_secret_env: Option<String>,
    client_authentication_method: ClientAuthenticationMethod,
    scopes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ClientAuthenticationMethod {
    ClientSecretBasic,
    ClientSecretPost,
    None,
}

impl ClientAuthenticationMethod {
    fn auth_type(self) -> AuthType {
        match self {
            Self::ClientSecretBasic => AuthType::BasicAuth,
            Self::ClientSecretPost | Self::None => AuthType::RequestBody,
        }
    }
}

fn resolve_common(
    common: CommonProviderConfig,
    environment: &impl Fn(&str) -> Option<String>,
    oidc: bool,
) -> Result<ResolvedProvider, OAuthConfigError> {
    let id = common.registration_id.trim();
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~'))
    {
        return Err(OAuthConfigError::Invalid(format!(
            "invalid provider registrationId: {}",
            common.registration_id
        )));
    }
    if common.name.trim().is_empty()
        || common.name.len() > 128
        || common.client_id.trim().is_empty()
        || common.client_id.len() > 1024
    {
        return Err(OAuthConfigError::Invalid(format!(
            "provider {id} must have a valid name and clientId"
        )));
    }
    if common.client_secret.is_some() && common.client_secret_env.is_some() {
        return Err(OAuthConfigError::Invalid(format!(
            "provider {id} must not set both clientSecret and clientSecretEnv"
        )));
    }
    let secret =
        if let Some(secret) = common.client_secret {
            Some(secret)
        } else if let Some(name) = common.client_secret_env {
            if name.trim().is_empty() {
                return Err(OAuthConfigError::Invalid(format!(
                    "provider {id} has a blank clientSecretEnv"
                )));
            }
            Some(environment(&name).ok_or_else(|| {
                OAuthConfigError::Invalid(format!("provider {id} environment secret {name} is missing"))
            })?)
        } else {
            None
        };
    if secret
        .as_ref()
        .is_some_and(|secret| secret.trim().is_empty() || secret.len() > 4096)
    {
        return Err(OAuthConfigError::Invalid(format!(
            "provider {id} has an invalid client secret"
        )));
    }
    match (secret.is_some(), common.client_authentication_method) {
        (false, ClientAuthenticationMethod::None) => {}
        (false, _) => {
            return Err(OAuthConfigError::Invalid(format!(
                "provider {id} requires a client secret"
            )));
        }
        (true, ClientAuthenticationMethod::None) => {
            return Err(OAuthConfigError::Invalid(format!(
                "provider {id} must not set a secret with clientAuthenticationMethod none"
            )));
        }
        (true, _) => {}
    }
    let scopes = common
        .scopes
        .into_iter()
        .map(|scope| scope.trim().to_owned())
        .filter(|scope| !scope.is_empty())
        .collect::<Vec<_>>();
    if scopes.is_empty()
        || scopes.len() > 64
        || scopes.iter().any(|scope| scope.len() > 256)
        || (oidc && !scopes.iter().any(|scope| scope == "openid"))
    {
        return Err(OAuthConfigError::Invalid(format!(
            "provider {id} must configure {}scopes",
            if oidc { "an openid scope and other " } else { "" }
        )));
    }
    if scopes.iter().collect::<BTreeSet<_>>().len() != scopes.len() {
        return Err(OAuthConfigError::Invalid(format!(
            "provider {id} contains duplicate scopes"
        )));
    }
    Ok(ResolvedProvider {
        registration_id: id.to_owned(),
        name: common.name.trim().to_owned(),
        client_id: common.client_id.trim().to_owned(),
        client_secret: secret,
        client_authentication_method: common.client_authentication_method,
        scopes,
    })
}

fn validate_url(field: &str, url: Url, base: bool) -> Result<Url, OAuthConfigError> {
    if url.as_str().len() > 4096
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || (base && url.query().is_some())
    {
        return Err(OAuthConfigError::Invalid(format!("invalid {field}: {url}")));
    }
    let secure = url.scheme() == "https";
    let loopback_http = url.scheme() == "http" && url.host_str().is_some_and(loopback_host);
    if !secure && !loopback_http {
        return Err(OAuthConfigError::Invalid(format!(
            "{field} must use HTTPS unless it targets loopback"
        )));
    }
    Ok(url)
}

fn loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn validate_discovered_urls(provider: &str, metadata: &CoreProviderMetadata) -> Result<(), OAuthConfigError> {
    for (field, value) in [
        ("issuer", metadata.issuer().as_str()),
        ("authorization_endpoint", metadata.authorization_endpoint().as_str()),
        ("jwks_uri", metadata.jwks_uri().as_str()),
    ] {
        let url = Url::parse(value)
            .map_err(|error| OAuthConfigError::Invalid(format!("provider {provider} has invalid {field}: {error}")))?;
        validate_url(field, url, false)?;
    }
    let token_endpoint = metadata.token_endpoint().ok_or_else(|| {
        OAuthConfigError::Invalid(format!("provider {provider} discovery metadata has no token_endpoint"))
    })?;
    validate_url(
        "token_endpoint",
        Url::parse(token_endpoint.as_str()).map_err(|error| {
            OAuthConfigError::Invalid(format!("provider {provider} has invalid token_endpoint: {error}"))
        })?,
        false,
    )?;
    if let Some(endpoint) = metadata.userinfo_endpoint() {
        validate_url(
            "userinfo_endpoint",
            Url::parse(endpoint.as_str()).map_err(|error| {
                OAuthConfigError::Invalid(format!("provider {provider} has invalid userinfo_endpoint: {error}"))
            })?,
            false,
        )?;
    }
    Ok(())
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        net::SocketAddr,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use axum::{
        Form, Router,
        body::Body,
        extract::{ConnectInfo, State},
        http::{Request, StatusCode, header},
        routing::{get, post},
    };
    use chrono::{Duration as ChronoDuration, Utc};
    use komga_core::PageRequest;
    use komga_storage::{Storage, TaskStorage};
    use openidconnect::{
        AccessToken, Audience, EmptyAdditionalClaims, EndUserEmail, JsonWebKeyId, PrivateSigningKey, StandardClaims,
        SubjectIdentifier,
        core::{CoreIdToken, CoreIdTokenClaims, CoreJsonWebKeySet, CoreJwsSigningAlgorithm, CoreRsaPrivateSigningKey},
    };
    use tokio::{net::TcpListener, task::JoinHandle};
    use tower::ServiceExt;

    use super::*;

    #[derive(Clone, Default)]
    struct FakeOAuthState {
        token_requests: Arc<Mutex<Vec<HashMap<String, String>>>>,
        authorization_headers: Arc<Mutex<Vec<String>>>,
        user_info: Arc<Mutex<serde_json::Value>>,
    }

    #[derive(Clone)]
    struct FakeOidcState {
        issuer: String,
        signing_key: Arc<RwLock<Arc<CoreRsaPrivateSigningKey>>>,
        claims: Arc<Mutex<FakeOidcClaims>>,
        discovery_requests: Arc<AtomicUsize>,
    }

    #[derive(Clone)]
    struct FakeOidcClaims {
        email: Option<String>,
        email_verified: Option<bool>,
        nonce: String,
    }

    const TEST_RSA_PRIVATE_KEY: &str = "-----BEGIN RSA PRIVATE KEY-----\n\
         MIIEowIBAAKCAQEAn4EPtAOCc9AlkeQHPzHStgAbgs7bTZLwUBZdR8/KuKPEHLd4\n\
         rHVTeT+O+XV2jRojdNhxJWTDvNd7nqQ0VEiZQHz/AJmSCpMaJMRBSFKrKb2wqVwG\n\
         U/NsYOYL+QtiWN2lbzcEe6XC0dApr5ydQLrHqkHHig3RBordaZ6Aj+oBHqFEHYpP\n\
         e7Tpe+OfVfHd1E6cS6M1FZcD1NNLYD5lFHpPI9bTwJlsde3uhGqC0ZCuEHg8lhzw\n\
         OHrtIQbS0FVbb9k3+tVTU4fg/3L/vniUFAKwuCLqKnS2BYwdq/mzSnbLY7h/qixo\n\
         R7jig3//kRhuaxwUkRz5iaiQkqgc5gHdrNP5zwIDAQABAoIBAG1lAvQfhBUSKPJK\n\
         Rn4dGbshj7zDSr2FjbQf4pIh/ZNtHk/jtavyO/HomZKV8V0NFExLNi7DUUvvLiW7\n\
         0PgNYq5MDEjJCtSd10xoHa4QpLvYEZXWO7DQPwCmRofkOutf+NqyDS0QnvFvp2d+\n\
         Lov6jn5C5yvUFgw6qWiLAPmzMFlkgxbtjFAWMJB0zBMy2BqjntOJ6KnqtYRMQUxw\n\
         TgXZDF4rhYVKtQVOpfg6hIlsaoPNrF7dofizJ099OOgDmCaEYqM++bUlEHxgrIVk\n\
         wZz+bg43dfJCocr9O5YX0iXaz3TOT5cpdtYbBX+C/5hwrqBWru4HbD3xz8cY1TnD\n\
         qQa0M8ECgYEA3Slxg/DwTXJcb6095RoXygQCAZ5RnAvZlno1yhHtnUex/fp7AZ/9\n\
         nRaO7HX/+SFfGQeutao2TDjDAWU4Vupk8rw9JR0AzZ0N2fvuIAmr/WCsmGpeNqQn\n\
         ev1T7IyEsnh8UMt+n5CafhkikzhEsrmndH6LxOrvRJlsPp6Zv8bUq0kCgYEAuKE2\n\
         dh+cTf6ERF4k4e/jy78GfPYUIaUyoSSJuBzp3Cubk3OCqs6grT8bR/cu0Dm1MZwW\n\
         mtdqDyI95HrUeq3MP15vMMON8lHTeZu2lmKvwqW7anV5UzhM1iZ7z4yMkuUwFWoB\n\
         vyY898EXvRD+hdqRxHlSqAZ192zB3pVFJ0s7pFcCgYAHw9W9eS8muPYv4ZhDu/fL\n\
         2vorDmD1JqFcHCxZTOnX1NWWAj5hXzmrU0hvWvFC0P4ixddHf5Nqd6+5E9G3k4E5\n\
         2IwZCnylu3bqCWNh8pT8T3Gf5FQsfPT5530T2BcsoPhUaeCnP499D+rb2mTnFYeg\n\
         mnTT1B/Ue8KGLFFfn16GKQKBgAiw5gxnbocpXPaO6/OKxFFZ+6c0OjxfN2PogWce\n\
         TU/k6ZzmShdaRKwDFXisxRJeNQ5Rx6qgS0jNFtbDhW8E8WFmQ5urCOqIOYk28EBi\n\
         At4JySm4v+5P7yYBh8B8YD2l9j57z/s8hJAxEbn/q8uHP2ddQqvQKgtsni+pHSk9\n\
         XGBfAoGBANz4qr10DdM8DHhPrAb2YItvPVz/VwkBd1Vqj8zCpyIEKe/07oKOvjWQ\n\
         SgkLDH9x2hBgY01SbP43CvPk0V72invu2TGkI/FXwXWJLLG7tDSgw4YyfhrYrHmg\n\
         1Vre3XB9HH8MYBVB6UIexaAq4xSeoemRKTBesZro7OKjKT8/GmiO\n\
         -----END RSA PRIVATE KEY-----";

    async fn fake_token(
        State(state): State<FakeOAuthState>,
        Form(parameters): Form<HashMap<String, String>>,
    ) -> Json<serde_json::Value> {
        state.token_requests.lock().await.push(parameters);
        Json(serde_json::json!({
            "access_token": "fake-access-token",
            "token_type": "Bearer"
        }))
    }

    async fn fake_user_info(State(state): State<FakeOAuthState>, headers: HeaderMap) -> Json<serde_json::Value> {
        state.authorization_headers.lock().await.push(
            headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned(),
        );
        Json(state.user_info.lock().await.clone())
    }

    async fn fake_github_emails() -> Json<serde_json::Value> {
        Json(serde_json::json!([
            {"email": "unverified@example.org", "primary": true, "verified": false},
            {"email": "new-github@example.org", "primary": true, "verified": true}
        ]))
    }

    async fn fake_oauth_provider() -> (Url, FakeOAuthState, JoinHandle<()>) {
        let state = FakeOAuthState::default();
        *state.user_info.lock().await = serde_json::json!({"email": "READER@example.org"});
        let app = Router::new()
            .route("/token", post(fake_token))
            .route("/user", get(fake_user_info))
            .route("/user/emails", get(fake_github_emails))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (base, state, task)
    }

    async fn fake_oidc_discovery(State(state): State<FakeOidcState>) -> Json<serde_json::Value> {
        state.discovery_requests.fetch_add(1, Ordering::Relaxed);
        let base = Url::parse(&state.issuer).unwrap();
        Json(serde_json::json!({
            "issuer": state.issuer,
            "authorization_endpoint": base.join("authorize").unwrap(),
            "token_endpoint": base.join("token").unwrap(),
            "jwks_uri": base.join("jwks").unwrap(),
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"],
            "scopes_supported": ["openid", "profile", "email"],
            "token_endpoint_auth_methods_supported": ["none"]
        }))
    }

    async fn fake_oidc_jwks(State(state): State<FakeOidcState>) -> Json<CoreJsonWebKeySet> {
        Json(CoreJsonWebKeySet::new(vec![
            state.signing_key.read().await.as_verification_key(),
        ]))
    }

    async fn fake_oidc_token(State(state): State<FakeOidcState>) -> Json<serde_json::Value> {
        let claims = state.claims.lock().await.clone();
        let standard_claims = StandardClaims::new(SubjectIdentifier::new("subject-1".into()))
            .set_email(claims.email.map(EndUserEmail::new))
            .set_email_verified(claims.email_verified);
        let access_token = AccessToken::new("oidc-access-token".into());
        let signing_key = state.signing_key.read().await;
        let id_token = CoreIdToken::new(
            CoreIdTokenClaims::new(
                IssuerUrl::new(state.issuer.clone()).unwrap(),
                vec![Audience::new("oidc-client".into())],
                Utc::now() + ChronoDuration::minutes(5),
                Utc::now(),
                standard_claims,
                EmptyAdditionalClaims {},
            )
            .set_nonce(Some(Nonce::new(claims.nonce))),
            signing_key.as_ref(),
            CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
            Some(&access_token),
            None,
        )
        .unwrap();
        Json(serde_json::json!({
            "access_token": access_token.secret(),
            "token_type": "Bearer",
            "id_token": id_token.to_string()
        }))
    }

    async fn fake_oidc_provider() -> (Url, FakeOidcState, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}/", listener.local_addr().unwrap());
        let signing_key =
            CoreRsaPrivateSigningKey::from_pem(TEST_RSA_PRIVATE_KEY, Some(JsonWebKeyId::new("test-key".into())))
                .unwrap();
        let state = FakeOidcState {
            issuer: issuer.clone(),
            signing_key: Arc::new(RwLock::new(Arc::new(signing_key))),
            claims: Arc::new(Mutex::new(FakeOidcClaims {
                email: Some("reader@example.org".into()),
                email_verified: Some(true),
                nonce: String::new(),
            })),
            discovery_requests: Arc::new(AtomicUsize::new(0)),
        };
        let app = Router::new()
            .route("/.well-known/openid-configuration", get(fake_oidc_discovery))
            .route("/jwks", get(fake_oidc_jwks))
            .route("/token", post(fake_oidc_token))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (Url::parse(&issuer).unwrap(), state, task)
    }

    fn ordinary_config(base: &Url, account_creation: bool) -> OAuthFileConfig {
        parse(serde_json::json!({
            "accountCreation": account_creation,
            "providers": [{
                "type": "oauth2",
                "registrationId": "test",
                "name": "Test Provider",
                "clientId": "client",
                "clientAuthenticationMethod": "none",
                "authorizationUri": base.join("authorize").unwrap(),
                "tokenUri": base.join("token").unwrap(),
                "userInfoUri": base.join("user").unwrap(),
                "emailAttribute": "email",
                "scopes": ["profile", "email"]
            }]
        }))
        .unwrap()
    }

    fn github_config(base: &Url) -> OAuthFileConfig {
        parse(serde_json::json!({
            "accountCreation": true,
            "providers": [{
                "type": "oauth2",
                "registrationId": "github",
                "name": "GitHub",
                "clientId": "client",
                "clientAuthenticationMethod": "none",
                "authorizationUri": base.join("authorize").unwrap(),
                "tokenUri": base.join("token").unwrap(),
                "userInfoUri": base.join("user").unwrap(),
                "emailAttribute": "email",
                "scopes": ["read:user", "user:email"]
            }]
        }))
        .unwrap()
    }

    fn oidc_config(issuer: &Url) -> OAuthFileConfig {
        parse(serde_json::json!({
            "providers": [{
                "type": "oidc",
                "registrationId": "oidc",
                "name": "OIDC Provider",
                "clientId": "oidc-client",
                "clientAuthenticationMethod": "none",
                "issuerUri": issuer,
                "scopes": ["openid", "profile", "email"]
            }]
        }))
        .unwrap()
    }

    fn proxy_request(uri: &str) -> http::request::Builder {
        Request::get(uri)
            .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 41_000))))
            .header(header::HOST, "internal.example:25600")
            .header("forwarded", "for=192.0.2.10;proto=https;host=reader.example")
            .header("x-forwarded-prefix", "/komga")
            .header(header::USER_AGENT, "oauth-test-agent")
    }

    fn response_cookie(response: &Response, name: &str) -> Option<String> {
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.starts_with(&format!("{name}=")))
            .and_then(|value| value.split(';').next())
            .map(str::to_owned)
    }

    fn response_set_cookie<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.starts_with(&format!("{name}=")))
    }

    async fn oidc_callback(
        app: &Router,
        fake: &FakeOidcState,
        email: Option<&str>,
        verified: Option<bool>,
        valid_nonce: bool,
    ) -> Response {
        let authorization = app
            .clone()
            .oneshot(proxy_request("/oauth2/authorization/oidc").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let binding = response_cookie(&authorization, OAUTH_BINDING_COOKIE).unwrap();
        let authorization_url =
            Url::parse(authorization.headers().get(header::LOCATION).unwrap().to_str().unwrap()).unwrap();
        let parameters = authorization_url.query_pairs().into_owned().collect::<HashMap<_, _>>();
        let nonce = parameters.get("nonce").unwrap();
        *fake.claims.lock().await = FakeOidcClaims {
            email: email.map(str::to_owned),
            email_verified: verified,
            nonce: if valid_nonce {
                nonce.clone()
            } else {
                "wrong-nonce".into()
            },
        };
        let callback_uri = format!(
            "/login/oauth2/code/oidc?code=valid-code&state={}",
            parameters.get("state").unwrap()
        );
        app.clone()
            .oneshot(
                proxy_request(&callback_uri)
                    .header(header::COOKIE, binding)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    fn parse(value: serde_json::Value) -> Result<OAuthFileConfig, serde_json::Error> {
        serde_json::from_value(value)
    }

    #[test]
    fn configuration_rejects_unknown_fields_including_provider_fields() {
        assert!(parse(serde_json::json!({"unknown": true})).is_err());
        assert!(
            parse(serde_json::json!({
                "providers": [{
                    "type": "oauth2",
                    "registrationId": "test",
                    "name": "Test",
                    "clientId": "client",
                    "clientAuthenticationMethod": "none",
                    "authorizationUri": "http://localhost/authorize",
                    "tokenUri": "http://localhost/token",
                    "userInfoUri": "http://localhost/user",
                    "emailAttribute": "email",
                    "scopes": ["profile"],
                    "unknown": true
                }]
            }))
            .is_err()
        );
    }

    #[test]
    fn documented_oauth2_configuration_deserializes() {
        let config = parse(serde_json::json!({
            "publicBaseUrl": "https://reader.example/komga",
            "accountCreation": false,
            "oidcEmailVerification": true,
            "providers": [{
                "type": "oauth2",
                "registrationId": "github",
                "name": "GitHub",
                "clientId": "client-id",
                "clientSecret": "secret",
                "clientAuthenticationMethod": "client_secret_basic",
                "authorizationUri": "https://github.com/login/oauth/authorize",
                "tokenUri": "https://github.com/login/oauth/access_token",
                "userInfoUri": "https://api.github.com/user",
                "emailAttribute": "email",
                "scopes": ["read:user", "user:email"]
            }]
        }))
        .unwrap();

        assert_eq!(config.providers.len(), 1);
    }

    #[test]
    fn common_configuration_enforces_secret_and_transport_rules() {
        let common = CommonProviderConfig {
            registration_id: "test".into(),
            name: "Test".into(),
            client_id: "client".into(),
            client_secret: None,
            client_secret_env: None,
            client_authentication_method: ClientAuthenticationMethod::ClientSecretBasic,
            scopes: vec!["profile".into()],
        };
        assert!(resolve_common(common, &|_| None, false).is_err());
        assert!(validate_url("url", Url::parse("http://example.org/path").unwrap(), false).is_err());
        assert!(validate_url("url", Url::parse("http://127.0.0.1/path").unwrap(), false).is_ok());
        assert!(validate_url("url", Url::parse("https://example.org/path").unwrap(), false).is_ok());

        let environment_secret = CommonProviderConfig {
            registration_id: "environment".into(),
            name: "Environment".into(),
            client_id: "client".into(),
            client_secret: None,
            client_secret_env: Some("OAUTH_SECRET".into()),
            client_authentication_method: ClientAuthenticationMethod::ClientSecretBasic,
            scopes: vec!["profile".into()],
        };
        assert!(resolve_common(environment_secret, &|_| Some("  ".into()), false).is_err());
    }

    #[tokio::test]
    async fn configuration_rejects_duplicate_providers_and_explicit_missing_file() {
        let config = parse(serde_json::json!({
            "providers": [
                {
                    "type": "oauth2",
                    "registrationId": "duplicate",
                    "name": "First",
                    "clientId": "client",
                    "clientAuthenticationMethod": "none",
                    "authorizationUri": "http://localhost/authorize",
                    "tokenUri": "http://localhost/token",
                    "userInfoUri": "http://localhost/user",
                    "emailAttribute": "email",
                    "scopes": ["profile"]
                },
                {
                    "type": "oauth2",
                    "registrationId": "duplicate",
                    "name": "Second",
                    "clientId": "client",
                    "clientAuthenticationMethod": "none",
                    "authorizationUri": "http://localhost/authorize",
                    "tokenUri": "http://localhost/token",
                    "userInfoUri": "http://localhost/user",
                    "emailAttribute": "email",
                    "scopes": ["email"]
                }
            ]
        }))
        .unwrap();
        assert!(OAuthService::from_config(config, |_| None).await.is_err());

        let temporary = tempfile::tempdir().unwrap();
        let missing = temporary.path().join("missing.json");
        assert!(OAuthService::load(&missing, false).await.unwrap().inner.is_none());
        assert!(matches!(
            OAuthService::load(&missing, true).await,
            Err(OAuthConfigError::Missing(_))
        ));
    }

    #[tokio::test]
    async fn oauth2_authorization_code_flow_uses_pkce_proxy_urls_and_single_use_state() {
        let (provider_base, fake, fake_task) = fake_oauth_provider().await;
        let storage = Storage::connect_memory().await.unwrap();
        let user = User::new("reader@example.org".into(), bcrypt::hash("unused", 4).unwrap());
        storage.create_user(&user).await.unwrap();
        let oauth = OAuthService::from_config(ordinary_config(&provider_base, false), |_| None)
            .await
            .unwrap();
        let mut state = AppState::new(storage.clone(), TaskStorage::connect_memory().await.unwrap())
            .await
            .unwrap()
            .with_trusted_proxies(vec!["127.0.0.0/8".parse().unwrap()]);
        state.oauth = oauth;
        let app = crate::build_app(state);

        let providers = app
            .clone()
            .oneshot(Request::get("/api/v1/oauth2/providers").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(providers.status(), StatusCode::OK);

        let authorization = app
            .clone()
            .oneshot(proxy_request("/oauth2/authorization/test").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(authorization.status(), StatusCode::TEMPORARY_REDIRECT);
        let binding_header = response_set_cookie(&authorization, OAUTH_BINDING_COOKIE).unwrap();
        assert!(binding_header.contains("Path=/komga"));
        assert!(binding_header.contains("Secure"));
        let binding = response_cookie(&authorization, OAUTH_BINDING_COOKIE).unwrap();
        let authorization_url =
            Url::parse(authorization.headers().get(header::LOCATION).unwrap().to_str().unwrap()).unwrap();
        let parameters = authorization_url.query_pairs().into_owned().collect::<HashMap<_, _>>();
        let state_token = parameters.get("state").unwrap();
        let challenge = parameters.get("code_challenge").unwrap();
        assert_eq!(
            parameters.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(
            parameters.get("redirect_uri").map(String::as_str),
            Some("https://reader.example/komga/login/oauth2/code/test")
        );

        let callback_uri = format!("/login/oauth2/code/test?code=valid-code&state={state_token}");
        let callback = app
            .clone()
            .oneshot(
                proxy_request(&callback_uri)
                    .header(header::COOKIE, &binding)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(callback.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            callback.headers().get(header::LOCATION).unwrap(),
            "https://reader.example/komga/?server_redirect=Y"
        );
        let session_header = response_set_cookie(&callback, crate::SESSION_COOKIE).unwrap();
        assert!(session_header.contains("Path=/komga"));
        assert!(session_header.contains("Secure"));
        let expired_binding = response_set_cookie(&callback, OAUTH_BINDING_COOKIE).unwrap();
        assert!(expired_binding.contains("Max-Age=0"));
        assert!(expired_binding.contains("Path=/komga"));
        assert!(expired_binding.contains("Secure"));

        let token_requests = fake.token_requests.lock().await;
        assert_eq!(token_requests.len(), 1);
        let verifier = token_requests[0].get("code_verifier").unwrap();
        assert_eq!(URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())), *challenge);
        assert_eq!(
            token_requests[0].get("redirect_uri").map(String::as_str),
            Some("https://reader.example/komga/login/oauth2/code/test")
        );
        drop(token_requests);
        assert_eq!(
            fake.authorization_headers.lock().await.as_slice(),
            ["Bearer fake-access-token"]
        );

        let activity = storage
            .list_authentication_activity(Some(&user), &PageRequest::default())
            .await
            .unwrap();
        assert_eq!(activity.content.len(), 1);
        assert!(activity.content[0].success);
        assert_eq!(activity.content[0].source.as_deref(), Some("OAuth2:Test Provider"));
        assert_eq!(activity.content[0].ip.as_deref(), Some("192.0.2.10"));

        let replay = app
            .oneshot(
                proxy_request(&callback_uri)
                    .header(header::COOKIE, binding)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(replay.status(), StatusCode::SEE_OTHER);
        assert!(
            replay
                .headers()
                .get(header::LOCATION)
                .unwrap()
                .to_str()
                .unwrap()
                .contains("error=OAuth2+authentication+failed")
        );
        assert!(response_cookie(&replay, crate::SESSION_COOKIE).is_none());
        assert_eq!(fake.token_requests.lock().await.len(), 1);
        fake_task.abort();
    }

    #[tokio::test]
    async fn browser_binding_prevents_cross_browser_state_replay() {
        let (provider_base, fake, fake_task) = fake_oauth_provider().await;
        let oauth = OAuthService::from_config(ordinary_config(&provider_base, false), |_| None)
            .await
            .unwrap();
        let mut state = AppState::new(
            Storage::connect_memory().await.unwrap(),
            TaskStorage::connect_memory().await.unwrap(),
        )
        .await
        .unwrap()
        .with_trusted_proxies(vec!["127.0.0.0/8".parse().unwrap()]);
        state.oauth = oauth;
        let app = crate::build_app(state);

        let authorization = app
            .clone()
            .oneshot(proxy_request("/oauth2/authorization/test").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let valid_binding = response_cookie(&authorization, OAUTH_BINDING_COOKIE).unwrap();
        let authorization_url =
            Url::parse(authorization.headers().get(header::LOCATION).unwrap().to_str().unwrap()).unwrap();
        let state_token = authorization_url
            .query_pairs()
            .find_map(|(name, value)| (name == "state").then(|| value.into_owned()))
            .unwrap();
        let callback_uri = format!("/login/oauth2/code/test?code=valid-code&state={state_token}");

        let wrong_browser = app
            .clone()
            .oneshot(
                proxy_request(&callback_uri)
                    .header(header::COOKIE, format!("{OAUTH_BINDING_COOKIE}=wrong-browser"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response_cookie(&wrong_browser, crate::SESSION_COOKIE).is_none());

        let replay = app
            .oneshot(
                proxy_request(&callback_uri)
                    .header(header::COOKIE, valid_binding)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response_cookie(&replay, crate::SESSION_COOKIE).is_none());
        assert!(fake.token_requests.lock().await.is_empty());
        fake_task.abort();
    }

    #[tokio::test]
    async fn github_uses_primary_verified_email_fallback_for_account_creation() {
        let (provider_base, fake, fake_task) = fake_oauth_provider().await;
        *fake.user_info.lock().await = serde_json::json!({"email": null});
        let storage = Storage::connect_memory().await.unwrap();
        let oauth = OAuthService::from_config(github_config(&provider_base), |_| None)
            .await
            .unwrap();
        let mut state = AppState::new(storage.clone(), TaskStorage::connect_memory().await.unwrap())
            .await
            .unwrap()
            .with_trusted_proxies(vec!["127.0.0.0/8".parse().unwrap()]);
        state.oauth = oauth;
        let app = crate::build_app(state);

        let authorization = app
            .clone()
            .oneshot(
                proxy_request("/oauth2/authorization/github")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let binding = response_cookie(&authorization, OAUTH_BINDING_COOKIE).unwrap();
        let authorization_url =
            Url::parse(authorization.headers().get(header::LOCATION).unwrap().to_str().unwrap()).unwrap();
        let state_token = authorization_url
            .query_pairs()
            .find_map(|(name, value)| (name == "state").then(|| value.into_owned()))
            .unwrap();
        let callback = app
            .oneshot(
                proxy_request(&format!(
                    "/login/oauth2/code/github?code=valid-code&state={state_token}"
                ))
                .header(header::COOKIE, binding)
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(callback.status(), StatusCode::SEE_OTHER);
        assert!(response_cookie(&callback, crate::SESSION_COOKIE).is_some());
        let user = storage
            .find_user_by_email("NEW-GITHUB@example.org")
            .await
            .unwrap()
            .unwrap();
        assert!(!user.is_admin());
        fake_task.abort();
    }

    #[tokio::test]
    async fn oidc_flow_validates_signed_claims_nonce_and_email_policy() {
        let (issuer, fake, fake_task) = fake_oidc_provider().await;
        let storage = Storage::connect_memory().await.unwrap();
        let user = User::new("reader@example.org".into(), bcrypt::hash("unused", 4).unwrap());
        storage.create_user(&user).await.unwrap();
        let oauth = OAuthService::from_config(oidc_config(&issuer), |_| None).await.unwrap();
        *fake.signing_key.write().await = Arc::new(
            CoreRsaPrivateSigningKey::from_pem(TEST_RSA_PRIVATE_KEY, Some(JsonWebKeyId::new("rotated-key".into())))
                .unwrap(),
        );
        let mut state = AppState::new(storage, TaskStorage::connect_memory().await.unwrap())
            .await
            .unwrap()
            .with_trusted_proxies(vec!["127.0.0.0/8".parse().unwrap()]);
        state.oauth = oauth;
        let app = crate::build_app(state);

        let success = oidc_callback(&app, &fake, Some("reader@example.org"), Some(true), true).await;
        assert_eq!(success.status(), StatusCode::SEE_OTHER);
        assert!(response_cookie(&success, crate::SESSION_COOKIE).is_some());
        assert_eq!(fake.discovery_requests.load(Ordering::Relaxed), 2);

        for (email, verified, valid_nonce, expected_error) in [
            (Some("reader@example.org"), Some(false), true, "ERR_1026"),
            (Some("reader@example.org"), None, true, "ERR_1027"),
            (None, None, true, "ERR_1028"),
            (
                Some("reader@example.org"),
                Some(true),
                false,
                "OAuth2+authentication+failed",
            ),
        ] {
            let response = oidc_callback(&app, &fake, email, verified, valid_nonce).await;
            assert_eq!(response.status(), StatusCode::SEE_OTHER);
            assert!(response_cookie(&response, crate::SESSION_COOKIE).is_none());
            assert!(
                response
                    .headers()
                    .get(header::LOCATION)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .contains(expected_error)
            );
        }
        fake_task.abort();
    }

    #[tokio::test]
    async fn account_creation_is_disabled_by_default_and_handles_concurrent_creation() {
        let storage = Storage::connect_memory().await.unwrap();
        let state = AppState::new(storage.clone(), TaskStorage::connect_memory().await.unwrap())
            .await
            .unwrap();

        assert_eq!(
            bind_user(&state, "new@example.org", false).await.unwrap_err(),
            "ERR_1025"
        );
        let (first, second) = tokio::join!(
            bind_user(&state, "new@example.org", true),
            bind_user(&state, "NEW@example.org", true)
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!(first.id, second.id);
        assert!(!first.is_admin());
        assert_eq!(storage.list_users().await.unwrap().len(), 1);
    }
}
