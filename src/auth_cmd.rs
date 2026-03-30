//! `auth` subcommand — authenticate Codex accounts via OpenAI OAuth2.
//!
//! Two flows are supported:
//!   1. **Browser (PKCE)** — opens the login URL, waits for loopback callback.
//!      This is the quickest flow when a browser is available.
//!   2. **Device Code** — prints a code + URL; user logs in on any device.
//!      Useful for headless / SSH sessions.
//!
//! After login, credentials (access_token + refresh_token + account_id) are
//! stored in the multi-account `auth.json`.

use anyhow::{Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// ── OpenAI OAuth2 constants (from Codex Desktop / zeroclaw reference) ────────

const OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OAUTH_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const OAUTH_DEVICE_CODE_URL: &str = "https://auth.openai.com/oauth/device/code";
const OAUTH_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const OAUTH_SCOPE: &str = "openid profile email offline_access";

// ── Auth file types ───────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct AuthAccount {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>, // unix timestamp
}

#[derive(Deserialize, Serialize, Debug)]
struct MultiAuthFile {
    #[serde(default)]
    current: usize,
    accounts: Vec<AuthAccount>,
}

impl MultiAuthFile {
    fn load_sync(path: &str) -> Result<Self> {
        let expanded = expand_home(path);
        if !std::path::Path::new(&expanded).exists() {
            return Ok(Self { current: 0, accounts: Vec::new() });
        }
        let content = std::fs::read_to_string(&expanded)
            .with_context(|| format!("Failed to read {expanded}"))?;

        if let Ok(multi) = serde_json::from_str::<Self>(&content) {
            return Ok(multi);
        }
        // Legacy single-account format (old auth.json)
        if let Ok(single) = serde_json::from_str::<LegacyAuthAccount>(&content) {
            return Ok(Self {
                current: 0,
                accounts: vec![AuthAccount {
                    label: Some("default".to_string()),
                    access_token: single.access_token.or(single.tokens.as_ref().map(|t| t.access_token.clone())),
                    refresh_token: single.tokens.as_ref().and_then(|t| t.refresh_token.clone()),
                    account_id: single.account_id.or(single.tokens.map(|t| t.account_id)),
                    expires_at: None,
                }],
            });
        }
        Ok(Self { current: 0, accounts: Vec::new() })
    }

    fn save_sync(&self, path: &str) -> Result<()> {
        let expanded = expand_home(path);
        if let Some(parent) = std::path::Path::new(&expanded).parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&expanded, json)?;
        Ok(())
    }

    fn upsert(&mut self, account: AuthAccount) {
        let label = account.label.as_deref().unwrap_or("");
        if let Some(pos) = self.accounts.iter().position(|a| a.label.as_deref() == Some(label)) {
            self.accounts[pos] = account;
        } else {
            self.accounts.push(account);
        }
    }
}

/// Legacy Codex Desktop / old proxy format
#[derive(Deserialize)]
struct LegacyAuthAccount {
    access_token: Option<String>,
    account_id: Option<String>,
    tokens: Option<LegacyTokens>,
}

#[derive(Deserialize)]
struct LegacyTokens {
    access_token: String,
    account_id: String,
    refresh_token: Option<String>,
}

// ── OAuth2 response types ─────────────────────────────────────────────────────

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    expires_in: u64,
    #[serde(default)]
    interval: Option<u64>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Deserialize)]
struct OAuthError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

// ── PKCE helpers ──────────────────────────────────────────────────────────────

struct PkceState {
    code_verifier: String,
    code_challenge: String,
    state: String,
}

fn generate_pkce() -> PkceState {
    use rand::RngCore;
    let mut rng = rand::thread_rng();

    let mut verifier_bytes = [0u8; 64];
    rng.fill_bytes(&mut verifier_bytes);
    let code_verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&verifier_bytes);

    let digest = Sha256::digest(code_verifier.as_bytes());
    let code_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest);

    let mut state_bytes = [0u8; 24];
    rng.fill_bytes(&mut state_bytes);
    let state = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&state_bytes);

    PkceState { code_verifier, code_challenge, state }
}

fn build_authorize_url(pkce: &PkceState) -> String {
    let params = [
        ("response_type", "code"),
        ("client_id", OAUTH_CLIENT_ID),
        ("redirect_uri", OAUTH_REDIRECT_URI),
        ("scope", OAUTH_SCOPE),
        ("code_challenge", &pkce.code_challenge),
        ("code_challenge_method", "S256"),
        ("state", &pkce.state),
        ("codex_cli_simplified_flow", "true"),
        ("id_token_add_organizations", "true"),
    ];

    let qs: String = params
        .iter()
        .map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v)))
        .collect::<Vec<_>>()
        .join("&");

    format!("{OAUTH_AUTHORIZE_URL}?{qs}")
}

