use iced::widget::mouse_area;
use iced::{Element, Length::Fill, widget::container};

use crate::Message;

#[derive(Default, Clone, Copy, PartialEq, PartialOrd, Debug)]
pub enum ModalViewStatus {
    #[default]
    Hidden,
    ShowAnimation(f32),
    Showing,
    HideAnimation(f32),
}

impl ModalViewStatus {
    /// Returns true if the modal is currently showing.
    pub const fn is_showing(&self) -> bool {
        !matches!(self, ModalViewStatus::Hidden)
    }
    pub fn update(&mut self, step: f32) {
        match self {
            ModalViewStatus::ShowAnimation(value) => {
                if *value + step >= 1.0 {
                    *self = ModalViewStatus::Showing;
                } else {
                    *self = ModalViewStatus::ShowAnimation(*value + step);
                }
            }
            ModalViewStatus::Showing | ModalViewStatus::Hidden => {}
            ModalViewStatus::HideAnimation(value) => {
                if *value + step >= 1.0 {
                    *self = ModalViewStatus::Hidden;
                } else {
                    *self = ModalViewStatus::HideAnimation(*value + step);
                }
            }
        }
    }
    pub const fn progress(&self) -> Option<f32> {
        match self {
            ModalViewStatus::ShowAnimation(progress) => Some(*progress),
            ModalViewStatus::Showing => Some(1.0),
            ModalViewStatus::HideAnimation(progress) => Some(1.0 - *progress),
            ModalViewStatus::Hidden => None,
        }
    }
    pub const fn is_in_progress(&self) -> bool {
        matches!(
            self,
            ModalViewStatus::ShowAnimation(_) | ModalViewStatus::HideAnimation(_)
        )
    }
    pub fn toggle(&mut self) {
        match self {
            ModalViewStatus::ShowAnimation(progress) => {
                *self = ModalViewStatus::HideAnimation(1.0 - *progress);
            }
            ModalViewStatus::Showing => *self = ModalViewStatus::HideAnimation(0.0),
            ModalViewStatus::HideAnimation(_) => *self = ModalViewStatus::Hidden,
            ModalViewStatus::Hidden => *self = ModalViewStatus::ShowAnimation(0.0),
        }
    }
}

pub fn modal<'a>(
    content: impl Into<Element<'a, Message>>,
    modal_view_status: ModalViewStatus,
) -> Option<Element<'a, Message>> {
    let progress = modal_view_status.progress()?;
    let offset = (1.0 - progress) * 800.0;

    Some(
        mouse_area(
            container(
                container(
                    mouse_area(
                        container(content)
                            .padding(20)
                            .style(container::bordered_box),
                    )
                    .on_press(Message::Nothing),
                )
                .padding(iced::Padding {
                    top: offset,
                    ..Default::default()
                }),
            )
            .width(Fill)
            .height(Fill)
            .center_x(Fill)
            .center_y(Fill)
            .style(move |theme: &iced::Theme| {
                let palette = theme.extended_palette();
                let mut bg = palette.background.base.color;
                bg.a = 0.8 * progress * progress;
                container::Style {
                    background: Some(iced::Background::Color(bg)),
                    ..Default::default()
                }
            }),
        )
        .on_press(Message::ToggleModal)
        .into(),
    )
}
