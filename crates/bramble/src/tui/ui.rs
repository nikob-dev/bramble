use bramble_core::money::format_minor;
use bramble_core::pet::Mood;
use bramble_core::protocol::{
    ActivityView, ConceptStatus, ConceptView, LessonView, NetworkView, PetView, StepKind,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use tui_term::widget::PseudoTerminal;

const MIN_TOP_HEIGHT: u16 = 13;
const MAX_TOP_HEIGHT: u16 = 20;
const PET_WIDTH: u16 = 38;
const MASTERY_WIDTH: u16 = 30;
/// Below this width the mastery column is hidden so the lesson stays readable.
const MASTERY_MIN_TOTAL_WIDTH: u16 = 120;

pub struct View<'a> {
    pub pet: Option<&'a PetView>,
    pub lesson: Option<&'a LessonView>,
    pub mastery: &'a [ConceptView],
    pub activity: Option<&'a ActivityView>,
    pub network: Option<&'a NetworkView>,
    pub say: Option<&'a str>,
    pub node_status: Option<&'a str>,
    pub frame: u64,
    pub screen: &'a vt100::Screen,
}

pub struct Areas {
    pub pet: Rect,
    pub lesson: Rect,
    pub mastery: Option<Rect>,
    pub lab: Rect,
}

pub fn areas(area: Rect) -> Areas {
    let top_height = (area.height * 2 / 5).clamp(MIN_TOP_HEIGHT, MAX_TOP_HEIGHT);
    let [top, lab] =
        Layout::vertical([Constraint::Length(top_height), Constraint::Min(5)]).areas(area);
    if area.width >= MASTERY_MIN_TOTAL_WIDTH {
        let [pet, lesson, mastery] = Layout::horizontal([
            Constraint::Length(PET_WIDTH),
            Constraint::Min(20),
            Constraint::Length(MASTERY_WIDTH),
        ])
        .areas(top);
        Areas {
            pet,
            lesson,
            mastery: Some(mastery),
            lab,
        }
    } else {
        let [pet, lesson] =
            Layout::horizontal([Constraint::Length(PET_WIDTH), Constraint::Min(20)]).areas(top);
        Areas {
            pet,
            lesson,
            mastery: None,
            lab,
        }
    }
}

/// Rows and columns available to the lab shell inside its border.
pub fn lab_size(area: Rect) -> (u16, u16) {
    let lab = areas(area).lab;
    (
        lab.height.saturating_sub(2).max(1),
        lab.width.saturating_sub(2).max(1),
    )
}

pub fn draw(frame: &mut Frame, view: &View) {
    let areas = areas(frame.area());
    draw_pet(frame, areas.pet, view);
    draw_lesson(frame, areas.lesson, view.lesson, view.activity);
    if let Some(area) = areas.mastery {
        draw_mastery(frame, area, view.mastery);
    }
    draw_lab(frame, areas.lab, view);
}

fn draw_pet(frame: &mut Frame, area: Rect, view: &View) {
    let title = view
        .pet
        .map(|p| format!(" {} · the pet-bank ", p.name))
        .unwrap_or_else(|| " your pet ".to_owned());
    let block = Block::bordered()
        .title(title.bold())
        .border_style(Style::new().fg(Color::Magenta));

    let Some(pet) = view.pet else {
        let waiting =
            Paragraph::new("\n  waking up your pet…\n  (waiting for the node)").block(block);
        frame.render_widget(waiting, area);
        return;
    };

    let blink = view.frame.is_multiple_of(24);
    let art = bramble_core::pet::art(pet.mood, blink);
    let mood = format!("{:?}", pet.mood).to_lowercase();
    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::raw(art[0]),
            Span::raw(format!("   mood  {mood}")).fg(mood_color(pet.mood)),
        ]),
        Line::from(vec![
            Span::raw(art[1]),
            Span::raw(format!("   lvl {} · {} xp", pet.level, pet.xp)),
        ]),
        Line::from(vec![
            Span::raw(art[2]),
            Span::raw("   energy ").dim(),
            bar(pet.energy, Color::Green),
        ]),
        Line::from(vec![
            Span::raw(art[3]),
            Span::raw("   curio  ").dim(),
            bar(pet.curiosity, Color::Cyan),
        ]),
        Line::raw(""),
    ];
    if let Some(network) = view.network {
        lines.push(Line::from(vec![
            Span::raw(format!(" {} ", network.node_id)).bold(),
            Span::raw("at the operator ").dim(),
            Span::raw(format_minor(network.reserves_minor)),
        ]));
        if network.ach_queued > 0 {
            lines.push(
                Line::from(format!(
                    " ACH queued: {} · {} (settles at cutoff)",
                    network.ach_queued,
                    format_minor(network.ach_queued_minor)
                ))
                .fg(Color::Yellow),
            );
        }
        lines.push(Line::raw(""));
    }
    if let Some(say) = view.say {
        lines.push(Line::from(format!("“{say}”").italic()));
    }
    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(block);
    frame.render_widget(paragraph, area);
}

