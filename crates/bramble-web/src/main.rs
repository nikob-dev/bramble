//! Bramble's browser dashboard: a live view of the pet, lesson, mastery, and the books.
//! The terminal stays the lab; this page watches it.

mod socket;

use bramble_core::money::format_minor;
use bramble_core::pet::{Mood, art};
use bramble_core::protocol::{
    ActivityView, ConceptStatus, ConceptView, EntryView, LedgerView, LessonView, NetworkView,
    OperatorView, PetView, ServerMsg, StepKind,
};
use bramble_core::teacher::ActivityKind;
use leptos::prelude::*;

use socket::Status;

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}

/// The latest state from the node, flattened for components.
#[derive(Clone, PartialEq)]
struct Snapshot {
    pet: PetView,
    lesson: Option<LessonView>,
    mastery: Vec<ConceptView>,
    activity: Option<ActivityView>,
    ledger: LedgerView,
    network: Option<NetworkView>,
}

/// A page shows either a bank (pet, lesson, books) or the operator (every bank's account).
#[derive(Clone, PartialEq)]
enum Page {
    Bank(Box<Snapshot>),
    Operator(OperatorView),
}

#[component]
fn App() -> impl IntoView {
    let (message, set_message) = signal(None::<ServerMsg>);
    let (status, set_status) = signal(Status::Connecting);
    socket::connect(set_message, set_status);

    let page = Memo::new(move |_| {
        message.get().map(|msg| match msg {
            ServerMsg::State {
                pet,
                lesson,
                mastery,
                activity,
                ledger,
                network,
                ..
            } => Page::Bank(Box::new(Snapshot {
                pet,
                lesson,
                mastery,
                activity,
                ledger: ledger.unwrap_or_default(),
                network,
            })),
            ServerMsg::Operator { operator, .. } => Page::Operator(operator),
        })
    });
    // The pet keeps talking about the last thing it said until it says something new.
    let (speech, set_speech) = signal(None::<String>);
    Effect::new(move |_| {
        if let Some(ServerMsg::State { say: Some(say), .. }) = message.get() {
            set_speech.set(Some(say));
        }
    });

    view! {
        <header class="topbar">
            <h1>"bramble"</h1>
            <span class="tagline">"a pet-bank that teaches payments"</span>
            <StatusBadge status />
        </header>
        <Show
            when=move || page.get().is_some()
            fallback=|| view! { <p class="waiting">"Waking up your pet…"</p> }
        >
            {move || page.get().map(|page| match page {
                Page::Bank(s) => {
                    let s = *s;
                    view! {
                        <main class="grid">
                            <div class="col">
                                <PetCard pet=s.pet speech />
                                {s.network.map(|network| view! { <NetworkCard network /> })}
                                <LedgerPanel ledger=s.ledger />
                            </div>
                            <div class="col">
                                <LessonCard lesson=s.lesson activity=s.activity />
                                <MasteryMap mastery=s.mastery />
                            </div>
                        </main>
                    }
                    .into_any()
                }
                Page::Operator(operator) => view! { <OperatorPage operator /> }.into_any(),
            })}
        </Show>
        <footer>
            {move || match page.get() {
                Some(Page::Operator(_)) => view! {
                    "Ring the ACH bell from a bank's lab: " <code>"bramble ach cutoff"</code>
                }.into_any(),
                _ => view! { "The lab lives in your terminal: " <code>"make play"</code> }.into_any(),
            }}
        </footer>
    }
}

#[component]
fn StatusBadge(status: ReadSignal<Status>) -> impl IntoView {
    view! {
        {move || match status.get() {
            Status::Connecting => view! { <span class="badge">"connecting…"</span> }.into_any(),
            Status::Live => view! { <span class="badge live">"● live"</span> }.into_any(),
            Status::Offline(why) => view! { <span class="badge offline" title=why>"offline"</span> }.into_any(),
        }}
    }
}

