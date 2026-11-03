//! Media keys and the system media overlay for the audio player: Windows System Media Transport Controls
//! (the volume flyout / lock screen card, keyboard media keys, Bluetooth headset buttons) and Linux MPRIS
//! (GNOME / KDE media controls, media keys through the desktop); elsewhere this does nothing.

/// A button the system pressed for us.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Button {
    Play,
    Pause,
    Stop,
    /// Jump to this position (seconds), from the overlay's timeline.
    Seek(f64),
}

pub type OnButton = Box<dyn Fn(Button) + Send + Sync>;

#[cfg(target_os = "linux")]
pub use linux::Session;
#[cfg(windows)]
pub use win::Session;

#[cfg(not(any(windows, target_os = "linux")))]
pub struct Session;

#[cfg(not(any(windows, target_os = "linux")))]
impl Session {
    pub fn new(_window: &slint::Window, _on: OnButton) -> Option<Self> {
        None
    }
    pub fn set_track(&self, _title: &str, _artist: &str) {}
    pub fn set_playing(&self, _playing: Option<bool>) {}
    pub fn set_timeline(&self, _pos: f64, _total: f64) {}
}

#[cfg(windows)]
mod win {
    use super::{Button, OnButton};
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::winit_030::WinitWindowAccessor;
    use windows::core::HSTRING;
    use windows::Foundation::{TimeSpan, TypedEventHandler};
    use windows::Media::{
        MediaPlaybackStatus, MediaPlaybackType, PlaybackPositionChangeRequestedEventArgs, SystemMediaTransportControls,
        SystemMediaTransportControlsButton, SystemMediaTransportControlsButtonPressedEventArgs,
        SystemMediaTransportControlsTimelineProperties,
    };
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;

    /// The audio window's entry in the system media overlay; removed when dropped.
    pub struct Session {
        smtc: SystemMediaTransportControls,
        tokens: [i64; 2],
    }

    fn ticks(secs: f64) -> TimeSpan {
        TimeSpan { Duration: (secs.max(0.0) * 10_000_000.0) as i64 }
    }

    impl Session {
        /// Register `window` with the overlay; `on` is called (on a system thread) for every button.
        pub fn new(window: &slint::Window, on: OnButton) -> Option<Self> {
            let hwnd = window
                .with_winit_window(|w| match w.window_handle().ok()?.as_raw() {
                    RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
                    _ => None,
                })
                .flatten()?;
            let interop = windows::core::factory::<SystemMediaTransportControls, ISystemMediaTransportControlsInterop>().ok()?;
            // SAFETY: the window handle is valid while the Slint window exists; the session is dropped with it.
            let smtc: SystemMediaTransportControls = unsafe { interop.GetForWindow(HWND(hwnd as *mut _)) }.ok()?;
            let _ = smtc.SetIsEnabled(true);
            let _ = smtc.SetIsPlayEnabled(true);
            let _ = smtc.SetIsPauseEnabled(true);
            let _ = smtc.SetIsStopEnabled(true);
            let on = std::sync::Arc::new(on);
            let on2 = on.clone();
            let t1 = smtc
                .ButtonPressed(&TypedEventHandler::<SystemMediaTransportControls, SystemMediaTransportControlsButtonPressedEventArgs>::new(move |_, args| {
                    if let Some(args) = args.as_ref() {
                        match args.Button()? {
                            SystemMediaTransportControlsButton::Play => on(Button::Play),
                            SystemMediaTransportControlsButton::Pause => on(Button::Pause),
                            SystemMediaTransportControlsButton::Stop => on(Button::Stop),
                            _ => {}
                        }
                    }
                    Ok(())
                }))
                .unwrap_or(0);
            let t2 = smtc
                .PlaybackPositionChangeRequested(&TypedEventHandler::<SystemMediaTransportControls, PlaybackPositionChangeRequestedEventArgs>::new(move |_, args| {
                    if let Some(args) = args.as_ref() {
                        on2(Button::Seek(args.RequestedPlaybackPosition()?.Duration as f64 / 10_000_000.0));
                    }
                    Ok(())
                }))
                .unwrap_or(0);
            Some(Self { smtc, tokens: [t1, t2] })
        }

        pub fn set_track(&self, title: &str, artist: &str) {
            let Ok(up) = self.smtc.DisplayUpdater() else { return };
            let _ = up.SetType(MediaPlaybackType::Music);
            if let Ok(m) = up.MusicProperties() {
                let _ = m.SetTitle(&HSTRING::from(title));
                let _ = m.SetArtist(&HSTRING::from(artist));
            }
            let _ = up.Update();
        }

        /// `None` = stopped (nothing open / at the start).
        pub fn set_playing(&self, playing: Option<bool>) {
            let status = match playing {
                Some(true) => MediaPlaybackStatus::Playing,
                Some(false) => MediaPlaybackStatus::Paused,
                None => MediaPlaybackStatus::Stopped,
            };
            let _ = self.smtc.SetPlaybackStatus(status);
        }

