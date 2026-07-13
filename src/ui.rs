use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Cell, Gauge, Paragraph, Row, Table, TableState, Tabs},
};

use crate::app::{App, Tab};
use crate::inventory::Source;

const MAX_CORE_ROWS: usize = 8;
const GIB: f64 = (1u64 << 30) as f64;
const CPU: Color = Color::Rgb(122, 162, 247);
const MEMORY: Color = Color::Rgb(158, 206, 106);
const GPU: Color = Color::Rgb(187, 154, 247);
const THERMAL: Color = Color::Rgb(247, 118, 142);
const ACCENT: Color = Color::Rgb(115, 218, 202);
const MUTED: Color = Color::Rgb(86, 95, 137);

pub fn render(frame: &mut Frame, app: &App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let [name_area, tabs_area] =
        Layout::horizontal([Constraint::Length(22), Constraint::Fill(1)]).areas(header);
    let mut title = vec!["Sysmo".fg(ACCENT).bold()];
    if app.paused {
        title.push(" PAUSED".fg(THERMAL).bold());
    }
    frame.render_widget(Paragraph::new(Line::from(title)), name_area);
    frame.render_widget(
        Tabs::new([Tab::Monitor.title(), Tab::Inventory.title()])
            .select(app.active_tab as usize)
            .divider("  ")
            .style(Style::new().fg(MUTED))
            .highlight_style(Style::new().fg(Color::Black).bg(ACCENT).bold()),
        tabs_area,
    );

    match app.active_tab {
        Tab::Monitor => render_monitor(frame, body, app),
        Tab::Inventory => render_inventory(frame, body, app),
    }

    let mut hints = match app.active_tab {
        Tab::Monitor => format!(
            "q quit | tab switch | c/m sort | j/k/↑↓ select | x kill | p pause | tick {}",
            app.tick
        ),
        Tab::Inventory => {
            let keys = "q quit | tab switch | j/k/↑↓ select | / filter | s source | r rescan";
            match app.scan_duration {
                Some(d) => format!(
                    "{} items in {:.2}s  |  {keys}",
                    app.inventory.len(),
                    d.as_secs_f64()
                ),
                None => keys.to_string(),
            }
        }
    };
    if let Some(msg) = app.status() {
        hints = format!("{msg}  |  {hints}");
    }
    frame.render_widget(Paragraph::new(hints).fg(MUTED), footer);
}

fn render_monitor(frame: &mut Frame, area: Rect, app: &App) {
    let Some(m) = &app.metrics else {
        frame.render_widget(Paragraph::new("collecting metrics…").centered(), area);
        return;
    };

    let cores = m.per_core_cpu.len();
    let columns = cores.div_ceil(MAX_CORE_ROWS).max(1);
    let rows = cores.div_ceil(columns);

    let [
        summary_area,
        freq_area,
        cpu_area,
        mem_area,
        power_area,
        sensors_area,
        table_area,
    ] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(rows as u16),
        Constraint::Length(2),
        Constraint::Length(3),
        Constraint::Length(4),
        Constraint::Fill(1),
    ])
    .areas(area);

    let mut summary = format!(
        "CPU {:5.1}%   {}   load {:.2} {:.2} {:.2}",
        m.global_cpu,
        m.cpu_power_w
            .map_or_else(|| "—".into(), |watts| format!("{watts:.2} W")),
        m.load_avg.0,
        m.load_avg.1,
        m.load_avg.2
    );
    if m.timestamp.elapsed().as_secs() > 3 {
        summary.push_str("   (stale)");
    }
    frame.render_widget(Paragraph::new(summary).fg(CPU).bold(), summary_area);

    let freqs = format!(
        "E clusters {}   P clusters {}",
        format_frequencies(m.ecluster_freq_mhz.as_deref()),
        format_frequencies(m.pcluster_freq_mhz.as_deref())
    );
    frame.render_widget(Paragraph::new(freqs).fg(MUTED), freq_area);

    let col_areas = Layout::horizontal(vec![Constraint::Fill(1); columns]).split(cpu_area);
    for (col, chunk) in m.per_core_cpu.chunks(rows).enumerate() {
        let row_areas =
            Layout::vertical(vec![Constraint::Length(1); rows]).split(col_areas[col]);
        for (row, &pct) in chunk.iter().enumerate() {
            frame.render_widget(
                Gauge::default()
                    .label(format!("{:>2} {pct:>5.1}%", col * rows + row))
                    .gauge_style(Style::new().fg(CPU))
                    .ratio(gauge_ratio(pct)),
                row_areas[row],
            );
        }
    }

    let [ram_area, swap_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(mem_area);
    mem_gauge(frame, ram_area, "RAM ", m.mem_used, m.mem_total, MEMORY);
    mem_gauge(frame, swap_area, "Swap", m.swap_used, m.swap_total, GPU);

    render_power(frame, power_area, m);
    render_sensors(
        frame,
        sensors_area,
        m.temps.as_deref().unwrap_or_default(),
        m.fans.as_deref().unwrap_or_default(),
    );

    let procs = app.sorted_processes();
    let table = Table::new(
        procs.iter().map(|p| {
            Row::new([
                p.pid.to_string(),
                p.name.clone(),
                format!("{:.1}", p.cpu),
                format_bytes(p.mem),
            ])
        }),
        [
            Constraint::Length(7),
            Constraint::Fill(1), // Name: ratatui clips overlong cells to the column
            Constraint::Length(7),
            Constraint::Length(10),
        ],
    )
    .header(
        Row::new(["PID", "Name", "CPU%", "Mem"])
            .fg(ACCENT)
            .bold()
            .underlined(),
    )
    .row_highlight_style(Style::new().fg(Color::Black).bg(CPU).bold());
    let mut state = TableState::default()
        .with_selected(app.selected.min(procs.len().saturating_sub(1)));
    frame.render_stateful_widget(table, table_area, &mut state);
}

