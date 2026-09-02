use super::*;

use std::collections::HashMap;

use gpui::ScrollHandle;
use page::PageEvent;

/// How long a loaded feed counts as current. Spotify rebuilds the page by
/// the hour; window switches inside this window cost no request.
const HOME_STALE_TIME: Duration = Duration::from_secs(30);
const CARD_WIDTH: f32 = 168.;
const CARD_PAD: f32 = 8.;
const CARD_GAP: f32 = 12.;
const ARTWORK_SIZE: f32 = CARD_WIDTH - 2. * CARD_PAD;
/// Every shelf is one row of this height, which is what lets the page
/// virtualize them: only the shelves on screen lay out and load artwork.
const SHELF_HEIGHT: f32 = 288.;

/// Spotify's Home page: the curated shelves the account is offered, each a
/// row of cards that opens the playlist, album or artist it names.
///
/// The feed is fetched when the listener arrives and refreshed on return
/// behind a short debounce. A shelf with more cards than its first page
/// ends in a "Show more" card that appends the next page in place.
pub(super) struct HomePage {
    backend: BackendHandle,
    image_cache: Entity<image_cache::BoundedImageCache>,
    feed: Option<model::HomeFeed>,
    loaded_at: Option<SystemTime>,
    /// Why there is no feed to show. Cleared by the next attempt.
    error: Option<String>,
    request: Option<gpui::Task<()>>,
    /// One per shelf, in feed order: the row's scroll position, which the
    /// shelf's arrows page through.
    shelf_scrolls: Vec<ScrollHandle>,
    /// Shelves whose next page is on its way, by index in the feed.
    shelf_requests: HashMap<usize, gpui::Task<()>>,
}

impl EventEmitter<PageEvent> for HomePage {}

impl HomePage {
    pub(super) fn new(backend: BackendHandle, cx: &mut Context<Self>) -> Self {
        Self {
            backend,
            image_cache: services::AppServices::image_cache(cx),
            feed: None,
            loaded_at: None,
            error: None,
            request: None,
            shelf_scrolls: Vec::new(),
            shelf_requests: HashMap::new(),
        }
    }

