use crossbeam_channel::{unbounded, Receiver, Sender};
use eframe::egui;
use serialport::{SerialPort, SerialPortType};
use std::io::{Read, Write};
//use std::sync::mpmc::TrySendError::Full;
use std::time::{Duration, Instant};
use eframe::epaint::FontId;
use egui::TextStyle;

fn main() -> eframe::Result<()> {
    //let native_options = eframe::NativeOptions::default();


    //use egui::{FontId, TextStyle};
    //
    // fn set_large_fonts(ctx: &egui::Context) {
    //     let mut style = (*ctx.style()).clone();
    //
    //     style.text_styles = [
    //         (TextStyle::Heading, FontId::proportional(32.0)),
    //         (TextStyle::Body,    FontId::proportional(20.0)),
    //         (TextStyle::Button,  FontId::proportional(20.0)),
    //         (TextStyle::Small,   FontId::proportional(16.0)),
    //         (TextStyle::Monospace, FontId::monospace(18.0)),
    //     ]
    //         .into();
    //
    //     ctx.set_style(style);
    // }

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([2000.0, 700.0]) // width, height in logical points
            .with_min_inner_size([800.0, 600.0])
            .with_title("Serial commander"),
        ..Default::default()
    };


    eframe::run_native(
        "egui serial live",
        native_options,
        Box::new(|_cc| {
            //set_large_fonts(&cc.egui_ctx);
            Ok(Box::new(App::new())) }),
    )
}

enum ToSerial {
    Open { port: String, baud: u32 },
    Close,
    //SendLine(String),
    RxPair { a : u32, b: u32 },
}

// enum FromSerial {
//     Status(String),
//     RxLine(String),
//     RxNumber(f64),
// }

enum FromSerial {
    Status(String),
    RxPair { a: u32, b: u32 },
}

impl ToSerial {
    fn to_payload(&self) -> Option<[u8; 8]> {
        match self {
            ToSerial::RxPair { a, b } => {
                let mut payload = [0u8; 8];
                payload[0..4].copy_from_slice(&a.to_le_bytes());
                payload[4..8].copy_from_slice(&b.to_le_bytes());
                Some(payload)
            }
            _ => None, // Only RxPair gets sent
        }
    }
}

struct FullPort {
    name : String,
    full_name: String
}

struct App {
    //ports: Vec<String>,
    fonts_initialized: bool,
    ports : Vec<FullPort>,
    selected_port: usize,
    baud: u32,

    number_text: String,
    status: String,
    port_opened : bool,

    latest_a : u32,
    latest_b : u32,
    //latest_line: String,
    //latest_number: Option<f64>,
    rx_count: u64,

    to_serial: Sender<ToSerial>,
    from_serial: Receiver<FromSerial>,

    last_poll: Instant,
}

impl App {
    fn new() -> Self {
        let (to_serial, to_serial_rx) = unbounded::<ToSerial>();
        let (from_serial_tx, from_serial) = unbounded::<FromSerial>();
        spawn_serial_thread(to_serial_rx, from_serial_tx);

        let mut app = Self {
            fonts_initialized: false,
            ports: vec![],
            selected_port: 0,
            baud: 115_200,
            number_text: String::new(),
            status: String::new(),
            port_opened: false,
            //latest_line: String::new(),
            //latest_number: None,
            rx_count: 0,
            latest_a : 0,
            latest_b : 0,
            to_serial,
            from_serial,
            last_poll: Instant::now(),
        };
        app.refresh_ports();
        app
    }

    fn refresh_ports(&mut self) {
        self.ports.clear();
        match serialport::available_ports() {
            Ok(list) => {
                //self.ports = list.into_iter().map(|p| p.port_name).collect();

                self.ports = list.into_iter()
                    .filter_map(|p| match p.port_type {
                        SerialPortType::UsbPort(usb_info) => {
                            Some(FullPort {
                                name: p.port_name.clone(),
                                full_name: format!("{} {} {}",
                                                   p.port_name,
                                                   usb_info.manufacturer.as_deref().unwrap_or("No manufacturer"),
                                                   usb_info.product.as_deref().unwrap_or("No product"))
                            })
                        }
                        SerialPortType::PciPort => Some(FullPort {
                            name : p.port_name.clone(),
                            full_name: format!("{} (PCI)", p.port_name)
                        }),
                        SerialPortType::BluetoothPort => Some(FullPort {
                            name: p.port_name.clone(),
                            full_name: format!("{} (Bluetooth)", p.port_name)
                        }),
                        SerialPortType::Unknown => None,  // Exclude this!
                    })
                    .collect();
                if self.selected_port >= self.ports.len() {
                    self.selected_port = 0;
                }
                self.status = format!("Found {} ports", self.ports.len());
            }
            Err(e) => self.status = format!("Port scan error: {e}"),
        }
    }

