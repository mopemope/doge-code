//! Terminal UI for `dgc-next`.
//!
//! The model is pure: [`AppState`](state::AppState) changes only in
//! [`update::update`], which turns an [`AppEvent`] into state changes and
//! [`Effect`]s, and [`render::render`] draws it. The runtime here connects
//! it to the world: [`run`] sets up the terminal, [`input`] reads keys on a
//! thread, [`backend::Backend`] talks to the daemon, and [`run_loop`] feeds
//! every event to `update`, hands the effects to the backend and draws.

pub mod backend;
pub mod input;
pub mod layout;
pub mod render;
pub mod state;
pub mod update;

use std::fmt;
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::time::Duration;

use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use dgc_daemon::paths::DaemonPaths;
use dgc_protocol::event::AgentEvent;
use dgc_protocol::ids::{AgentId, PermissionId};
use dgc_protocol::model::{Agent, PendingPermission, Project, Workspace};
use input::KeyInput;
use ratatui::Terminal;
use ratatui::backend::{Backend as TerminalBackend, CrosstermBackend};
use state::AppState;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;
use tokio::time::{Instant, Interval, MissedTickBehavior};

/// State of the connection to the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    Connected,
    /// The reason is stored escaped (safe to show).
    Disconnected {
        reason: String,
    },
    /// Trying to (re)connect; the initial state before the first connection.
    Reconnecting,
}

/// What an RPC call was for, so [`AppEvent::RpcDone`] can update the state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestTag {
    Send {
        agent_id: AgentId,
    },
    Respond {
        permission_id: PermissionId,
    },
    CreateAgent,
    CreateWorkspace,
    Interrupt {
        agent_id: AgentId,
    },
    Resume {
        agent_id: AgentId,
    },
    Archive {
        agent_id: AgentId,
    },
    /// A failed history load: the backend reports the error with this tag
    /// (`RpcDone` with `Err`), in addition to the empty final `History` page.
    LoadHistory {
        agent_id: AgentId,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppEvent {
    Key(KeyInput),
    Snapshot {
        projects: Vec<Project>,
        workspaces: Vec<Workspace>,
        agents: Vec<Agent>,
        permissions: Vec<PendingPermission>,
    },
    /// A live event from the subscription.
    AgentEvent(AgentEvent),
    /// One `agent.events` page; `done` when the page was empty.
    ///
    /// Contract with the backend: `generation` is copied from the
    /// `Effect::LoadHistory` that asked for the page (every page of one
    /// chain carries it); pages of an older generation are discarded. A
    /// failed `agent.events` call must be reported as an empty `done: true`
    /// page with that generation, followed or preceded by `RpcDone` with
    /// `RequestTag::LoadHistory` and `Err`, so loading never sticks and the
    /// error shows in the status line.
    History {
        agent_id: AgentId,
        events: Vec<AgentEvent>,
        done: bool,
        generation: u64,
    },
    RpcDone {
        request: RequestTag,
        result: Result<serde_json::Value, String>,
    },
    Connection(ConnectionState),
    /// The subscription ended because it fell behind.
    Lagged,
    Resize {
        width: u16,
        height: u16,
    },
    Tick,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    LoadSnapshot,
    /// Fetch events after `since_seq`. The result must come back as
    /// `AppEvent::History` carrying the same `generation`.
    LoadHistory {
        agent_id: AgentId,
        since_seq: u64,
        generation: u64,
    },
    Call {
        request: RequestTag,
        method: &'static str,
        params: serde_json::Value,
    },
    Resubscribe,
    Quit,
}

/// Input from the terminal, forwarded by the input thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermEvent {
    Key(KeyInput),
    Resize { width: u16, height: u16 },
}

/// Whether the loop goes on after an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
}

#[derive(Debug)]
pub enum TuiError {
    /// Standard output is not a terminal.
    NotATerminal,
    /// Setting up the terminal or reading the signals failed.
    Terminal(io::Error),
    /// Drawing (or measuring) the screen failed.
    Draw(String),
    /// The daemon I/O task is gone.
    BackendStopped,
    /// The key input thread is gone.
    InputStopped,
}

