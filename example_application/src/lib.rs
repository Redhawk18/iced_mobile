/// Basic widgets
pub mod widgets;

#[derive(Debug, Clone, PartialEq)]
pub struct Counter {
    value: i64,
    modal: ModalViewStatus,
    theme: Theme,
}
impl Default for Counter {
    fn default() -> Self {
        Self {
            value: Default::default(),
            modal: Default::default(),
            theme: Theme::Nord,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    Nothing,
    Increment,
    Decrement,
    ToggleModal,
    ToggleTheme,
    #[cfg(target_arch = "wasm32")]
    AnimationTick(wasmtimer::std::Instant),
    #[cfg(not(target_arch = "wasm32"))]
    AnimationTick(std::time::Instant),
}

use iced::{
    Element,
    Length::Fill,
    Subscription, Theme,
    alignment::Horizontal,
    widget::{button, column, container, stack, text},
};

use crate::widgets::modal::{ModalViewStatus, modal};

impl Counter {
    pub fn update(&mut self, message: Message) {
        match message {
            Message::Nothing => {}
            Message::Increment => {
                self.value += 1;
                tracing::info!("New value: {}", self.value);
            }
            Message::Decrement => {
                self.value -= 1;
                tracing::info!("New value: {}", self.value);
            }
            Message::ToggleModal => {
                self.modal.toggle();
            }
            Message::ToggleTheme => match self.theme {
                Theme::Light => self.theme = Theme::Dark,
                Theme::Dark => self.theme = Theme::GruvboxLight,
                Theme::GruvboxLight => self.theme = Theme::GruvboxDark,
                _ => self.theme = Theme::Light,
            },
            Message::AnimationTick(t) => {
                const DURATION: f32 = 0.5;
                let step = t.elapsed().as_secs_f32() / DURATION;
                self.modal.update(step);
            }
        }
    }
    pub fn view(&self) -> Element<'_, Message> {
        tracing::info!("Creating view {}", self.value);
        // The buttons
        let increment = button("+").on_press(Message::Increment);
        let decrement = button("-").on_press(Message::Decrement);

        // The number
        let counter = text(self.value);

        // The layout
        let interface = column![
            increment,
            counter,
            decrement,
            button("Toggle Modal").on_press(Message::ToggleModal),
            button("Toggle Theme").on_press(Message::ToggleTheme)
        ]
        .spacing(10)
        .padding(15)
        .align_x(Horizontal::Center);
        let content = container(interface)
            .width(Fill)
            .height(Fill)
            .center_x(Fill)
            .center_y(Fill);
        if let Some(modal) = modal(
            column![text("Modal Content"), button("test"), text("Modal text")],
            self.modal,
        ) {
            stack![content, modal].into()
        } else {
            content.into()
        }
    }

    pub fn needs_redraw(&self) -> bool {
        self.modal.is_in_progress()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        if self.modal.is_in_progress() {
            iced::time::every(std::time::Duration::from_millis(8)).map(Message::AnimationTick)
        } else {
            Subscription::none()
        }
    }

    pub fn theme(&self) -> Theme {
        self.theme.clone()
        // Define your base colors
        // let palette = Palette {
        //     background: Color::from_str("#0f1116").unwrap_or_default(), // Dark Grey/Blue
        //     text: Color::from_rgb8(200, 200, 200),                      // Off White
        //     primary: Color::from_str("#0868b0").unwrap_or_default(),    // Purple
        //     success: Color::from_rgb8(80, 250, 123),                    // Green
        //     danger: Color::from_rgb8(255, 85, 85),
        //     warning: Color::from_rgb8(255, 85, 85), // Red
        // };

        // Theme::custom_with_fn("theTheme", palette, |palette| {
        //     // 'palette' here is the base palette you defined
        //     // We return an 'Extended' palette struct
        //     let extended = iced::theme::palette::Extended::generate(palette);

        //     // Manually override specific shades
        //     // extended.primary.strong.color = Color::from_rgb(0.5, 0.0, 1.0);

        //     extended
        // })
    }
}
