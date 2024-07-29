use pathfinder_geometry::rect::RectF;
use pathfinder_geometry::transform2d::Transform2F;
use pathfinder_geometry::vector::Vector2F;
use pdf::object::PageRc;
use std::collections::vec_deque::{Iter, IterMut};
use std::collections::VecDeque;
use std::vec::IntoIter;

pub trait PageLoader {
    fn load_page(&self, page_nr: u32) -> Option<PageRc>;
    fn num_pages(&self) -> u32;
    fn get_page_bounds(&self, page: &PageRc) -> RectF;
}

pub struct ContinuousScroll<T> {
    sliding_window: SlidingWindow<(u32, PageRc, Option<Transform2F>)>,
    current_page_nr: u32,
    page_loader: T,
}

impl<T: PageLoader> ContinuousScroll<T> {
    pub fn new(sliding_window_size: u32, page_loader: T) -> Self {
        let sliding_window = SlidingWindow::new(sliding_window_size);

        ContinuousScroll {
            sliding_window,
            current_page_nr: 0,
            page_loader,
        }
    }

    pub fn go_to_page(&mut self, page_nr: Option<u32>) {
        let current_page_nr = page_nr.unwrap_or(self.current_page_nr);

        let start_page = current_page_nr.saturating_sub(self.sliding_window.get_size() / 2);
        let end_page = (current_page_nr + self.sliding_window.get_size() / 2)
            .min(self.page_loader.num_pages());

        for page_nr in start_page..=end_page {
            if let Some(page) = self.page_loader.load_page(page_nr) {
                self.sliding_window.push_back((page_nr, page, None));
            }
        }

        self.current_page_nr = current_page_nr;

        self.calculate_positions();
    }

    pub fn get_current_page_nr(&self) -> u32 {
        self.current_page_nr
    }

    pub fn find_page(&self, page_nr: u32) -> Option<&(u32, PageRc, Option<Transform2F>)> {
        self.sliding_window.iter().find(|(nr, _, _)| *nr == page_nr)
    }

    pub fn scroll(&mut self, transform: Transform2F, window_br: Vector2F, scroll_direction: &ScrollDirection) -> Scroll 
    {
        let threshold = (window_br.y() * 2.0/3.0 , window_br.y() / 3.0);
        let current_page_nr = self.get_current_page_nr();

        let view_box = self.get_current_page_view_box(current_page_nr, transform).unwrap();
        let bottom_y = view_box.max_y();
        dbg!(current_page_nr, &scroll_direction, 0.0 <= bottom_y && bottom_y <= threshold.1);
        match scroll_direction {
            ScrollDirection::Up => {
                if current_page_nr == self.page_loader.num_pages() - 1 {
                    return Scroll::Last(bottom_y);
                }
                // Advance current page number when the bottom y of current page
                // enters the top 1/3 of the window height
                if 0.0 <= bottom_y && bottom_y <= threshold.1 {
                    self.current_page_nr = (current_page_nr + 1).min(self.page_loader.num_pages());
                    let (tail_page_nr, _, translate) = self.sliding_window.back().unwrap();

                    let next_page_nr = (*tail_page_nr + 1).min(self.page_loader.num_pages());
                        if next_page_nr > *tail_page_nr {
                            debug!("loading new page {}", next_page_nr);
                            if let Some(page) = self.page_loader.load_page(next_page_nr) {
                                let view_box = self.page_loader.get_page_bounds(&page);
                                let translate =
                                        translate.unwrap().translate(Vector2F::new(0.0, view_box.height()));

                                self.sliding_window
                                    .push_back((next_page_nr, page, Some(translate)));
                            }   
                        }
                }
            }
            ScrollDirection::Down => {
                // first page
                if current_page_nr == 0 {
                    return Scroll::First(bottom_y);
                }
                // Subtract the current page number when the top y of current page enters
                // the bottom 1/3 of the window height
                let top_y = view_box.min_y();
                if threshold.0 <= top_y && top_y <= window_br.y() {
                    self.current_page_nr = current_page_nr.saturating_sub(1);

                    let(head_page_nr, _, translate) = self.sliding_window.front().unwrap();
                    let prev_page_nr = (*head_page_nr).saturating_sub(1);
                    if prev_page_nr < (*head_page_nr) {
                        debug!("loading prev page {}", prev_page_nr);
                        if let Some(page) = self.page_loader.load_page(prev_page_nr) {
                            let view_box = self.page_loader.get_page_bounds(&page);
                            let translate =
                                    translate.unwrap().translate(Vector2F::new(0.0, -view_box.height()));

                            self.sliding_window.push_front((prev_page_nr, page, Some(translate)));
                        }
                    }
                }
            }
        }

        return Scroll::CurrentPage((self.current_page_nr, bottom_y));
    }

