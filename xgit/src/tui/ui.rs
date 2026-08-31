use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, HighlightSpacing, Padding, Paragraph, Row, Table, Wrap,
};

use crate::model::{IssueLink, ItemRow, ItemState, LinkKind, Role, View};
use crate::timeutil::{relative, relative_short, truncate_width, wrap_text};

use super::{App, FlatRow, Focus, Mode, StatusKind};

const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

struct Theme;
impl Theme {
    const TEXT: Color = Color::Rgb(226, 232, 240);
    const MUTED: Color = Color::Rgb(148, 163, 184);
    const DIM: Color = Color::Rgb(100, 116, 139);
    const FAINT: Color = Color::Rgb(71, 85, 105);
    const ACCENT: Color = Color::Rgb(56, 189, 248);
    const SURFACE: Color = Color::Rgb(30, 41, 59);
    const PURPLE: Color = Color::Rgb(139, 92, 246);
    const BORDER: Color = Color::Rgb(76, 50, 145);
    const AMBER: Color = Color::Rgb(251, 191, 36);
    const GREEN: Color = Color::Rgb(52, 211, 153);
    const RED: Color = Color::Rgb(248, 113, 113);
    const MAGENTA: Color = Color::Rgb(232, 121, 249);
    const BLUE: Color = Color::Rgb(125, 211, 252);
}

pub(super) fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(8),
        Constraint::Length(1),
    ])
    .split(area);

    draw_title(frame, app, chunks[0]);
    draw_tabs(frame, app, chunks[1]);
    draw_body(frame, app, chunks[2]);
    draw_status(frame, app, chunks[3]);

    match app.mode {
        Mode::Help => draw_help(frame, area),
        Mode::SyncMenu => draw_sync_menu(frame, app, area),
        Mode::Filter | Mode::Normal => {}
    }
}

fn draw_title(frame: &mut Frame, app: &App, area: Rect) {
    let who = app
        .cfg
        .username
        .clone()
        .or_else(|| app.db.meta_get("viewer_login").ok().flatten())
        .unwrap_or_else(|| "local".into());
    let sync = if app.syncing {
        format!(
            "{} {}",
            SPINNER[app.spinner % SPINNER.len()],
            if app.sync_label.is_empty() {
                "syncing"
            } else {
                &app.sync_label
            }
        )
    } else if let Some(t) = app.last_sync {
        format!("synced {}s ago", t.elapsed().as_secs())
    } else if app.cfg.offline {
        "offline".into()
    } else {
        "waiting to sync".into()
    };

    let left = Line::from(vec![
        Span::styled(
            " gitsync ",
            Style::new().fg(Color::Black).bg(Theme::PURPLE).bold(),
        ),
        Span::raw("  "),
        Span::styled(who, Style::new().fg(Theme::TEXT)),
    ]);
    let right = Line::from(vec![
        Span::styled(
            if app.query.view.uses_time_filter() {
                app.query.time.label()
            } else {
                String::new()
            },
            Style::new().fg(Theme::MUTED),
        ),
        Span::raw("  "),
        Span::styled(sync, Style::new().fg(Theme::DIM)),
        Span::raw(" "),
    ]);
    frame.render_widget(Paragraph::new(left), area);
    frame.render_widget(Paragraph::new(right).right_aligned(), area);
}

