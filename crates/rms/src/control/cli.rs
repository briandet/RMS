//! The line-based CLI on port 9090 (`Slim::Plugin::CLI::Plugin`, `Slim::Control::Stdio`).

use super::request::{Request, Source, Status};
use crate::server::Server;
use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// `URI::Escape::uri_escape_utf8` default: everything but `A-Za-z0-9-_.~`.
const CLI_ESCAPE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

pub fn escape(s: &str) -> String {
    utf8_percent_encode(s, CLI_ESCAPE).to_string()
}

pub fn unescape(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

/// `array_to_string`: optional client id prefix plus escaped elements.
pub fn render_line(client: Option<&str>, words: &[String]) -> String {
    let mut out: Vec<String> = Vec::with_capacity(words.len() + 1);
    if let Some(c) = client {
        out.push(escape(c));
    }
    out.extend(words.iter().map(|w| escape(w)));
    out.join(" ")
}

static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

pub async fn listen(server: Arc<Server>) -> anyhow::Result<()> {
    let addr = format!("{}:{}", server.config.bind_addr, server.cli_port());
    let listener = TcpListener::bind(&addr).await?;
    info!("CLI listening on {}", addr);
    loop {
        let (sock, peer) = listener.accept().await?;
        let server = server.clone();
        tokio::spawn(async move {
            debug!("CLI connection from {}", peer);
            if let Err(e) = handle(server, sock).await {
                debug!("CLI connection {} ended: {}", peer, e);
            }
        });
    }
}

enum Subscribe {
    None,
    All,
    Some(Vec<String>),
}

async fn handle(server: Arc<Server>, sock: TcpStream) -> anyhow::Result<()> {
    let conn_id = NEXT_CONN.fetch_add(1, Ordering::SeqCst);
    let cnx = format!("cli:{}", conn_id);
    let (mut rd, mut wr) = sock.into_split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if wr.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
    });
    let mut authed = !server.prefs.server.get_bool("authorize");
    let mut terminator = "\n".to_string();
    let subscribe = Arc::new(parking_lot::Mutex::new(Subscribe::None));
    let term_shared = Arc::new(parking_lot::Mutex::new(terminator.clone()));

    // notification echo task
    let notif = {
        let server = server.clone();
        let tx = tx.clone();
        let subscribe = subscribe.clone();
        let term_shared = term_shared.clone();
        tokio::spawn(async move {
            let mut nrx = server.notifier.subscribe();
            loop {
                let n = match nrx.recv().await {
                    Ok(n) => n,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => return,
                };
                if n.source == Source::Cli(conn_id) {
                    continue;
                }
                let send = match &*subscribe.lock() {
                    Subscribe::None => false,
                    Subscribe::All => true,
                    Subscribe::Some(list) => n.first_in(list),
                };
                if send {
                    let line = render_line(n.player.map(|p| p.to_string()).as_deref(), &n.words);
                    let t = term_shared.lock().clone();
                    if tx.send(format!("{}{}", line, t)).is_err() {
                        return;
                    }
                }
            }
        })
    };

    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    'outer: loop {
        let n = rd.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        // a line ends at any run of CR, LF or NUL
        while let Some(pos) = buf.iter().position(|&b| b == b'\r' || b == b'\n' || b == 0) {
            let mut end = pos;
            while end < buf.len() && matches!(buf[end], b'\r' | b'\n' | 0) {
                end += 1;
            }
            // only treat as complete if we saw the full terminator run (or more data after it)
            let line = String::from_utf8_lossy(&buf[..pos]).to_string();
            let term = String::from_utf8_lossy(&buf[pos..end]).to_string();
            buf.drain(..end);
            terminator = term;
            *term_shared.lock() = terminator.clone();
            match process_line(
                &server,
                &line,
                conn_id,
                &cnx,
                &mut authed,
                &subscribe,
                &tx,
                &terminator,
            )
            .await
            {
                LineResult::Continue => {}
                LineResult::Close => break 'outer,
            }
        }
    }
    notif.abort();
    server.subscriptions.unregister_connection(&cnx);
    drop(tx);
    let _ = writer.await;
    Ok(())
}

enum LineResult {
    Continue,
    Close,
}