fn render_sensors(
    frame: &mut Frame,
    area: Rect,
    temperatures: &[(String, f32)],
    fans: &[(String, f32)],
) {
    let cpu_high = temperatures
        .iter()
        .filter(|(label, _)| !label.starts_with("Tg") && !label.starts_with("TG"))
        .map(|(_, value)| *value)
        .max_by(f32::total_cmp);
    let headline = format!(
        "CPU max {}   Fans {}",
        cpu_high.map_or_else(|| "—".into(), |value| format!("{value:.1}°C")),
        if fans.is_empty() {
            "—".into()
        } else {
            fans.iter()
                .map(|(label, rpm)| format!("{label} {rpm:.0} RPM"))
                .collect::<Vec<_>>()
                .join(" · ")
        }
    );
    let list = temperatures
        .iter()
        .map(|(label, value)| format!("{label} {value:.1}°"))
        .collect::<Vec<_>>()
        .join(" · ");
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(headline, Style::new().fg(THERMAL).bold())),
            Line::from(Span::styled(list, Style::new().fg(MUTED))),
        ])
        .block(
            Block::bordered()
                .title("Temperatures / Fans")
                .border_style(Style::new().fg(THERMAL)),
        ),
        area,
    );
}

// ponytail: no public ANE utilization counter exists; usage is derived as
// power / max power (asitop approach). 8 W ≈ M1-class ceiling — tune per chip
// if the gauge saturates or never moves.
const ANE_MAX_W: f32 = 8.0;

fn render_power(frame: &mut Frame, area: Rect, metrics: &crate::metrics::Metrics) {
    let areas = Layout::horizontal([Constraint::Fill(1); 2]).split(area);
    let ane_util_pct = metrics.ane_power_w.map(|w| w / ANE_MAX_W * 100.0);
    let gauges = [
        ("GPU", metrics.gpu_util_pct, metrics.gpu_power_w, GPU),
        ("ANE", ane_util_pct, metrics.ane_power_w, ACCENT),
    ];
    for (slot, (title, util, watts, color)) in areas.iter().zip(gauges) {
        let label = format!(
            "{}   {}",
            util.map_or_else(|| "—".into(), |value| format!("{value:.1}%")),
            watts.map_or_else(|| "—".into(), |value| format!("{value:.2} W"))
        );
        frame.render_widget(
            Gauge::default()
                .block(
                    Block::bordered()
                        .title(title)
                        .border_style(Style::new().fg(color)),
                )
                .label(label)
                .gauge_style(Style::new().fg(color))
                .ratio(gauge_ratio(util.unwrap_or(0.0))),
            *slot,
        );
    }
}

fn format_frequencies(frequencies: Option<&[f32]>) -> String {
    frequencies.map_or_else(
        || "—".into(),
        |values| {
            values
                .iter()
                .map(|value| format!("{value:.0}"))
                .collect::<Vec<_>>()
                .join("/")
                + " MHz"
        },
    )
}

