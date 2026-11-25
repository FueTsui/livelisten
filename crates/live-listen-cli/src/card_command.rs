use super::{Args, Failure};

#[derive(Default)]
pub(super) struct Options {
    pub kind: Option<String>,
    pub date: Option<String>,
    pub text: Option<String>,
    pub stdin: bool,
}

impl Options {
    pub(super) fn used(&self) -> bool {
        self.kind.is_some() || self.date.is_some() || self.text.is_some() || self.stdin
    }

    #[cfg(windows)]
    fn kind(&self) -> Result<&str, Failure> {
        match self.kind.as_deref() {
            Some(kind @ ("note" | "todo")) => Ok(kind),
            _ => Err(Failure::Usage("cards-add requires --kind note or --kind todo".into())),
        }
    }
}

#[cfg(windows)]
pub(super) fn run(args: &Args) -> Result<(), Failure> {
    use std::io::{self, IsTerminal, Read};
    use std::sync::mpsc;
    use std::time::Duration;

    use live_listen_core::paths::AppDirs;
    use live_listen_core::win::cards_pipe::{self, MAX_REQUEST_BYTES};
    use serde_json::json;

    // Includes the connection wait and the desktop core's existing reply deadline.
    const REPLY_TIMEOUT: Duration = Duration::from_secs(28);
    let kind = args.card.kind()?;
    if let Some(date) = args.card.date.as_deref() {
        validate_date(date)?;
    }
    let text = match (args.card.text.as_deref(), args.card.stdin) {
        (Some(text), false) => text.to_string(),
        (None, true) if !io::stdin().is_terminal() => {
            let mut text = String::new();
            io::stdin()
                .take((MAX_REQUEST_BYTES + 1) as u64)
                .read_to_string(&mut text)
                .map_err(|_| Failure::Usage("could not read UTF-8 text from stdin".into()))?;
            text
        }
        _ => return Err(Failure::Usage("use --text TEXT or pipe UTF-8 text with --stdin".into())),
    };
    if text.trim().is_empty() || text.len() > MAX_REQUEST_BYTES {
        return Err(Failure::Usage("card text is empty or too large".into()));
    }
    let request = serde_json::to_vec(&json!({
        "command": "cards.add", "kind": kind, "date": args.card.date, "text": text.trim(),
    }))
    .map_err(|_| Failure::Usage("invalid card request".into()))?;
    if request.len() > MAX_REQUEST_BYTES {
        return Err(Failure::Usage("card text is too large".into()));
    }

    let dirs = AppDirs::resolve(args.config.as_deref());
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(cards_pipe::add(&dirs.config, &request));
    });
    let reply = receive.recv_timeout(REPLY_TIMEOUT).unwrap_or(Err("card_outcome_unknown")).map_err(runtime_error)?;
    if reply["ok"].as_bool() != Some(true) {
        return Err(runtime_error(reply["error"].as_str().unwrap_or("card_outcome_unknown")));
    }
    let id = reply["id"].as_u64().ok_or_else(|| runtime_error("card_outcome_unknown"))?;
    if args.json {
        println!("{}", super::ascii_json(&json!({"type": "card", "id": id})));
    } else {
        println!("Card #{id} added");
    }
    Ok(())
}

#[cfg(not(windows))]
pub(super) fn run(_: &Args) -> Result<(), Failure> {
    Err(Failure::Runtime("cards-add requires the Windows desktop app".into()))
}

#[cfg(windows)]
fn runtime_error(code: &str) -> Failure {
    let message = match code {
        "card_pipe_unavailable" => "open the matching Live Listen desktop app before adding cards",
        "card_pipe_access_denied" => "the card endpoint is not accessible to this Windows user",
        "card_outcome_unknown" | "timeout" => "result unknown; check the card list before retrying",
        "card_save_failed" => "the app accepted the card but is retrying its save; check the card list before retrying",
        "card_markdown_failed" => "the card was saved; the Markdown export has a pending error",
        "text_too_long" => "the card text exceeds the app's character limit",
        "stopped" => "the desktop app is stopping",
        "bad_request" | "bad_date" | "empty_card" => "the app rejected the card request",
        _ => return Failure::Runtime("card request failed; check the card list before retrying".into()),
    };
    Failure::Runtime(format!("{code}: {message}"))
}

#[cfg(windows)]
fn validate_date(date: &str) -> Result<(), Failure> {
    let invalid = || Failure::Usage("--date must be a valid date in YYYY-MM-DD format (2000–2099)".into());
    let bytes = date.as_bytes();
    if bytes.len() != 10
        || bytes.iter().enumerate().any(
            |(i, byte)| {
                if matches!(i, 4 | 7) { *byte != b'-' } else { !byte.is_ascii_digit() }
            },
        )
    {
        return Err(invalid());
    }
    let year: u32 = date[..4].parse().map_err(|_| invalid())?;
    let month: u32 = date[5..7].parse().map_err(|_| invalid())?;
    let day: u32 = date[8..].parse().map_err(|_| invalid())?;
    let days = match month {
        2 if year % 4 == 0 => 29, // All leap years in the accepted interval.
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => 0,
    };
    if (2000..2100).contains(&year) && (1..=days).contains(&day) { Ok(()) } else { Err(invalid()) }
}
