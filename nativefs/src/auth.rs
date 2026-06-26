use reqwest::Client;
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct AuthState {
    pub logged_in: bool,
    pub tier: u8,
    pub token: String,
}

impl Default for AuthState {
    fn default() -> Self {
        Self {
            logged_in: false,
            tier: 0,
            token: String::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AuthResponse {
    pub valid: bool,
    pub tier: Option<u8>,
}

/// POST /api/auth — returns (token, tier) on success
pub async fn login(
    client: &Client,
    server_url: &str,
    password: &str,
) -> Result<(String, u8), String> {
    let url = format!("{}/api/auth", server_url);

    let params = [
        ("password", password),
        ("cookieDays", "7"),
    ];

    let resp = client
        .post(&url)
        .form(&params)
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    if resp.status() == 403 {
        return Err("Invalid password".to_string());
    }

    // Extract Set-Cookie header for tvfs_token
    let token = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .find_map(|v| {
            let s = v.to_str().ok()?;
            if s.starts_with("tvfs_token=") {
                let token_part = s.split(';').next()?;
                Some(token_part.trim_start_matches("tvfs_token=").to_string())
            } else {
                None
            }
        });

    let body: AuthResponse = resp
        .json()
        .await
        .map_err(|e| format!("Parse error: {e}"))?;

    if !body.valid {
        return Err("Invalid password".to_string());
    }

    let tier = body.tier.unwrap_or(1);
    let tok = token.unwrap_or_default();

    Ok((tok, tier))
}

/// Verify that a stored token is still valid by hitting /api/status
pub async fn verify_token(client: &Client, server_url: &str, token: &str) -> Option<u8> {
    let url = format!("{}/api/status", server_url);
    let resp = client
        .get(&url)
        .header("Cookie", format!("tvfs_token={token}"))
        .send()
        .await
        .ok()?;

    if resp.status().is_success() {
        // Read tier from tvfs_tier cookie if returned, else assume 1
        let tier = resp
            .headers()
            .get_all("set-cookie")
            .iter()
            .find_map(|v| {
                let s = v.to_str().ok()?;
                if s.starts_with("tvfs_tier=") {
                    let t = s.split(';').next()?;
                    t.trim_start_matches("tvfs_tier=").parse().ok()
                } else {
                    None
                }
            })
            .unwrap_or(1);
        Some(tier)
    } else {
        None
    }
}

/// POST /api/logout
pub async fn logout(client: &Client, server_url: &str, token: &str) {
    let url = format!("{}/api/logout", server_url);
    let _ = client
        .post(&url)
        .header("Cookie", format!("tvfs_token={token}"))
        .send()
        .await;
}