impl fmt::Display for TuiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotATerminal => write!(f, "the TUI needs a terminal"),
            Self::Terminal(err) => write!(f, "terminal error: {err}"),
            Self::Draw(message) => write!(f, "cannot draw the screen: {message}"),
            Self::BackendStopped => write!(f, "the daemon connection task stopped"),
            Self::InputStopped => write!(f, "the terminal input stopped"),
        }
    }
}

impl std::error::Error for TuiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Terminal(err) => Some(err),
            _ => None,
        }
    }
}

fn draw_error(err: impl fmt::Display) -> TuiError {
    TuiError::Draw(err.to_string())
}

/// Tick period: error timeouts and the permission dialog's arming count
/// these.
pub const TICK: Duration = Duration::from_millis(250);

/// Ticks every [`TICK`]. A late tick delays the next ones instead of firing
/// a burst, so a stalled loop cannot arm the permission dialog at once.
pub fn tick_interval() -> Interval {
    let mut interval = tokio::time::interval_at(Instant::now() + TICK, TICK);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    interval
}

/// Daemon events handled before the screen is drawn again.
const DAEMON_BATCH: usize = 64;

/// Redraws after a frame turned out to have another size than `state.size`.
const MAX_REDRAWS: usize = 3;

/// The state, the terminal it is drawn on, and where effects go.
///
/// Every event reaches `update` after a `Resize` whenever the terminal's
/// size differs from `state.size` (the permission dialog judges what was
/// readable from it), and the first event is always the real size.
pub struct Runtime<B: TerminalBackend, U> {
    terminal: Terminal<B>,
    state: AppState,
    effects: mpsc::UnboundedSender<Effect>,
    update: U,
}

impl<B, U> Runtime<B, U>
where
    B: TerminalBackend,
    U: FnMut(&mut AppState, AppEvent) -> Vec<Effect>,
{
    /// `update` is [`update::update`] outside tests.
    pub fn new(terminal: Terminal<B>, effects: mpsc::UnboundedSender<Effect>, update: U) -> Self {
        Self {
            terminal,
            state: AppState::default(),
            effects,
            update,
        }
    }

    #[cfg(test)]
    pub fn state(&self) -> &AppState {
        &self.state
    }

    #[cfg(test)]
    pub fn terminal(&self) -> &Terminal<B> {
        &self.terminal
    }

    #[cfg(test)]
    pub fn terminal_mut(&mut self) -> &mut Terminal<B> {
        &mut self.terminal
    }

    /// Feed the terminal's real size (the first event `update` sees) and draw.
    pub fn start(&mut self) -> Result<(), TuiError> {
        let size = self.terminal.size().map_err(draw_error)?;
        self.apply(AppEvent::Resize {
            width: size.width,
            height: size.height,
        })?;
        self.draw()
    }

    /// Feed `event`, then draw unless it ended the TUI.
    pub fn handle(&mut self, event: AppEvent) -> Result<Flow, TuiError> {
        let flow = self.feed(event)?;
        if flow == Flow::Continue {
            self.draw()?;
        }
        Ok(flow)
    }

    pub fn handle_terminal(&mut self, event: TermEvent) -> Result<Flow, TuiError> {
        match event {
            TermEvent::Key(key) => self.handle(AppEvent::Key(key)),
            TermEvent::Resize { width, height } => self.handle(AppEvent::Resize { width, height }),
        }
    }

    /// Feed `event` without drawing, preceded by a `Resize` when the
    /// terminal's size is not `state.size`.
    pub fn feed(&mut self, event: AppEvent) -> Result<Flow, TuiError> {
        if !matches!(event, AppEvent::Resize { .. }) {
            let size = self.terminal.size().map_err(draw_error)?;
            if (size.width, size.height) != self.state.size {
                self.apply(AppEvent::Resize {
                    width: size.width,
                    height: size.height,
                })?;
            }
        }
        self.apply(event)
    }

    /// Draw the state. A frame of another size than `state.size` (a resize
    /// not reported yet, or reported wrong) is fed as `Resize` and drawn again.
    pub fn draw(&mut self) -> Result<(), TuiError> {
        for _ in 0..MAX_REDRAWS {
            let state = &self.state;
            let area = self
                .terminal
                .draw(|frame| render::render(state, frame))
                .map_err(draw_error)?
                .area;
            if (area.width, area.height) == self.state.size {
                break;
            }
            self.apply(AppEvent::Resize {
                width: area.width,
                height: area.height,
            })?;
        }
        Ok(())
    }

    /// Run `update` and pass its effects on; `Quit` ends the TUI.
    fn apply(&mut self, event: AppEvent) -> Result<Flow, TuiError> {
        for effect in (self.update)(&mut self.state, event) {
            match effect {
                Effect::Quit => return Ok(Flow::Quit),
                effect => self
                    .effects
                    .send(effect)
                    .map_err(|_| TuiError::BackendStopped)?,
            }
        }
        Ok(Flow::Continue)
    }
}