fn draw_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let compact = area.width < 92;
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    for view in View::ALL {
        let count = app
            .counts
            .iter()
            .find(|(v, _)| *v == view)
            .map(|(_, n)| *n)
            .unwrap_or(0);
        let selected = app.query.view == view;
        let label = if compact {
            format!(" {} {count} ", view.name())
        } else {
            format!(" {} {count}  ", view.name())
        };
        let style = if selected {
            Style::new()
                .fg(Color::Black)
                .bg(Theme::PURPLE)
                .add_modifier(Modifier::BOLD)
        } else if view == View::Inbox && count > 0 {
            Style::new().fg(Theme::AMBER)
        } else {
            Style::new().fg(Theme::MUTED)
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
    }
    if !app.query.search.is_empty() && app.mode != Mode::Filter {
        spans.push(Span::styled(
            format!("  /{} ", app.query.search),
            Style::new().fg(Theme::ACCENT),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_body(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.preview_open {
        let cols = Layout::horizontal([Constraint::Percentage(25), Constraint::Percentage(75)])
            .split(area);
        draw_list(frame, app, cols[0]);
        draw_detail(frame, app, cols[1]);
    } else {
        draw_list(frame, app, area);
    }
}

fn pane(focused: bool) -> Block<'static> {
    let border = if focused {
        Theme::PURPLE
    } else {
        Theme::BORDER
    };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border))
        .padding(Padding::horizontal(1))
        .style(Style::new().fg(Theme::TEXT))
}

fn draw_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::List && app.mode == Mode::Normal;
    let pos = if app.flat.is_empty() {
        0
    } else {
        app.selected + 1
    };
    let title = if app.mode == Mode::Filter {
        format!("  /{}▌ ", app.filter_buf)
    } else {
        format!(" {}  {}/{} ", app.query.view.name(), pos, app.flat.len())
    };
    let block = pane(focused).title(Span::styled(title, Style::new().fg(Theme::MUTED)));

    if app.flat.is_empty() {
        let empty = if app.cfg.has_token() {
            "Nothing here.  r sync   h/l views"
        } else {
            "No local data. Set GITHUB_TOKEN and press R."
        };
        frame.render_widget(
            Paragraph::new(empty).block(block).fg(Theme::DIM).centered(),
            area,
        );
        return;
    }

    if app.query.view == View::Inbox {
        draw_inbox_table(frame, app, area, block);
        return;
    }

    let compact = area.width < 64;
    let show_review = !compact && area.width >= 96;
    let repo_w = if area.width >= 110 {
        24
    } else if compact {
        16
    } else {
        20
    };

    let (widths, header_cells) = if compact {
        let header_style = Style::new().fg(Theme::FAINT);
        (
            vec![
                Constraint::Length(1),
                Constraint::Length(repo_w),
                Constraint::Min(8),
            ],
            vec![
                Cell::from(""),
                Cell::from("repo").style(header_style),
                Cell::from("title").style(header_style),
            ],
        )
    } else {
        let header_style = Style::new().fg(Theme::FAINT);
        let mut widths = vec![
            Constraint::Length(1),
            Constraint::Length(10),
            Constraint::Length(7),
            Constraint::Length(repo_w),
            Constraint::Min(16),
            Constraint::Length(6),
            Constraint::Length(6),
        ];
        let mut header_cells = vec![
            Cell::from(""),
            Cell::from("role").style(header_style),
            Cell::from("state").style(header_style),
            Cell::from("repository").style(header_style),
            Cell::from("title").style(header_style),
            Cell::from("update").style(header_style),
            Cell::from("linked").style(header_style),
        ];
        if show_review {
            widths.push(Constraint::Length(6));
            header_cells.push(Cell::from("review").style(header_style));
        }
        (widths, header_cells)
    };
    let header = Row::new(header_cells).height(1);

    let rows: Vec<Row> = app
        .flat
        .iter()
        .filter_map(|row| match row {
            FlatRow::Item(i) => Some(item_row(&app.items[*i], show_review, compact)),
            FlatRow::Notif(_) => None,
            FlatRow::Child { parent, link } => {
                let parent = app.items.get(*parent)?;
                let link = parent.links.get(*link)?;
                Some(child_row(link, show_review, compact))
            }
        })
        .collect();

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .column_spacing(2)
        .row_highlight_style(Style::new().bg(Theme::SURFACE).fg(Theme::TEXT))
        .highlight_spacing(HighlightSpacing::Never)
        .highlight_symbol("");
    frame.render_stateful_widget(table, area, &mut app.table_state);
}