    //// I treat the view box of a page is the page bounds and page position in the display view port
    //// that is why name  it as view_box
    fn get_current_page_view_box(
        &self,
        current_page_nr: u32,
        transform: Transform2F,
    ) -> Option<RectF> {
        if let Some((_, page, current_position)) = self.find_page(current_page_nr) {
            if let Some(current_position) = current_position {
                // Set new current page number if the position of current page cross a threshold.
                let bounds = self.page_loader.get_page_bounds(page);
                let absolute_page_position = (*current_position) * transform;

                return Some(absolute_page_position * bounds);
            }
        }

        None
    }

    pub fn iter(&self) -> Iter<'_, (u32, PageRc, Option<Transform2F>)> {
        self.sliding_window.iter()
    }

    fn calculate_positions(&mut self) {
        // Position each page in the sliding window
        let mut vertical_offset = 0.0;
        for (_, page, translate) in self.sliding_window.iter_mut() {
            let page_bounds = self.page_loader.get_page_bounds(page);

            *translate = Some(Transform2F::from_translation(Vector2F::new(
                0.0,
                vertical_offset,
            )));
            vertical_offset += page_bounds.height();
        }

        // Put current page in the display view port
        if let Some((_, _, position)) = self.find_page(self.get_current_page_nr()) {
            if let Some(current_position) = position {
                let (_, _, first) = self.iter().nth(0).unwrap();
                // Calculate the offset of the first page to current page
                let offset =
                    current_position.translation().y() - ((*first).unwrap()).translation().y();

                for (_, _, old_translate) in self.sliding_window.iter_mut() {
                    if let Some(translate) = old_translate {
                        //Adjust position for all pages by the offset

                        *old_translate = Some(translate.translate(Vector2F::new(0.0, -offset)));
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
pub enum Scroll {
    Last(f32),
    First(f32),
    CurrentPage((u32, f32))
}

#[derive(Debug)]
pub enum ScrollDirection {
    Up,
    Down,
}
pub struct SlidingWindow<T> {
    queue: VecDeque<T>,
    size: u32,
}

impl<T> SlidingWindow<T> {
    fn new(size: u32) -> Self {
        SlidingWindow {
            queue: VecDeque::with_capacity(size as usize),
            size,
        }
    }

    fn get_size(&self) -> u32 {
        self.size
    }

    fn push_front(&mut self, item: T) {
        if self.queue.len() as u32 == self.size {
            self.queue.pop_back();
        }
        self.queue.push_front(item);
    }

    fn front(&self) -> Option<&T> {
        self.queue.front()
    }

    fn back(&self) -> Option<&T> {
        self.queue.back()
    }

    fn push_back(&mut self, item: T) {
        if self.queue.len() as u32 == self.size {
            self.queue.pop_front();
        }
        self.queue.push_back(item);
    }

    fn len(&self) -> usize {
        self.queue.len()
    }

    fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        self.queue.extend(iter);
    }

    pub fn iter(&self) -> Iter<'_, T> {
        self.queue.iter()
    }
    pub fn iter_mut(&mut self) -> IterMut<'_, T> {
        self.queue.iter_mut()
    }
}

impl<T> IntoIterator for SlidingWindow<T> {
    type Item = T;
    type IntoIter = IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.queue.into_iter().collect::<Vec<T>>().into_iter()
    }
}
