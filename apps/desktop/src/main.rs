mod app;
mod backend;
mod bridge;
mod buffer;
#[cfg(feature = "mock")]
mod mock;
mod ui;

use app::LangmuirDesktopApp;
use backend::RealBackendBridge;
use bridge::BackendBridge;

fn main() -> eframe::Result<()> {
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
        "航天设备集成控制中心",
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