fn draw_inbox_table(frame: &mut Frame, app: &mut App, area: Rect, block: Block<'static>) {
    let compact = area.width < 70;
    let (widths, header_cells) = if compact {
        let h = Style::new().fg(Theme::FAINT);
        (
            vec![
                Constraint::Length(1),
                Constraint::Length(8),
                Constraint::Min(8),
                Constraint::Length(6),
            ],
            vec![
                Cell::from(""),
                Cell::from("reason").style(h),
                Cell::from("title").style(h),
                Cell::from("update").style(h),
            ],
        )
    } else {
        let h = Style::new().fg(Theme::FAINT);
        (
            vec![
                Constraint::Length(1),
                Constraint::Length(8),
                Constraint::Length(6),
                Constraint::Length(22),
                Constraint::Min(12),
                Constraint::Length(6),
            ],
            vec![
                Cell::from(""),
                Cell::from("reason").style(h),
                Cell::from("type").style(h),
                Cell::from("repository").style(h),
                Cell::from("title").style(h),
                Cell::from("update").style(h),
            ],
        )
    };
    let rows: Vec<Row> = app.inbox.iter().map(|n| inbox_row(n, compact)).collect();
    let table = Table::new(rows, widths)
        .header(Row::new(header_cells).height(1))
        .block(block)
        .column_spacing(2)
        .row_highlight_style(Style::new().bg(Theme::SURFACE).fg(Theme::TEXT))
        .highlight_spacing(HighlightSpacing::Never)
        .highlight_symbol("");
    frame.render_stateful_widget(table, area, &mut app.table_state);
}

fn inbox_row(n: &crate::model::InboxRow, compact: bool) -> Row<'static> {
    let unread = if n.unread {
        Cell::from("●").style(Style::new().fg(Theme::AMBER).bold())
    } else {
        Cell::from(" ")
    };
    let updated = n
        .updated_at
        .as_deref()
        .map(relative_short)
        .unwrap_or_default();
    let repo = match n.number {
        Some(num) => format!("{}/{}#{num}", n.owner, n.repo),
        None => format!("{}/{}", n.owner, n.repo),
    };
    if compact {
        return Row::new(vec![
            unread,
            Cell::from(n.reason_label().to_string()).style(Style::new().fg(Theme::MAGENTA)),
            Cell::from(n.title.clone()).style(Style::new().fg(Theme::TEXT)),
            Cell::from(updated).style(Style::new().fg(Theme::DIM)),
        ])
        .height(1);
    }
    Row::new(vec![
        unread,
        Cell::from(n.reason_label().to_string()).style(Style::new().fg(Theme::MAGENTA)),
        Cell::from(n.type_label().to_string()).style(Style::new().fg(Theme::BLUE)),
        Cell::from(repo).style(Style::new().fg(Color::White)),
        Cell::from(n.title.clone()).style(Style::new().fg(Theme::TEXT)),
        Cell::from(updated).style(Style::new().fg(Theme::DIM)),
    ])
    .height(1)
}

fn item_row(item: &ItemRow, show_review: bool, compact: bool) -> Row<'static> {
    let unread = if item.unread {
        Cell::from("●").style(Style::new().fg(Theme::AMBER).bold())
    } else {
        Cell::from(" ")
    };
    let role = item.primary_role();
    let (state_text, state_fg) = if item.draft && item.state == ItemState::Open {
        ("Draft", Theme::AMBER)
    } else {
        (state_label(item.state), state_color(item.state))
    };
    if compact {
        return Row::new(vec![
            unread,
            Cell::from(format!("{}#{}", item.repo, item.number))
                .style(Style::new().fg(Color::White)),
            Cell::from(item.title.clone()).style(Style::new().fg(Theme::TEXT)),
        ])
        .height(1);
    }
    let mut cells = vec![
        unread,
        Cell::from(role_label(role)).style(Style::new().fg(role_color(role))),
        Cell::from(state_text).style(Style::new().fg(state_fg)),
        Cell::from(format_repo(&item.owner, &item.repo, item.number))
            .style(Style::new().fg(Color::White)),
        Cell::from(item.title.clone()).style(Style::new().fg(Theme::TEXT)),
        Cell::from(
            item.updated_at
                .as_deref()
                .map(relative_short)
                .unwrap_or_default(),
        )
        .style(Style::new().fg(Theme::DIM)),
        linked_cell(item.linked_count()),
    ];
    if show_review {
        let text = item.review_summary();
        let color = if item.review_total == 0 {
            Theme::DIM
        } else if item.approvals >= item.review_total {
            Theme::GREEN
        } else {
            Theme::AMBER
        };
        cells.push(Cell::from(text).style(Style::new().fg(color)));
    }
    Row::new(cells).height(1)
}

