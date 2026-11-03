//! Tiny local-socket IPC: the first naTiVeFS process listens, later launches
//! (or the link router) send it a message and exit. Blocking and threaded on
//! purpose, so an idle app costs nothing.

use std::io::{self, BufRead, BufReader, Write};
use std::thread::JoinHandle;

use interprocess::local_socket::{prelude::*, GenericNamespaced, ListenerOptions, Stream};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Message {
    /// Only check that an instance is alive (used by silent autostart).
    Ping,
    /// Bring the main window to the front.
    Show,
    /// Open a TVFS link (`https://...` or `tvfs://...`).
    Open(String),
    /// Upload these local files (used by the OS context/share menus).
    Upload(Vec<String>),
    /// Open a local file in its viewer ("Open locally in naTiVeFS" in the file manager).
    OpenLocal(String),
    Quit,
}

fn socket_name() -> String {
    let user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default();
    // debug builds get their own instance, so testing never hands over to (or drives) an installed release
    if cfg!(debug_assertions) {
        format!("nativefs-ipc-dev-{user}")
    } else {
        format!("nativefs-ipc-{user}")
    }
}

/// Send a message to the running instance. `Err` = no instance is listening.
pub fn send(msg: &Message) -> io::Result<()> {
    let name = socket_name().to_ns_name::<GenericNamespaced>()?;
    let mut stream = Stream::connect(name)?;
    let mut line = serde_json::to_string(msg).map_err(io::Error::other)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()
}

/// The claimed instance socket (see `listen`).
pub struct Listener(interprocess::local_socket::Listener);

/// Claim the instance socket right away, before the slow start-up. Fails when another instance already holds it
/// (Windows: first pipe instance; Linux: abstract socket in use), so of several processes started at once (a file
/// manager menu on many files) exactly one becomes the instance and the others hand their message over with
/// `send`. Connections that arrive before `serve` wait in the socket.
pub fn listen() -> io::Result<Listener> {
    let name = socket_name().to_ns_name::<GenericNamespaced>()?;
    // no overwrite: taking over a live socket would split the instances
    Ok(Listener(ListenerOptions::new().name(name).create_sync()?))
}

/// Start handling messages on a claimed socket. The handler runs on the IPC thread.
pub fn serve<F>(listener: Listener, handler: F) -> JoinHandle<()>
where
    F: Fn(Message) + Send + 'static,
{
    std::thread::spawn(move || {
        for conn in listener.0.incoming().filter_map(Result::ok) {
            let mut line = String::new();
            if BufReader::new(conn).read_line(&mut line).is_ok() {
                if let Ok(msg) = serde_json::from_str::<Message>(line.trim()) {
                    handler(msg);
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn roundtrip() {
        let (tx, rx) = mpsc::channel();
        assert!(send(&Message::Show).is_err(), "no instance should be running in tests");
        let l = listen().unwrap();
        assert!(listen().is_err(), "a second instance must not claim the socket");
        let _t = serve(l, move |m| tx.send(m).unwrap());
        send(&Message::Open("https://files.tomasekvalla.cz/files/audio/a.flac".into())).unwrap();
        let got = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(got, Message::Open("https://files.tomasekvalla.cz/files/audio/a.flac".into()));
    }
}
