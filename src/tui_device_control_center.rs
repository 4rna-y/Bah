//! Terminal device control centre.  It deliberately has no GPUI dependency: the
//! terminal owns the screen, while the existing system-control worker retains all
//! privileged D-Bus and sysfs access in one small module.
use std::{io, time::Duration};

use anyhow::Result;
use ratatui::crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Tabs, Wrap},
};
use ratatui_image::{
    StatefulImage,
    picker::{Picker, ProtocolType},
    protocol::StatefulProtocol,
};

use crate::{
    app::{DeviceControlCenterPage, DeviceControlCenterRoute},
    config::Config,
    hyprland::display::DisplayLayout,
    modules::system_controls::{ControlAction, ControlChannels, ControlSnapshot, WifiNetwork},
};

const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::DarkGray;

pub fn run(route: DeviceControlCenterRoute) {
    if let Err(error) = run_inner(route) {
        eprintln!("bah dcc: {error:#}");
    }
}

struct RestoreTerminal;
impl Drop for RestoreTerminal {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut out = io::stdout();
        let _ = execute!(out, LeaveAlternateScreen);
    }
}

struct Dcc {
    page: DeviceControlCenterPage,
    focus: usize,
    snapshot: ControlSnapshot,
    controls: ControlChannels,
    layout: Option<DisplayLayout>,
    selected_monitor: usize,
    message: Option<String>,
    image: Option<StatefulProtocol>,
    image_warning: Option<String>,
    mouse_rows: Rect,
}

fn run_inner(route: DeviceControlCenterRoute) -> Result<()> {
    enable_raw_mode()?;
    let _restore = RestoreTerminal;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;
    let controls = crate::modules::system_controls::start_worker();
    let (image, image_warning) = image_protocol();
    let mut app = Dcc {
        page: route.page,
        focus: 0,
        snapshot: ControlSnapshot::default(),
        controls,
        layout: crate::hyprland::display::load_layout().ok(),
        selected_monitor: 0,
        message: route
            .ssid
            .map(|ssid| format!("指定されたネットワーク: {}", String::from_utf8_lossy(&ssid))),
        image,
        image_warning,
        mouse_rows: Rect::default(),
    };
    app.discovery();
    loop {
        while let Ok(snapshot) = app.controls.updates.try_recv() {
            app.snapshot = snapshot;
        }
        terminal.draw(|frame| draw(frame, &mut app))?;
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if !app.key(key.code, key.modifiers) {
                        break;
                    }
                }
                Event::Mouse(mouse) => app.mouse(mouse.kind, mouse.column, mouse.row),
                _ => {}
            }
        }
    }
    let _ = app
        .controls
        .actions
        .try_send(ControlAction::SetWifiDiscovery(false));
    let _ = app
        .controls
        .actions
        .try_send(ControlAction::SetBluetoothDiscovery(false));
    Ok(())
}

fn image_protocol() -> (Option<StatefulProtocol>, Option<String>) {
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    if picker.protocol_type() != ProtocolType::Kitty {
        return (
            None,
            Some("Kitty Graphics Protocol 非対応: 画像をテキストで表示します".into()),
        );
    }
    let config = Config::load().unwrap_or_default();
    let source = config
        .wallpaper
        .or_else(|| config.wallpapers.values().next().cloned());
    match source.and_then(|path| image::ImageReader::open(path).ok()?.decode().ok()) {
        Some(image) => (Some(picker.new_resize_protocol(image)), None),
        None => (None, Some("壁紙プレビューは未設定です".into())),
    }
}