fn url_encode(input: &str) -> String {
    input.bytes().map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
            (b as char).to_string()
        }
        _ => format!("%{b:02X}"),
    }).collect()
}

fn parse_query_params(query: &str) -> std::collections::HashMap<String, String> {
    query.split('&').filter_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        Some((url_decode(k), url_decode(v)))
    }).collect()
}

fn url_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            ) {
                if let Ok(b) = u8::try_from(hi * 16 + lo) {
                    out.push(b);
                    i += 3;
                    continue;
                }
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Decode the JWT payload and extract the account_id claim.
fn extract_account_id_from_jwt(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&decoded).ok()?;

    for key in ["account_id", "accountId", "acct", "sub", "https://api.openai.com/account_id"] {
        if let Some(v) = claims.get(key).and_then(|v| v.as_str()) {
            if !v.trim().is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

// ── Public command entrypoints ────────────────────────────────────────────────

/// `auth login [--label <name>]`
///
/// Offers browser OAuth2 (PKCE) or device-code flow.
pub async fn run_login(label: &str, auth_path: &str) -> Result<()> {
    println!();
    println!("  ╔══════════════════════════════════════╗");
    println!("  ║   Codex Proxy — Add/Update Account   ║");
    println!("  ╚══════════════════════════════════════╝");
    println!();

    let label = if label.is_empty() {
        print!("  Label for this account [default]: ");
        use std::io::Write;
        std::io::stdout().flush()?;
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf)?;
        let trimmed = buf.trim().to_string();
        if trimmed.is_empty() { "default".to_string() } else { trimmed }
    } else {
        label.to_string()
    };

    println!("  Logging in as: {label}");
    println!();
    println!("  Choose login method:");
    println!("    1) Browser (recommended) — opens login URL, auto-captures token");
    println!("    2) Device Code — show a code, login on any device");
    println!();
    print!("  Choice [1]: ");
    use std::io::Write;
    std::io::stdout().flush()?;
    let mut choice = String::new();
    std::io::stdin().read_line(&mut choice)?;

    let token_resp = if choice.trim() == "2" {
        device_code_flow().await?
    } else {
        browser_pkce_flow().await?
    };

    let account_id = extract_account_id_from_jwt(&token_resp.access_token);
    let expires_at = token_resp.expires_in.map(|secs| {
        chrono::Utc::now().timestamp() + secs
    });

    let account = AuthAccount {
        label: Some(label.clone()),
        access_token: Some(token_resp.access_token),
        refresh_token: token_resp.refresh_token,
        account_id: account_id.clone(),
        expires_at,
    };

    let mut file = MultiAuthFile::load_sync(auth_path)?;
    file.upsert(account);
    file.save_sync(auth_path)?;

    println!();
    println!("  ✓ Account \"{label}\" saved to {auth_path}");
    if let Some(acct_id) = account_id {
        println!("  ✓ account_id: {acct_id}");
    }
    println!();

    Ok(())
}

/// `auth list`
pub fn run_list(auth_path: &str) -> Result<()> {
    let file = MultiAuthFile::load_sync(auth_path)?;

    if file.accounts.is_empty() {
        println!("  No accounts configured.");
        println!("  Run: codex-openai-proxy auth login --label default");
        return Ok(());
    }

    println!();
    println!("  Accounts in {auth_path}:");
    println!();
    println!("  {:<4} {:<20} {:<10} {}", "#", "Label", "Status", "Account ID");
    println!("  {}", "─".repeat(60));

    for (i, acc) in file.accounts.iter().enumerate() {
        let label = acc.label.as_deref().unwrap_or("(unlabeled)");
        let status = if i == file.current { "● active" } else { "  ──    " };
        let acct_id = acc.account_id.as_deref().unwrap_or("?");
        println!("  {i:<4} {label:<20} {status:<10} {acct_id}");
    }
    println!();

    Ok(())
}

/// `auth remove --label <name>`
pub fn run_remove(label: &str, auth_path: &str) -> Result<()> {
    let mut file = MultiAuthFile::load_sync(auth_path)?;
    let before = file.accounts.len();
    file.accounts.retain(|a| a.label.as_deref() != Some(label));

    if file.accounts.len() == before {
        println!("  Account \"{label}\" not found.");
        return Ok(());
    }

    if file.current >= file.accounts.len() && !file.accounts.is_empty() {
        file.current = file.accounts.len() - 1;
    }

    file.save_sync(auth_path)?;
    println!("  ✓ Account \"{label}\" removed from {auth_path}");
    Ok(())
}

// ── OAuth2 flows ──────────────────────────────────────────────────────────────

async fn browser_pkce_flow() -> Result<TokenResponse> {
    let pkce = generate_pkce();
    let url = build_authorize_url(&pkce);

    println!("  Opening login URL in your browser...");
    println!("  If it doesn't open automatically, visit:");
    println!();
    println!("    {url}");
    println!();

    // Try to open browser
    let _ = open_browser(&url);

    println!("  Waiting for authorization callback on localhost:1455...");
    println!("  (Press Ctrl+C to cancel)");
    println!();

    let code = receive_callback(&pkce.state).await?;
    let client = reqwest::Client::new();
    exchange_code(&client, &code, &pkce).await
}

async fn device_code_flow() -> Result<TokenResponse> {
    let client = reqwest::Client::new();

    let form = [
        ("client_id", OAUTH_CLIENT_ID),
        ("scope", OAUTH_SCOPE),
    ];

    let resp = client
        .post(OAUTH_DEVICE_CODE_URL)
        .form(&form)
        .send()
        .await
        .context("Failed to start device code flow")?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Device code request failed: {body}");
    }

    let device: DeviceCodeResponse = resp.json().await.context("Failed to parse device code response")?;

    println!("  ┌─────────────────────────────────────────────┐");
    println!("  │  Go to: {}   │", device.verification_uri_complete.as_deref().unwrap_or(&device.verification_uri));
    println!("  │  Enter code: {}                              │", device.user_code);
    println!("  └─────────────────────────────────────────────┘");
    if let Some(msg) = &device.message {
        println!("  {msg}");
    }
    println!();
    println!("  Waiting for you to complete login...");

    let interval = device.interval.unwrap_or(5).max(1);
    let deadline = std::time::Instant::now() + Duration::from_secs(device.expires_in);

    loop {
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("Device code expired before login completed");
        }

        tokio::time::sleep(Duration::from_secs(interval)).await;

        let form = [
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", &device.device_code),
            ("client_id", OAUTH_CLIENT_ID),
        ];

        let resp = client.post(OAUTH_TOKEN_URL).form(&form).send().await?;

        if resp.status().is_success() {
            return resp.json::<TokenResponse>().await.context("Failed to parse token response");
        }

        let text = resp.text().await.unwrap_or_default();
        if let Ok(err) = serde_json::from_str::<OAuthError>(&text) {
            match err.error.as_str() {
                "authorization_pending" => continue,
                "slow_down" => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
                "access_denied" => anyhow::bail!("Authorization was denied"),
                "expired_token" => anyhow::bail!("Device code expired"),
                _ => anyhow::bail!("{}", err.error_description.unwrap_or(err.error)),
            }
        }
    }
}

