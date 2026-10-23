//! The loop's wiring: the worker thread that owns the ports, the terminal's
//! frame heartbeat, and the main loop that lets the model speak.
//!
//! # Threading, in one paragraph
//!
//! Two threads plus short-lived fetch threads, no async runtime
//! (`docs/DECISIONS.md`, D-03). The main thread polls crossterm every 50 ms
//! — no key means a [`Msg::Tick`], a key means a [`Msg::Key`] — and drains
//! every reply the worker has sent. The worker thread owns the ports
//! *exclusively* for user work, strictly in order: that sequence is still
//! the limiter for everything the operator is waiting on.
//!
//! Enrichment (row and hit stats) runs on detached fetch threads, capped
//! by the scheduler's four-in-the-air rule, and every thread pushes
//! through one shared pacing gate in the adapter, so the per-minute
//! window stays honest across concurrency — `docs/DECISIONS.md`, D-15.
//!
//! # Ordering
//!
//! User effects leave and run in the order `update` returned them;
//! enrichment may complete out of order, which is safe by construction —
//! replies fill the row whose id they carry, and a stale enrich finds
//! nothing to fill. Everything else — staleness tokens, page cohesion —
//! is `update`'s job (see its module docs).
//!
//! # Ordering
//!
//! Effects leave the main loop in the order `update` returned them; the
//! worker completes them in that order; replies arrive in completion order,
//! which is send order. Everything else — staleness tokens, page cohesion —
//! is `update`'s job (see its module docs).

use std::sync::{Arc, mpsc};
use std::time::Duration;