impl Dcc {
    fn discovery(&self) {
        let _ = self
            .controls
            .actions
            .try_send(ControlAction::SetWifiDiscovery(
                self.page == DeviceControlCenterPage::Network,
            ));
        let _ = self
            .controls
            .actions
            .try_send(ControlAction::SetBluetoothDiscovery(
                self.page == DeviceControlCenterPage::Bluetooth,
            ));
    }
    fn set_page(&mut self, page: DeviceControlCenterPage) {
        self.page = page;
        self.focus = 0;
        self.discovery();
    }
    fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => return false,
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => return false,
            KeyCode::Char('1') => self.set_page(DeviceControlCenterPage::Network),
            KeyCode::Char('2') => self.set_page(DeviceControlCenterPage::Bluetooth),
            KeyCode::Char('3') => self.set_page(DeviceControlCenterPage::Display),
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
                if self.page == DeviceControlCenterPage::Display && self.focus > 0 =>
            {
                self.move_monitor(code, modifiers)
            }
            KeyCode::Tab | KeyCode::Down | KeyCode::Char('j') => {
                self.focus = self.focus.saturating_add(1)
            }
            KeyCode::BackTab | KeyCode::Up | KeyCode::Char('k') => {
                self.focus = self.focus.saturating_sub(1)
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.activate(),
            KeyCode::Char('r') if self.page == DeviceControlCenterPage::Display => {
                self.layout = crate::hyprland::display::load_layout().ok();
            }
            _ => {}
        }
        true
    }
    fn mouse(&mut self, kind: MouseEventKind, column: u16, row: u16) {
        if matches!(kind, MouseEventKind::Down(_)) && row == 1 {
            self.set_page(match column {
                0..=19 => DeviceControlCenterPage::Network,
                20..=39 => DeviceControlCenterPage::Bluetooth,
                _ => DeviceControlCenterPage::Display,
            });
        } else if matches!(kind, MouseEventKind::Down(_))
            && self.mouse_rows.contains((column, row).into())
        {
            self.focus = usize::from(row - self.mouse_rows.y);
            self.activate();
        }
    }
    fn activate(&mut self) {
        match self.page {
            DeviceControlCenterPage::Network => {
                if self.focus == 0 {
                    let _ = self.controls.actions.try_send(ControlAction::ToggleWifi);
                    return;
                }
                if let Some(network) = self.snapshot.wifi_networks.get(self.focus - 1).cloned() {
                    self.connect(network);
                }
            }
            DeviceControlCenterPage::Bluetooth => {
                if self.focus == 0 {
                    let _ = self
                        .controls
                        .actions
                        .try_send(ControlAction::ToggleBluetooth);
                    return;
                }
                if let Some(device) = self.snapshot.bluetooth_devices.get(self.focus - 1) {
                    let action = if device.connected {
                        ControlAction::DisconnectBluetooth {
                            device_path: device.path.clone(),
                        }
                    } else if device.paired {
                        ControlAction::ConnectBluetooth {
                            device_path: device.path.clone(),
                        }
                    } else {
                        ControlAction::PairBluetooth {
                            device_path: device.path.clone(),
                        }
                    };
                    let _ = self.controls.actions.try_send(action);
                }
            }
            DeviceControlCenterPage::Display => {
                if self.focus == 0 {
                    self.apply_layout();
                } else {
                    self.selected_monitor = self.focus - 1;
                }
            }
        }
    }
    fn connect(&mut self, network: WifiNetwork) {
        if matches!(
            network.security,
            crate::modules::system_controls::WifiSecurity::Personal
        ) && !network.saved
        {
            self.message = Some(format!(
                "{} はパスワードが必要です。TUIのパスワード入力は次の更新で追加されます。",
                network.label
            ));
        } else {
            let _ = self.controls.actions.try_send(ControlAction::ConnectWifi {
                ssid: network.ssid,
                password: None,
            });
        }
    }
    fn move_monitor(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        let Some(layout) = self.layout.as_mut() else {
            return;
        };
        let Some(monitor) = layout.monitors.get(self.selected_monitor).cloned() else {
            return;
        };
        let step = if modifiers.contains(KeyModifiers::CONTROL) {
            1
        } else if modifiers.contains(KeyModifiers::SHIFT) {
            100
        } else {
            10
        };
        let (x, y) = match code {
            KeyCode::Left => (monitor.x - step, monitor.y),
            KeyCode::Right => (monitor.x + step, monitor.y),
            KeyCode::Up => (monitor.x, monitor.y - step),
            _ => (monitor.x, monitor.y + step),
        };
        layout.move_monitor(&monitor.name, x, y);
    }
    fn apply_layout(&mut self) {
        match self
            .layout
            .as_ref()
            .map(crate::hyprland::display::apply_layout)
        {
            Some(Ok(())) => self.message = Some("ディスプレイ設定を適用しました".into()),
            Some(Err(error)) => self.message = Some(format!("適用できません: {error}")),
            None => self.message = Some("モニター情報を取得できません".into()),
        }
    }
}

