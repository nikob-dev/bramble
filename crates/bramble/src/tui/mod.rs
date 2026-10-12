//! `bramble tui`: your pet, the current lesson, and a lab shell, in one screen.

mod client;
mod keys;
mod markers;
mod pty;
mod ui;

use std::time::Duration;

use bramble_core::PROTOCOL_VERSION;
use bramble_core::protocol::{
    ActivityView, ClientMsg, ConceptView, LessonView, PetView, ServerMsg,
};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use tokio::sync::mpsc;

use crate::config::Config;
use client::NodeEvent;
use markers::MarkerScanner;
use pty::{Lab, PtyEvent};

const SCROLLBACK: usize = 1_000;
const ANIMATION_TICK: Duration = Duration::from_millis(250);

pub fn run(config: Config) -> anyhow::Result<()> {
    tokio::runtime::Runtime::new()?.block_on(async {
        let mut terminal = ratatui::init();
        let result = App::run(&mut terminal, config).await;
        ratatui::restore();
        result
    })
}

struct App {
    pet: Option<PetView>,
    lesson: Option<LessonView>,
    mastery: Vec<ConceptView>,
    activity: Option<ActivityView>,
    say: Option<String>,
    node_status: Option<String>,
    frame: u64,
    parser: vt100::Parser,
    lab_size: (u16, u16),
}

impl App {
    async fn run(terminal: &mut DefaultTerminal, config: Config) -> anyhow::Result<()> {
        let size = terminal.size()?;
        let lab_size = ui::lab_size(Rect::new(0, 0, size.width, size.height));
        let mut app = App {
            pet: None,
            lesson: None,
            mastery: Vec::new(),
            activity: None,
            say: None,
            node_status: Some("connecting…".into()),
            frame: 0,
            parser: vt100::Parser::new(lab_size.0, lab_size.1, SCROLLBACK),
            lab_size,
        };

        let (pty_tx, mut pty_rx) = mpsc::unbounded_channel();
        let mut lab = Lab::spawn(&config.lab_dir, lab_size.0, lab_size.1, pty_tx)?;
        let mut scanner = MarkerScanner::default();

        let (node_tx, mut node_rx) = mpsc::unbounded_channel();
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        tokio::spawn(client::run(config.node_url.clone(), node_tx, out_rx));

        let mut input_rx = spawn_input_thread();
        let mut animation = tokio::time::interval(ANIMATION_TICK);

        loop {
            terminal.draw(|frame| {
                ui::draw(
                    frame,
                    &ui::View {
                        pet: app.pet.as_ref(),
                        lesson: app.lesson.as_ref(),
                        mastery: &app.mastery,
                        activity: app.activity.as_ref(),
                        say: app.say.as_deref(),
                        node_status: app.node_status.as_deref(),
                        frame: app.frame,
                        screen: app.parser.screen(),
                    },
                )
            })?;

            tokio::select! {
                Some(event) = input_rx.recv() => match event {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if key.code == KeyCode::Char('q') && key.modifiers.contains(KeyModifiers::CONTROL) {
                            break;
                        }
                        if let Some(bytes) = keys::encode(key, app.parser.screen().application_cursor()) {
                            lab.write(&bytes)?;
                        }
                    }
                    Event::Paste(text) => lab.write(text.as_bytes())?,
                    Event::Resize(width, height) => {
                        let size = ui::lab_size(Rect::new(0, 0, width, height));
                        if size != app.lab_size {
                            app.lab_size = size;
                            lab.resize(size.0, size.1)?;
                            app.parser.screen_mut().set_size(size.0, size.1);
                        }
                    }
                    _ => {}
                },
                Some(event) = pty_rx.recv() => match event {
                    PtyEvent::Output(bytes) => {
                        app.parser.process(&bytes);
                        for observation in scanner.feed(&bytes) {
                            // If the node task is gone we're shutting down anyway.
                            let _ = out_tx.send(ClientMsg::Observe { v: PROTOCOL_VERSION, observation });
                        }
                    }
                    PtyEvent::Exited => break,
                },
                Some(event) = node_rx.recv() => match event {
                    NodeEvent::Connected => app.node_status = None,
                    NodeEvent::Disconnected(reason) => {
                        app.node_status = Some(format!("offline ({reason})"));
                    }
                    NodeEvent::Message(msg) => {
                        let ServerMsg::State { pet, lesson, say, mastery, activity, .. } = *msg;
                        app.pet = Some(pet);
                        app.lesson = lesson;
                        app.mastery = mastery;
                        app.activity = activity;
                        if say.is_some() {
                            app.say = say;
                        }
                    }
                },
                _ = animation.tick() => app.frame = app.frame.wrapping_add(1),
            }
        }
        Ok(())
    }
}

/// crossterm's blocking reader runs on its own thread and forwards events.
fn spawn_input_thread() -> mpsc::UnboundedReceiver<Event> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        loop {
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => match event::read() {
                    Ok(event) => {
                        if tx.send(event).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                },
                Ok(false) => {
                    if tx.is_closed() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}
