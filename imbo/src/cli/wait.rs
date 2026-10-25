//! `imbo wait` (FLOW-LISTEN): an agent in MiniMax Code or ZCode runs it in
//! the background. It prints the session's next messages and ends; the
//! tool then starts a turn with them.

use std::io::Write;

use super::Reported;
use crate::adapters;
use crate::local::{Heard, Local};
use crate::ui;

const UNKNOWN: &str = "Imbo 认不出这个 imbo wait 属于哪个会话，没有开始监听。它只在 MiniMax Code、ZCode 的主会话里有用，子 agent 里不用运行。";
const ALREADY: &str = "Imbo：这个会话已经有一个 imbo wait 在后台等消息，这个直接退出，不用再运行。";

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
