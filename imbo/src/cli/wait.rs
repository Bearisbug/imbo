//! `imbo wait` (FLOW-LISTEN): an agent in MiniMax Code, ZCode or Kimi Code runs it in
//! the background. It prints the session's next messages and ends; the
//! tool then starts a turn with them.

use std::io::Write;

use super::Reported;
use crate::adapters;
use crate::local::{Heard, Local};
use crate::ui;

const UNKNOWN: &str = "Imbo can't tell which session this imbo wait belongs to, so it isn't listening. It's only needed in the main session of MiniMax Code, ZCode or Kimi Code, not in subagents.";
const ALREADY: &str = "Imbo: an imbo wait is already waiting for messages in the background for this session, so this one exits. No need to run it again.";

pub async fn wait() -> anyhow::Result<()> {
    let target = adapters::all()
        .iter()
        .filter(|a| a.listens())
        .find_map(|a| a.wait_target().map(|t| (*a, t)));
    let Some((a, (session_id, pid))) = target else {
        ui::error_line(UNKNOWN);
        return Err(Reported.into());
    };
    let show = |text: &str| {
        let mut stdout = std::io::stdout();
        let _ = writeln!(stdout, "{text}");
        let _ = stdout.flush();
        true
    };
    match Local::new().listen(a.id(), &session_id, pid, show).await {
        Ok(Heard::Already) => println!("{ALREADY}"),
        Ok(Heard::Messages | Heard::Nothing) => {}
        Err(p) => return Err(super::report(&p)),
    }
    Ok(())
}
