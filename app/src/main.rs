//! JeffyCoin desktop app: the node, the wallet, and the GPU miner in one window.
//!
//! Opening the program creates a wallet key on first run, starts a node that
//! connects through the public relay, and shows the balance, the chain height,
//! and controls for mining and sending. See `backend.rs` for what runs behind
//! the window.

// No console window on Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod applog;
mod backend;
mod telemetry;
mod update;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use jcoin::amount::format_jef;

use backend::{connection_kind, display_path, short, zero_bits, Backend, MiningState, Settings};

fn main() -> eframe::Result {
    // Start reporting before the window and the backend's threads, so a
    // panic anywhere is reported. The guard sends what is queued at exit.
    let settings = Settings::from_env();
    let _telemetry = telemetry::init(&settings.data_dir, settings.regtest);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([520.0, 720.0])
            .with_min_inner_size([420.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "JeffyCoin",
        options,
        Box::new(move |_cc| Ok(Box::new(App::new(settings)))),
    )
}

struct App {
    /// The running backend, or the reason it could not start.
    backend: Result<Backend, String>,
    /// Selected GPU: `None` lets the system choose.
    adapter: Option<usize>,
    recipient: String,
    amount: String,
    fee: String,
    /// Result of the last send: the transaction id, or the error.
    send_result: Option<Result<String, String>>,
    keys: KeysForm,
    updates: Updates,
    /// The label for this computer, as typed in the Developer section.
    device: String,
    device_result: Option<Result<String, String>>,
}

/// What the update check or an install is doing.
#[derive(Clone)]
enum UpdateState {
    Checking,
    /// The releases with a build for this system, newest first.
    Listed(Vec<update::Release>),
    Installing(String),
    Failed(String),
}

/// The Updates section: a background thread writes `state`.
struct Updates {
    state: Arc<Mutex<UpdateState>>,
    /// Index of the selected release in the list.
    selected: usize,
}

impl Updates {
    fn start() -> Updates {
        update::clean_up();
        let updates = Updates {
            state: Arc::new(Mutex::new(UpdateState::Checking)),
            selected: 0,
        };
        updates.check();
        updates
    }

    fn check(&self) {
        let state = self.state.clone();
        *state.lock().unwrap() = UpdateState::Checking;
        std::thread::spawn(move || {
            let result = match update::fetch_releases() {
                Ok(list) => UpdateState::Listed(list),
                Err(e) => UpdateState::Failed(e),
            };
            *state.lock().unwrap() = result;
        });
    }

    fn install(&self, release: update::Release) {
        let state = self.state.clone();
        *state.lock().unwrap() = UpdateState::Installing(release.tag.clone());
        std::thread::spawn(move || {
            // `restart` only returns on failure.
            let result = update::install(&release).and_then(|()| update::restart());
            if let Err(e) = result {
                *state.lock().unwrap() = UpdateState::Failed(e);
            }
        });
    }
}

/// Input fields and the last result of the Keys section.
#[derive(Default)]
struct KeysForm {
    new_name: String,
    import_name: String,
    import_secret: String,
    /// What the last key action did, or why it failed.
    result: Option<Result<String, String>>,
}

impl App {
    fn new(settings: Settings) -> App {
        let device = telemetry::device_name(&settings.data_dir);
        App {
            backend: Backend::start(settings),
            adapter: None,
            recipient: String::new(),
            amount: String::new(),
            fee: String::new(),
            send_result: None,
            keys: KeysForm::default(),
            updates: Updates::start(),
            device,
            device_result: None,
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // The figures change without input, so redraw on a timer.
        ui.ctx().request_repaint_after(Duration::from_millis(500));

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading("JeffyCoin");
                let backend = match &mut self.backend {
                    Ok(backend) => backend,
                    Err(e) => {
                        ui.add_space(8.0);
                        ui.colored_label(ui.visuals().error_fg_color, format!("Cannot start: {e}"));
                        return;
                    }
                };
                backend.pump_miner();
                if backend.settings.regtest {
                    ui.colored_label(ui.visuals().warn_fg_color, "Test network (regtest)");
                }
                ui.add_space(8.0);
                network_section(ui, backend);
                ui.add_space(12.0);
                wallet_section(ui, backend);
                ui.add_space(12.0);
                keys_section(ui, backend, &mut self.keys);
                ui.add_space(12.0);
                mining_section(ui, backend, &mut self.adapter);
                ui.add_space(12.0);
                send_section(
                    ui,
                    backend,
                    &mut self.recipient,
                    &mut self.amount,
                    &mut self.fee,
                    &mut self.send_result,
                );
                ui.add_space(12.0);
                updates_section(ui, &mut self.updates, backend);
                ui.add_space(12.0);
                developer_section(ui, backend, &mut self.device, &mut self.device_result);
                ui.add_space(12.0);
                ui.separator();
                ui.small(format!("Data folder: {}", display_path(&backend.settings.data_dir)));
                ui.small(
                    "Your coins are controlled by the key files in this folder. Back them up; anyone who has one can spend its coins.",
                );
            });
        });
    }
}

