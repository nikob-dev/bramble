use bramble_core::pet::Mood;
use bramble_core::protocol::{LessonView, PetView, StepKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use tui_term::widget::PseudoTerminal;

const MIN_TOP_HEIGHT: u16 = 13;
const MAX_TOP_HEIGHT: u16 = 20;
const PET_WIDTH: u16 = 38;

pub struct View<'a> {
    pub pet: Option<&'a PetView>,
    pub lesson: Option<&'a LessonView>,
    pub say: Option<&'a str>,
    pub node_status: Option<&'a str>,
    pub frame: u64,
    pub screen: &'a vt100::Screen,
}

pub struct Areas {
    pub pet: Rect,
    pub lesson: Rect,
    pub lab: Rect,
}

pub fn areas(area: Rect) -> Areas {
    let top_height = (area.height * 2 / 5).clamp(MIN_TOP_HEIGHT, MAX_TOP_HEIGHT);
    let [top, lab] =
        Layout::vertical([Constraint::Length(top_height), Constraint::Min(5)]).areas(area);
    let [pet, lesson] =
        Layout::horizontal([Constraint::Length(PET_WIDTH), Constraint::Min(20)]).areas(top);
    Areas { pet, lesson, lab }
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
    draw_lesson(frame, areas.lesson, view.lesson);
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
    let art = art(pet.mood, blink);
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
    if let Some(say) = view.say {
        lines.push(Line::from(format!("“{say}”").italic()));
    }
    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(block);
    frame.render_widget(paragraph, area);
}

fn art(mood: Mood, blink: bool) -> [&'static str; 4] {
    let face = match (mood, blink) {
        (_, true) => "  (-.-) ",
        (Mood::Happy, _) => "  (^.^) ",
        (Mood::Curious, _) => "  (o.O) ",
        (Mood::Bored, _) => "  (-_-) ",
        (Mood::Sleepy, _) => "  (u.u)z",
    };
    ["   ,_,  ", face, "  /)_)$ ", "   \" \"  "]
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

fn draw_lesson(frame: &mut Frame, area: Rect, lesson: Option<&LessonView>) {
    let Some(lesson) = lesson else {
        let block = Block::bordered().title(" lesson ");
        frame.render_widget(Paragraph::new("No lessons loaded.").block(block), area);
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

    let mut lines: Vec<Line> = reflow(&lesson.text).into_iter().map(Line::raw).collect();
    if let Some(hint) = &lesson.hint {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::raw("hint: ").bold().yellow(),
            Span::raw(hint.clone()).yellow(),
        ]));
    }
    let footer = match (lesson.completed, lesson.kind) {
        (true, _) => {
            let sources = lesson
                .sources
                .iter()
                .map(|s| s.title.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            format!("source: {sources}")
        }
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

/// Joins hard-wrapped prose into paragraphs so the widget can wrap to the panel width.
/// Blank lines separate paragraphs; indented or bulleted lines (commands, lists) stay as-is.
fn reflow(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut joinable = false;
    for line in text.trim().lines() {
        let verbatim = line.starts_with(' ') || line.starts_with('\t') || line.starts_with('•');
        if line.trim().is_empty() {
            out.push(String::new());
            joinable = false;
        } else if joinable && !verbatim {
            if let Some(last) = out.last_mut() {
                last.push(' ');
                last.push_str(line.trim());
            }
        } else {
            out.push(line.to_owned());
            joinable = !verbatim;
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_reflow_joins_prose_and_keeps_indented_lines() {
        let text =
            "Amounts are integers,\nnever floats.\n\nRun:\n  bramble next\n  • a bullet\nAfter.";
        assert_eq!(
            reflow(text),
            vec![
                "Amounts are integers, never floats.",
                "",
                "Run:",
                "  bramble next",
                "  • a bullet",
                "After."
            ]
        );
    }
}
