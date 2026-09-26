mod ai_context;
mod app;
mod claude;
mod config;
mod fonts;
mod history;
mod lock;
mod pipeline;
mod placement;
mod session;
mod stt;
mod wav;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use app::App;
use config::{Config, VoiceConfig};

fn main() -> iced::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // `--stop` (release edge of the push-to-talk bind): signal the
    // already-running instance and exit immediately. Never starts a
    // second instance/window — see lock.rs and the README for why
    // SIGUSR1 was chosen over a socket.
    if args.iter().any(|a| a == "--stop") {
        if lock::send_stop() {
            eprintln!("og-voice: sent stop signal to running instance");
        } else {
            eprintln!("og-voice: no running instance to stop");
        }
        return Ok(());
    }

    // `--start` (press edge, when an instance is already running): the
    // popup is still open from the previous turn (`Done`/`Failed`/
    // `Cancelled`) — signal it to start a fresh recording in place
    // instead of launching a second window. `og-voice-launch` sends this
    // when it finds an existing instance rather than silently no-op'ing.
    if args.iter().any(|a| a == "--start") {
        if lock::send_start() {
            eprintln!("og-voice: sent start signal to running instance");
        } else {
            eprintln!("og-voice: no running instance to start");
        }
        return Ok(());
    }

    // `--interactive`: opens the popup idle (text input + mic toggle)
    // instead of auto-starting a recording — the taskbar's launcher
    // button uses this, as opposed to the hotkey press/release pair
    // driving the default recording-first flow above. Picked
    // `--interactive` over e.g. `--idle`/`--typed` since it names the
    // *mode* (type-or-talk) rather than one implementation detail of it,
    // matching `--stop`'s convention of naming the action/mode, not a
    // mechanism.
    let interactive = args.iter().any(|a| a == "--interactive");

    // Press edge: refuse to start a second recording session if one is
    // already in flight. `og-voice-launch` also guards against this, but
    // guarding here too means launching `og-voice` directly (bypassing
    // the wrapper) is still safe. Also covers `--interactive` launched
    // while another instance (recording or idle-interactive) already
    // holds the lock — rather than silently doing nothing there, best
    // effort focus whatever window is already open so the user isn't
    // left wondering why nothing happened.
    if lock::running_pid().is_some() {
        if interactive {
            eprintln!("og-voice: already running, focusing existing window instead of opening a second one");
            placement::focus_existing();
        } else {
            eprintln!("og-voice: already running, not starting a second instance");
        }
        return Ok(());
    }
    lock::acquire();

    let cfg = Config::load();
    let voice_cfg = VoiceConfig::load();
    let ai_cli = cfg.default_ai_cli.clone();
    let system_prompt = ai_context::system_prompt();

    // SIGUSR1 is the stop signal from `og-voice --stop`. `signal_hook`'s
    // `flag::register` just flips an `AtomicBool` from the signal
    // handler — no async-signal-safety concerns beyond what the crate
    // already guarantees, and the capture loop in `pipeline.rs` polls it
    // between reads rather than doing anything signal-handler-unsafe
    // directly. Chosen over a Unix socket for this: a signal plus a PID
    // file needs no listener bookkeeping/accept loop at all, and the
    // kernel already guarantees exactly-once, race-free delivery to a
    // specific PID — a socket would have to reinvent both of those for
    // no benefit here, since there's never more than one (sender,
    // receiver) pair at a time.
    let stop_flag = Arc::new(AtomicBool::new(false));
    if signal_hook::flag::register(signal_hook::consts::SIGUSR1, Arc::clone(&stop_flag)).is_err() {
        eprintln!("og-voice: warning: failed to install SIGUSR1 handler; --stop will not work");
    }

    // SIGUSR2 is the "start a new turn" signal from a hotkey press while
    // this instance is still showing the previous answer — see `--start`
    // above and `App`'s `Tick` handler, which polls this the same way
    // the capture loop polls `stop_flag`.
    let start_flag = Arc::new(AtomicBool::new(false));
    if signal_hook::flag::register(signal_hook::consts::SIGUSR2, Arc::clone(&start_flag)).is_err() {
        eprintln!("og-voice: warning: failed to install SIGUSR2 handler; pressing the hotkey again won't start a new turn");
    }

    let initial = if interactive { pipeline::Shared::idle() } else { pipeline::Shared::default() };
    let shared = Arc::new(Mutex::new(initial));

    // Default (hotkey) launch auto-starts recording immediately, same as
    // always. `--interactive` does NOT — it stays idle until the user
    // types+Enters or clicks the mic button (see `app.rs`'s `SubmitText`/
    // `MicToggle` handlers, which call `pipeline::submit_text`/
    // `pipeline::start` themselves at that point).
    if !interactive {
        pipeline::start(
            voice_cfg.stt_server_url.clone(),
            ai_cli.clone(),
            system_prompt.clone(),
            voice_cfg.allow_execution,
            Arc::clone(&stop_flag),
            Arc::clone(&shared),
        );
    }

    let result = run_ui(shared, stop_flag, start_flag, interactive, ai_cli, system_prompt, voice_cfg.stt_server_url.clone());
    lock::release();
    result
}

#[allow(clippy::too_many_arguments)]
fn run_ui(
    shared: pipeline::SharedState,
    stop_flag: Arc<AtomicBool>,
    start_flag: Arc<AtomicBool>,
    interactive: bool,
    ai_cli: String,
    system_prompt: Option<String>,
    stt_url: String,
) -> iced::Result {
    let build = iced::application("og-voice", App::update, App::view)
        .subscription(App::subscription)
        .style(|_state, _theme| iced::application::Appearance {
            background_color: iced::Color::TRANSPARENT,
            text_color: iced::Color::WHITE,
        });

    let build = match fonts::load_default_font() {
        Some((family, bytes)) => build.font(bytes).default_font(iced::Font::with_name(family)),
        None => build,
    };

    build
        .window(iced::window::Settings {
            size: iced::Size::new(app::SIZE.0, app::SIZE.1),
            decorations: false,
            transparent: true,
            visible: false,
            position: placement::hint(app::SIZE),
            platform_specific: iced::window::settings::PlatformSpecific {
                application_id: "og-voice".to_string(),
                ..Default::default()
            },
            ..Default::default()
        })
        .run_with(move || {
            App::new(shared.clone(), stop_flag.clone(), start_flag.clone(), interactive, ai_cli.clone(), system_prompt.clone(), stt_url.clone())
        })
}