use crossterm::{
    cursor,
    event::{self, Event, KeyEventKind},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::{
    domain::{browse::EntityKind, browse::WantEntry},
    ports::{CatalogSource, Clock, WantlistStore},
    tui,
    tui::model::{Effect, EffectClass, Model, Msg, OpenRequest, Screen},
    tui::update::update,
    tui::view,
};

/// How often the loop wakes to redraw and schedule. Smooth enough for the
/// spinner, slow enough to matter to no battery.
const FRAME: Duration = Duration::from_millis(50);

/// The whole digger, from terminal entry to teardown.
///
/// # The exit contract
///
/// `Ok(())` on a deliberate quit (`Ctrl+C`, or `Esc` over an empty home
/// prompt). An `Err(String)` means the terminal could not be entered, a
/// draw failed, or the worker died mid-run — the message is for the operator
/// and says which half failed.
///
/// The clock arrives as a port like everything else: the wiring
/// (`src/bin/digger.rs`) chose it. The ports arrive shared, because the
/// enrichment fetch threads use them too — and every adapter involved is
/// thread-safe by construction (`Mutex` state, no interior `&mut`).
pub fn run(
    api: Arc<dyn CatalogSource + Send + Sync>,
    store: Arc<dyn WantlistStore + Send + Sync>,
    clock: Arc<dyn Clock + Send + Sync>,
) -> Result<(), String> {
    let (effects_tx, msg_rx) = spawn_worker(api, store, clock);

    let mut model = Model {
        stack: vec![Screen::Home],
        ..Default::default()
    };
    // Boot: the wantlist is on disk; the model should know it before the
    // first key press wants something *new* in front of the user.
    model.busy += 1;
    effects_tx
        .send(tui::model::Effect::LoadWantlist)
        .map_err(|_| "worker stopped before startup".to_string())?;

    let (mut terminal, _guard) = enter_terminal()?;
    main_loop(&mut terminal, &mut model, &effects_tx, &msg_rx)
}

// ---------------------------------------------------------------------------
// The worker thread.
// ---------------------------------------------------------------------------

/// The worker's send side, one queue per class. Dropping it ends user work;
/// detached enrichment fetch threads wind down on their own.
struct EffectSender {
    user: mpsc::Sender<Effect>,
    mesh: mpsc::Sender<Effect>,
}

impl EffectSender {
    /// Route one effect to its class queue (`docs/DECISIONS.md`, D-14).
    fn send(&self, effect: Effect) -> Result<(), String> {
        let sender = match effect.class() {
            EffectClass::User => &self.user,
            EffectClass::Enrichment => &self.mesh,
        };
        sender
            .send(effect)
            .map_err(|_| "the worker stopped; quitting".to_string())
    }
}

/// Spawn the worker. Returns `(effects main→worker, replies worker→main)`.
///
/// User work runs in queue order on this thread — sequential, the limiter
/// for everything the operator waits on. Each enrichment job is handed to
/// its own short-lived detached thread so four detail fetches overlap
/// instead of queueing behind one another (`docs/DECISIONS.md`, D-15);
/// the adapter's shared pacing gate keeps the wire honest across those
/// threads. It exits when the main loop drops its senders; detached fetch
/// threads finish on their own, late replies falling on a closed channel.
fn spawn_worker(
    api: Arc<dyn CatalogSource + Send + Sync>,
    store: Arc<dyn WantlistStore + Send + Sync>,
    clock: Arc<dyn Clock + Send + Sync>,
) -> (EffectSender, mpsc::Receiver<Msg>) {
    let (user_tx, user_rx) = mpsc::channel();
    let (mesh_tx, mesh_rx) = mpsc::channel();
    let (msg_tx, msg_rx) = mpsc::channel();
    std::thread::spawn(move || {
        loop {
            // User work in queue order — sequential, the limiter for what
            // the operator is waiting on.
            for effect in user_rx.try_iter() {
                run_effect(effect, &api, &store, &clock, &msg_tx);
            }
            // Enrichment: each job goes to its own short-lived detached
            // thread, so four detail fetches overlap instead of queueing
            // behind one another. The scheduler sends at most four before
            // a reply lands — that is the cap — and the adapter's shared
            // pacing gate keeps the wire honest across the threads
            // (docs/DECISIONS.md, D-15). Replies are order-free by
            // construction: they fill the row whose id they carry.
            for effect in mesh_rx.try_iter() {
                let api = Arc::clone(&api);
                let store = Arc::clone(&store);
                let clock = Arc::clone(&clock);
                let msg_tx = msg_tx.clone();
                std::thread::spawn(move || {
                    run_effect(effect, &api, &store, &clock, &msg_tx);
                });
            }
            // Wait for the next user effect, in frame-sized slices so the
            // fetch threads' replies and fresh mesh sends are noticed
            // promptly.
            match user_rx.recv_timeout(FRAME) {
                Ok(effect) => run_effect(effect, &api, &store, &clock, &msg_tx),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                // User senders are gone: the app quit. Detached fetch
                // threads finish on their own; late replies fall on a
                // closed channel and harm nobody.
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    });
    (
        EffectSender {
            user: user_tx,
            mesh: mesh_tx,
        },
        msg_rx,
    )
}

/// Drive one effect and send its replies. A failed send only means the UI
/// quit; a dead receiver harms nobody.
fn run_effect(
    effect: Effect,
    api: &Arc<dyn CatalogSource + Send + Sync>,
    store: &Arc<dyn WantlistStore + Send + Sync>,
    clock: &Arc<dyn Clock + Send + Sync>,
    msg_tx: &mpsc::Sender<Msg>,
) {
    for msg in execute(&**api, &**store, &**clock, effect) {
        let _ = msg_tx.send(msg);
    }
}

/// Effect in, replies out. One logical job per effect, translated honestly:
/// errors become [`Msg::Failed`] (or the quiet [`Msg::EnrichFailed`]); a
/// panic never reaches here, because nothing in the ports panics.
fn execute(
    api: &dyn CatalogSource,
    store: &dyn WantlistStore,
    clock: &dyn Clock,
    effect: tui::model::Effect,
) -> Vec<Msg> {
    use tui::model::Effect as E;
    match effect {
        E::LoadArtistProfile(id) => match api.artist(id) {
            Ok(profile) => vec![Msg::ArtistLoaded(id, Box::new(profile))],
            Err(error) => vec![failed(true, format!("artist {id}"), error.to_string())],
        },
        E::LoadLabelProfile(id) => match api.label(id) {
            Ok(profile) => vec![Msg::LabelLoaded(id, Box::new(profile))],
            Err(error) => vec![failed(true, format!("label {id}"), error.to_string())],
        },
        E::LoadRows { kind, id, page } => {
            let rows = match kind {
                EntityKind::Artist => api.artist_releases(id, page),
                EntityKind::Label => api.label_releases(id, page),
                // A release opens as a detail; there is no row table to load.
                EntityKind::Release => {
                    return vec![failed(
                        true,
                        format!("rows for release {id}"),
                        "a release opens on its detail screen, not as rows".to_string(),
                    )];
                }
            };
            match rows {
                Ok(data) => vec![Msg::RowsLoaded {
                    kind,
                    id,
                    page,
                    data: Box::new(data),
                }],
                Err(error) => vec![failed(
                    true,
                    format!("{kind} {id} page {page}"),
                    error.to_string(),
                )],
            }
        }
        E::LoadDetail { release, request } => match api.release_detail(release) {
            Ok(detail) => vec![Msg::DetailLoaded(request, Box::new(detail))],
            Err(error) => {
                // A failed open says so on the status line. A failed enrich
                // says nothing at all: the row's previous state was silence
                // anyway. Fires for the entity table and the search hits
                // alike — both guards answer the same quiet message.
                match request {
                    OpenRequest::Enrich(_) | OpenRequest::EnrichHit(_) => {
                        vec![Msg::EnrichFailed(release)]
                    }
                    _ => vec![failed(
                        true,
                        format!("release {}", release.get()),
                        error.to_string(),
                    )],
                }
            }
        },
        E::Enrich(release_id) => match api.release_detail(release_id) {
            Ok(detail) => vec![Msg::DetailLoaded(
                OpenRequest::Enrich(release_id),
                Box::new(detail),
            )],
            Err(_) => vec![Msg::EnrichFailed(release_id)],
        },
        E::Search { query, kind, page } => match api.search(&query, kind, page) {
            Ok(page_data) => vec![Msg::SearchLoaded(Box::new(page_data))],
            Err(error) => {
                vec![failed(true, format!("search “{query}”"), error.to_string())]
            }
        },
        E::LoadWantlist => match store.list() {
            Ok(wants) => vec![Msg::WantlistLoaded(wants)],
            // The runtime counts the boot's wantlist load by hand, so its
            // failure closes that job too — a failed boot must not leave
            // the spinner spinning for the whole session.
            Err(error) => {
                vec![failed(true, "the wantlist".to_string(), error.to_string())]
            }
        },
        E::AddWant { kind, id, name } => {
            let entry = WantEntry {
                kind,
                id,
                name: name.clone(),
                // The worker owns the clock for stamps: the *write time* is
                // the entry's birthday, once and consistently.
                added_at: clock.now_unix(),
            };
            match store.add(&entry) {
                Ok(added) => vec![Msg::WantAdded {
                    kind,
                    id,
                    name,
                    added_at: entry.added_at,
                    added,
                }],
                Err(error) => vec![failed(false, format!("adding {name}"), error.to_string())],
            }
        }
        E::RemoveWant { kind, id } => match store.remove(kind, id) {
            Ok(removed) => vec![Msg::WantRemoved { kind, id, removed }],
            Err(error) => vec![failed(
                false,
                format!("removing {kind} {id}"),
                error.to_string(),
            )],
        },
        E::Quit => Vec::new(),
    }
}

/// A failed port call, for the status line. `job` is the mirror of the
/// effect that failed: a network effect was *counted* when it left `update`
/// and its failure must close the count — otherwise one failed request
/// leaves the spinner spinning forever (`Msg::is_terminal`, the `job: true`
/// arm). File-talk failures were never counted, so they must not close.
fn failed(job: bool, context: String, error: String) -> Msg {
    Msg::Failed {
        job,
        context: Some(context),
        message: error,
    }
}

// ---------------------------------------------------------------------------
// Terminal: enter, guard, leave.
// ---------------------------------------------------------------------------

/// Raw mode, alternate screen, cursor hidden. The guard restores all three —
/// including on panic, which is exactly when restoration matters most.
fn enter_terminal() -> Result<(Tui, TerminalGuard), String> {
    terminal::enable_raw_mode().map_err(|e| e.to_string())?;
    execute!(std::io::stdout(), EnterAlternateScreen, cursor::Hide).map_err(|e| e.to_string())?;
    let backend = CrosstermBackend::new(std::io::stdout());
    let terminal = Terminal::new(backend).map_err(|e| e.to_string())?;
    Ok((terminal, TerminalGuard))
}

type Tui = Terminal<CrosstermBackend<std::io::Stdout>>;

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen, cursor::Show);
        let _ = terminal::disable_raw_mode();
    }
}