fn child_row(link: &IssueLink, show_review: bool, compact: bool) -> Row<'static> {
    let tag = match link.kind {
        LinkKind::Closes => "closes",
        LinkKind::Mentioned => "linked",
    };
    let title = link.title.clone().unwrap_or_default();
    if compact {
        return Row::new(vec![
            Cell::from(" ").style(Style::new().fg(Theme::FAINT)),
            Cell::from(format!("    {}#{}", short_repo(&link.repo), link.number))
                .style(Style::new().fg(Color::White)),
            Cell::from(format!("    {title}")).style(Style::new().fg(Theme::MUTED)),
        ])
        .height(1);
    }
    let state_text = link.state.map(state_label).unwrap_or("");
    let state_fg = link.state.map(state_color).unwrap_or(Theme::DIM);
    let mut cells = vec![
        Cell::from(" ").style(Style::new().fg(Theme::FAINT)),
        Cell::from(format!("    {tag}")).style(Style::new().fg(Theme::DIM).italic()),
        Cell::from(state_text).style(Style::new().fg(state_fg)),
        Cell::from(format!("    {}#{}", link.repo, link.number))
            .style(Style::new().fg(Color::White)),
        Cell::from(format!("    {title}")).style(Style::new().fg(Theme::MUTED)),
        Cell::from("").style(Style::new().fg(Theme::DIM)),
        Cell::from(""),
    ];
    if show_review {
        cells.push(Cell::from(""));
    }
    Row::new(cells).height(1)
}

fn short_repo(repo: &str) -> &str {
    repo.rsplit_once('/').map(|(_, n)| n).unwrap_or(repo)
}

fn linked_cell(n: usize) -> Cell<'static> {
    if n == 0 {
        Cell::from("").style(Style::new().fg(Theme::FAINT))
    } else {
        Cell::from(n.to_string()).style(Style::new().fg(Theme::ACCENT))
    }
}

fn format_repo(owner: &str, repo: &str, number: i64) -> String {
    format!("{owner}/{repo}#{number}")
}