        pub fn set_timeline(&self, pos: f64, total: f64) {
            let Ok(t) = SystemMediaTransportControlsTimelineProperties::new() else { return };
            let _ = t.SetStartTime(ticks(0.0));
            let _ = t.SetEndTime(ticks(total));
            let _ = t.SetMinSeekTime(ticks(0.0));
            let _ = t.SetMaxSeekTime(ticks(total));
            let _ = t.SetPosition(ticks(pos.min(total)));
            let _ = self.smtc.UpdateTimelineProperties(&t);
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            let _ = self.smtc.RemoveButtonPressed(self.tokens[0]);
            let _ = self.smtc.RemovePlaybackPositionChangeRequested(self.tokens[1]);
            if let Ok(up) = self.smtc.DisplayUpdater() {
                let _ = up.ClearAll();
            }
            let _ = self.smtc.SetIsEnabled(false);
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::rc::Rc;
    use std::sync::mpsc::{self, TryRecvError};
    use std::thread::JoinHandle;
    use std::time::Duration;

    use mpris_server::{Metadata, PlaybackStatus, Player, Time, TrackId};

    use super::{Button, OnButton};

    enum Cmd {
        Track(String, String),
        Playing(Option<bool>),
        Timeline(f64, f64),
    }

    /// An MPRIS player on the session bus (`org.mpris.MediaPlayer2.naTiVeFS.instance<pid>`), served from its own
    /// thread; removed when dropped.
    pub struct Session {
        tx: Option<mpsc::Sender<Cmd>>,
        thread: Option<JoinHandle<()>>,
    }

    fn micros(secs: f64) -> Time {
        Time::from_micros((secs.max(0.0) * 1_000_000.0) as i64)
    }

    impl Session {
        pub fn new(_window: &slint::Window, on: OnButton) -> Option<Self> {
            let (tx, rx) = mpsc::channel::<Cmd>();
            let thread = std::thread::Builder::new()
                .name("nativefs-mpris".into())
                .spawn(move || {
                    let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
                    let local = tokio::task::LocalSet::new();
                    local.block_on(&rt, serve(on, rx));
                })
                .ok()?;
            Some(Self { tx: Some(tx), thread: Some(thread) })
        }

        fn send(&self, c: Cmd) {
            if let Some(tx) = &self.tx {
                let _ = tx.send(c);
            }
        }

        pub fn set_track(&self, title: &str, artist: &str) {
            self.send(Cmd::Track(title.to_owned(), artist.to_owned()));
        }

        pub fn set_playing(&self, playing: Option<bool>) {
            self.send(Cmd::Playing(playing));
        }

        pub fn set_timeline(&self, pos: f64, total: f64) {
            self.send(Cmd::Timeline(pos, total));
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            // the closed channel ends the server loop
            self.tx = None;
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    async fn serve(on: OnButton, rx: mpsc::Receiver<Cmd>) {
        let Ok(player) = Player::builder(&format!("naTiVeFS.instance{}", std::process::id()))
            .identity("naTiVeFS")
            .desktop_entry(crate::desktop_entry::APP_ID)
            .can_play(true)
            .can_pause(true)
            .can_seek(true)
            .can_control(true)
            .build()
            .await
        else {
            return;
        };
        let on = Rc::new(on);
        {
            let on = on.clone();
            player.connect_play(move |_| on(Button::Play));
        }
        {
            let on = on.clone();
            player.connect_pause(move |_| on(Button::Pause));
        }
        {
            let on = on.clone();
            player.connect_play_pause(move |p| on(if p.playback_status() == PlaybackStatus::Playing { Button::Pause } else { Button::Play }));
        }
        {
            let on = on.clone();
            player.connect_stop(move |_| on(Button::Stop));
        }
        {
            let on = on.clone();
            player.connect_set_position(move |_, _, t| on(Button::Seek(t.as_micros() as f64 / 1_000_000.0)));
        }
        {
            let on = on.clone();
            player.connect_seek(move |p, offset| on(Button::Seek((p.position().as_micros() + offset.as_micros()) as f64 / 1_000_000.0)));
        }
        tokio::task::spawn_local(player.run());
        let track = TrackId::try_from("/org/nativefs/track/current").ok();
        let (mut title, mut artist, mut total) = (String::new(), String::new(), 0.0);
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            loop {
                match rx.try_recv() {
                    Ok(Cmd::Track(t, a)) => {
                        (title, artist) = (t, a);
                    }
                    Ok(Cmd::Playing(p)) => {
                        let status = match p {
                            Some(true) => PlaybackStatus::Playing,
                            Some(false) => PlaybackStatus::Paused,
                            None => PlaybackStatus::Stopped,
                        };
                        let _ = player.set_playback_status(status).await;
                        continue;
                    }
                    Ok(Cmd::Timeline(pos, len)) => {
                        player.set_position(micros(pos));
                        let _ = player.seeked(micros(pos)).await;
                        if (len - total).abs() < 1e-3 {
                            continue;
                        }
                        total = len;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
                let mut m = Metadata::builder().title(title.clone()).artist([artist.clone()]).length(micros(total));
                if let Some(id) = &track {
                    m = m.trackid(id.clone());
                }
                let _ = player.set_metadata(m.build()).await;
            }
        }
    }
}