// ---------------------------------------------------------------------------
// The main loop.
// ---------------------------------------------------------------------------

fn main_loop(
    terminal: &mut Tui,
    model: &mut Model,
    effects_tx: &EffectSender,
    msg_rx: &mpsc::Receiver<Msg>,
) -> Result<(), String> {
    let mut quitting = false;
    while !quitting {
        // Drain every reply that is waiting; the *latest* state wins the
        // frame, so intermediate rebuilds simply never draw.
        while let Ok(msg) = msg_rx.try_recv() {
            for effect in update(model, msg) {
                if effect == tui::model::Effect::Quit {
                    quitting = true;
                } else {
                    effects_tx.send(effect)?;
                }
            }
        }
        if quitting {
            break;
        }

        // The size it draws with is the size it was told.
        model.viewport = terminal
            .size()
            .map(|size| (size.width, size.height))
            .unwrap_or((80, 24));

        terminal
            .draw(|frame| view::draw(frame, model))
            .map_err(|e| e.to_string())?;

        // One frame of waiting; no key means a tick, a key burst means the
        // burst arrives as one batch, not one key per frame.
        if !event::poll(FRAME).map_err(|e| e.to_string())? {
            for effect in update(model, Msg::Tick) {
                let _ = effects_tx.send(effect);
            }
            continue;
        }
        for event in read_available()? {
            if let Event::Key(key) = event {
                // Release and repeat events are noise for a menu UI; presses
                // are the whole conversation.
                if key.kind == KeyEventKind::Press {
                    for effect in update(model, Msg::Key(key)) {
                        if effect == tui::model::Effect::Quit {
                            quitting = true;
                        } else {
                            effects_tx.send(effect)?;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Read every queued event now, not one per frame.
fn read_available() -> Result<Vec<Event>, String> {
    let mut buffered = Vec::new();
    while event::poll(Duration::ZERO).map_err(|e| e.to_string())? {
        buffered.push(event::read().map_err(|e| e.to_string())?);
    }
    Ok(buffered)
}