#[component]
fn PetCard(pet: PetView, speech: ReadSignal<Option<String>>) -> impl IntoView {
    let body = art(pet.mood, false).join("\n");
    let mood = format!("{:?}", pet.mood).to_lowercase();
    let mood_class = format!("mood {}", mood_class(pet.mood));
    view! {
        <section class="card pet">
            <h2>{pet.name.clone()} <span class="muted">" · the pet-bank"</span></h2>
            <div class="pet-row">
                <pre class="pet-art" aria-hidden="true">{body}</pre>
                <dl class="stats">
                    <dt>"mood"</dt><dd class=mood_class>{mood}</dd>
                    <dt>"level"</dt><dd>{format!("{} · {} xp", pet.level, pet.xp)}</dd>
                    <dt>"energy"</dt><dd><Meter value=pet.energy kind="energy" /></dd>
                    <dt>"curiosity"</dt><dd><Meter value=pet.curiosity kind="curiosity" /></dd>
                </dl>
            </div>
            <p class="speech" aria-live="polite">
                {move || speech.get().map(|s| format!("“{s}”"))}
            </p>
        </section>
    }
}

fn mood_class(mood: Mood) -> &'static str {
    match mood {
        Mood::Happy => "happy",
        Mood::Curious => "curious",
        Mood::Bored => "bored",
        Mood::Sleepy => "sleepy",
    }
}

#[component]
fn Meter(value: u8, kind: &'static str) -> impl IntoView {
    view! {
        <span class="meter" role="meter" aria-valuemin="0" aria-valuemax="100" aria-valuenow=value>
            <span class=format!("fill {kind}") style:width=format!("{value}%")></span>
        </span>
        <span class="meter-value">{value}</span>
    }
}

#[component]
fn LessonCard(lesson: Option<LessonView>, activity: Option<ActivityView>) -> impl IntoView {
    let kind = activity.as_ref().map(|a| a.kind);
    let reason = activity.map(|a| a.reason);
    let Some(lesson) = lesson else {
        return view! {
            <section class="card lesson">
                <h2>"All caught up"</h2>
                <p>{reason}</p>
            </section>
        }
        .into_any();
    };
    let done = if lesson.completed {
        lesson.step_count
    } else {
        lesson.step_index
    };
    let progress = format!("{}%", done * 100 / lesson.step_count.max(1));
    let label = match kind {
        Some(ActivityKind::Review) => "spaced review",
        Some(ActivityKind::Remediation) => "practice",
        _ => "lesson",
    };
    let footer = match (lesson.completed, lesson.kind) {
        (true, _) => {
            view! { "Run " <code>"bramble next"</code> " in the lab and I'll pick what's next." }
                .into_any()
        }
        (false, StepKind::Explain) => {
            view! { "Run " <code>"bramble next"</code> " in the lab to continue." }.into_any()
        }
        (false, StepKind::Task) => {
            view! { "Do this in the lab. Stuck? Run " <code>"bramble hint"</code> }.into_any()
        }
    };
    view! {
        <section class="card lesson">
            <p class="eyebrow">{label} " · step " {(lesson.step_index + 1).min(lesson.step_count)} " of " {lesson.step_count}</p>
            <h2>{lesson.title.clone()}</h2>
            <div class="progress"><span style:width=progress></span></div>
            {reason.map(|r| view! { <p class="reason">"Why this: " {r}</p> })}
            {lesson.worked_example.clone().map(|w| view! {
                <div class="worked"><strong>"Worked example"</strong><pre>{w.trim().to_owned()}</pre></div>
            })}
            {lesson.hint.clone().map(|h| view! { <p class="hint"><strong>"Hint: "</strong>{h}</p> })}
            <div class="lesson-text">
                {bramble_core::text::reflow(&lesson.text).into_iter().map(|para| view! { <p>{para}</p> }).collect_view()}
            </div>
            <p class="lesson-footer">{footer}</p>
            {lesson.completed.then(|| view! {
                <ul class="sources">
                    {lesson.sources.iter().map(|s| view! {
                        <li><a href=s.url.clone() target="_blank" rel="noopener">{s.title.clone()}</a></li>
                    }).collect_view()}
                </ul>
            })}
        </section>
    }
    .into_any()
}

