use pathfinder_geometry::rect::RectF;
use pathfinder_geometry::transform2d::Transform2F;
use pathfinder_geometry::vector::Vector2F;
use pdf::object::PageRc;
use std::collections::vec_deque::Iter;
use std::collections::VecDeque;

pub trait PageLoader {
    fn load_page(&self, page_nr: u32) -> Option<PageRc>;
    fn num_pages(&self) -> u32;
    fn get_page_bounds(&self, page: &PageRc) -> RectF;
    fn get_window_size(&self) -> Vector2F;
    fn set_transform(&mut self, transform: Transform2F);
}

const PAGE_GAP: f32 = 30.0;

pub struct ContinuousScroll<T> {
    sliding_window: VecDeque<(u32, PageRc, RectF)>,
    page_loader: T,
}

impl<T: PageLoader> ContinuousScroll<T> {
    pub fn new(page_loader: T) -> Self {
        let sliding_window = VecDeque::new();

        ContinuousScroll {
            sliding_window,
            page_loader,
        }
    }

    pub fn go_to_page(&mut self, page_nr: u32) ->Result<(), String> {
        if !(page_nr < self.page_loader.num_pages()) {
            return Err("Page number out of range".to_string());
        }

        self.sliding_window.clear();

        if let Some((page, view_box)) = self.load_page_info(page_nr) {
            self.sliding_window.push_back((page_nr, page, Transform2F::from_translation(Vector2F::new(0.0, PAGE_GAP)) * view_box));
        }

        self.scroll(ScrollDirection::Up, Transform2F::default());
        self.scroll(ScrollDirection::Down, Transform2F::default());

        dbg!(self.sliding_window.len());
        Ok(())
    }

    pub fn iter(&self) -> Iter<'_, (u32, PageRc, RectF)> {
        self.sliding_window.iter()
    }

    pub fn scroll(&mut self, direction: ScrollDirection, transform: Transform2F) -> CurrentPageReplacement {
        if self.sliding_window.is_empty() {
            return CurrentPageReplacement {
                max_pages: self.page_loader.num_pages(),
                current_page_nr: 0,
                top_y_offset: 0.0,
            };
        }

        for (_, _, translate) in  self.sliding_window.iter_mut() {
            *translate = transform * (*translate);
        }

        match direction {
            ScrollDirection::Up => {
                while let Some((tail_page_nr, _, translate)) = self.sliding_window.back() {
                    if *tail_page_nr == self.page_loader.num_pages() -1 {
                        break;
                    }

                    // Make sure last page is two window size away from the bottom of the window
                    if translate.max_y() < (2.0 * self.page_loader.get_window_size().y()) {
                        self.next_page();
                    } else {
                        break;
                    }
                }
            }
            ScrollDirection::Down => {
                while let Some((head_page_nr, _, translate)) = self.sliding_window.front() {
                    if *head_page_nr == 0 {
                        break;
                    }
                    // Make sure head page is two window sizes away from the top of the window
                    if translate.min_y().abs() <= (2.0 * self.page_loader.get_window_size().y()) {
                        self.prev_page();
                    } else {
                        break;
                    }
                }
            }
        }

        dbg!(self.sliding_window.len());
        self.get_current_page_replacement()
    }

    fn get_current_page_replacement(&self) -> CurrentPageReplacement {
        // The one who is closed to top 1/3 of the window height is current page
        // Although this way not perfect but it is good enough for now.
        let threshold =  self.page_loader.get_window_size().y()/3.0;

        let (current_page_nr, _ , position) = self.sliding_window.iter().min_by_key(|(_, _, position)| {
            (threshold - position.min_y()).abs() as i32
        }).unwrap();

        CurrentPageReplacement {
            max_pages: self.page_loader.num_pages(),
            current_page_nr: *current_page_nr,
            top_y_offset:position.min_y(),
        }
    }

    fn next_page(&mut self) {
        if let Some((tail_page_nr, _, translate)) = self.sliding_window.back() {
            let next_page_nr: u32 = (*tail_page_nr + 1).min(self.page_loader.num_pages());
            if next_page_nr > *tail_page_nr {
                debug!("loading new page {}", next_page_nr);
                if let Some((page, view_box)) = self.load_page_info(next_page_nr) {
                    let translate =
                            Transform2F::from_translation(Vector2F::new(translate.min_x(), translate.max_y() + PAGE_GAP)) * view_box;
    
                    self.sliding_window
                        .push_back((next_page_nr, page, translate));
                    if self.sliding_window.len() > 6 {
                        self.sliding_window.pop_front();
                    }
                }
            }
        }
    }

    fn prev_page(&mut self) {
        if let Some((head_page_nr, _, translate)) = self.sliding_window.front() {
            let prev_page_nr = (*head_page_nr).saturating_sub(1);
            if prev_page_nr < (*head_page_nr) {
                debug!("loading prev page {}", prev_page_nr);
                if let Some((page, view_box)) = self.load_page_info(prev_page_nr) {
                    let translate =
                    Transform2F::from_translation(Vector2F::new(view_box.min_x(), translate.min_y() - view_box.height() + PAGE_GAP)) * view_box;
                    self.sliding_window.push_front((prev_page_nr, page, translate));
                    if self.sliding_window.len() > 6 {
                        self.sliding_window.pop_back();
                    }
                }
            }
        }
    }

    fn load_page_info(&self, page_nr: u32) -> Option<(PageRc, RectF)> {
        if let Some(page) = self.page_loader.load_page(page_nr) {
            let view_box: RectF = self.page_loader.get_page_bounds(&page);

            return Some((page, view_box));
        }

        None
    }
}

#[derive(Debug)]
pub struct CurrentPageReplacement {
    max_pages: u32,
    current_page_nr: u32,
    top_y_offset: f32,
}

impl CurrentPageReplacement{
    pub fn is_last_page(&self) -> bool
    {
        self.current_page_nr == self.max_pages-1
    }

    pub fn is_first_page(&self) -> bool
    {
        self.current_page_nr == 0
    }

    pub fn get_current_page_nr(&self) -> u32
    {
        self.current_page_nr
    }

    pub fn get_top_y_offset(&self) -> f32
    {
        self.top_y_offset
    }
}

#[derive(Debug, Copy, Clone)]
pub enum ScrollDirection {
    Up,
    Down,
}