/// Run the TUI until `update` asks to quit: the real size first, then
/// terminal input, daemon events and ticks as they come, drawing after each.
pub async fn run_loop<B, U>(
    runtime: &mut Runtime<B, U>,
    mut terminal_events: mpsc::UnboundedReceiver<TermEvent>,
    mut daemon_events: mpsc::Receiver<AppEvent>,
) -> Result<(), TuiError>
where
    B: TerminalBackend,
    U: FnMut(&mut AppState, AppEvent) -> Vec<Effect>,
{
    runtime.start()?;
    let mut ticks = tick_interval();
    loop {
        let flow = tokio::select! {
            event = terminal_events.recv() => match event {
                Some(event) => runtime.handle_terminal(event)?,
                None => return Err(TuiError::InputStopped),
            },
            event = daemon_events.recv() => match event {
                Some(event) => handle_daemon_events(runtime, event, &mut daemon_events)?,
                None => return Err(TuiError::BackendStopped),
            },
            _ = ticks.tick() => runtime.handle(AppEvent::Tick)?,
        };
        if flow == Flow::Quit {
            return Ok(());
        }
    }
}

/// Feed `first` and what else the backend has ready (bounded), then draw
/// once: a burst of agent events is not drawn event by event.
fn handle_daemon_events<B, U>(
    runtime: &mut Runtime<B, U>,
    first: AppEvent,
    daemon_events: &mut mpsc::Receiver<AppEvent>,
) -> Result<Flow, TuiError>
where
    B: TerminalBackend,
    U: FnMut(&mut AppState, AppEvent) -> Vec<Effect>,
{
    let mut flow = runtime.feed(first)?;
    for _ in 1..DAEMON_BATCH {
        if flow == Flow::Quit {
            return Ok(flow);
        }
        match daemon_events.try_recv() {
            Ok(event) => flow = runtime.feed(event)?,
            Err(_) => break,
        }
    }
    if flow == Flow::Continue {
        runtime.draw()?;
    }
    Ok(flow)
}

/// Restores the terminal when dropped: on return, on error and while a
/// panic unwinds.
pub struct TerminalGuard {
    restore: fn(),
}

impl TerminalGuard {
    pub fn new(restore: fn()) -> Self {
        Self { restore }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        (self.restore)();
    }
}

/// Make panics call `restore` before the previous hook prints the message,
/// so it lands on the normal screen and not in raw mode.
pub fn install_panic_hook(restore: fn()) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
}

/// Raw mode off, back to the main screen, cursor visible. Safe to repeat.
fn restore_terminal() {
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
}