#[allow(clippy::too_many_arguments)]
async fn process_line(
    server: &Arc<Server>,
    line: &str,
    conn_id: u64,
    cnx: &str,
    authed: &mut bool,
    subscribe: &Arc<parking_lot::Mutex<Subscribe>>,
    tx: &mpsc::UnboundedSender<String>,
    term: &str,
) -> LineResult {
    let mut tokens: Vec<String> = line.split_whitespace().map(unescape).collect();
    if tokens.is_empty() {
        return LineResult::Continue;
    }
    // player prefix
    let mut client = server.resolve_client(&tokens[0]);
    let mut client_raw: Option<String> = None;
    // a known player id, or any id in front of `menu` (Jive menus for disconnected players)
    if client.is_some() || (tokens.len() > 1 && tokens[1] == "menu") {
        client_raw = Some(tokens.remove(0));
    }
    if tokens.is_empty() {
        return LineResult::Continue;
    }
    let send = |words: Vec<String>, client: Option<String>| {
        let _ = tx.send(format!(
            "{}{}",
            render_line(client.as_deref(), &words),
            term
        ));
    };
    match tokens[0].as_str() {
        "login" => {
            let user = tokens.get(1).cloned().unwrap_or_default();
            let pass = tokens.get(2).cloned().unwrap_or_default();
            if !*authed {
                let ok = user == server.prefs.server.get_str("username")
                    && crate::web::auth::check_password(
                        &pass,
                        &server.prefs.server.get_str("password"),
                    );
                send(vec!["login".into(), user, "******".into()], None);
                if !ok {
                    warn!("CLI login failed");
                    return LineResult::Close;
                }
                *authed = true;
            } else {
                send(vec!["login".into(), user, "******".into()], None);
            }
            return LineResult::Continue;
        }
        _ if !*authed => {
            send(tokens, None);
            return LineResult::Close;
        }
        "exit" => {
            send(tokens, None);
            return LineResult::Close;
        }
        "shutdown" => {
            send(tokens, None);
            let s = server.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                s.shutdown.cancel();
            });
            return LineResult::Close;
        }
        "listen" => {
            let mut sub = subscribe.lock();
            match tokens.get(1).map(String::as_str) {
                Some("?") => {
                    let on = !matches!(*sub, Subscribe::None);
                    drop(sub);
                    send(vec!["listen".into(), (on as i32).to_string()], None);
                    return LineResult::Continue;
                }
                Some("0") => *sub = Subscribe::None,
                Some("1") => *sub = Subscribe::All,
                _ => {
                    *sub = if matches!(*sub, Subscribe::None) {
                        Subscribe::All
                    } else {
                        Subscribe::None
                    };
                }
            }
            drop(sub);
            send(tokens, None);
            return LineResult::Continue;
        }
        "subscribe" => {
            if let Some(list) = tokens.get(1) {
                *subscribe.lock() = Subscribe::Some(list.split(',').map(str::to_string).collect());
            }
            send(tokens, None);
            return LineResult::Continue;
        }
        _ => {}
    }
    let connected = client.as_ref().map(|p| p.is_connected()).unwrap_or(false);
    let mut req = server.dispatch.parse(
        client.as_ref().map(|p| p.id),
        client_raw.as_deref(),
        connected,
        &tokens,
    );
    if req.status == Status::NeedsClient {
        // CLI fallback: a random connected player
        if let Some(p) = server.connected_players().into_iter().next() {
            req = server.dispatch.parse(Some(p.id), None, true, &tokens);
            client = Some(p);
        }
    }
    req.source = Source::Cli(conn_id);
    let echo_client = client
        .as_ref()
        .map(|p| p.id.to_string())
        .or(req.disconnected_client.clone());
    let req = if req.status == Status::Dispatchable {
        super::dispatch::execute(server, req).await
    } else {
        req
    };
    if req.status == Status::NotDispatchable || req.status == Status::NeedsClient {
        debug!(
            "CLI: unknown or missing client -- echoing as is: {:?}",
            tokens
        );
    }
    if req.is_query && req.param("subscribe").is_some() {
        let tx2 = tx.clone();
        let term2 = term.to_string();
        let ec = echo_client.clone();
        let sink: super::subscribe::Sink = Arc::new(move |r: Request| {
            tx2.send(format!(
                "{}{}",
                render_line(ec.as_deref(), &r.render_as_array()),
                term2
            ))
            .is_ok()
        });
        server.subscriptions.register(server, cnx, &req, sink);
    }
    let words = if req.terms.is_empty() {
        tokens
    } else {
        req.render_as_array()
    };
    send(words, echo_client);
    LineResult::Continue
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping() {
        assert_eq!(escape("00:04:20:06:00:01"), "00%3A04%3A20%3A06%3A00%3A01");
        assert_eq!(escape("playlist index"), "playlist%20index");
        assert_eq!(escape("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(unescape("Hello%20World"), "Hello World");
        assert_eq!(
            render_line(None, &["version".into(), "9.2.0".into()]),
            "version 9.2.0"
        );
    }
}
