//! `goose routstr ...` subcommand.
//!
//! Manages the user's set of Routstr profiles (each is `{url, api_key}`) and
//! moves sats between the local Cashu wallet and the active profile's
//! tracked balance on the proxy.

use anyhow::{anyhow, bail, Result};
use cdk::Amount;
use console::style;
use goose::config::Config;
use goose::providers::routstr_api::{
    active_profile_name, balance_info, create_balance, load_profile, load_profiles,
    refund_balance, remove_profile, set_active_profile, topup_balance, upsert_profile,
    BalanceInfoResponse, ProviderApiError, RoutstrProfile, ROUTSTR_DEFAULT_HOST,
};
#[cfg(test)]
use goose::providers::routstr_api::ROUTSTR_DEFAULT_PROFILE;

use crate::commands::wallet::{open_wallet, receive_into_wallet, withdraw_to_token};

/// Default top-up amount in sats when the user runs `goose routstr topup`
/// without an explicit number.
pub const DEFAULT_TOPUP_SATS: u64 = 2000;

pub async fn handle_profile_add(name: String, url: String) -> Result<()> {
    let config = Config::global();
    let mut profiles = load_profiles(config)?;
    if profiles.contains_key(&name) {
        bail!("Routstr profile {name:?} already exists. Use `goose routstr profile use {name}` to switch to it, or pick a different name.");
    }
    profiles.insert(name.clone(), RoutstrProfile::new(url.clone()));
    goose::providers::routstr_api::save_profiles(config, &profiles)?;
    if active_profile_name(config) != name && profiles.len() == 1 {
        // first profile created — make it active automatically.
        set_active_profile(config, &name)?;
    }
    println!(
        "{}",
        style(format!("✓ added routstr profile {name:?} → {url}")).green()
    );
    Ok(())
}

pub async fn handle_profile_list() -> Result<()> {
    let config = Config::global();
    let profiles = load_profiles(config)?;
    if profiles.is_empty() {
        println!("No routstr profiles configured. Add one with:");
        println!("    goose routstr profile add default --url {ROUTSTR_DEFAULT_HOST}");
        return Ok(());
    }

    let active = active_profile_name(config);
    println!("{:8}  {:25}  {:40}  {}", "active", "name", "url", "balance");
    for (name, profile) in &profiles {
        let marker = if *name == active { "  *" } else { "" };
        let balance = if profile.api_key.is_empty() {
            "(no api_key yet)".to_string()
        } else {
            match balance_info(&profile.url, &profile.api_key).await {
                Ok(info) => format!("{} sats ({} mSats)", info.balance / 1000, info.balance),
                Err(e) => format!("(?, {})", short_err(&e)),
            }
        };
        println!(
            "{:8}  {:25}  {:40}  {}",
            marker, name, profile.url, balance
        );
    }
    Ok(())
}