fn draw_detail(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Detail && app.mode == Mode::Normal;
    let Some(d) = &app.detail else {
        let hint = if let Some(n) = app.selected_inbox() {
            format!(
                "{}\n\n{}  {}  {}#{}\n\nnot in local cache yet — wait for the next poll or press r → this item",
                n.title,
                n.reason_label(),
                n.type_label(),
                n.repo,
                n.number.map(|x| x.to_string()).unwrap_or_default()
            )
        } else {
            "Select an item, then press i".into()
        };
        frame.render_widget(
            Paragraph::new(hint)
                .block(
                    pane(focused).title(Span::styled(" preview ", Style::new().fg(Theme::MUTED))),
                )
                .fg(Theme::DIM),
            area,
        );
        return;
    };

    let width = area.width.saturating_sub(6) as usize;
    let mut lines: Vec<Line> = Vec::new();
    let mut ident = vec![
        Span::styled(
            format!("{}/{}#{}", d.row.owner, d.row.repo, d.row.number),
            Style::new().fg(Theme::ACCENT).bold(),
        ),
        Span::raw("   "),
        badge(state_label(d.row.state), state_color(d.row.state)),
        Span::raw("  "),
        Span::styled(d.row.kind.to_string(), Style::new().fg(Theme::DIM)),
    ];
    if d.row.draft {
        ident.push(Span::raw("  "));
        ident.push(badge("draft", Theme::AMBER));
    }
    if d.row.unread {
        ident.push(Span::raw("  "));
        ident.push(badge("unread", Theme::AMBER));
    }
    lines.push(Line::from(ident));
    lines.push(Line::from(Span::styled(
        d.row.title.clone(),
        Style::new().fg(Theme::TEXT).add_modifier(Modifier::BOLD),
    )));
    if let Some(url) = &d.row.html_url {
        lines.push(Line::styled(url.clone(), Style::new().fg(Theme::FAINT)));
    }
    lines.push(Line::from(""));
    lines.push(section("people"));

    let roles: Vec<Span> = {
        let mut s = vec![label("roles")];
        for (i, role) in d.row.roles.iter().enumerate() {
            if i > 0 {
                s.push(Span::styled(" · ", Style::new().fg(Theme::FAINT)));
            }
            s.push(Span::styled(
                role_label(*role),
                Style::new().fg(role_color(*role)),
            ));
        }
        s
    };
    lines.push(Line::from(roles));

    let author = d.row.author.clone().unwrap_or_else(|| "—".into());
    let assignees = if d.assignees.is_empty() {
        "—".into()
    } else {
        d.assignees.join(", ")
    };
    lines.push(Line::from(vec![
        label("author"),
        Span::styled(author, Style::new().fg(Theme::GREEN)),
    ]));
    lines.push(Line::from(vec![
        label("assignees"),
        Span::styled(assignees, Style::new().fg(Theme::TEXT)),
    ]));

    if d.row.kind == crate::model::Kind::Pr {
        lines.push(Line::from(""));
        lines.push(section("review"));
        if d.row.review_total > 0 {
            let color = if d.row.approvals >= d.row.review_total {
                Theme::GREEN
            } else {
                Theme::AMBER
            };
            lines.push(Line::from(vec![
                label("progress"),
                Span::styled(d.row.review_summary(), Style::new().fg(color)),
                Span::styled(
                    format!("   {} of {} approved", d.row.approvals, d.row.review_total),
                    Style::new().fg(Theme::DIM),
                ),
            ]));
        }
        if d.reviews.is_empty() {
            lines.push(Line::from(vec![
                label(""),
                Span::styled("no reviews yet", Style::new().fg(Theme::FAINT)),
            ]));
        } else {
            for rev in &d.reviews {
                let when = rev
                    .submitted_at
                    .as_deref()
                    .map(relative)
                    .unwrap_or_default();
                lines.push(Line::from(vec![
                    label(""),
                    Span::styled(
                        format!("{:<16}", truncate_width(&rev.author, 16)),
                        Style::new().fg(Theme::MUTED),
                    ),
                    Span::styled(
                        format!("{:<10}", short_review(&rev.state)),
                        Style::new().fg(review_color(&rev.state)),
                    ),
                    Span::styled(when, Style::new().fg(Theme::FAINT)),
                ]));
                if !rev.body.trim().is_empty() {
                    for wrapped in wrap_text(rev.body.trim(), width.saturating_sub(12).max(20)) {
                        lines.push(Line::from(vec![
                            label(""),
                            Span::styled(wrapped, Style::new().fg(Theme::TEXT)),
                        ]));
                    }
                }
            }
        }
    }

    if !d.labels.is_empty() || !d.links.is_empty() {
        lines.push(Line::from(""));
        lines.push(section("context"));
    }
    if !d.labels.is_empty() {
        let mut parts = vec![label("labels")];
        for lab in &d.labels {
            parts.push(Span::styled(
                format!(" {} ", lab.name),
                Style::new().bg(parse_hex(&lab.color)).fg(Color::Black),
            ));
            parts.push(Span::raw(" "));
        }
        lines.push(Line::from(parts));
    }
    if !d.links.is_empty() {
        for link in &d.links {
            let tag = match link.kind {
                crate::model::LinkKind::Closes => "closes",
                crate::model::LinkKind::Mentioned => "mentions",
            };
            let st = link
                .state
                .map(|s| format!(" {}", state_label(s)))
                .unwrap_or_default();
            let title = link.title.clone().unwrap_or_default();
            lines.push(Line::from(vec![
                label(tag),
                Span::styled(
                    format!("{}#{}{st}", link.repo, link.number),
                    Style::new().fg(Theme::BLUE),
                ),
                Span::raw("  "),
                Span::styled(title, Style::new().fg(Theme::MUTED)),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(section("timeline"));
    if let Some(c) = &d.created_at {
        lines.push(Line::from(vec![
            label("created"),
            Span::styled(relative(c), Style::new().fg(Theme::DIM)),
        ]));
    }
    if let Some(u) = &d.row.updated_at {
        lines.push(Line::from(vec![
            label("updated"),
            Span::styled(relative(u), Style::new().fg(Theme::DIM)),
        ]));
    }
    if let Some(c) = &d.closed_at {
        lines.push(Line::from(vec![
            label("closed"),
            Span::styled(relative(c), Style::new().fg(Theme::DIM)),
        ]));
    }
    if let Some(m) = &d.merged_at {
        lines.push(Line::from(vec![
            label("merged"),
            Span::styled(relative(m), Style::new().fg(Theme::DIM)),
        ]));
    }
    match (d.row.additions, d.row.deletions, d.changed_files) {
        (Some(a), Some(del), Some(f)) => lines.push(Line::from(vec![
            label("diff"),
            Span::styled(
                format!("+{a} / -{del}   {f} files"),
                Style::new().fg(Theme::DIM),
            ),
        ])),
        (Some(a), Some(del), _) => lines.push(Line::from(vec![
            label("diff"),
            Span::styled(format!("+{a} / -{del}"), Style::new().fg(Theme::DIM)),
        ])),
        _ => {}
    }

    lines.push(Line::from(""));
    lines.push(section("body"));
    let body = d.body.trim();
    if body.is_empty() {
        lines.push(Line::styled(
            "no description",
            Style::new().fg(Theme::FAINT),
        ));
    } else {
        for wrapped in wrap_text(body, width.max(20)) {
            lines.push(Line::styled(wrapped, Style::new().fg(Theme::TEXT)));
        }
    }

    lines.push(Line::from(""));
    lines.push(section(&format!(
        "comments  {}",
        if d.comments.is_empty() {
            d.comments_count.to_string()
        } else {
            d.comments.len().to_string()
        }
    )));
    if d.comments.is_empty() {
        let hint = if d.comments_count > 0 {
            "loading comments…  ·  c to retry"
        } else {
            "no comments  ·  c to fetch"
        };
        lines.push(Line::styled(hint, Style::new().fg(Theme::FAINT).italic()));
    } else {
        for cm in &d.comments {
            let when = cm.created_at.as_deref().map(relative).unwrap_or_default();
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(format!("@{}  ", cm.author), Style::new().fg(Theme::ACCENT)),
                Span::styled(
                    format!("{when}  {}", cm.kind),
                    Style::new().fg(Theme::FAINT),
                ),
            ]));
            for wrapped in wrap_text(cm.body.trim(), width.max(20)) {
                lines.push(Line::styled(wrapped, Style::new().fg(Theme::TEXT)));
            }
        }
    }

    let block = pane(focused).title(Span::styled(
        " preview    i close  ·  tab list  ·  j/k scroll  ·  c comments  ·  o open ",
        Style::new().fg(Theme::FAINT),
    ));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .scroll((app.detail_scroll, 0))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn section(title: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("── {title} "),
        Style::new().fg(Theme::FAINT),
    ))
}

fn label(text: &str) -> Span<'static> {
    if text.is_empty() {
        Span::styled(format!("{:>8}  ", ""), Style::new().fg(Theme::FAINT))
    } else {
        Span::styled(format!("{text:<8}  "), Style::new().fg(Theme::FAINT))
    }
}