fn mood_color(mood: Mood) -> Color {
    match mood {
        Mood::Happy => Color::Green,
        Mood::Curious => Color::Cyan,
        Mood::Bored => Color::Yellow,
        Mood::Sleepy => Color::DarkGray,
    }
}

fn bar(value: u8, color: Color) -> Span<'static> {
    let filled = usize::from(value.min(100)) / 10;
    Span::styled(
        format!(
            "{}{} {value:>3}",
            "█".repeat(filled),
            "░".repeat(10 - filled)
        ),
        Style::new().fg(color),
    )
}

fn draw_lesson(
    frame: &mut Frame,
    area: Rect,
    lesson: Option<&LessonView>,
    activity: Option<&ActivityView>,
) {
    let Some(lesson) = lesson else {
        let block = Block::bordered()
            .title(" all caught up ".bold())
            .border_style(Style::new().fg(Color::Blue));
        let text = activity.map_or("Waiting for the node…", |a| a.reason.as_str());
        frame.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: false }).block(block),
            area,
        );
        return;
    };
    let progress = if lesson.completed {
        "✓ complete".to_owned()
    } else {
        format!("{}/{}", lesson.step_index + 1, lesson.step_count)
    };
    let block = Block::bordered()
        .title(format!(" {} ", lesson.title).bold())
        .title(Line::from(format!(" {progress} ")).right_aligned())
        .border_style(Style::new().fg(Color::Blue));

    // Revealed help goes first: the learner has already read the prompt, and on short
    // terminals anything at the bottom of the panel gets clipped.
    let mut lines: Vec<Line> = Vec::new();
    if let Some(example) = &lesson.worked_example {
        lines.push(Line::from("worked example:".bold().green()));
        // Verbatim: worked examples are commands, so never reflow them.
        lines.extend(
            example
                .trim_matches('\n')
                .lines()
                .map(|l| Line::from(l.to_owned().green())),
        );
        lines.push(Line::raw(""));
    }
    if let Some(hint) = &lesson.hint {
        lines.push(Line::from(vec![
            Span::raw("hint: ").bold().yellow(),
            Span::raw(hint.clone()).yellow(),
        ]));
        lines.push(Line::raw(""));
    }
    lines.extend(
        bramble_core::text::reflow(&lesson.text)
            .into_iter()
            .map(Line::raw),
    );
    if lesson.completed && !lesson.sources.is_empty() {
        lines.push(Line::raw(""));
        for source in &lesson.sources {
            lines.push(Line::from(format!("source: {}", source.title).dim()));
        }
    }
    let footer = match (lesson.completed, lesson.kind) {
        (true, _) => "▶ run `bramble next` and I'll pick what's next".to_owned(),
        (false, StepKind::Explain) => "▶ run `bramble next` to continue".to_owned(),
        (false, StepKind::Task) => "stuck? run `bramble hint`".to_owned(),
    };

    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [body, footer_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
    frame.render_widget(
        Paragraph::new(Line::from(footer.bold().fg(Color::Blue))),
        footer_area,
    );
}

fn draw_mastery(frame: &mut Frame, area: Rect, mastery: &[ConceptView]) {
    let block = Block::bordered()
        .title(" mastery ".bold())
        .border_style(Style::new().fg(Color::Yellow));
    let width = usize::from(area.width.saturating_sub(2));
    let lines: Vec<Line> = mastery
        .iter()
        .map(|concept| {
            let (marker, color) = match concept.status {
                ConceptStatus::Mastered => ("✓", Color::Green),
                ConceptStatus::Learning => ("◐", Color::Yellow),
                ConceptStatus::Ready => ("○", Color::Cyan),
                ConceptStatus::Locked => ("·", Color::DarkGray),
            };
            let detail = match concept.status {
                ConceptStatus::Locked => "locked".to_owned(),
                // Unstudied: the percentage would only be the model's prior, not progress.
                ConceptStatus::Ready => "new".to_owned(),
                _ if concept.review_due => "review!".to_owned(),
                _ => format!("{:>3.0}%", concept.p_known * 100.0),
            };
            let name_width = width.saturating_sub(detail.chars().count() + 3);
            let name: String = concept.title.chars().take(name_width).collect();
            Line::from(vec![
                Span::styled(format!("{marker} "), Style::new().fg(color)),
                Span::raw(format!("{name:<name_width$} ")),
                Span::styled(detail, Style::new().fg(color)),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_lab(frame: &mut Frame, area: Rect, view: &View) {
    let mut block = Block::new()
        .borders(Borders::ALL)
        .title(" lab · a real bash shell · Ctrl-Q quits ".bold())
        .border_style(Style::new().fg(Color::Green));
    if let Some(status) = view.node_status {
        block = block.title(
            Line::from(format!(" node: {status} "))
                .right_aligned()
                .style(Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)),
        );
    }
    frame.render_widget(PseudoTerminal::new(view.screen).block(block), area);
}