pub async fn handle_profile_use(name: String) -> Result<()> {
    let config = Config::global();
    let profiles = load_profiles(config)?;
    if !profiles.contains_key(&name) {
        bail!(
            "Routstr profile {name:?} not found. Available: {:?}",
            profiles.keys().collect::<Vec<_>>()
        );
    }

    let current = active_profile_name(config);
    if current == name {
        println!(
            "{}",
            style(format!("Already on routstr profile {name:?}.")).dim()
        );
        return Ok(());
    }

    // Refund the currently-active profile back into the local wallet (best
    // effort — log a warning if the proxy is unreachable, don't block the
    // switch).
    if let Some(active_profile) = profiles.get(&current) {
        if !active_profile.api_key.is_empty() {
            match refund_active_into_wallet(&current, active_profile).await {
                Ok(sats) => {
                    println!(
                        "{}",
                        style(format!(
                            "✓ refunded {sats} sats from {current:?} into local wallet"
                        ))
                        .green()
                    );
                    // Clear the now-spent api_key on the old profile.
                    let mut updated = active_profile.clone();
                    updated.api_key.clear();
                    upsert_profile(config, &current, updated)?;
                }
                Err(e) => {
                    eprintln!(
                        "{}",
                        style(format!(
                            "⚠ refund of {current:?} failed: {e}. Switching anyway; \
                             retry with `goose routstr profile use {current}` to reclaim those sats."
                        ))
                        .yellow()
                    );
                }
            }
        }
    }

    set_active_profile(config, &name)?;
    println!(
        "{}",
        style(format!("✓ active routstr profile is now {name:?}")).green()
    );

    // Auto-topup the new profile from the local wallet (best-effort, capped
    // at DEFAULT_TOPUP_SATS or whatever the local wallet has).
    let new_profile = profiles
        .get(&name)
        .cloned()
        .ok_or_else(|| anyhow!("internal: profile vanished during switch"))?;
    if let Err(e) = autotopup_after_switch(&name, &new_profile).await {
        eprintln!(
            "{}",
            style(format!(
                "⚠ auto-topup skipped: {e}. Run `goose routstr topup` manually once the local wallet has sats."
            ))
            .yellow()
        );
    }

    Ok(())
}

pub async fn handle_profile_remove(name: String) -> Result<()> {
    let config = Config::global();
    let (existed, profile) = match load_profile(config, Some(&name)) {
        Ok((n, p)) => (true, Some((n, p))),
        Err(_) => (false, None),
    };
    if !existed {
        bail!("Routstr profile {name:?} not found.");
    }

    if let Some((_, p)) = &profile {
        if !p.api_key.is_empty() {
            match refund_active_into_wallet(&name, p).await {
                Ok(sats) => println!(
                    "{}",
                    style(format!(
                        "✓ refunded {sats} sats from {name:?} before removal"
                    ))
                    .green()
                ),
                Err(e) => eprintln!(
                    "{}",
                    style(format!(
                        "⚠ refund failed during remove: {e}. Profile dropped anyway."
                    ))
                    .yellow()
                ),
            }
        }
    }

    if remove_profile(config, &name)? {
        println!(
            "{}",
            style(format!("✓ removed routstr profile {name:?}")).green()
        );
    }
    Ok(())
}

pub async fn handle_topup(amount_sats: Option<u64>) -> Result<()> {
    let config = Config::global();
    let active = active_profile_name(config);
    let (active_name, mut profile) = load_profile(config, Some(&active))?;

    let amount_sats = amount_sats.unwrap_or(DEFAULT_TOPUP_SATS);

    let wallet = open_wallet().await?;
    let local_balance: Amount = wallet.total_balance().await?;
    if local_balance == Amount::ZERO {
        bail!(
            "Local Cashu wallet is empty. Run `goose wallet topup <cashu-token>` first to fund it."
        );
    }

    let to_send: Amount = Amount::from(amount_sats).min(local_balance);
    let token = withdraw_to_token(&wallet, to_send).await?;

    if profile.api_key.is_empty() {
        // First-time funding for this profile — call /v1/balance/create.
        let resp = create_balance(&profile.url, &token)
            .await
            .map_err(|e| anyhow!(e))?;
        profile.api_key = resp.api_key;
        upsert_profile(config, &active_name, profile.clone())?;
        println!(
            "{}",
            style(format!(
                "✓ created api_key for {active_name:?} with {} sats ({} mSats) initial balance",
                resp.balance / 1000,
                resp.balance,
            ))
            .green()
        );
    } else {
        let _ = topup_balance(&profile.url, &profile.api_key, &token)
            .await
            .map_err(|e| anyhow!(e))?;
        println!(
            "{}",
            style(format!(
                "✓ topped up {active_name:?} by {} sats",
                u64::from(to_send)
            ))
            .green()
        );
    }

    let local_after: Amount = wallet.total_balance().await?;
    println!(
        "  local wallet: {} sats ({} sats sent to proxy)",
        u64::from(local_after),
        u64::from(to_send),
    );
    Ok(())
}