fn network_section(ui: &mut egui::Ui, backend: &Backend) {
    let view = backend.view();
    ui.strong("Network");
    if let Some(reason) = &view.fatal {
        ui.colored_label(ui.visuals().error_fg_color, format!("Node stopped: {reason}"));
        return;
    }
    let status = if !view.online {
        "Starting the node…".to_string()
    } else if view.peers == 0 {
        "Looking for other nodes…".to_string()
    } else if view.peers == 1 {
        "Connected to 1 node".to_string()
    } else {
        format!("Connected to {} nodes", view.peers)
    };
    egui::Grid::new("network").num_columns(2).show(ui, |ui| {
        ui.label("Status");
        ui.label(status);
        ui.end_row();
        ui.label("Block height");
        ui.label(view.height.to_string());
        ui.end_row();
    });
}

fn wallet_section(ui: &mut egui::Ui, backend: &Backend) {
    let view = backend.view();
    ui.strong("Wallet");
    let balance = if view.account_known {
        format!("{} JEF", format_jef(view.balance))
    } else {
        "… JEF".to_string()
    };
    ui.label(egui::RichText::new(balance).size(28.0));
    if view.pending > 0 {
        ui.label(format!(
            "{} JEF is committed to transfers not yet in a block",
            format_jef(view.pending)
        ));
    }
    ui.horizontal(|ui| {
        ui.label(format!("Address of key '{}'", backend.key_name));
        ui.monospace(short(&backend.address));
        if ui.button("Copy").clicked() {
            ui.ctx().copy_text(backend.address.clone());
        }
    });
}

fn keys_section(ui: &mut egui::Ui, backend: &mut Backend, form: &mut KeysForm) {
    ui.strong("Keys");
    ui.label("Each key is a separate address with its own balance. The key in use receives mining rewards and signs what you send.");
    let mining = !matches!(backend.mining_state(), MiningState::Idle | MiningState::Failed(_));

    let mut select = None;
    egui::Grid::new("keys").num_columns(4).show(ui, |ui| {
        for key in backend.keys() {
            ui.label(&key.name);
            ui.monospace(short(&key.address));
            if ui.button("Copy address").clicked() {
                ui.ctx().copy_text(key.address.clone());
            }
            if key.name == backend.key_name {
                ui.label("in use");
            } else if ui
                .add_enabled(!mining, egui::Button::new("Use"))
                .on_disabled_hover_text("Stop mining before switching keys")
                .clicked()
            {
                select = Some(key.name.clone());
            }
            ui.end_row();
        }
    });
    if let Some(name) = select {
        form.result = Some(
            backend
                .select_key(&name)
                .map(|()| format!("Now using key '{name}'.")),
        );
    }

    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut form.new_name)
                .desired_width(160.0)
                .hint_text("name for a new key"),
        );
        if ui.button("Create key").clicked() {
            let name = form.new_name.trim().to_string();
            form.result = Some(
                backend
                    .create_key(&name)
                    .map(|()| format!("Created key '{name}'. Press Use to switch to it.")),
            );
            if matches!(form.result, Some(Ok(_))) {
                form.new_name.clear();
            }
        }
    });

    ui.collapsing("Back up or import a key", |ui| {
        ui.label(format!(
            "Back up: the secret key of '{}' is all that is needed to spend its coins. Keep a copy somewhere safe and show it to nobody.",
            backend.key_name
        ));
        ui.small(format!("It is stored in {}", display_path(&backend.key_file())));
        if ui.button("Copy secret key").clicked() {
            ui.ctx().copy_text(backend.secret_hex());
            form.result = Some(Ok(format!(
                "The secret key of '{}' is on the clipboard.",
                backend.key_name
            )));
        }
        ui.add_space(6.0);
        ui.label("Import: paste a secret key (64 hex characters) to add it under a new name.");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut form.import_name)
                    .desired_width(120.0)
                    .hint_text("name"),
            );
            ui.add(
                egui::TextEdit::singleline(&mut form.import_secret)
                    .desired_width(200.0)
                    .password(true)
                    .hint_text("secret key"),
            );
            if ui.button("Import").clicked() {
                let name = form.import_name.trim().to_string();
                form.result = Some(
                    backend
                        .import_key(&name, &form.import_secret)
                        .map(|()| format!("Imported key '{name}'. Press Use to switch to it.")),
                );
                if matches!(form.result, Some(Ok(_))) {
                    form.import_name.clear();
                    form.import_secret.clear();
                }
            }
        });
    });

    match &form.result {
        Some(Ok(text)) => {
            ui.label(text);
        }
        Some(Err(e)) => {
            ui.colored_label(ui.visuals().error_fg_color, e.as_str());
        }
        None => {}
    }
}

