use pdf::file::{CachedFile, FileOptions};
use pdf::PdfError;

use iced::widget::{button, shader, column, container, row, text, tooltip};
use iced::Element;
use iced::{Center, Fill, Font, Subscription, Task};

use crate::pdf::PDF;
use std::io;
use std::path::{Path, PathBuf};

pub struct PdfView {
    pdf: Option<PDF>,
    is_loading: bool,
}

#[derive(Debug, Clone)]
pub enum PdfViewMessage {
    OpenPdfFile,
    PdfFileOpened(Result<PathBuf, Error>),
    TextSelect,
    Pen,
    Highlighter,
    Eraser,
    Lasso,
}

impl PdfView {
    pub fn title(&self) -> String {
        "Awesome PDF viewer".to_string()
    }

    pub fn new() ->  Self
    {
        PdfView {
            pdf: None,
            is_loading: false,
        }
    }

    pub fn update(&mut self, message: PdfViewMessage) -> Task<PdfViewMessage> {
        match message {
            PdfViewMessage::PdfFileOpened(result) => {
                if let Ok((path)) = result {
                    let file = FileOptions::cached().open(&path);

                    self.pdf = Some(PDF::new(file, 0));
                }

                Task::none()
            }
            PdfViewMessage::OpenPdfFile => {
                if self.is_loading {
                    Task::none()
                } else {
                    self.is_loading = true;

                    Task::perform(open_pdf_file(), PdfViewMessage::PdfFileOpened)
                }
            }
            _ => Task::none(),
        }
    }

    pub fn subscription(&self) -> Subscription<PdfViewMessage> {
        Subscription::none()
    }

    pub fn view(&self) -> Element<PdfViewMessage> {
        let controls = row![
            action(new_icon(), "Text select", Some(PdfViewMessage::TextSelect)),
            action(open_icon(), "Pen", Some(PdfViewMessage::Pen)),
            action(save_icon(), "Highlighter", Some(PdfViewMessage::Highlighter)),
            action(save_icon(), "Eraser", Some(PdfViewMessage::Eraser)),
            action(save_icon(), "Lasso", Some(PdfViewMessage::Lasso)),
        ]
        .spacing(10)
        .align_y(Center);

        let content: Element<PdfViewMessage>;
        if let Some(pdf)  = self.pdf.as_ref() {
            content = shader(pdf).width(Fill).height(Fill).into();
        }

        column![controls, content].spacing(10).padding(10) .into()
    }
}

#[derive(Debug, Clone)]
pub enum Error {
    DialogClosed,
    IoError(io::ErrorKind),
}

fn action<'a, PdfViewMessage: Clone + 'a>(
    content: impl Into<Element<'a, PdfViewMessage>>, label: &'a str, on_press: Option<PdfViewMessage>,
) -> Element<'a, PdfViewMessage> {
    let action = button(container(content).center_x(30));

    if let Some(on_press) = on_press {
        tooltip(action.on_press(on_press), label, tooltip::Position::FollowCursor)
            .style(container::rounded_box)
            .into()
    } else {
        action.style(button::secondary).into()
    }
}

fn new_icon<'a, PdfViewMessage>() -> Element<'a, PdfViewMessage> {
    icon('\u{0e800}')
}

fn save_icon<'a, PdfViewMessage>() -> Element<'a, PdfViewMessage> {
    icon('\u{0e801}')
}

fn open_icon<'a, PdfViewMessage>() -> Element<'a, PdfViewMessage> {
    icon('\u{0f115}')
}

fn icon<'a, PdfViewMessage>(codepoint: char) -> Element<'a, PdfViewMessage> {
    const ICON_FONT: Font = Font::with_name("editor-icons");

    text(codepoint).font(ICON_FONT).into()
}

async fn open_pdf_file() -> Result<PathBuf, Error> {
    let picked_file = rfd::AsyncFileDialog::new()
        .add_filter("text", &["pdf"])
        .pick_file()
        .await
        .ok_or(Error::DialogClosed)?;

    Ok(picked_file.path().to_owned())
}