fn draw(frame: &mut Frame<'_>, app: &mut Dcc) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(area);
    let selected = match app.page {
        DeviceControlCenterPage::Network => 0,
        DeviceControlCenterPage::Bluetooth => 1,
        DeviceControlCenterPage::Display => 2,
    };
    frame.render_widget(
        Tabs::new(["1 Network", "2 Bluetooth", "3 Display"])
            .select(selected)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Bah Device Control Center "),
            )
            .highlight_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        chunks[0],
    );
    match app.page {
        DeviceControlCenterPage::Network => draw_network(frame, app, chunks[1]),
        DeviceControlCenterPage::Bluetooth => draw_bluetooth(frame, app, chunks[1]),
        DeviceControlCenterPage::Display => draw_display(frame, app, chunks[1]),
    }
    let mut help = "1-3: tab  ↑↓/j/k: select  Enter: action  q: close".to_string();
    if let Some(message) = &app.message {
        help.push_str("  |  ");
        help.push_str(message);
    }
    frame.render_widget(
        Paragraph::new(help).style(Style::default().fg(MUTED)),
        chunks[2],
    );
}

fn draw_network(frame: &mut Frame<'_>, app: &mut Dcc, area: Rect) {
    let mut rows = vec![format!(
        "[{}] Wi-Fi",
        if app.snapshot.wifi.enabled {
            "on"
        } else {
            "off"
        }
    )];
    rows.extend(app.snapshot.wifi_networks.iter().map(|network| {
        format!(
            "{}  {}%{}",
            network.label,
            network.strength,
            if network.connected { "  connected" } else { "" }
        )
    }));
    app.mouse_rows = Rect {
        x: area.x + 1,
        y: area.y + 2,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    list(frame, "Network", rows, app.focus, area);
}
fn draw_bluetooth(frame: &mut Frame<'_>, app: &mut Dcc, area: Rect) {
    let mut rows = vec![format!(
        "[{}] Bluetooth",
        if app.snapshot.bluetooth.enabled {
            "on"
        } else {
            "off"
        }
    )];
    rows.extend(app.snapshot.bluetooth_devices.iter().map(|device| {
        format!(
            "{}  {}",
            device.label,
            if device.connected {
                "connected (Enter: disconnect)"
            } else if device.paired {
                "paired (Enter: connect)"
            } else {
                "Enter: pair"
            }
        )
    }));
    if app.snapshot.airpods.connected {
        rows.push(format!(
            "AirPods: L {}% / R {}%",
            app.snapshot.airpods.left_percent.unwrap_or(0),
            app.snapshot.airpods.right_percent.unwrap_or(0)
        ));
    }
    app.mouse_rows = Rect {
        x: area.x + 1,
        y: area.y + 2,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    list(frame, "Bluetooth", rows, app.focus, area);
}
fn draw_display(frame: &mut Frame<'_>, app: &mut Dcc, area: Rect) {
    let split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);
    let rows = app
        .layout
        .as_ref()
        .map(|layout| {
            let mut values = vec!["Apply display layout".into()];
            values.extend(layout.monitors.iter().map(|monitor| {
                format!(
                    "{}: {}×{} @ {},{}{}",
                    monitor.name,
                    monitor.width,
                    monitor.height,
                    monitor.x,
                    monitor.y,
                    if monitor.name == layout.main {
                        " ★"
                    } else {
                        ""
                    }
                )
            }));
            values
        })
        .unwrap_or_else(|| vec!["No Hyprland monitor data".into()]);
    app.mouse_rows = Rect {
        x: split[0].x + 1,
        y: split[0].y + 2,
        width: split[0].width.saturating_sub(2),
        height: split[0].height.saturating_sub(2),
    };
    list(
        frame,
        "Display (arrow keys move selected monitor)",
        rows,
        app.focus,
        split[0],
    );
    if let Some(image) = app.image.as_mut() {
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title("Wallpaper preview"),
            split[1],
        );
        frame.render_stateful_widget(
            StatefulImage::default(),
            Block::default().borders(Borders::ALL).inner(split[1]),
            image,
        );
    } else {
        frame.render_widget(
            Paragraph::new(
                app.image_warning
                    .clone()
                    .unwrap_or_else(|| "壁紙プレビューなし".into()),
            )
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Wallpaper preview"),
            ),
            split[1],
        );
    }
}
fn list(frame: &mut Frame<'_>, title: &str, rows: Vec<String>, selected: usize, area: Rect) {
    let items = rows.into_iter().enumerate().map(|(index, row)| {
        ListItem::new(Line::from(Span::styled(
            row,
            if index == selected {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            },
        )))
    });
    frame.render_widget(
        List::new(items).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}