    /// Fetches the feed unless one is on its way or still current. The
    /// shelves already on screen stay up until the answer replaces them.
    pub(super) fn revalidate(&mut self, cx: &mut Context<Self>) {
        if self.request.is_some() || is_fresh(self.loaded_at, HOME_STALE_TIME) {
            return;
        }
        self.load(cx);
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        let reply = catalog::request(&self.backend, |respond| BackendCommand::LoadHomeFeed {
            respond,
        });
        self.request = Some(cx.spawn(async move |this, cx| {
            let result = reply.await;
            let _ = this.update(cx, |page, cx| {
                page.request = None;
                match result {
                    Ok(feed) => {
                        // A refresh may reorder the shelves, so nothing tied
                        // to a shelf's position survives it: rows start at
                        // their left edge, and a page still loading for the
                        // old shelves is dropped rather than appended to the
                        // wrong one.
                        page.shelf_scrolls =
                            feed.shelves.iter().map(|_| ScrollHandle::new()).collect();
                        page.shelf_requests.clear();
                        page.feed = Some(feed);
                        page.loaded_at = Some(SystemTime::now());
                        cx.emit(PageEvent::Loaded);
                    }
                    Err(error) => {
                        page.error = Some(error.clone());
                        cx.emit(PageEvent::Failed(error));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Appends the next page of cards to the shelf at `index`.
    fn load_more(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.shelf_requests.contains_key(&index) {
            return;
        }
        let Some(shelf) = self.feed.as_ref().and_then(|feed| feed.shelves.get(index)) else {
            return;
        };
        let (Some(shelf_uri), Some(offset)) = (shelf.uri.clone(), shelf.next_offset) else {
            return;
        };
        let reply = catalog::request(&self.backend, move |respond| {
            BackendCommand::LoadHomeShelf {
                shelf_uri,
                offset,
                respond,
            }
        });
        let task = cx.spawn(async move |this, cx| {
            let result = reply.await;
            let _ = this.update(cx, |page, cx| {
                page.shelf_requests.remove(&index);
                match result {
                    Ok(more) => {
                        if let Some(shelf) = page
                            .feed
                            .as_mut()
                            .and_then(|feed| feed.shelves.get_mut(index))
                        {
                            shelf.cards.extend(more.cards);
                            shelf.next_offset = more.next_offset;
                        }
                    }
                    Err(error) => cx.emit(PageEvent::Failed(error)),
                }
                cx.notify();
            });
        });
        self.shelf_requests.insert(index, task);
        cx.notify();
    }

    pub(super) fn clear(&mut self, cx: &mut Context<Self>) {
        self.request = None;
        self.shelf_requests.clear();
        self.shelf_scrolls.clear();
        self.feed = None;
        self.loaded_at = None;
        self.error = None;
        cx.notify();
    }

    fn shelves(&self, count: usize, cx: &mut Context<Self>) -> AnyElement {
        uniform_list(
            "home-shelves",
            count,
            cx.processor(move |this, range: Range<usize>, _, cx| {
                range.map(|index| this.shelf(index, cx)).collect()
            }),
        )
        .flex_1()
        .min_h_0()
        .into_any_element()
    }

    /// One shelf: its title with paging arrows, then its cards in a row that
    /// scrolls sideways. The wheel keeps scrolling the page; the arrows and
    /// a sideways gesture move the row.
    fn shelf(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let palette = appearance::Appearance::palette(cx);
        let Some(shelf) = self.feed.as_ref().and_then(|feed| feed.shelves.get(index)) else {
            return div().into_any_element();
        };
        let scroll = self
            .shelf_scrolls
            .get(index)
            .cloned()
            .unwrap_or_else(ScrollHandle::new);
        let has_more = shelf.uri.is_some() && shelf.next_offset.is_some();
        let loading_more = self.shelf_requests.contains_key(&index);
        let arrow = |id: &'static str, icon: &'static str, direction: f32| {
            let scroll = scroll.clone();
            components::icon_button(palette, (id, index), icon).on_click(cx.listener(
                move |_, _, _, cx| {
                    page_shelf(&scroll, direction);
                    cx.notify();
                },
            ))
        };
        let header = div()
            .h(px(32.))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(20.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text_primary))
                    // A shelf Spotify draws without a heading keeps the
                    // arrows and an empty title.
                    .child(shelf.title.clone().unwrap_or_default()),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .child(arrow("home-shelf-back", "chevron-left", -1.))
                    .child(arrow("home-shelf-forward", "chevron-right", 1.)),
            );
        let row = div()
            .id(("home-shelf-row", index))
            .overflow_x_scroll()
            .restrict_scroll_to_axis()
            .track_scroll(&scroll)
            .flex()
            .items_start()
            .gap(px(CARD_GAP))
            .children(
                shelf
                    .cards
                    .iter()
                    .filter_map(|card| self.card(index, card, palette, cx)),
            )
            .when(has_more, |row| {
                row.child(self.more_card(index, loading_more, palette, cx))
            });

        div()
            .h(px(SHELF_HEIGHT))
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(header)
            .child(row)
            .into_any_element()
    }

    /// A card, or nothing for the kinds Cadence has no page for.
    fn card(
        &self,
        shelf: usize,
        card: &model::HomeCard,
        palette: CadencePalette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (open, subtitle, radius, fallback) = match card.kind {
            model::HomeCardKind::Playlist => (
                PageEvent::OpenPlaylist(card.playlist()?),
                match (&card.made_for, &card.owner) {
                    (Some(_), _) => "Made for you".to_owned(),
                    (None, Some(owner)) => format!("By {owner}"),
                    (None, None) => "Playlist".to_owned(),
                },
                8.,
                "list-music",
            ),
            model::HomeCardKind::Album => (
                PageEvent::OpenAlbum(card.album()?),
                card.owner.clone().unwrap_or_else(|| "Album".to_owned()),
                8.,
                "music",
            ),
            model::HomeCardKind::Artist => (
                PageEvent::OpenArtist(card.artist()?),
                "Artist".to_owned(),
                ARTWORK_SIZE / 2.,
                "user",
            ),
            model::HomeCardKind::Other => return None,
        };
        let id = (
            ElementId::from(("home-card", shelf)),
            SharedString::from(card.uri.clone()),
        );
        Some(
            components::button(palette, id)
                .flex_none()
                .flex_col()
                .items_start()
                .justify_start()
                .w(px(CARD_WIDTH))
                .p(px(CARD_PAD))
                .gap(px(8.))
                .rounded(px(12.))
                .hover(|style| style.bg(rgb(palette.surface_raised)))
                .child(components::artwork(
                    palette,
                    &self.image_cache,
                    card.artwork_url.as_deref(),
                    ARTWORK_SIZE,
                    radius,
                    fallback,
                ))
                .child(
                    div()
                        .w_full()
                        .truncate()
                        .text_size(px(14.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(rgb(palette.text_primary))
                        .child(card.name.clone()),
                )
                .child(
                    div()
                        .w_full()
                        .truncate()
                        .text_size(px(12.))
                        .text_color(rgb(palette.text_muted))
                        .child(subtitle),
                )
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(open.clone())))
                .into_any_element(),
        )
    }

    /// The card at the end of a shelf with pages left, which fetches the
    /// next one.
    fn more_card(
        &self,
        shelf: usize,
        loading: bool,
        palette: CadencePalette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        components::button(palette, ("home-shelf-more", shelf))
            .flex_none()
            .flex_col()
            .gap(px(8.))
            .w(px(CARD_WIDTH))
            .h(px(ARTWORK_SIZE + 2. * CARD_PAD))
            .rounded(px(12.))
            .bg(rgb(palette.control))
            .hover(|style| style.bg(rgb(palette.control_hover)))
            .text_size(px(13.))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(rgb(palette.text_primary))
            .child(if loading {
                Spinner::new().into_any_element()
            } else {
                components::icon("chevron-right", 20., palette.text_primary).into_any_element()
            })
            .child(if loading { "Loading…" } else { "Show more" })
            .when(!loading, |card| {
                card.on_click(cx.listener(move |this, _, _, cx| this.load_more(shelf, cx)))
            })
            .into_any_element()
    }

    fn error_state(
        &self,
        error: String,
        palette: CadencePalette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .items_start()
            .gap(px(12.))
            .child(components::empty_state(
                palette,
                format!("Home is unavailable: {error}"),
            ))
            .child(
                components::pill(palette, "home-retry", "Try again", false)
                    .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
            )
            .into_any_element()
    }
}

/// Moves a shelf's row one viewport along `direction` (-1 back, 1 forward),
/// stopping at either end.
fn page_shelf(scroll: &ScrollHandle, direction: f32) {
    let width = scroll.bounds().size.width;
    let furthest = -scroll.max_offset().x;
    let target = (scroll.offset().x - width * direction).clamp(furthest, px(0.));
    scroll.set_offset(point(target, px(0.)));
}

impl Render for HomePage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = appearance::Appearance::palette(cx);
        let title = self
            .feed
            .as_ref()
            .and_then(|feed| feed.greeting.clone())
            .unwrap_or_else(|| "Home".to_owned());
        let refreshing = self.request.is_some() && self.feed.is_some();
        let detail = components::revalidating_detail("Picked for you by Spotify", refreshing);
        let shelf_count = self.feed.as_ref().map(|feed| feed.shelves.len());
        let content = match (shelf_count, self.error.clone()) {
            (Some(0), _) => {
                components::empty_state(palette, "Nothing on your Home page yet").into_any_element()
            }
            (Some(count), _) => self.shelves(count, cx),
            (None, Some(error)) => self.error_state(error, palette, cx),
            (None, None) => {
                components::empty_state(palette, "Loading your Home page…").into_any_element()
            }
        };

        components::page("home-page")
            .pt(px(12.))
            .child(components::page_heading(palette, title, detail))
            .child(content)
    }
}