fn mem_gauge(frame: &mut Frame, area: Rect, label: &str, used: u64, total: u64, color: Color) {
    let ratio = if total == 0 {
        0.0
    } else {
        gauge_ratio(100.0 * used as f32 / total as f32)
    };
    frame.render_widget(
        Gauge::default()
            .label(format!(
                "{label} {:.1}/{:.1} GiB",
                used as f64 / GIB,
                total as f64 / GIB
            ))
            .gauge_style(Style::new().fg(color))
            .ratio(ratio),
        area,
    );
}

fn render_inventory(frame: &mut Frame, area: Rect, app: &App) {
    let [status_area, table_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);

    let count = |s: Source| app.inventory.iter().filter(|i| i.source == s).count();
    let mut status = String::new();
    if app.scanning {
        const SPIN: [char; 4] = ['|', '/', '-', '\\'];
        status.push(SPIN[app.tick as usize % SPIN.len()]);
        status.push_str(" scanning…  ");
    }
    status.push_str(&format!(
        "Apps {} · Brew {} · Casks {} · Tools {} · Languages {}",
        count(Source::App),
        count(Source::Brew),
        count(Source::Cask),
        count(Source::Tool),
        count(Source::Language),
    ));
    if let Some(src) = app.inv_source_filter {
        status.push_str(&format!("  [source: {}]", src.label()));
    }
    if app.inv_filter_mode || !app.inv_filter.is_empty() {
        status.push_str(&format!(
            "  /{}{}",
            app.inv_filter,
            if app.inv_filter_mode { "▏" } else { "" }
        ));
    }
    frame.render_widget(Paragraph::new(status).fg(ACCENT).bold(), status_area);

    let items = app.filtered_inventory();
    let path_width = usize::from(table_area.width / 3);
    let table = Table::new(
        items.iter().map(|i| {
            let path = i
                .path
                .as_ref()
                .map(|p| truncate_left(&p.to_string_lossy(), path_width))
                .unwrap_or_default();
            Row::new(vec![
                Cell::from(i.name.clone()),
                Cell::from(i.version.clone().unwrap_or_default()),
                Cell::from(i.source.label()),
                Cell::from(Span::styled(path, Style::new().fg(MUTED))),
            ])
        }),
        [
            Constraint::Fill(1),
            Constraint::Length(14),
            Constraint::Length(9),
            Constraint::Length(path_width as u16),
        ],
    )
    .header(
        Row::new(["Name", "Version", "Source", "Path"])
            .fg(ACCENT)
            .bold()
            .underlined(),
    )
    .row_highlight_style(Style::new().fg(Color::Black).bg(CPU).bold());
    let mut state =
        TableState::default().with_selected(app.inv_selected.min(items.len().saturating_sub(1)));
    frame.render_stateful_widget(table, table_area, &mut state);
}

/// Truncate from the left so the tail (the binary/bundle name) stays visible.
fn truncate_left(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max || max == 0 {
        return s.to_string();
    }
    let tail: String = s.chars().skip(len - max + 1).collect();
    format!("…{tail}")
}

/// Gauge panics outside 0.0..=1.0; sysinfo reports >100% for multi-threaded
/// processes and can produce NaN early on.
fn gauge_ratio(pct: f32) -> f64 {
    let ratio = f64::from(pct) / 100.0;
    if ratio.is_finite() { ratio.clamp(0.0, 1.0) } else { 0.0 }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gauge_ratio_is_always_valid() {
        for pct in [-5.0, 0.0, 42.0, 100.0, 250.0, f32::NAN, f32::INFINITY] {
            let r = gauge_ratio(pct);
            assert!((0.0..=1.0).contains(&r), "pct {pct} gave ratio {r}");
        }
    }

    #[test]
    fn truncate_left_keeps_tail() {
        assert_eq!(truncate_left("/usr/local/bin/node", 10), "…/bin/node");
        assert_eq!(truncate_left("short", 10), "short");
        assert_eq!(truncate_left("abc", 0), "abc");
    }

    #[test]
    fn format_bytes_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(5 * 1024 * 1024 + 512 * 1024), "5.5 MiB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
        assert_eq!(format_bytes(u64::MAX), format!("{:.1} GiB", u64::MAX as f64 / GIB));
    }
}
