pub fn main() -> iced::Result {
    tracing_subscriber::fmt::init();
    iced::application(
        iced_application::Counter::default,
        iced_application::Counter::update,
        iced_application::Counter::view,
    )
    .title("Iced App")
    .theme(iced_application::Counter::theme)
    .subscription(iced_application::Counter::subscription)
    .run()
}
