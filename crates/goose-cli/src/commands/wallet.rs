use anyhow::Result;
use bip39::Mnemonic;
use cdk::amount::SplitTarget;
use cdk::nuts::CurrencyUnit;
use cdk::nuts::Token;
use cdk::wallet::{SendOptions, Wallet};
use cdk::Amount;
use cdk_redb::WalletRedbDatabase;
use goose::config::Config;
use home::home_dir;
use reqwest::Client;
use serde_json::Value;
use std::fs;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_MINT_URL: &str = "https://mint.minibits.cash/Bitcoin";

pub async fn handle_wallet_balance() -> Result<()> {
    let wallet = initialize_wallet().await?;

    if let Some(current_token) = get_current_token().ok() {
        handle_refund(&current_token, &wallet).await?;
    }

    let balance = wallet.total_balance().await?;

    println!("sats: {}", balance);

    let proofs = consolidate_proofs(&wallet, balance).await?;

    let token = Token::new(
        wallet.mint_url.clone(),
        proofs,
        None,
        wallet.unit.clone(),
    );

    set_current_token(token.to_string())?;

    Ok(())
}

async fn consolidate_proofs(wallet: &Wallet, balance: Amount) -> Result<cdk::nuts::Proofs> {
    let unspent = wallet.get_unspent_proofs().await?;
    if balance == Amount::ZERO || unspent.is_empty() {
        return Ok(unspent);
    }

    let swapped = wallet
        .swap(
            Some(balance),
            SplitTarget::default(),
            unspent.clone(),
            None,  // spending_conditions
            false, // include_fees
            false, // use_p2bk
        )
        .await?;

    // swap returns Ok(None) when proofs are already in optimal denominations;
    // fall back to the unspent set so the encoded token still matches the balance.
    Ok(swapped.unwrap_or(unspent))
}

fn get_current_token() -> Result<String> {
    let config = Config::global();

    Ok(config
        .get_param::<String>("ROUTSTR_API_KEY")?
        .to_string()
        .trim()
        .to_string())
}

fn set_current_token(token: String) -> Result<()> {
    let config = Config::global();

    config.set_param("ROUTSTR_API_KEY", Value::String(token.trim().to_string()))?;

    Ok(())
}

fn clear_current_token() -> Result<()> {
    let config = Config::global();
    config.delete("ROUTSTR_API_KEY")?;
    Ok(())
}

async fn handle_refund(current_token: &str, wallet: &Wallet) -> Result<()> {
    let config = Config::global();

    let host: String = config.get_param("ROUTSTR_HOST")?;

    let base_url = url::Url::parse(&host)?;
    let url = base_url.join("/v1/wallet/refund")?;

    let client = Client::builder()
        .timeout(Duration::from_secs(600))
        .build()?;

    let response = client
        .post(url)
        .header("Authorization", format!("Bearer {current_token}"))
        .send()
        .await?;

    let response: Value = response.json().await?;

    if let Some(token) = response.get("token") {
        match wallet
            .receive(
                &token.to_string().trim_matches('"'),
                cdk::wallet::ReceiveOptions::default(),
            )
            .await
        {
            Ok(amount) => {
                tracing::debug!("Claimed change from mint: {} sats.", amount);
            }
            Err(e) => {
                tracing::error!("Failed to claim change: {}", e);
                tracing::error!("{}", token);
            }
        }
        clear_current_token()?;
    }

    Ok(())
}

pub async fn handle_wallet_topup(top_up_token: String) -> Result<()> {
    let wallet = initialize_wallet().await?;

    if top_up_token.trim().is_empty() {
        println!("No token provided. Operation cancelled.");
        return Ok(());
    }

    if let Some(current_token) = get_current_token().ok() {
        handle_refund(&current_token, &wallet).await?;
    }

    match wallet
        .receive(
            &top_up_token.to_string().trim_matches('"'),
            cdk::wallet::ReceiveOptions::default(),
        )
        .await
    {
        Ok(amount) => {
            tracing::debug!("Claimed change from mint: {} sats.", amount);
        }
        Err(e) => {
            tracing::error!("Failed to claim change: {}", e);
            tracing::error!("{}", top_up_token);
        }
    }

    let balance = wallet.total_balance().await?;

    let proofs = consolidate_proofs(&wallet, balance).await?;

    let token = Token::new(
        wallet.mint_url.clone(),
        proofs,
        None,
        wallet.unit.clone(),
    );

    set_current_token(token.to_string())?;

    Ok(())
}

pub async fn handle_wallet_withdraw(amount: Option<u64>) -> Result<()> {
    let wallet = initialize_wallet().await?;

    if let Some(current_token) = get_current_token().ok() {
        println!("{}", current_token);
        handle_refund(&current_token, &wallet).await?;
    }

    let balance = wallet.total_balance().await?;

    if balance > Amount::ZERO {
        let amount = amount.map(Amount::from);

        let amount = amount.unwrap_or(balance);

        let prep_send = wallet.prepare_send(amount, SendOptions::default()).await?;

        let token = prep_send.confirm(None).await?;

        println!("{}", token);
    } else {
        println!("Wallet is empty.");
    }

    Ok(())
}

async fn initialize_wallet() -> Result<Wallet> {
    let work_dir = home_dir().unwrap().join(".cdk-gooose");
    fs::create_dir_all(&work_dir)?;
    let cdk_wallet_path = work_dir.join("cdk-goose.redb");

    let wallet_db = WalletRedbDatabase::new(&cdk_wallet_path)?;

    let seed_path = work_dir.join("seed");

    let mnemonic = match fs::metadata(seed_path.clone()) {
        Ok(_) => {
            let contents = fs::read_to_string(seed_path.clone())?;
            Mnemonic::from_str(&contents)?
        }
        Err(_) => {
            let mnemonic = Mnemonic::generate(12)?;
            tracing::info!("Creating new seed");
            fs::write(seed_path, mnemonic.to_string())?;
            mnemonic
        }
    };

    let seed = mnemonic.to_seed_normalized("");
    let currency_unit = CurrencyUnit::Sat;

    let wallet = Wallet::new(
        DEFAULT_MINT_URL,
        currency_unit,
        Arc::new(wallet_db),
        seed,
        None,
    )?;

    // Best-effort: release any proofs left in `Reserved` from an interrupted
    // swap/send/melt. A failure here must not block opening the wallet.
    if let Err(e) = wallet.recover_incomplete_sagas().await {
        tracing::warn!("recover_incomplete_sagas failed: {}", e);
    }

    Ok(wallet)
}
