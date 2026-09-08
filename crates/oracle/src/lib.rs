//! Two independent price sources, kept deliberately separate rather than
//! merged behind one "get me a price" abstraction:
//!
//! - `on_chain` reads the same `oracle_adapter` (Reflector, staleness-
//!   checked) the deployed `vault` itself trusts for real - this is what
//!   the *contract* actually decides rebalances on.
//! - `coingecko` is an independent off-chain source with no relationship
//!   to Reflector at all, used to cross-check the on-chain price and
//!   catch a stale/wrong feed before the scheduler blindly trusts it -
//!   see PROJECT.md's "Real-time pricing" note.
//!
//! Important scope note: "CoinGecko as fallback" (PROJECT.md) means
//! fallback for *this backend's own observability* - price_snapshots
//! rows, divergence warnings, dashboards - never a fallback the on-chain
//! `rebalance` decision can use. Soroban contracts can't reach an HTTP
//! API, so `vault::needs_rebalance` is Reflector-only no matter what this
//! crate observes; a CoinGecko/Reflector disagreement is something to
//! alert a human about, not something this crate can act on
//! automatically today.

pub mod coingecko;
pub mod on_chain;

/// Absolute divergence between an on-chain price (raw fixed-point, at
/// `decimals` places) and a CoinGecko USD price, in bps of the CoinGecko
/// price. Pure and source-agnostic on purpose - callers decide what
/// counts as "too much" and what to do about it (this crate only
/// measures).
pub fn divergence_bps(on_chain_price: i128, coingecko_price_usd: f64, decimals: u32) -> u32 {
    if coingecko_price_usd <= 0.0 {
        return u32::MAX;
    }
    let on_chain_usd = on_chain_price as f64 / 10f64.powi(decimals as i32);
    let diff = (on_chain_usd - coingecko_price_usd).abs();
    let bps = (diff / coingecko_price_usd) * 10_000.0;
    if bps >= u32::MAX as f64 {
        u32::MAX
    } else {
        bps.round() as u32
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn zero_divergence_for_identical_prices() {
        assert_eq!(divergence_bps(100_000_000_000_000, 1.0, 14), 0);
    }

    /// Real values captured live: `oracle_adapter.get_price(Other("XLM"))`
    /// returned `18808876553202` at 14 decimals (0.18808876553202 USD)
    /// while CoinGecko's simple-price endpoint reported 0.188497 USD for
    /// `stellar` at the same moment - about 22 bps apart, a real-world
    /// sanity check that two independently-sourced live feeds for the
    /// same asset actually land in the same neighborhood.
    #[test]
    fn matches_live_captured_divergence() {
        assert_eq!(divergence_bps(18_808_876_553_202, 0.188497, 14), 22);
    }

    #[test]
    fn large_divergence_is_not_silently_clamped_to_zero() {
        // On-chain says $2, CoinGecko says $1 - 100% off, 10_000 bps.
        assert_eq!(divergence_bps(200_000_000_000_000, 1.0, 14), 10_000);
    }

    #[test]
    fn non_positive_coingecko_price_is_treated_as_maximal_divergence() {
        assert_eq!(divergence_bps(100, 0.0, 14), u32::MAX);
    }
}