fn mining_section(ui: &mut egui::Ui, backend: &mut Backend, adapter: &mut Option<usize>) {
    ui.strong("Mining");
    let state = backend.mining_state();
    let busy = matches!(state, MiningState::Starting | MiningState::Running);

    let gpus = backend.gpus();
    ui.add_enabled_ui(!busy, |ui| {
        let selected = match (*adapter, &gpus) {
            (Some(i), Some(names)) => names.get(i).cloned().unwrap_or_default(),
            (_, None) => "Listing GPUs…".to_string(),
            (None, _) => "Automatic (fastest GPU)".to_string(),
        };
        egui::ComboBox::from_label("GPU")
            .selected_text(selected)
            .width(320.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(adapter, None, "Automatic (fastest GPU)");
                for (i, name) in gpus.iter().flatten().enumerate() {
                    ui.selectable_value(adapter, Some(i), name);
                }
            });
    });

    if busy {
        if ui.button("Stop mining").clicked() {
            backend.stop_mining();
        }
    } else if ui.button("Start mining").clicked() {
        backend.start_mining(*adapter);
    }

    match state {
        MiningState::Idle => {
            ui.label(format!("Not mining. Rewards for blocks you mine go to key '{}'.", backend.key_name));
        }
        MiningState::Starting => {
            ui.label("Opening the GPU…");
        }
        MiningState::Failed(e) => {
            ui.colored_label(ui.visuals().error_fg_color, format!("Mining stopped: {e}"));
        }
        MiningState::Running => {
            if let Some((rate, height, found, status)) = backend.mining_stats() {
                egui::Grid::new("mining").num_columns(2).show(ui, |ui| {
                    ui.label("Hash rate");
                    ui.label(format!("{:.1} MH/s", rate as f64 / 1e6));
                    ui.end_row();
                    ui.label("Working on block");
                    ui.label(height.to_string());
                    ui.end_row();
                    ui.label("Blocks mined this session");
                    ui.label(found.to_string());
                    ui.end_row();
                });
                if !status.is_empty() {
                    ui.small(status);
                }
            }
        }
    }
}

