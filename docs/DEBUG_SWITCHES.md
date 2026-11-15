# Debug switches

Environment variables for testing. **Debug builds only**: the app reads them through `crates/nativefs-app/src/debug.rs`
(`debug::var` / `debug::flag`), which returns "unset" in release builds. Add new switches there and to this table.

Shipped behaviour (also in release builds), read by `tvfs-core` / the link router:

| Variable | What it does |
|---|---|
| `NATIVEFS_CONFIG=<file>` | Uses this config file instead of the one in the config folder. Debug builds also keep their login in `session.json` beside it instead of the system keychain. |
| `NATIVEFS_BASE_URL=<url>` | Talks to another TVFS server (local test server). |
| `NATIVEFS_LINK_FALLBACK=<unix time>` | Set by `naTiVeFS-link` on the browser it starts as a fallback; a second fallback within 30 s is not opened (loop guard). Not meant to be set by hand. |

Debug builds only:

| Variable | What it does |
|---|---|
| `NATIVEFS_NO_FOCUS=1` | Windows never take the keyboard focus (tests beside someone working). |
| `NATIVEFS_WINDOW_POS=x,y` | Opens the main window at this screen position (physical px), e.g. away from the screen someone works on. |
| `NATIVEFS_VIEWER_POS=x,y` | The same for viewer windows (they cascade by 28 px). |
| `NATIVEFS_WINDOW_SIZE=WxH` | Opens the main window at this logical size (desktop layout tests). |
| `NATIVEFS_DEMO=1` / `0` | Logged-in screen with fake recent files / logged-out screen with fake status chips; no server needed. |
| `NATIVEFS_PAGE=settings\|check` | Opens that page of the main window. |
| `NATIVEFS_SCROLL=<px>` | Scrolls the main page (screenshots of its lower parts). |
| `NATIVEFS_UPDATE_OFFER=1` | Shows the "restart to update" offer. |
| `NATIVEFS_STARTUP=a,b` | Sets "Start with the computer" / "Start in the background" (`1` = on). |
| `NATIVEFS_ROUTER=0\|1` | Switches the link router like the settings toggle. |
| `NATIVEFS_NO_SETTINGS=1` | The link router never opens the system's default-apps settings. |
| `NATIVEFS_FILE_MENUS=1` | Debug builds register the file manager menus only with this set. |
| `NATIVEFS_TRAY=<action>,...` | Runs tray menu actions one second apart. |
| `NATIVEFS_CHECK_URL=<link>` | Pre-fills the File Check field. |
| `NATIVEFS_LOCAL_FILE=<path>[;<path>...]` | Opens local files in their viewers at start (`NATIVEFS_AUDIO_FILE` is an older name). |
| `NATIVEFS_CAMERA=photo\|video` | Opens the camera window. |
| `NATIVEFS_CAMERA_AUTO=<s>` | Takes the photo / records `<s>` seconds by itself. |
| `NATIVEFS_VOICE=<s>[,listen]` | Records a voice note for `<s>` seconds and uses it (or plays it back). |
| `NATIVEFS_VOICE_UI=1\|menu` | Shows the recorder's ready state with its level controls and a made-up microphone list, without opening the microphone (`menu` also opens the microphone picker). |
| `NATIVEFS_VOICE_MIC=<n>[,<n>...]` | Once the recorder has listed the microphones, picks entry `n` of its picker (0 = System default), one every 3 s. |
| `NATIVEFS_VOICE_PLAYER=1` | Every local audio file opens in the voice note player. |
| `NATIVEFS_AUDIO_MUTE=1` | Audio windows, the voice player and the recorder's preview play muted. |
| `NATIVEFS_AUDIO_NOPLAY=1` | Audio windows and the voice player open without starting playback. |
| `NATIVEFS_AUDIO_EQ=1\|<n>` | Opens the audio player's EQ panel (and applies preset `n`). |
| `NATIVEFS_AUDIO_TOUR=<step>` | Opens the audio player's tour at that step. |
| `NATIVEFS_AUDIO_LOG=1` | Logs what the sound controls send to the audio engine. |
| `NATIVEFS_READER_FIND=<text>` | Opens Find in the reader with that text (`NATIVEFS_READER_FIND_STEP=<n>` steps to the n-th match). |
| `NATIVEFS_PLAYER_STATS=0..3`, `NATIVEFS_PLAYER_MENU=1..4` | Preset the video player's statistics overlay / menu. |
| `NATIVEFS_PLAYER_UI=1` | The video player keeps its controls up. |
| `NATIVEFS_PLAYER_FULLSCREEN=1` | The video player starts in fullscreen. |
| `NATIVEFS_PLAYER_PAUSE_AFTER=<s>` | The video player pauses by itself (idle measurements). |
| `NATIVEFS_PLAYER_PROFILE=1` | Prints the video player's rendering timings once a second. |
| `NATIVEFS_PLAYER_DRIVE=1`, `NATIVEFS_PLAYER_EXACT_HZ=1`, `NATIVEFS_MPV_NOBLOCK=1`, `NATIVEFS_NO_REPORT_SWAP=1`, `NATIVEFS_SWAP_INTERVAL=<n>`, `NATIVEFS_STATS_TEST=noread\|noapply` | Video player rendering experiments (see `player/mod.rs`). |
| `NATIVEFS_MPV_OPTS="k=v;k=v"` | Adds / overrides mpv options. |
