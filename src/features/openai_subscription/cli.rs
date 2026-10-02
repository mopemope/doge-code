use super::{auth, credentials::CredentialStore};
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Subcommand)]
pub enum AuthCommand {
    Login {
        #[arg(value_parser = ["openai-chatgpt"])]
        provider: String,
        #[arg(long, conflicts_with = "new_account")]
        account: Option<String>,
        #[arg(long)]
        new_account: bool,
        #[arg(long)]
        no_browser: bool,
        /// Explicitly request consent for ChatGPT plan usage after an earlier decline
        #[arg(long)]
        enable_plan_usage: bool,
    },
    Status {
        #[arg(value_parser = ["openai-chatgpt"])]
        provider: String,
    },
    List {
        #[arg(value_parser = ["openai-chatgpt"])]
        provider: String,
    },
    Use {
        #[arg(value_parser = ["openai-chatgpt"])]
        provider: String,
        account: String,
    },
    Logout {
        #[arg(value_parser = ["openai-chatgpt"])]
        provider: String,
        account: Option<String>,
    },
}

pub async fn run(
    command: AuthCommand,
    store: CredentialStore,
    cancel: CancellationToken,
) -> Result<()> {
    match command {
        AuthCommand::Login {
            account,
            new_account,
            no_browser,
            enable_plan_usage,
            ..
        } => {
            let selected = if new_account {
                None
            } else {
                account.or(store.load()?.active)
            };
            let label = auth::login(
                store.clone(),
                selected.as_deref(),
                no_browser,
                enable_plan_usage,
                &cancel,
            )
            .await?;
            println!(
                "Signed in: {label}. Select --provider openai-chatgpt explicitly to use this account."
            );
            let registry = store.load()?;
            if !registry
                .accounts
                .iter()
                .find(|a| a.label == label)
                .and_then(|a| a.tokens.as_ref())
                .is_some_and(|t| t.scopes.iter().any(|s| s == "chatgpt.tokens.use.direct"))
            {
                println!(
                    "Identity verified; ChatGPT plan usage was not granted. Run dgc auth login openai-chatgpt --account {label} --enable-plan-usage to request consent."
                );
            }
        }
        AuthCommand::List { .. } | AuthCommand::Status { .. } => {
            let registry = store.load()?;
            if registry.accounts.is_empty() {
                println!("No ChatGPT registrations. Run dgc auth login openai-chatgpt.");
            }
            for account in registry.accounts {
                let state = match account.tokens {
                    None => "signed out",
                    Some(ref t) if t.renewal_uncertain => {
                        "token renewal interrupted (sign in again)"
                    }
                    Some(ref t) if !t.scopes.iter().any(|s| s == "chatgpt.tokens.use.direct") => {
                        "plan permission not granted"
                    }
                    Some(ref t) if t.expires_at <= auth::now() => {
                        "access token expired (refresh required)"
                    }
                    Some(_) => "access token locally unexpired (server access unverified)",
                };
                println!(
                    "{} {}: {}",
                    if registry.active.as_deref() == Some(&account.label) {
                        "*"
                    } else {
                        " "
                    },
                    account.label,
                    state
                );
            }
        }
        AuthCommand::Use { account, .. } => {
            let _lock = store.lock(&cancel).await?;
            let mut registry = store.load()?;
            let entry = registry
                .accounts
                .iter()
                .find(|a| a.label == account)
                .context("unknown ChatGPT account label")?;
            if entry.tokens.is_none() {
                bail!("account is signed out; sign in before selecting it");
            }
            registry.active = Some(account.clone());
            store.save(&registry)?;
            println!("Selected {account}. Start a new session when switching accounts.");
        }
        AuthCommand::Logout { account, .. } => {
            if !auth::logout(&store, account.as_deref(), &cancel).await? {
                eprintln!(
                    "Local tokens cleared; remote revocation was not confirmed. Disconnect the app in ChatGPT Settings if needed."
                );
            }
            println!("Signed out.");
        }
    }
    Ok(())
}

pub async fn models(store: CredentialStore, cancel: CancellationToken) -> Result<()> {
    let handle = auth::AuthHandle::selected(store)?;
    for model in super::models::fetch(&handle, &cancel).await? {
        println!("{}\t{}", model.slug, model.display_name);
    }
    Ok(())
}