fn updates_section(ui: &mut egui::Ui, updates: &mut Updates, backend: &Backend) {
    ui.strong("Updates");
    let state = updates.state.lock().unwrap().clone();
    match state {
        UpdateState::Checking => {
            ui.label(format!("Version {}. Checking for updates…", update::CURRENT));
        }
        UpdateState::Installing(tag) => {
            ui.label(format!("Downloading {tag}. The app restarts when it is ready…"));
        }
        UpdateState::Failed(e) => {
            ui.label(format!("Version {}.", update::CURRENT));
            ui.colored_label(ui.visuals().error_fg_color, e);
            if ui.button("Check again").clicked() {
                updates.check();
            }
        }
        UpdateState::Listed(list) => {
            let newest = list.first().filter(|r| r.is_newer());
            match newest {
                Some(r) => ui.colored_label(
                    ui.visuals().warn_fg_color,
                    format!("Version {}. A newer version is available: {}.", update::CURRENT, r.tag),
                ),
                None => ui.label(format!("Version {}. This is the newest version.", update::CURRENT)),
            };
            updates.selected = updates.selected.min(list.len().saturating_sub(1));
            let describe = |r: &update::Release| match r.relation() {
                "" => format!("{} ({})", r.tag, r.date),
                relation => format!("{} ({}, {relation})", r.tag, r.date),
            };
            ui.horizontal(|ui| {
                if let Some(chosen) = list.get(updates.selected) {
                    egui::ComboBox::from_id_salt("release")
                        .selected_text(describe(chosen))
                        .width(240.0)
                        .show_ui(ui, |ui| {
                            for (i, r) in list.iter().enumerate() {
                                ui.selectable_value(&mut updates.selected, i, describe(r));
                            }
                        });
                    let mining = !matches!(
                        backend.mining_state_peek(),
                        MiningState::Idle | MiningState::Failed(_)
                    );
                    let same = chosen.relation() == "this version";
                    if ui
                        .add_enabled(!mining && !same, egui::Button::new("Install and restart"))
                        .on_disabled_hover_text(if same {
                            "This version is already running"
                        } else {
                            "Stop mining before updating"
                        })
                        .clicked()
                    {
                        updates.install(chosen.clone());
                    }
                }
                if ui.button("Check again").clicked() {
                    updates.check();
                }
            });
            if list.is_empty() {
                ui.label("No release has a download for this system yet.");
            } else {
                ui.small("A different version may use different network rules. If the chain does not sync after a change, ask before deleting anything.");
            }
        }
    }
}

/// How long ago `since` (Unix seconds) was, in words.
fn age(since: u64) -> String {
    let secs = applog::now().saturating_sub(since);
    match secs {
        0..=59 => format!("{secs} s"),
        60..=3599 => format!("{} min", secs / 60),
        _ => format!("{} h {} min", secs / 3600, secs % 3600 / 60),
    }
}

fn developer_section(
    ui: &mut egui::Ui,
    backend: &mut Backend,
    device: &mut String,
    device_result: &mut Option<Result<String, String>>,
) {
    // JCOIN_APP_DEV=1 starts with the section open.
    let open = std::env::var("JCOIN_APP_DEV").is_ok_and(|v| v == "1");
    egui::CollapsingHeader::new("Developer").default_open(open).show(ui, |ui| {
        let view = backend.view();
        egui::Grid::new("dev-node").num_columns(2).show(ui, |ui| {
            ui.label("Version");
            ui.label(env!("CARGO_PKG_VERSION"));
            ui.end_row();
            ui.label("Difficulty");
            ui.label(format!("{} leading zero bits", zero_bits(&view.target)))
                .on_hover_text(format!("Target {}", view.target));
            ui.end_row();
        });
        if ui.button("Copy diagnostics").on_hover_text(
            "Copies the node state, connections, and recent log as text, to paste into a bug report. It holds no secret keys.",
        ).clicked() {
            ui.ctx().copy_text(backend.diagnostics());
        }

        ui.add_space(6.0);
        ui.strong("Reporting");
        ui.label("This app sends its log and crash reports to the developer, to find problems. They hold addresses and network details, never secret keys.");
        ui.horizontal(|ui| {
            ui.label("Label for this computer");
            ui.add(egui::TextEdit::singleline(device).desired_width(160.0).hint_text("for example: Noah desktop"));
            if ui.button("Save").clicked() {
                *device_result = Some(telemetry::set_device_name(&backend.settings.data_dir, device));
                if let Some(Ok(saved)) = device_result {
                    *device = saved.clone();
                    backend.log.push(false, &format!("[app] device label set to '{saved}'"));
                }
            }
        });
        match device_result {
            Some(Ok(saved)) if saved.is_empty() => {
                ui.small("Label removed.");
            }
            Some(Ok(saved)) => {
                ui.small(format!("Reports from this computer are now labeled '{saved}'."));
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e.as_str());
            }
            None => {}
        }

        ui.add_space(6.0);
        ui.strong(format!("Connections ({})", view.connections.len()));
        if view.connections.is_empty() {
            ui.label("None yet.");
        }
        egui::Grid::new("dev-connections").num_columns(5).striped(true).show(ui, |ui| {
            for c in &view.connections {
                ui.monospace(short(&c.peer)).on_hover_text(&c.peer);
                ui.label(connection_kind(c)).on_hover_text(&c.address);
                ui.label(if c.outbound { "outbound" } else { "inbound" });
                ui.label(if c.session {
                    "exchanging blocks"
                } else if c.is_relay {
                    "relay service"
                } else {
                    "transport only"
                });
                ui.label(age(c.since));
                ui.end_row();
            }
        });
        ui.collapsing("This node's addresses", |ui| {
            for a in &view.advertised {
                ui.small(a);
            }
        });

        ui.add_space(6.0);
        let won = backend.won_blocks();
        ui.strong(format!("Blocks mined since the app started ({})", won.len()));
        if won.is_empty() {
            ui.label("None yet.");
        }
        egui::Grid::new("dev-won").num_columns(5).striped(true).show(ui, |ui| {
            for b in won.iter().rev() {
                ui.label(format!("#{}", b.height));
                ui.label(format!("{} UTC", applog::utc_clock(b.timestamp)));
                ui.label(format!("{} JEF", format_jef(b.reward)));
                ui.label(match b.transfers {
                    1 => "1 transfer".to_string(),
                    n => format!("{n} transfers"),
                });
                ui.monospace(short(&b.hash)).on_hover_text(format!(
                    "Hash {}\n{} leading zero bits",
                    b.hash,
                    zero_bits(&b.target)
                ));
                ui.end_row();
            }
        });

        ui.add_space(6.0);
        ui.strong("Log (UTC)");
        ui.small(format!("Also written to {}", display_path(&backend.log.path)));
        let tail = backend.log.tail();
        egui::ScrollArea::vertical()
            .id_salt("dev-log")
            .max_height(220.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in tail.iter().skip(tail.len().saturating_sub(200)) {
                    let text = egui::RichText::new(format!(
                        "{} {}",
                        applog::utc_clock(line.time),
                        line.text
                    ))
                    .monospace()
                    .small();
                    if line.error {
                        ui.label(text.color(ui.visuals().error_fg_color));
                    } else {
                        ui.label(text);
                    }
                }
            });
    });
}