/// Concepts laid out in columns by prerequisite depth, so the DAG reads left to right.
#[component]
fn MasteryMap(mastery: Vec<ConceptView>) -> impl IntoView {
    let columns = depth_columns(&mastery);
    let title_of = |id: &str| {
        mastery
            .iter()
            .find(|c| c.id == id)
            .map_or(id.to_owned(), |c| c.title.clone())
    };
    view! {
        <section class="card mastery">
            <h2>"Mastery"</h2>
            <div class="dag">
                {columns.into_iter().map(|column| view! {
                    <div class="dag-column">
                        {column.into_iter().map(|c| {
                            let needs = c.prereqs.iter().map(|p| title_of(p)).collect::<Vec<_>>().join(", ");
                            view! { <ConceptChip concept=c.clone() needs /> }
                        }).collect_view()}
                    </div>
                }).collect_view()}
            </div>
        </section>
    }
}

#[component]
fn ConceptChip(concept: ConceptView, needs: String) -> impl IntoView {
    let (status, detail) = match concept.status {
        ConceptStatus::Mastered => ("mastered", format!("✓ {:.0}%", concept.p_known * 100.0)),
        ConceptStatus::Learning => ("learning", format!("{:.0}%", concept.p_known * 100.0)),
        ConceptStatus::Ready => ("ready", "new".to_owned()),
        ConceptStatus::Locked => ("locked", "locked".to_owned()),
    };
    let detail = if concept.review_due {
        "review due".to_owned()
    } else {
        detail
    };
    view! {
        <div class=format!("chip {status}")>
            <span class="chip-title">{concept.title}</span>
            <span class="chip-detail">{detail}</span>
            {(!needs.is_empty()).then(|| view! { <span class="chip-needs">"needs " {needs}</span> })}
        </div>
    }
}

fn depth_columns(mastery: &[ConceptView]) -> Vec<Vec<ConceptView>> {
    // `mastery` arrives prerequisite-first, so one pass computes every depth.
    let mut depth: Vec<(String, usize)> = Vec::new();
    for concept in mastery {
        let d = concept
            .prereqs
            .iter()
            .filter_map(|p| depth.iter().find(|(id, _)| id == p).map(|(_, d)| d + 1))
            .max()
            .unwrap_or(0);
        depth.push((concept.id.clone(), d));
    }
    let max = depth.iter().map(|(_, d)| *d).max().unwrap_or(0);
    (0..=max)
        .map(|level| {
            mastery
                .iter()
                .zip(&depth)
                .filter(|(_, (_, d))| *d == level)
                .map(|(c, _)| c.clone())
                .collect()
        })
        .collect()
}

#[component]
fn LedgerPanel(ledger: LedgerView) -> impl IntoView {
    let total: i64 = ledger.accounts.iter().map(|a| a.balance_minor).sum();
    view! {
        <section class="card ledger">
            <h2>"The books"</h2>
            <table>
                <thead><tr><th>"account"</th><th>"kind"</th><th class="num">"balance"</th></tr></thead>
                <tbody>
                    {ledger.accounts.into_iter().map(|a| view! {
                        <tr>
                            <td><code>{a.account}</code></td>
                            <td class="muted">{a.kind}</td>
                            <td class=amount_class(a.balance_minor)>{format_minor(a.balance_minor)}</td>
                        </tr>
                    }).collect_view()}
                </tbody>
                <tfoot><tr>
                    <td colspan="2">"sum of every posting"</td>
                    <td class="num" class:broken=move || total != 0>{format_minor(total)}</td>
                </tr></tfoot>
            </table>
            <h3>"Recent entries"</h3>
            <EntryList entries=ledger.recent_entries />
        </section>
    }
}

#[component]
fn EntryList(entries: Vec<EntryView>) -> impl IntoView {
    view! {
        <ol class="entries">
            {entries.into_iter().map(|e| view! {
                <li>
                    <span class="entry-id">"#" {e.entry_id}</span>
                    <span class="entry-memo">{e.memo}</span>
                    <ul class="postings">
                        {e.postings.into_iter().map(|p| view! {
                            <li>
                                <code>{p.account}</code>
                                <span class=amount_class(p.amount_minor)>{format_minor(p.amount_minor)}</span>
                            </li>
                        }).collect_view()}
                    </ul>
                </li>
            }).collect_view()}
        </ol>
    }
}

