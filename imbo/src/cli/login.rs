//! `imbo login`: email code sign-in (FLOW-ONBOARD S2–S5).

use clap::Args;
use imbo_proto::api::{CodeResponse, VerifyResponse};
use imbo_proto::text::{valid_email, validate_name};
use imbo_proto::{ErrorCode, Problem};
use serde_json::json;

use super::Reported;
use crate::local::Local;
use crate::net;
use crate::paths::{Config, Credentials, Home, hostname, platform};
use crate::ui;

#[derive(Args)]
pub struct LoginArgs {
    /// Server to sign in to (default https://api.imbo.dev).
    #[arg(long)]
    pub server: Option<String>,
    /// Email address; asked for when missing.
    #[arg(long)]
    pub email: Option<String>,
}

pub async fn login(a: LoginArgs) -> anyhow::Result<()> {
    let home = Home::resolve();
    need_terminal("imbo login")?;
    cliclack::intro(ui::heading("imbo login"))?;
    let server = choose_server(&home, a.server)?;
    let creds = sign_in(&home, &server, a.email).await?;
    reload_daemon().await;
    cliclack::outro(format!(
        "Signed in as {} {}",
        ui::heading(&creds.name),
        ui::dim(format!("· account ID {}", creds.account_id))
    ))?;
    Ok(())
}

pub fn need_terminal(what: &str) -> anyhow::Result<()> {
    if !ui::is_interactive() {
        ui::error_line(&format!("{what} asks questions, so it needs a terminal."));
        eprintln!(
            "  {}",
            ui::dim(
                "Run it in Terminal (if piped from curl, the installer reads answers from /dev/tty)."
            )
        );
        return Err(Reported.into());
    }
    Ok(())
}

/// Use `--server` if given (and remember it), else the configured server.
pub fn choose_server(home: &Home, flag: Option<String>) -> anyhow::Result<String> {
    match flag {
        Some(s) => {
            let s = s.trim().trim_end_matches('/').to_string();
            home.save_config(&Config { server: s.clone() })?;
            Ok(s)
        }
        None => Ok(home.config().server),
    }
}

pub async fn reload_daemon() {
    let _ = Local::new()
        .send(&imbo_proto::local::LocalRequest::Reload)
        .await;
}

fn ask_email(initial: Option<String>) -> anyhow::Result<String> {
    if let Some(e) = initial.filter(|e| valid_email(e.trim())) {
        cliclack::log::step(format!("Email  {}", ui::heading(e.trim())))?;
        return Ok(e.trim().to_lowercase());
    }
    let e: String = cliclack::input("Email")
        .placeholder("you@example.com")
        .validate(|s: &String| {
            if valid_email(s.trim()) {
                Ok(())
            } else {
                Err("That doesn't look like an email address.")
            }
        })
        .interact()?;
    Ok(e.trim().to_lowercase())
}

/// Ask the server to email a code. Returns false when the email must be re-entered.
async fn send_code(client: &net::HttpClient, server: &str, email: &str) -> anyhow::Result<bool> {
    let sp = cliclack::spinner();
    sp.start(format!("Sending a code to {email}"));
    let r = net::request(
        client,
        server,
        "POST",
        "/v1/auth/code",
        None,
        None,
        Some(&json!({ "email": email })),
    )
    .await;
    match r {
        Ok(res) => {
            let _ = serde_json::from_value::<CodeResponse>(res.body);
            sp.stop(format!(
                "Code sent to {}  {}",
                ui::heading(email),
                ui::dim("check your inbox, and spam")
            ));
            Ok(true)
        }
        Err(p) if p.is(ErrorCode::RateLimited) => {
            sp.stop(format!(
                "A code was sent to {email} less than a minute ago  {}",
                ui::dim("use that one")
            ));
            Ok(true)
        }
        Err(p) if p.is(ErrorCode::InvalidEmail) => {
            sp.error(p.detail.clone());
            Ok(false)
        }
        Err(p) => {
            sp.error(&p.title);
            Err(fail(&p))
        }
    }
}

