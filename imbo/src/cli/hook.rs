//! `imbo hook …`: what the hooks Imbo installs in the tools run.
//! `imbo hook prompt` is Claude Code's; the others are `imbo hook <tool>
//! <event>`.

use crate::adapters;

pub async fn hook(args: Vec<String>) -> anyhow::Result<()> {
    match args.as_slice() {
        [event] => adapters::claude::prompt_hook(event.clone()).await,
        [tool, event] => {
            let Some(a) = adapters::get(tool) else {
                return Ok(());
            };
            let input = adapters::read_stdin();
            if let Some(out) = a.hook(event, &input).await {
                use std::io::Write;
                let mut stdout = std::io::stdout();
                let _ = stdout.write_all(out.as_bytes());
                let _ = stdout.flush();
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
