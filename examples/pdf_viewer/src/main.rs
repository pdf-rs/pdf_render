use iced::window::settings::PlatformSpecific;
use iced::window::Settings;
use pdf_viewer::pdf_view::PdfView;

pub fn main() -> Result<(), Report>
{
    let mut window_settings = Settings::default();
    window_settings.transparent = true;
    window_settings.platform_specific = PlatformSpecific {
        title_hidden: false,
        titlebar_transparent: true,
        fullsize_content_view: true,
    };

    let app = iced::application(PdfView::title, PdfView::update, PdfView::view);
    app.window(window_settings)
        .subscription(PdfView::subscription)
        .run_with(PdfView::new)
        ?;

    Ok(())
}

