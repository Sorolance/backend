//! Minimal client for CoinGecko's public "simple price" endpoint - no API
//! key needed at CoinGecko's public rate limit, which is all this
//! project's polling interval needs. See the crate-level doc comment for
//! what this source is (and isn't) used for.

use std::collections::HashMap;

pub struct CoinGeckoClient {
    http: reqwest::Client,
    base_url: String,
}

#[derive(Debug)]
pub enum CoinGeckoError {
    Request(reqwest::Error),
    Decode(serde_json::Error),
    MissingPrice(String),
}

impl std::fmt::Display for CoinGeckoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(e) => write!(f, "coingecko request failed: {e}"),
            Self::Decode(e) => write!(f, "coingecko response was not the expected shape: {e}"),
            Self::MissingPrice(id) => write!(f, "coingecko response had no usd price for {id}"),
        }
    }
}

impl std::error::Error for CoinGeckoError {}

impl Default for CoinGeckoClient {
    fn default() -> Self {
        Self::new()
    }
}

impl CoinGeckoClient {
    /// Confirmed live: CoinGecko's public API rejects reqwest's default
    /// User-Agent outright ("Please add a descriptive User-Agent to your
    /// request") rather than just rate-limiting it harder, so this isn't
    /// an optional politeness header - `price_usd` fails every call
    /// without it.
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!(
                "stellar-portfolio-rebalancer-scheduler/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .expect("static TLS/User-Agent config is always valid");
        Self {
            http,
            base_url: "https://api.coingecko.com/api/v3".to_string(),
        }
    }

    /// Current USD price for a CoinGecko coin id (e.g. `"stellar"`,
    /// `"usd-coin"` - see `rebalancer_scheduler`'s asset config for the
    /// ids this project actually uses).
    pub async fn price_usd(&self, coingecko_id: &str) -> Result<f64, CoinGeckoError> {
        let url = format!(
            "{}/simple/price?ids={coingecko_id}&vs_currencies=usd",
            self.base_url
        );
        let body = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(CoinGeckoError::Request)?
            .text()
            .await
            .map_err(CoinGeckoError::Request)?;
        parse_simple_price(&body, coingecko_id)
    }
}

/// Split out from `price_usd` so the parsing logic is testable against
/// real captured response text without any network access or mock HTTP
/// server.
fn parse_simple_price(body: &str, coingecko_id: &str) -> Result<f64, CoinGeckoError> {
    let parsed: HashMap<String, HashMap<String, f64>> =
        serde_json::from_str(body).map_err(CoinGeckoError::Decode)?;
    parsed
        .get(coingecko_id)
        .and_then(|by_currency| by_currency.get("usd"))
        .copied()
        .ok_or_else(|| CoinGeckoError::MissingPrice(coingecko_id.to_string()))
}

#[cfg(test)]
mod test;