fn badge(text: &str, color: Color) -> Span<'static> {
    Span::styled(format!(" {text} "), Style::new().fg(Color::Black).bg(color))
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let (msg_fg, msg_bg) = match app.status.kind {
        StatusKind::Info => (Theme::DIM, Color::Reset),
        StatusKind::Ok => (Theme::GREEN, Color::Reset),
        StatusKind::Warn => (Theme::AMBER, Color::Reset),
        StatusKind::Err => (Color::White, Theme::RED),
    };
    let left = if app.status.set_at.elapsed().as_secs() < 6 {
        app.status.message.clone()
    } else {
        format!(
            "{} · {} items",
            app.query.view.name().to_ascii_lowercase(),
            app.items.len()
        )
    };
    let keys =
        "r sync   t link   T all links   i preview   y copy   h/l views   j/k   / filter   ?";
    let line = Line::from(vec![
        Span::styled(format!(" {left} "), Style::new().fg(msg_fg).bg(msg_bg)),
        Span::raw(" "),
        Span::styled(keys, Style::new().fg(Theme::FAINT)),
    ]);
    frame.render_widget(Paragraph::new(truncate_line(line, area.width)), area);
}

fn truncate_line(line: Line<'static>, width: u16) -> Line<'static> {
    let raw: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    if unicode_width::UnicodeWidthStr::width(raw.as_str()) <= width as usize {
        return line;
    }
    Line::from(truncate_width(&raw, width as usize))
}

fn draw_help(frame: &mut Frame, area: Rect) {
    let text = "\
gitsync

Views   (h / l  or  ← →)
  Inbox         notifications, review requests, assignments waiting on you
  All PRs       open PRs you authored or were asked to review
  My PRs        open PRs you authored
  Closed PRs    closed / merged PRs  (t time)
  All Issues    open issues, with linked PRs nested
  Closed Issues closed issues, with linked PRs nested

Movement
  h/l           previous / next view
  j/k           list  (or preview when focused)
  i             open / close preview (right, 3/4 width)
  tab           list / preview  (when preview is open)
  esc           close preview
  g / G         top / bottom
  n / N         next / previous unread
  J / K         scroll preview
  /             filter title, repo, author, number
  t             toggle linked items for the selected PR/issue
  T             toggle linked items for the whole list

Actions
  y             copy GitHub URL (works over SSH)
  o  enter      open in browser
  m             toggle local read/unread
  c             fetch comments
  r             sync menu (this item / last 7–90d / all)
  q             quit
";
    let popup = centered(area, 62, 32);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(text)
            .block(pane(true).title(Span::styled(" help    esc ", Style::new().fg(Theme::MUTED)))),
        popup,
    );
}