    fn open_selected(&mut self) {
        if self.ports.is_empty() {
            self.status = "No ports. Click Refresh.".to_string();
            return;
        }
        let port = self.ports[self.selected_port].name.clone();
        let baud = self.baud;
        let _ = self.to_serial.send(ToSerial::Open { port, baud });
    }

    fn close_port(&mut self) {
        let _ = self.to_serial.send(ToSerial::Close);
    }

    fn send_number(&mut self) {
        // send as ASCII line "123\n"
        let v: f32 = match self.number_text.trim().parse() {
            Ok(v) => v,
            Err(e) => {
                self.status = format!("Invalid value: {e}");
                return;
            }
        };
        if v < 15.0 || v > 80.0 {
            self.status = format!("Invalid value, must be with-in 15.0 to 80.0 KHz: {v}");
            return;
        }
        let period = 100000000.0 / (v * 1000.0);
        let period_u : u32 = period.round() as u32;
        let _ = self.to_serial.send(ToSerial::RxPair {a : period_u, b : 0});
        //let line = format!("{v}\n");
        //let _ = self.to_serial.send(ToSerial::SendLine(line));
    }

    fn poll_incoming(&mut self) {
        // Drain channel without blocking so UI stays smooth.
        while let Ok(msg) = self.from_serial.try_recv() {
            match msg {
                FromSerial::Status(s) =>
                    {
                        self.status = s;
                        if self.status.len() >= 4 && &self.status[0..4] == "Open" {
                            self.port_opened = true;
                        }
                        if self.status.len() >= 4 && &self.status[0..4] == "Clos" {
                            self.port_opened = false;
                        }
                    }
                FromSerial::RxPair { a, b } => {
                    self.latest_a = a;
                    self.latest_b = b;
                    self.rx_count += 1;
                }
                /*
                FromSerial::RxLine(line) => {
                    self.latest_line = line.clone();
                    self.rx_count += 1;
                    // Optional: try parse a number from the line
                    if let Ok(n) = line.trim().parse::<f64>() {
                        self.latest_number = Some(n);
                    }
                }
                FromSerial::RxNumber(n) => {
                    self.latest_number = Some(n);
                    self.rx_count += 1;
                }
                 */
            }
        }
    }
}

fn set_large_fonts(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();

    style.text_styles = [
        (TextStyle::Heading, FontId::proportional(32.0)),
        (TextStyle::Body,    FontId::proportional(20.0)),
        (TextStyle::Button,  FontId::proportional(20.0)),
        (TextStyle::Small,   FontId::proportional(16.0)),
        (TextStyle::Monospace, FontId::monospace(18.0)),
    ].into();

    ctx.set_style(style);
}


impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {

        if !self.fonts_initialized {
            set_large_fonts(ctx);
            self.fonts_initialized = true;
        }


        // Poll serial messages regularly
        if self.last_poll.elapsed() >= Duration::from_millis(10) {
            self.poll_incoming();
            self.last_poll = Instant::now();
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Serial Control Panel");
            ui.spacing();

            ui.horizontal(|ui| {
                if ui.button("Refresh").clicked() {
                    self.refresh_ports();
                }

                ui.label("Port:");
                if self.ports.is_empty() {
                    ui.label("(none)");
                } else {
                    egui::ComboBox::from_id_salt("port_combo") // from_id_source("port_combo")
                        .selected_text(&self.ports[self.selected_port].full_name)
                        .show_ui(ui, |ui| {
                            for (i, full_port) in self.ports.iter().enumerate() {
                                ui.selectable_value(&mut self.selected_port, i, &full_port.full_name);
                            }
                        });
                }

                ui.label("Baud:");
                ui.add(egui::DragValue::new(&mut self.baud).range(1200..=2_000_000));

                /*
                if ui.button("Open").clicked() {
                    self.open_selected();
                }
                if ui.button("Close").clicked() {
                    self.close_port();
                }
                */
                ui.add_enabled_ui(!self.port_opened, |ui| {
                    if ui.button("Open").clicked() {
                        self.open_selected();
                    }
                })
                    .response
                    .on_disabled_hover_text("Port is already opened");

                ui.add_enabled_ui(self.port_opened, |ui| {
                    if ui.button("Close").clicked() {
                        self.close_port();
                    }
                })
                    .response
                    .on_disabled_hover_text("Port is not open");
            });

            ui.separator();

            ui.horizontal(|ui| {
                ui.label("Send Frequency is KHz:");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.number_text)
                        .hint_text("e.g. 24.5")
                        .desired_width(120.0),
                );
                if self.port_opened {
                    let enter_pressed =
                        resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

                    if ui.button("Send").clicked() || enter_pressed {
                        self.send_number();
                    }
                }
            });

            ui.separator();

            ui.label(format!("Status: {}", self.status));
            ui.label(format!("RX count: {}", self.rx_count));
            ui.separator();
            ui.label(format!("Period value: {}", self.latest_a));
            ui.label(format!("Value B: 0x{:X}", self.latest_b));
            /*
            ui.label(format!("Latest line: {}", self.latest_line));
            ui.label(format!(
                "Latest number: {}",
                self.latest_number
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "(none)".to_string())
            ));

             */
        });

        // Request repaint so UI updates even if user doesn't interact (good for live data)
        ctx.request_repaint_after(Duration::from_millis(16));
    }
}

const SYNC0: u8 = 0xA5;
const SYNC1: u8 = 0x5A;
const FRAME_LEN: usize = 2 + 8 + 2; // 12

#[derive(Debug, Clone, Copy)]
struct TwoU32 {
    a: u32,
    b: u32,
}

fn build_frame(payload: &[u8; 8]) -> [u8; FRAME_LEN] {
    let mut buf = [0u8; FRAME_LEN];
    buf[0] = SYNC0;
    buf[1] = SYNC1;
    buf[2..10].copy_from_slice(payload);

    let crc = crc16_ccitt_false(&buf[0..10]);
    buf[10..12].copy_from_slice(&crc.to_le_bytes());
    buf
}


