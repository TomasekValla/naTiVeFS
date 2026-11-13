//! Tiny local-socket IPC: the first naTiVeFS process listens, later launches
//! (or the link router) send it a message and exit. Blocking and threaded on
//! purpose, so an idle app costs nothing.
//!
//! An instance answers `ok` once the message has been handled on its UI thread, so a sender that needs to know
//! (the link router) can tell a working instance from a stuck one (`send_ack`). Older instances never answer;
//! they just close the connection, which `send_ack` reports as `Delivery::Unconfirmed`.

use std::io::{self, BufRead, BufReader, Write};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use interprocess::local_socket::{prelude::*, GenericNamespaced, ListenerOptions, Stream};
use serde::{Deserialize, Serialize};

const ACK: &str = "ok";

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

/// What `send_ack` learned about the delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// The instance handled the message on its UI thread.
    Handled,
    /// The instance read the message and closed the connection without answering (an older version).
    Unconfirmed,
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

/// Windows lets only the foreground process (or a process it just started) bring a window to the front. The
/// process that receives a link or a file is usually in the background, so whoever hands work over passes its own
/// right on first; otherwise the window only flashes in the taskbar. Harmless when this process has no such right.
/// No-op on other systems.
pub fn allow_foreground() {
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, ASFW_ANY};
        // SAFETY: plain Win32 call without pointers
        let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
    }
}

fn connect_and_write(msg: &Message) -> io::Result<Stream> {
    allow_foreground();
    let name = socket_name().to_ns_name::<GenericNamespaced>()?;
    let mut stream = Stream::connect(name)?;
    let mut line = serde_json::to_string(msg).map_err(io::Error::other)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()?;
    Ok(stream)
}

/// Send a message to the running instance. `Err` = no instance is listening.
pub fn send(msg: &Message) -> io::Result<()> {
    connect_and_write(msg).map(drop)
}

/// Send a message and wait until the instance has handled it. `Err` with `ErrorKind::TimedOut` = an instance took
/// the message but did not handle it in time (stuck); any other `Err` = no instance is listening.
pub fn send_ack(msg: &Message, timeout: Duration) -> io::Result<Delivery> {
    let stream = connect_and_write(msg)?;
    let (tx, rx) = mpsc::channel();
    // reading has no timeout on every platform's local sockets: read on a helper thread and stop waiting instead
    std::thread::spawn(move || {
        let mut reply = String::new();
        let _ = tx.send(BufReader::new(stream).read_line(&mut reply).map(|_| reply));
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(reply)) if reply.trim() == ACK => Ok(Delivery::Handled),
        Ok(Ok(_)) => Ok(Delivery::Unconfirmed),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "the running naTiVeFS did not answer")),
    }
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

/// The answer channel of one received message. Call `ok` once the message has been handled; dropping it without
/// `ok` closes the connection unanswered.
pub struct Reply(Option<Stream>);

impl Reply {
    pub fn ok(mut self) {
        if let Some(mut s) = self.0.take() {
            // flushing a pipe waits for the reader: never on the caller's (UI) thread
            std::thread::spawn(move || {
                let _ = s.write_all(format!("{ACK}\n").as_bytes());
                let _ = s.flush();
            });
        }
    }
}

/// Start handling messages on a claimed socket. The handler runs on the IPC thread.
pub fn serve<F>(listener: Listener, handler: F) -> JoinHandle<()>
where
    F: Fn(Message, Reply) + Send + 'static,
{
    std::thread::spawn(move || {
        for conn in listener.0.incoming().filter_map(Result::ok) {
            let mut line = String::new();
            let mut reader = BufReader::new(conn);
            if reader.read_line(&mut line).is_ok() {
                if let Ok(msg) = serde_json::from_str::<Message>(line.trim()) {
                    handler(msg, Reply(Some(reader.into_inner())));
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // one test: every case shares the single per-user socket name
    #[test]
    fn roundtrip_and_acks() {
        let (tx, rx) = mpsc::channel();
        assert!(send(&Message::Show).is_err(), "no instance should be running in tests");
        assert!(send_ack(&Message::Ping, Duration::from_secs(1)).is_err());
        let l = listen().unwrap();
        assert!(listen().is_err(), "a second instance must not claim the socket");
        let held = Mutex::new(Vec::new());
        let _t = serve(l, move |m, reply| {
            match &m {
                Message::Ping => reply.ok(),
                Message::Show => drop(reply),            // like an older instance
                _ => held.lock().unwrap().push(reply), // like a stuck UI thread
            }
            tx.send(m).unwrap();
        });

        send(&Message::Open("https://files.tomasekvalla.cz/files/audio/a.flac".into())).unwrap();
        let got = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(got, Message::Open("https://files.tomasekvalla.cz/files/audio/a.flac".into()));

        assert_eq!(send_ack(&Message::Ping, Duration::from_secs(5)).unwrap(), Delivery::Handled);
        assert_eq!(send_ack(&Message::Show, Duration::from_secs(5)).unwrap(), Delivery::Unconfirmed);
        let stuck = send_ack(&Message::Quit, Duration::from_millis(300)).unwrap_err();
        assert_eq!(stuck.kind(), io::ErrorKind::TimedOut);
    }
}