/// The Rails track's cutaway (the netting lesson says the same in the terminal).
const RAILS_COMPARED: [(&str, &str, &str); 6] = [
    ("Speed", "seconds, any day", "the next settlement window"),
    ("Settles", "gross: one at a time", "net: once per window"),
    (
        "Final?",
        "yes, the moment it settles",
        "can still come back (a return)",
    ),
    (
        "Liquidity",
        "reserves for every payment",
        "only each bank's net position",
    ),
    (
        "Failures",
        "refused before money moves",
        "found later, as returns (R03)",
    ),
    (
        "Float",
        "none: money moves at once",
        "payer debited now, payee paid later",
    ),
];

/// This bank's place in the payment network: money at the operator, ACH waiting.
#[component]
fn NetworkCard(network: NetworkView) -> impl IntoView {
    view! {
        <section class="card network">
            <h2>"On the network " <span class="muted">"as " <code>{network.node_id}</code></span></h2>
            <dl class="stats">
                <dt>"at the operator"</dt>
                <dd class="num">{format_minor(network.reserves_minor)}</dd>
                <dt>"ACH queued"</dt>
                <dd class="num">
                    {if network.ach_queued == 0 {
                        "nothing waiting".to_owned()
                    } else {
                        format!("{} · {}", network.ach_queued, format_minor(network.ach_queued_minor))
                    }}
                </dd>
            </dl>
            <p class="muted small">
                "Instant (RTP) payments move reserves at once. ACH waits for "
                <code>"bramble ach cutoff"</code> ", then settles net."
            </p>
            <details class="cutaway">
                <summary>"RTP vs ACH at a glance"</summary>
                <table>
                    <thead><tr><th></th><th>"RTP (instant)"</th><th>"ACH (batch)"</th></tr></thead>
                    <tbody>
                        {RAILS_COMPARED.iter().map(|(aspect, rtp, ach)| view! {
                            <tr><th>{*aspect}</th><td>{*rtp}</td><td>{*ach}</td></tr>
                        }).collect_view()}
                    </tbody>
                </table>
            </details>
        </section>
    }
}

/// The clearing house: every bank's account, and what it settled recently.
#[component]
fn OperatorPage(operator: OperatorView) -> impl IntoView {
    let total: i64 = operator.banks.iter().map(|b| b.balance_minor).sum();
    view! {
        <main class="grid">
            <div class="col">
                <section class="card">
                    <h2>"The operator " <span class="muted">"settles between banks"</span></h2>
                    <table>
                        <thead><tr><th>"bank"</th><th class="num">"balance here"</th></tr></thead>
                        <tbody>
                            {operator.banks.into_iter().map(|b| view! {
                                <tr>
                                    <td><code>{b.bank}</code></td>
                                    <td class="num">{format_minor(b.balance_minor)}</td>
                                </tr>
                            }).collect_view()}
                        </tbody>
                        <tfoot><tr>
                            <td>"held for all banks"</td>
                            <td class="num">{format_minor(total)}</td>
                        </tr></tfoot>
                    </table>
                    <p class="muted small">
                        "Each bank should show the same number as " <code>"reserves:operator"</code>
                        " in its own books. " <code>"bramble reconcile"</code> " checks."
                    </p>
                    <p>"ACH windows settled: " <strong>{operator.ach_windows_settled}</strong></p>
                </section>
            </div>
            <div class="col">
                <section class="card ledger">
                    <h2>"Recent settlements"</h2>
                    <EntryList entries=operator.recent_settlements />
                </section>
            </div>
        </main>
    }
}

fn amount_class(amount: i64) -> &'static str {
    if amount < 0 {
        "num credit"
    } else {
        "num debit"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn concept(id: &str, prereqs: &[&str]) -> ConceptView {
        ConceptView {
            id: id.into(),
            title: id.into(),
            prereqs: prereqs.iter().map(|p| (*p).to_owned()).collect(),
            p_known: 0.2,
            status: ConceptStatus::Ready,
            review_due: false,
        }
    }

    #[test]
    fn web_depth_columns_lay_out_the_dag_by_prerequisite_depth() {
        let columns = depth_columns(&[
            concept("rows", &[]),
            concept("double", &["rows"]),
            concept("hash", &["rows"]),
            concept("idem", &["double"]),
        ]);
        let ids: Vec<Vec<&str>> = columns
            .iter()
            .map(|c| c.iter().map(|x| x.id.as_str()).collect())
            .collect();
        assert_eq!(
            ids,
            vec![vec!["rows"], vec!["double", "hash"], vec!["idem"]]
        );
    }
}