fn draw_sync_menu(frame: &mut Frame, app: &App, area: Rect) {
    let options = [
        "1   this item",
        "2   last 7 days",
        "3   last 30 days",
        "4   last 60 days",
        "5   last 90 days",
        "6   all",
    ];
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            "created date on GitHub, not last update",
            Style::new().fg(Theme::DIM),
        )),
        Line::from(""),
    ];
    for (i, opt) in options.iter().enumerate() {
        let selected = i == app.sync_choice;
        let style = if selected {
            Style::new()
                .fg(Color::Black)
                .bg(Theme::PURPLE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(Theme::TEXT)
        };
        lines.push(Line::styled(format!(" {opt} "), style));
    }
    lines.push(Line::from(""));
    lines.push(Line::styled(
        "j/k  ·  1-6  ·  enter  ·  esc",
        Style::new().fg(Theme::FAINT),
    ));
    let popup = centered(area, 42, 13);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines)
            .block(pane(true).title(Span::styled(" sync ", Style::new().fg(Theme::PURPLE)))),
        popup,
    );
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::Authored => "Authored",
        Role::Assigned => "Assigned",
        Role::Reviewed => "Reviewed",
        Role::ReviewRequested => "Requested",
        Role::Commented => "Commented",
        Role::Mentioned => "Mentioned",
        Role::Involved => "Involved",
    }
}

fn state_label(state: ItemState) -> &'static str {
    match state {
        ItemState::Open => "Open",
        ItemState::Closed => "Closed",
        ItemState::Merged => "Merged",
    }
}

fn role_color(role: Role) -> Color {
    match role {
        Role::Authored => Theme::ACCENT,
        Role::Assigned => Theme::MAGENTA,
        Role::Reviewed => Theme::GREEN,
        Role::ReviewRequested => Theme::RED,
        Role::Commented => Theme::BLUE,
        Role::Mentioned => Theme::AMBER,
        Role::Involved => Theme::DIM,
    }
}

fn state_color(state: ItemState) -> Color {
    match state {
        ItemState::Open => Theme::GREEN,
        ItemState::Closed => Theme::RED,
        ItemState::Merged => Theme::MAGENTA,
    }
}

fn short_review(s: &str) -> String {
    match s {
        "APPROVED" => "approved".into(),
        "CHANGES_REQUESTED" => "changes".into(),
        "COMMENTED" => "commented".into(),
        "DISMISSED" => "dismissed".into(),
        "PENDING" => "pending".into(),
        other => other.replace('_', " ").to_ascii_lowercase(),
    }
}

fn review_color(s: &str) -> Color {
    match s {
        "APPROVED" => Theme::GREEN,
        "CHANGES_REQUESTED" => Theme::RED,
        "COMMENTED" => Theme::BLUE,
        "DISMISSED" => Theme::DIM,
        "PENDING" => Theme::AMBER,
        _ => Theme::MUTED,
    }
}

fn parse_hex(color: &str) -> Color {
    let s = color.trim().trim_start_matches('#');
    if s.len() == 6 {
        if let (Ok(r), Ok(g), Ok(b)) = (
            u8::from_str_radix(&s[0..2], 16),
            u8::from_str_radix(&s[2..4], 16),
            u8::from_str_radix(&s[4..6], 16),
        ) {
            return Color::Rgb(r, g, b);
        }
    }
    Theme::MUTED
}