fn send_section(
    ui: &mut egui::Ui,
    backend: &mut Backend,
    recipient: &mut String,
    amount: &mut String,
    fee: &mut String,
    result: &mut Option<Result<String, String>>,
) {
    ui.strong("Send");
    egui::Grid::new("send").num_columns(2).show(ui, |ui| {
        ui.label("To address");
        ui.add(
            egui::TextEdit::singleline(recipient)
                .desired_width(340.0)
                .hint_text("64 hex characters"),
        );
        ui.end_row();
        ui.label("Amount (JEF)");
        ui.add(egui::TextEdit::singleline(amount).desired_width(120.0));
        ui.end_row();
        ui.label("Fee (JEF)");
        ui.add(egui::TextEdit::singleline(fee).desired_width(120.0).hint_text("0"));
        ui.end_row();
    });
    if ui.button("Send").clicked() {
        *result = Some(backend.send(recipient, amount, fee));
        if matches!(result, Some(Ok(_))) {
            recipient.clear();
            amount.clear();
            fee.clear();
        }
    }
    match result {
        Some(Ok(id)) => {
            ui.label(format!("Sent. Transaction {}", short(id)));
        }
        Some(Err(e)) => {
            ui.colored_label(ui.visuals().error_fg_color, e.as_str());
        }
        None => {}
    }

    let sent: Vec<_> = backend.sent.iter().filter(|tx| tx.sender == backend.address).collect();
    if !sent.is_empty() {
        ui.add_space(6.0);
        ui.label("Sent this session");
        // A transfer is in the chain once the confirmed nonce has passed its own.
        let confirmed_nonce = backend.view().nonce;
        egui::Grid::new("sent").num_columns(3).show(ui, |ui| {
            for tx in sent.iter().rev() {
                ui.label(format!("{} JEF", format_jef(tx.amount)));
                ui.monospace(format!("to {}", short(&tx.recipient)));
                let status = if confirmed_nonce > tx.nonce {
                    "confirmed"
                } else {
                    "waiting for a block"
                };
                ui.label(status)
                    .on_hover_text(format!("Transaction {}", tx.id));
                ui.end_row();
            }
        });
    }
}