/// Run the TUI on the controlling terminal against the daemon at `paths`
/// (started with `exe daemon run` if it is not running). Quitting leaves the
/// daemon and its agents running.
///
/// No tracing subscriber is installed: nothing may write to the terminal
/// behind ratatui's back.
pub async fn run(paths: DaemonPaths, exe: PathBuf) -> Result<(), TuiError> {
    if !io::stdout().is_terminal() {
        return Err(TuiError::NotATerminal);
    }
    // In raw mode Ctrl-C is a key; these signals end the TUI cleanly too.
    let mut terminate = signal(SignalKind::terminate()).map_err(TuiError::Terminal)?;
    let mut hangup = signal(SignalKind::hangup()).map_err(TuiError::Terminal)?;
    let mut interrupt = signal(SignalKind::interrupt()).map_err(TuiError::Terminal)?;

    crossterm::terminal::enable_raw_mode().map_err(TuiError::Terminal)?;
    let _guard = TerminalGuard::new(restore_terminal);
    install_panic_hook(restore_terminal);
    crossterm::execute!(io::stdout(), EnterAlternateScreen).map_err(TuiError::Terminal)?;
    let terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).map_err(TuiError::Terminal)?;

    let (effects, daemon_events) = backend::Backend::start(paths, exe);
    let (keys, terminal_events) = mpsc::unbounded_channel();
    // Not joined: it notices the closed channel and ends on its own.
    input::spawn_reader(keys).map_err(TuiError::Terminal)?;
    let mut runtime = Runtime::new(terminal, effects, update::update);
    tokio::select! {
        result = run_loop(&mut runtime, terminal_events, daemon_events) => result,
        _ = terminate.recv() => Ok(()),
        _ = hangup.recv() => Ok(()),
        _ = interrupt.recv() => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use std::sync::{Arc, Mutex};

    use ratatui::backend::TestBackend;
    use tokio::time::MissedTickBehavior;

    use super::state::PERMISSION_ARM_TICKS;
    use super::*;

    #[tokio::test]
    async fn ticks_every_250ms_and_never_bursts() {
        let interval = tick_interval();
        assert_eq!(interval.period(), Duration::from_millis(250));
        assert_eq!(interval.missed_tick_behavior(), MissedTickBehavior::Delay);
    }

    #[test]
    fn permission_keys_stay_disabled_for_at_least_half_a_second() {
        // The dialog can open just before a tick: only the later ticks are
        // full periods.
        let ticks = u32::from(PERMISSION_ARM_TICKS) - 1;
        assert!(TICK * ticks >= Duration::from_millis(500));
    }

    type Recorder = Arc<Mutex<Vec<AppEvent>>>;
    type RecordingRuntime =
        Runtime<TestBackend, Box<dyn FnMut(&mut AppState, AppEvent) -> Vec<Effect>>>;

    /// A runtime whose `update` records every event it is fed.
    fn recording_runtime(
        width: u16,
        height: u16,
    ) -> (RecordingRuntime, Recorder, mpsc::UnboundedReceiver<Effect>) {
        let fed = Recorder::default();
        let log = fed.clone();
        let update = Box::new(move |state: &mut AppState, event: AppEvent| {
            log.lock().expect("lock").push(event.clone());
            update::update(state, event)
        });
        let terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        let (fx, fx_rx) = mpsc::unbounded_channel();
        (Runtime::new(terminal, fx, update), fed, fx_rx)
    }

    fn fed(recorder: &Recorder) -> Vec<AppEvent> {
        recorder.lock().expect("lock").clone()
    }

    #[test]
    fn runtime_feeds_the_real_size_first() {
        let (mut runtime, recorder, _fx) = recording_runtime(100, 30);
        runtime.start().expect("start");
        assert_eq!(
            fed(&recorder),
            vec![AppEvent::Resize {
                width: 100,
                height: 30
            }],
            "the first event is the terminal's real size"
        );
        assert_eq!(runtime.state().size, (100, 30));
        let drawn = runtime
            .terminal()
            .backend()
            .buffer()
            .content()
            .iter()
            .any(|c| c.symbol() != " ");
        assert!(drawn, "start draws the screen");
    }

    #[test]
    fn terminal_resize_reaches_update_before_the_next_key() {
        let (mut runtime, recorder, _fx) = recording_runtime(100, 30);
        runtime.start().expect("start");
        runtime.terminal_mut().backend_mut().resize(60, 20);
        let key = AppEvent::Key(KeyInput::char('j'));
        runtime.handle(key.clone()).expect("handle");
        let events = fed(&recorder);
        assert_eq!(
            events[1..],
            [
                AppEvent::Resize {
                    width: 60,
                    height: 20
                },
                key
            ]
        );
        assert_eq!(runtime.state().size, (60, 20));
    }

    #[test]
    fn drawing_corrects_a_stale_reported_size() {
        let (mut runtime, recorder, _fx) = recording_runtime(100, 30);
        runtime.start().expect("start");
        // A resize report that does not match what the terminal really draws.
        let flow = runtime
            .handle_terminal(TermEvent::Resize {
                width: 200,
                height: 50,
            })
            .expect("handle");
        assert_eq!(flow, Flow::Continue);
        assert_eq!(
            runtime.state().size,
            (100, 30),
            "the drawn frame's size wins"
        );
        assert_eq!(
            fed(&recorder).last(),
            Some(&AppEvent::Resize {
                width: 100,
                height: 30
            })
        );
    }

    #[test]
    fn effects_go_to_the_backend_and_quit_stops() {
        let (mut runtime, _recorder, mut fx) = recording_runtime(100, 30);
        runtime.start().expect("start");
        let flow = runtime
            .handle(AppEvent::Connection(ConnectionState::Connected))
            .expect("handle");
        assert_eq!(flow, Flow::Continue);
        assert_eq!(fx.try_recv().ok(), Some(Effect::LoadSnapshot));
        let flow = runtime
            .handle_terminal(TermEvent::Key(KeyInput::char('q')))
            .expect("handle");
        assert_eq!(flow, Flow::Quit);
        assert!(fx.try_recv().is_err(), "Quit is not sent to the backend");
    }

    static GUARD_RESTORES: AtomicUsize = AtomicUsize::new(0);

    fn count_guard_restore() {
        GUARD_RESTORES.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn guard_restores_the_terminal_on_return_and_on_panic() {
        drop(TerminalGuard::new(count_guard_restore));
        assert_eq!(GUARD_RESTORES.load(Ordering::SeqCst), 1, "normal exit");

        fn fails() -> Result<(), TuiError> {
            let _guard = TerminalGuard::new(count_guard_restore);
            Err(TuiError::BackendStopped)
        }
        assert!(fails().is_err());
        assert_eq!(GUARD_RESTORES.load(Ordering::SeqCst), 2, "error exit");

        let panicked = std::panic::catch_unwind(|| {
            let _guard = TerminalGuard::new(count_guard_restore);
            panic!("boom");
        });
        assert!(panicked.is_err());
        assert_eq!(GUARD_RESTORES.load(Ordering::SeqCst), 3, "panic exit");
    }

    static HOOK_RESTORES: AtomicUsize = AtomicUsize::new(0);

    fn count_hook_restore() {
        HOOK_RESTORES.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn panic_hook_restores_the_terminal_before_reporting() {
        let original = std::panic::take_hook();
        let reported = std::sync::Arc::new(AtomicUsize::new(0));
        let seen = reported.clone();
        std::panic::set_hook(Box::new(move |_| {
            // The previous hook (which prints the message) runs after the
            // terminal was restored.
            assert!(HOOK_RESTORES.load(Ordering::SeqCst) >= 1);
            seen.fetch_add(1, Ordering::SeqCst);
        }));
        install_panic_hook(count_hook_restore);
        let result = std::thread::spawn(|| panic!("boom")).join();
        std::panic::set_hook(original);
        assert!(result.is_err());
        assert!(HOOK_RESTORES.load(Ordering::SeqCst) >= 1);
        assert!(
            reported.load(Ordering::SeqCst) >= 1,
            "the old hook still runs"
        );
    }
}