fn fail(p: &Problem) -> anyhow::Error {
    cliclack::outro_cancel(&p.detail).ok();
    if let Some(id) = p.request_id() {
        eprintln!(
            "  {}",
            ui::dim(format!("ref {}", &id[id.len().saturating_sub(8)..]))
        );
    }
    Reported.into()
}

fn ask_code() -> anyhow::Result<String> {
    let c: String = cliclack::input("Code from the email")
        .placeholder("6 digits")
        .validate(|s: &String| {
            let t = s.trim();
            if t.len() == 6 && t.chars().all(|c| c.is_ascii_digit()) {
                Ok(())
            } else {
                Err("The code is 6 digits.")
            }
        })
        .interact()?;
    Ok(c.trim().to_string())
}

fn ask_name() -> anyhow::Result<String> {
    let n: String = cliclack::input("Your name")
        .placeholder("what people and agents will see, e.g. alice")
        .validate(|s: &String| validate_name(s).map(|_| ()))
        .interact()?;
    Ok(validate_name(&n).unwrap_or(n))
}

/// The whole sign-in conversation. Saves and returns the credentials.
pub async fn sign_in(
    home: &Home,
    server: &str,
    email: Option<String>,
) -> anyhow::Result<Credentials> {
    let client = net::http_client(server);
    if server != crate::paths::DEFAULT_SERVER {
        cliclack::log::remark(format!("Server  {}", ui::dim(server)))?;
    }
    let mut email_arg = email;
    let email = loop {
        let e = ask_email(email_arg.take())?;
        if send_code(&client, server, &e).await? {
            break e;
        }
    };
    let mut name: Option<String> = None;
    let mut code = ask_code()?;
    let resp = loop {
        let body = json!({
            "email": email,
            "code": code,
            "name": name,
            "device_name": hostname(),
            "platform": platform(),
            "client_version": crate::VERSION,
        });
        let sp = cliclack::spinner();
        sp.start("Checking the code");
        match net::request(
            &client,
            server,
            "POST",
            "/v1/auth/verify",
            None,
            None,
            Some(&body),
        )
        .await
        {
            Ok(res) => {
                sp.clear();
                break serde_json::from_value::<VerifyResponse>(res.body)?;
            }
            Err(p) if p.is(ErrorCode::NameRequired) => {
                sp.clear();
                cliclack::log::info(
                    "New here. Pick a name; people add you by your account ID, and see this name.",
                )?;
                name = Some(ask_name()?);
            }
            Err(p) if p.is(ErrorCode::InvalidName) => {
                sp.error(p.detail.clone());
                name = Some(ask_name()?);
            }
            Err(p) if p.is(ErrorCode::CodeInvalid) => {
                let left = p
                    .attempts_left
                    .map(|n| format!(" · {n} tries left"))
                    .unwrap_or_default();
                sp.error(format!("Wrong code{left}"));
                code = ask_code()?;
            }
            Err(p) if p.is(ErrorCode::CodeExpired) || p.is(ErrorCode::CodeLocked) => {
                sp.error(p.detail.clone());
                let again: bool = cliclack::confirm("Send a new code?")
                    .initial_value(true)
                    .interact()?;
                if !again {
                    cliclack::outro_cancel("Not signed in.")?;
                    return Err(Reported.into());
                }
                send_code(&client, server, &email).await?;
                code = ask_code()?;
            }
            Err(p) => {
                sp.error(&p.title);
                return Err(fail(&p));
            }
        }
    };
    let creds = Credentials {
        server: server.to_string(),
        token: resp.token,
        device_id: resp.device_id.to_string(),
        account_id: resp.account.id,
        name: resp.account.name.clone(),
        email: resp.account.email.unwrap_or(email),
    };
    home.save_credentials(&creds)?;
    if resp.new_account {
        cliclack::log::success(format!(
            "Account created  {}  {}",
            ui::heading(&creds.name),
            ui::s(ui::ok(), format!("ID {}", creds.account_id))
        ))?;
    } else {
        cliclack::log::success(format!("Welcome back, {}", ui::heading(&creds.name)))?;
    }
    Ok(creds)
}