pub async fn handle_refund() -> Result<()> {
    let config = Config::global();
    let active = active_profile_name(config);
    let (active_name, mut profile) = load_profile(config, Some(&active))?;

    if profile.api_key.is_empty() {
        bail!("Active routstr profile {active_name:?} has no api_key to refund.");
    }

    let sats = refund_active_into_wallet(&active_name, &profile).await?;
    profile.api_key.clear();
    upsert_profile(config, &active_name, profile)?;
    println!(
        "{}",
        style(format!(
            "✓ refunded {sats} sats from {active_name:?} into local wallet"
        ))
        .green()
    );
    Ok(())
}

pub async fn handle_balance() -> Result<()> {
    let config = Config::global();
    let local = crate::commands::wallet::wallet_status().await?;
    println!("local wallet: {} sats   (mint: {})", local.balance_sats, local.mint_url);

    let profiles = load_profiles(config)?;
    if profiles.is_empty() {
        println!("(no routstr profiles configured — `goose routstr profile add`)");
        return Ok(());
    }

    let active = active_profile_name(config);
    for (name, profile) in profiles {
        let marker = if name == active { " *" } else { "  " };
        if profile.api_key.is_empty() {
            println!(
                "{} {:20} {} (no api_key — fund with `goose routstr topup`)",
                marker, name, profile.url
            );
            continue;
        }
        match balance_info(&profile.url, &profile.api_key).await {
            Ok(info) => println!(
                "{} {:20} {} → {} sats ({} mSats, {} requests / {} mSats spent)",
                marker,
                name,
                profile.url,
                info.balance / 1000,
                info.balance,
                info.total_requests,
                info.total_spent,
            ),
            Err(e) => println!(
                "{} {:20} {} → ?  ({})",
                marker,
                name,
                profile.url,
                short_err(&e)
            ),
        }
    }

    Ok(())
}

// =================== helpers ===================

async fn refund_active_into_wallet(name: &str, profile: &RoutstrProfile) -> Result<u64> {
    let resp = refund_balance(&profile.url, &profile.api_key)
        .await
        .map_err(|e| anyhow!("refund {name:?} failed: {e}"))?;
    let wallet = open_wallet().await?;
    let received = receive_into_wallet(&wallet, &resp.token).await?;
    Ok(received.max(resp.amount.as_sats() as u64))
}

async fn autotopup_after_switch(name: &str, profile: &RoutstrProfile) -> Result<()> {
    // Inspect the new profile's current balance (if it has an api_key).
    let current_sats: u64 = if profile.api_key.is_empty() {
        0
    } else {
        let info: BalanceInfoResponse = balance_info(&profile.url, &profile.api_key)
            .await
            .map_err(|e| anyhow!(e))?;
        (info.balance / 1000) as u64
    };
    if current_sats >= DEFAULT_TOPUP_SATS {
        println!(
            "  {name:?} already has {current_sats} sats; skipping auto-topup."
        );
        return Ok(());
    }

    let needed = DEFAULT_TOPUP_SATS.saturating_sub(current_sats);
    // Open the wallet in its own scope so the redb lock is released before
    // `handle_topup` re-opens the same database.
    let local_sats = {
        let wallet = open_wallet().await?;
        u64::from(wallet.total_balance().await?)
    };
    if local_sats == 0 {
        bail!(
            "local wallet empty — top up with `goose wallet topup <cashu-token>` then run `goose routstr topup`"
        );
    }
    let to_send = local_sats.min(needed);
    handle_topup(Some(to_send)).await
}

fn short_err(e: &ProviderApiError) -> String {
    let s = e.to_string();
    if s.len() > 80 {
        format!("{}...", &s[..77])
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_topup_is_2000_sats() {
        assert_eq!(DEFAULT_TOPUP_SATS, 2000);
    }

    #[test]
    fn default_profile_constant() {
        assert_eq!(ROUTSTR_DEFAULT_PROFILE, "default");
    }
}