async fn receive_callback(expected_state: &str) -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:1455")
        .await
        .context("Failed to bind on localhost:1455 — is another process using that port?")?;

    let (mut stream, _) =
        tokio::time::timeout(Duration::from_secs(120), listener.accept())
            .await
            .context("Timed out waiting for browser callback (120s)")??;

    let mut buf = vec![0u8; 8192];
    let n = stream.read(&mut buf).await.context("Failed to read callback")?;
    let request = String::from_utf8_lossy(&buf[..n]);

    let path = request
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("");

    let query = path.split_once('?').map(|(_, q)| q).unwrap_or(path);
    let params = parse_query_params(query);

    if let Some(err) = params.get("error") {
        let desc = params.get("error_description").map(|s| s.as_str()).unwrap_or("OAuth error");
        // Send error page before bailing
        let body = "<html><body><h2>Login failed</h2><p>You can close this tab.</p></body></html>";
        let _ = stream.write_all(http_response(body).as_bytes()).await;
        anyhow::bail!("OAuth error: {err} — {desc}");
    }

    if let Some(state) = params.get("state") {
        if state != expected_state {
            anyhow::bail!("OAuth state mismatch (CSRF protection)");
        }
    }

    let code = params.get("code").cloned()
        .ok_or_else(|| anyhow::anyhow!("No authorization code in callback"))?;

    let body = "<html><body><h2>Codex login complete ✓</h2><p>You can close this tab.</p></body></html>";
    let _ = stream.write_all(http_response(body).as_bytes()).await;

    println!("  ✓ Authorization code received");
    Ok(code)
}

fn http_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(), body
    )
}

async fn exchange_code(client: &reqwest::Client, code: &str, pkce: &PkceState) -> Result<TokenResponse> {
    println!("  Exchanging authorization code for tokens...");

    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("client_id", OAUTH_CLIENT_ID),
        ("redirect_uri", OAUTH_REDIRECT_URI),
        ("code_verifier", &pkce.code_verifier),
    ];

    let resp = client
        .post(OAUTH_TOKEN_URL)
        .form(&form)
        .send()
        .await
        .context("Failed to exchange code for tokens")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Token exchange failed ({status}): {body}");
    }

    resp.json::<TokenResponse>().await.context("Failed to parse token response")
}

fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", url])
            .spawn();
    }
    Ok(())
}

fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    path.to_string()
}
