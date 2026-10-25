mod app;
mod backend;
mod bridge;
mod buffer;
#[cfg(feature = "mock")]
mod mock;
mod perf;
mod ui;
mod voltage;

use app::LangmuirDesktopApp;
use backend::RealBackendBridge;
use bridge::BackendBridge;

const APP_NAME: &str = "Langmuier Probe Ground Control";

fn main() -> eframe::Result<()> {
    let arguments: Vec<String> = std::env::args().collect();
    if let Some(index) = arguments.iter().position(|arg| arg == "--audit-pcap") {
        let Some(path) = arguments.get(index + 1) else {
            eprintln!("--audit-pcap requires a PCAPNG path");
            std::process::exit(2);
        };
        match backend::audit_pcap(path) {
            Ok(report) => println!("{report}"),
            Err(error) => {
                eprintln!("PCAP audit failed: {error}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    let mock_requested = std::env::args().any(|argument| argument == "--mock");

    #[cfg(not(feature = "mock"))]
    if mock_requested {
        eprintln!(
            "--mock 需要以 `cargo run -p langmuir-desktop --features mock -- --mock` 构建启动"
        );
        return Ok(());
    }

    #[cfg(feature = "mock")]
    let bridge: Box<dyn BackendBridge> = if mock_requested {
        Box::new(mock::MockBridge::new())
    } else {
        match RealBackendBridge::spawn() {
            Ok(bridge) => Box::new(bridge),
            Err(error) => {
                eprintln!("{error}");
                return Ok(());
            }
        }
    };
    #[cfg(not(feature = "mock"))]
    let bridge: Box<dyn BackendBridge> = match RealBackendBridge::spawn() {
        Ok(bridge) => Box::new(bridge),
        Err(error) => {
            eprintln!("{error}");
            return Ok(());
        }
    };

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1_360.0, 860.0])
            .with_min_inner_size([800.0, 600.0])
            .with_resizable(true),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |context| {
            Ok(Box::new(LangmuirDesktopApp::new(
                context,
                bridge,
                #[cfg(feature = "mock")]
                mock_requested,
            )))
        }),
    )
}