fn crc16_ccitt_false(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            if (crc & 0x8000) != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

fn try_extract_frame(rx: &mut Vec<u8>) -> Option<TwoU32> {
    // Keep enough data for at least a frame
    loop {
        if rx.len() < FRAME_LEN {
            return None;
        }

        // Find sync
        let mut i = 0usize;
        while i + 1 < rx.len() {
            if rx[i] == SYNC0 && rx[i + 1] == SYNC1 {
                break;
            }
            i += 1;
        }

        // No sync found -> keep last byte (could be start of sync)
        if i + 1 >= rx.len() {
            let last = *rx.last().unwrap();
            rx.clear();
            rx.push(last);
            return None;
        }

        // Drop bytes before sync
        if i > 0 {
            rx.drain(0..i);
        }

        // Need full frame after sync
        if rx.len() < FRAME_LEN {
            return None;
        }

        // Validate CRC
        let crc_expected = u16::from_le_bytes([rx[10], rx[11]]);
        let crc_calc = crc16_ccitt_false(&rx[0..10]);
        if crc_calc != crc_expected {
            // False sync hit, drop first byte and keep searching
            rx.drain(0..1);
            continue;
        }

        // Parse payload
        let a = u32::from_le_bytes([rx[2], rx[3], rx[4], rx[5]]);
        let b = u32::from_le_bytes([rx[6], rx[7], rx[8], rx[9]]);

        // Consume this frame
        rx.drain(0..FRAME_LEN);

        return Some(TwoU32 { a, b });
    }
}

/// Background thread:
/// - keeps the port open
/// - reads bytes, splits into lines by '\n'
/// - also receives commands from UI to open/close/send
fn spawn_serial_thread(cmd_rx: Receiver<ToSerial>, out_tx: Sender<FromSerial>) {
    std::thread::spawn(move || {
        let mut port: Option<Box<dyn SerialPort>> = None;
        let mut read_buf = [0u8; 256];
        let mut line_buf: Vec<u8> = Vec::with_capacity(512);

        let set_status = |s: String| {
            let _ = out_tx.send(FromSerial::Status(s));
        };

        let mut rx_bytes: Vec<u8> = Vec::with_capacity(8192);

        loop {
            // 1) Handle pending commands (non-blocking)
            while let Ok(cmd) = cmd_rx.try_recv() {
                match cmd {
                    ToSerial::Open { port: name, baud } => {
                        match serialport::new(name.clone(), baud)
                            .timeout(Duration::from_millis(20))
                            .open()
                        {
                            Ok(p) => {
                                port = Some(p);
                                set_status(format!("Opened {name} @ {baud}"));
                                line_buf.clear();
                            }
                            Err(e) => set_status(format!("Open failed ({name}): {e}")),
                        }
                    }
                    ToSerial::Close => {
                        port.take();
                        set_status("Closed".to_string());
                        line_buf.clear();
                    }
                    ToSerial::RxPair {a,b} => {
                        if let Some(p) = port.as_mut() {
                            let msg = ToSerial::RxPair {a, b};
                            if let Some(payload) = msg.to_payload() {
                                let frame = build_frame(&payload);
                                if let Err(e) = p.write_all(&frame) {
                                    set_status(format!("Write failed: {e}"));
                                } else if let Err(e) = p.flush() {
                                    set_status(format!("Flush failed: {e}"));
                                }
                            }
                        } else {
                            set_status("Send failed: port not open".to_string());
                        }
                    }
                    /*
                    ToSerial::SendLine(s) => {
                        if let Some(p) = port.as_mut() {
                            if let Err(e) = p.write_all(s.as_bytes()) {
                                set_status(format!("Write failed: {e}"));
                            } else if let Err(e) = p.flush() {
                                set_status(format!("Flush failed: {e}"));
                            }
                        } else {
                            set_status("Send failed: port not open".to_string());
                        }
                    }
                     */
                }
            }

            // 2) Read from serial (if open)
            if let Some(p) = port.as_mut() {
                match p.read(&mut read_buf) {
                    Ok(n) if n > 0 => {
                        rx_bytes.extend_from_slice(&read_buf[..n]);

                        while let Some(pair) = try_extract_frame(&mut rx_bytes) {
                            let _ = out_tx.send(FromSerial::RxPair { a: pair.a, b: pair.b });
                        }

                        // Optional safety: prevent runaway memory if stream is garbage
                        if rx_bytes.len() > 65536 {
                            rx_bytes.clear();
                            set_status("RX buffer cleared (too large)".to_string());
                        }
                    }
                    Ok(_) => {}
                    Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(e) => {
                        set_status(format!("Read error: {e}"));
                        // optional: auto-close on error
                        // port.take();
                    }
                }
/*
                match p.read(&mut read_buf) {
                    Ok(n) if n > 0 => {
                        for &b in &read_buf[..n] {
                            if b == b'\n' {
                                // finalize a line (trim optional '\r')
                                if let Some(&b'\r') = line_buf.last() {
                                    line_buf.pop();
                                }
                                if let Ok(line) = String::from_utf8(line_buf.clone()) {
                                    let _ = out_tx.send(FromSerial::RxLine(line));
                                } else {
                                    set_status("RX utf8 decode error".to_string());
                                }
                                line_buf.clear();
                            } else {
                                // avoid unbounded growth if device spews without newlines
                                if line_buf.len() < 4096 {
                                    line_buf.push(b);
                                } else {
                                    line_buf.clear();
                                    set_status("RX line too long; buffer reset".to_string());
                                }
                            }
                        }
                    }
                    Ok(_) => {} // n == 0
                    Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(e) => {
                        set_status(format!("Read error: {e}"));
                        // optional: auto-close on error
                        // port.take();
                    }
                }
 */
            } else {
                // If not open, don't spin at 100% CPU
                std::thread::sleep(Duration::from_millis(20));
            }

            // Small sleep to avoid busy loop even when open (timeout already helps)
            std::thread::sleep(Duration::from_millis(1));
        }
    });
}
