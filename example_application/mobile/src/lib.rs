//! Mobile application library

#[cfg(any(target_os = "android", target_os = "ios"))]
use iced_mobile::MobileAppRunner;

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: android_activity::AndroidApp) {
    #[cfg(target_os = "android")]
    let _ = iced_mobile::ANDROID_APP.set(app);
    // Initialize tracing
    tracing_subscriber::fmt::init();

    let _ = iced::application(
        iced_application::Counter::default,
        iced_application::Counter::update,
        iced_application::Counter::view,
    )
    .title("Iced App")
    .theme(iced_application::Counter::theme)
    .subscription(iced_application::Counter::subscription)
    .mobile_run();
}

/// Entry point called from Objective-C main.m trampoline.
/// This starts the winit event loop, which on iOS internally handles
/// UIKit lifecycle (UIApplicationMain equivalent).
/// This function never returns on iOS.
#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn start_app() {
    tracing_subscriber::fmt::init();
    _ = iced::application(
        iced_application::Counter::default,
        iced_application::Counter::update,
        iced_application::Counter::view,
    )
    .title("Iced App")
    .theme(iced_application::Counter::theme)
    .subscription(iced_application::Counter::subscription)
    .mobile_run();
}
