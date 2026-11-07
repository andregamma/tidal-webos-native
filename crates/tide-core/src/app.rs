use crate::anim::Smooth;
use crate::draw::DrawList;
use crate::geom::{Color, Rect};
use crate::i18n;
use crate::images::Images;
use crate::input::Key;
use crate::quality::Quality;
use crate::screens::{
    home::HomeScreen, liked::LikedScreen, login::LoginScreen, player::PlayerScreen,
    settings::SettingsScreen,
    sidebar::{Sidebar, NOW_PLAYING, SETTINGS}, Action,
};
use crate::services::{AppSide, Cmd, Evt};
use crate::text::{fit_text, TextMetrics};
use crate::theme::Theme;
use crate::widgets::minibar::{self, BarAction, MiniBar};
use crate::{ui, Cx, H, W};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Login,
    Home,
    Liked,
    Player,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Sidebar,
    Content,
    /// The mini bar (play/pause, open player).
    Bar,
}

/// Where the app begins (handy for the simulator / screenshots).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    Login,
    Home,
    Liked,
    Settings,
}

pub struct App {
    pub theme: Theme,
    route: Route,
    /// Where Back from the player returns to.
    return_to: Route,
    focus: Focus,
    sidebar: Sidebar,
    login: LoginScreen,
    home: HomeScreen,
    liked: LikedScreen,
    player: PlayerScreen,
    settings: SettingsScreen,
    minibar: MiniBar,
    /// Current language (`pt-BR`): the key for the app's own texts once i18n lands, and what the
    /// backend was told to ask the service for.
    locale: String,
    svc: AppSide,
    time: f32,
    fade: Smooth,
    toast: Option<(String, f32)>,
    quit: bool,
    /// Streaming quality the user asked for (Settings). The backend caps it by the account's
    /// ceiling; `Max` (the default) means "the best available".
    quality: Quality,
    /// Back presses before this time are ignored (key repeat / duplicate events).
    back_guard: f32,
}

impl App {
    pub fn new(svc: AppSide, theme: Theme, start: Start) -> Self {
        let mut app = App {
            theme,
            route: Route::Login,
            return_to: Route::Home,
            focus: Focus::Content,
            sidebar: Sidebar::new(),
            login: LoginScreen::default(),
            home: HomeScreen::default(),
            liked: LikedScreen::default(),
            player: PlayerScreen::new(),
            settings: SettingsScreen::default(),
            minibar: MiniBar::new(),
            locale: i18n::DEFAULT_LOCALE.to_string(),
            svc,
            time: 0.0,
            fade: Smooth::new(0.0).with_rate(10.0),
            toast: None,
            quit: false,
            quality: Quality::default(),
            back_guard: 0.0,
        };
        app.settings.set_quality(app.quality);
        match start {
            Start::Login => {
                app.fade.snap(1.0);
                app.send(Cmd::Boot);
            }
            Start::Home => app.enter_main(Route::Home),
            Start::Liked => app.enter_main(Route::Liked),
            Start::Settings => app.enter_main(Route::Settings),
        }
        app
    }

    /// Start with the saved language. Only sets what is shown: the backend reads the same saved
    /// value on its own, so no command is sent.
    pub fn with_locale(mut self, code: &str) -> Self {
        self.locale = i18n::resolve(code).to_string();
        self.settings.set_locale(&self.locale);
        self
    }

    /// Current language code (`pt-BR`).
    pub fn locale(&self) -> &str {
        &self.locale
    }

    /// Start with the saved audio quality. Only sets what is shown, like [`App::with_locale`]: the
    /// backend reads the same saved value on its own, so no command is sent.
    pub fn with_quality(mut self, quality: Quality) -> Self {
        self.quality = quality;
        self.settings.set_quality(quality);
        self
    }

    /// Streaming quality the user chose (`Max` = best the account allows).
    pub fn quality(&self) -> Quality {
        self.quality
    }

    /// The user picked a quality: tell the backend (applies + persists it).
    fn change_quality(&mut self, quality: Quality) {
        if quality == self.quality {
            return;
        }
        self.quality = quality;
        self.settings.set_quality(quality);
        self.send(Cmd::SetQuality(quality));
        self.toast = Some((format!("Qualidade do áudio: {}", quality.label()), 3.0));
    }

    /// The user picked a language: tell the backend (applies + persists it) and refetch what the
    /// service localises.
    fn change_locale(&mut self, code: String) {
        let code = i18n::resolve(&code).to_string();
        if code == self.locale {
            return;
        }
        self.locale = code.clone();
        self.settings.set_locale(&code);
        self.send(Cmd::SetLocale(code.clone()));
        self.send(Cmd::LoadHome);
        self.toast = Some((format!("Idioma: {}", i18n::language_name(&code)), 3.0));
    }

    pub fn route(&self) -> Route {
        self.route
    }

    /// Set when the user pressed Back on the root of the app.
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// True while the mini bar is (or is sliding) on screen.
    pub fn mini_bar_visible(&self) -> bool {
        self.minibar.is_shown()
    }

    fn has_track(&self) -> bool {
        self.player.track_id().is_some()
    }

    /// The bar shows on every page except full-screen ones (login, the player itself).
    fn bar_available(&self) -> bool {
        self.has_track() && !matches!(self.route, Route::Login | Route::Player)
    }

    /// Open the full player for what is already playing (from the sidebar or the mini bar).
    fn open_player(&mut self) {
        if matches!(self.route, Route::Login | Route::Player) {
            return;
        }
        if !self.has_track() {
            self.toast = Some(("Nenhuma música tocando".into(), 3.0));
            return;
        }
        self.return_to = self.route;
        self.goto(Route::Player);
        self.focus = Focus::Content;
    }

    /// Play/pause from any page. A finished or failed stream is loaded again instead.
    fn toggle_playback(&mut self) {
        if !self.has_track() {
            return;
        }
        if self.player.is_dead() {
            self.restart_playback();
        } else {
            let c = self.player.toggle();
            let _ = self.svc.player.send(c);
        }
    }

    fn restart_playback(&mut self) {
        if let Some((track_id, duration)) = self.player.restart() {
            self.send(Cmd::Play { track_id, duration });
        }
    }

    fn send(&self, c: Cmd) {
        let _ = self.svc.tx.send(c);
    }

    fn enter_main(&mut self, route: Route) {
        self.send(Cmd::LoadHome);
        self.send(Cmd::LoadLiked);
        self.send(Cmd::LoadAccount);
        self.goto(route);
        self.focus = Focus::Content;
    }

    fn goto(&mut self, route: Route) {
        self.route = route;
        self.fade.snap(0.0);
        self.fade.set(1.0);
        match route {
            Route::Home => self.sidebar.select(0),
            Route::Liked => self.sidebar.select(1),
            Route::Settings => self.sidebar.select(SETTINGS),
            _ => {}
        }
    }

    fn pump_events(&mut self) {
        while let Ok(evt) = self.svc.rx.try_recv() {
            match evt {
                Evt::LoginRequired => {
                    self.settings.clear_account();
                    self.login.set_checking();
                    self.send(Cmd::StartLogin);
                }
                Evt::PkceLogin { qr_url, paste_url, expires_in } => {
                    self.login.set_pkce(&qr_url, paste_url.as_deref(), expires_in, self.time)
                }
                Evt::FallbackOffered(reason) => self.login.set_offer(reason),
                Evt::LoginCancelled => self.login.set_idle(),
                Evt::LoginCode { user_code, verify_url, qr_url, expires_in } => {
                    self.login.set_code(user_code, verify_url, &qr_url, expires_in, self.time)
                }
                Evt::LoggedIn { max_quality } => match max_quality {
                    // The service says the account cannot stream HiRes (the fallback never does):
                    // say so before dropping the user into the library.
                    Some(q) if !is_hi_res(&q) => {
                        self.goto(Route::Login);
                        self.login.set_reduced_quality(q);
                    }
                    _ => self.enter_main(Route::Home),
                },
                Evt::LoginFailed(msg) => {
                    if self.route != Route::Login {
                        self.goto(Route::Login);
                    }
                    self.login.set_failed(msg);
                }
                Evt::Home(shelves) => self.home.set_shelves(shelves),
                Evt::Liked(tracks) => self.liked.set_tracks(tracks),
                Evt::Account(account) => self.settings.set_account(account),
                Evt::Lyrics { track_id, lines, synced } => self.player.set_lyrics(track_id, lines, synced),
                Evt::StreamInfo(i) => self.player.set_stream_info(i),
                Evt::Position(s) => self.player.set_position(s),
                Evt::Seeked(s) => self.player.on_seeked(s),
                Evt::TrackEnded => self.player.on_ended(),
                Evt::PlaybackError(m) => {
                    self.player.on_failed();
                    self.toast = Some((m, 5.0));
                }
                Evt::Error(e) => self.toast = Some((e, 4.0)),
            }
        }
    }

    pub fn on_key(&mut self, k: Key) {
        if k == Key::Back {
            if self.time < self.back_guard {
                return;
            }
            self.back_guard = self.time + 0.35;
        }
        // Play/pause works on every page that shows the bar (the player screen handles it itself).
        if k == Key::PlayPause && matches!(self.route, Route::Home | Route::Liked | Route::Settings) {
            self.toggle_playback();
            return;
        }
        match self.route {
            Route::Login => match self.login.on_key(k) {
                Action::Back => self.quit = true,
                Action::RetryLogin => self.send(Cmd::StartLogin),
                Action::UseFallback => self.send(Cmd::StartLoginFallback),
                Action::CancelLogin => self.send(Cmd::CancelLogin),
                Action::Continue => self.enter_main(Route::Home),
                _ => {}
            },
            Route::Player => {
                if matches!(k, Key::Ok | Key::PlayPause) && self.player.is_dead() {
                    self.restart_playback();
                    return;
                }
                match self.player.on_key(k) {
                    // Leaving the player never stops the music: the mini bar takes over.
                    Action::Back => {
                        let r = self.return_to;
                        self.goto(r);
                        self.focus = Focus::Content;
                    }
                    Action::Transport(c) => {
                        let _ = self.svc.player.send(c);
                    }
                    // Shortcut from the player: Settings, on the quality field.
                    Action::OpenQuality => {
                        self.goto(Route::Settings);
                        self.focus = Focus::Content;
                        self.settings.focus_quality();
                    }
                    _ => {}
                }
            }
            Route::Home | Route::Liked | Route::Settings => match self.focus {
                Focus::Sidebar => match k {
                    Key::Up => self.sidebar.move_sel(-1),
                    Key::Down => self.sidebar.move_sel(1),
                    Key::Ok | Key::Right if self.sidebar.sel == NOW_PLAYING => self.open_player(),
                    Key::Ok | Key::Right => {
                        let r = match self.sidebar.sel {
                            0 => Route::Home,
                            1 => Route::Liked,
                            _ => Route::Settings,
                        };
                        if r != self.route {
                            self.goto(r);
                        }
                        self.focus = Focus::Content;
                    }
                    Key::Back => self.quit = true,
                    _ => {}
                },
                Focus::Content => {
                    let action = match self.route {
                        Route::Home => self.home.on_key(k),
                        Route::Settings => self.settings.on_key(k),
                        _ => self.liked.on_key(k),
                    };
                    match action {
                        Action::None
                        | Action::Back
                        | Action::RetryLogin
                        | Action::UseFallback
                        | Action::CancelLogin
                        | Action::Continue
                        | Action::Transport(_) => {}
                        Action::Toast(m) => self.toast = Some((m, 3.0)),
                        Action::SetLocale(code) => self.change_locale(code),
                        Action::SetQuality(q) => self.change_quality(q),
                        Action::OpenQuality => {
                            self.goto(Route::Settings);
                            self.focus = Focus::Content;
                            self.settings.focus_quality();
                        }
                        Action::FocusSidebar => self.focus = Focus::Sidebar,
                        Action::FocusBar => {
                            if self.has_track() {
                                self.focus = Focus::Bar;
                                self.minibar.focus();
                            }
                        }
                        Action::Play(track) => {
                            self.return_to = self.route;
                            if self.player.track_id() == Some(track.id) {
                                // Same track: just show the player. Only a finished/failed one is reloaded.
                                if self.player.is_dead() {
                                    self.restart_playback();
                                }
                            } else {
                                self.send(Cmd::LoadLyrics(track.id));
                                self.send(Cmd::Play { track_id: track.id, duration: track.duration });
                                self.player.open(track);
                            }
                            self.goto(Route::Player);
                        }
                    }
                }
                Focus::Bar => match self.minibar.on_key(k) {
                    BarAction::None => {}
                    BarAction::Leave => self.focus = Focus::Content,
                    BarAction::TogglePlay => self.toggle_playback(),
                    BarAction::OpenPlayer => self.open_player(),
                },
            },
        }
    }

    pub fn update(&mut self, dt: f32, keys: &[Key]) {
        self.time += dt;
        self.pump_events();
        for &k in keys {
            self.on_key(k);
        }
        self.fade.update(dt);
        self.sidebar.update(dt, self.focus == Focus::Sidebar && self.route != Route::Player);
        // The player keeps its clock running on every page: the mini bar mirrors it.
        self.player.update(dt);
        match self.route {
            Route::Home => self.home.update(dt),
            Route::Liked => self.liked.update(dt),
            Route::Settings => self.settings.update(dt),
            Route::Player | Route::Login => {}
        }
        let inset = if self.has_track() { minibar::RESERVED } else { 0.0 };
        self.home.set_bottom_inset(inset);
        self.liked.set_bottom_inset(inset);
        self.settings.set_bottom_inset(inset);
        self.minibar.update(dt, self.bar_available());
        if self.focus == Focus::Bar && !self.bar_available() {
            self.focus = Focus::Content;
        }
        // Keep list screens animating even when hidden behind the player.
        if self.route == Route::Player {
            self.home.update(dt);
            self.liked.update(dt);
        }
        if let Some((_, t)) = &mut self.toast {
            *t -= dt;
            if *t <= 0.0 {
                self.toast = None;
            }
        }
    }

    pub fn view(&self, out: &mut DrawList, m: &dyn TextMetrics, images: &dyn Images) {
        let cx = Cx { theme: &self.theme, m, images, time: self.time };
        let content_focused = self.focus == Focus::Content;

        out.set_alpha(self.fade.value);
        match self.route {
            Route::Login => self.login.view(out, &cx),
            Route::Home => self.home.view(out, &cx, content_focused),
            Route::Liked => self.liked.view(out, &cx, content_focused),
            Route::Player => self.player.view(out, &cx),
            Route::Settings => self.settings.view(out, &cx, content_focused),
        }
        out.set_alpha(1.0);

        // Above the page, below the sidebar overlay and the toast.
        if let Some(np) = self.player.now_playing() {
            self.minibar.view(out, &cx, &np, self.focus == Focus::Bar);
        }

        if matches!(self.route, Route::Home | Route::Liked | Route::Settings) {
            let current = match self.route {
                Route::Home => 0,
                Route::Liked => 1,
                _ => SETTINGS,
            };
            self.sidebar.view(out, &cx, current);
        }

        if let Some((msg, _)) = &self.toast {
            let s = fit_text(m, msg, 28.0, 1200.0);
            let w = m.text_width(&s, 28.0) + 64.0;
            let lift = if self.bar_available() { minibar::RESERVED } else { 0.0 };
            let r = Rect::new((W - w) / 2.0, H - 140.0 - lift, w, 64.0);
            out.rect(r, Color::BLACK.with_alpha(0.85), 20.0);
            ui::text_centered(out, m, r, 28.0, self.theme.text, &s);
        }
    }
}

/// `HI_RES_LOSSLESS` is the tier this app exists for: anything else is "not HiRes".
fn is_hi_res(quality: &str) -> bool {
    let q = quality.to_ascii_uppercase();
    q.contains("HI_RES") || q.contains("HIRES")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Card, Shelf, Track};
    use crate::draw::Cmd as Draw;
    use crate::services::{channel_pair, channels, PlayerCmd, PlayerSide, BackendSide};
    use crate::text::MonoMetrics;

    #[test]
    fn headless_frame_and_flow() {
        let (app_side, backend) = channel_pair();
        let mut app = App::new(app_side, Theme::default(), Start::Home);
        let track = Track { id: 7, title: "T".into(), artist: "A".into(), album: "B".into(), duration: 60.0, art: None };
        let shelf = Shelf {
            title: "S".into(),
            cards: vec![Card { title: "c".into(), subtitle: "s".into(), art_seed: 1, art: None, play: Some(track) }],
        };
        backend.tx.send(Evt::Home(vec![shelf])).unwrap();

        let mut list = DrawList::new();
        app.update(1.0 / 60.0, &[]);
        list.clear();
        app.view(&mut list, &MonoMetrics, &crate::images::NoImages);
        assert!(!list.cmds.is_empty());

        // OK on the focused card opens the player and requests lyrics.
        app.update(1.0 / 60.0, &[Key::Ok]);
        assert_eq!(app.route(), Route::Player);
        let cmds: Vec<Cmd> = backend.rx.try_iter().collect();
        assert!(cmds.iter().any(|c| matches!(c, Cmd::LoadLyrics(7))));
        assert!(cmds.iter().any(|c| matches!(c, Cmd::Play { track_id: 7, .. })));

        // Back returns to Home.
        app.update(1.0 / 60.0, &[Key::Back]);
        assert_eq!(app.route(), Route::Home);
    }

    // ---- mini bar / persistent playback ------------------------------------------------

    fn track(id: u64) -> Track {
        Track { id, title: format!("Title{id}"), artist: "A".into(), album: "B".into(), duration: 60.0, art: None }
    }

    fn shelf_of(ids: &[u64]) -> Shelf {
        Shelf {
            title: "S".into(),
            cards: ids
                .iter()
                .map(|&i| Card { title: "c".into(), subtitle: "s".into(), art_seed: i, art: None, play: Some(track(i)) })
                .collect(),
        }
    }

    /// Home with one shelf of playable cards (ids 1, 2, ...), plus the channel ends a test inspects.
    fn home_app(ids: &[u64]) -> (App, BackendSide, PlayerSide) {
        let c = channels();
        let mut app = App::new(c.app, Theme::default(), Start::Home);
        c.backend.tx.send(Evt::Home(vec![shelf_of(ids)])).unwrap();
        app.update(1.0 / 60.0, &[]);
        (app, c.backend, c.player)
    }

    /// Advance past the 0.35 s Back debounce and the bar's slide animation.
    fn settle(app: &mut App) {
        for _ in 0..90 {
            app.update(1.0 / 60.0, &[]);
        }
    }

    fn press(app: &mut App, k: Key) {
        settle(app);
        app.update(1.0 / 60.0, &[k]);
    }

    fn player_cmds(p: &PlayerSide) -> Vec<PlayerCmd> {
        p.rx.try_iter().collect()
    }

    fn texts(app: &App) -> Vec<String> {
        let mut list = DrawList::new();
        app.view(&mut list, &MonoMetrics, &crate::images::NoImages);
        list.cmds.iter().filter_map(|c| if let Draw::Text { text, .. } = c { Some(text.clone()) } else { None }).collect()
    }

    #[test]
    fn back_from_player_keeps_the_music_playing() {
        let (mut app, _be, player) = home_app(&[1]);
        press(&mut app, Key::Ok);
        assert_eq!(app.route(), Route::Player);
        press(&mut app, Key::Back);
        assert_eq!(app.route(), Route::Home);
        assert!(!player_cmds(&player).iter().any(|c| matches!(c, PlayerCmd::Stop)), "Back must not stop playback");
        assert!(app.player.now_playing().unwrap().playing);
    }

    #[test]
    fn mini_bar_shows_after_leaving_the_player_and_not_before_playing() {
        let (mut app, _be, _p) = home_app(&[1]);
        settle(&mut app);
        assert!(!app.mini_bar_visible());
        assert!(!texts(&app).contains(&"Title1".to_string()));

        press(&mut app, Key::Ok);
        settle(&mut app);
        assert!(!app.mini_bar_visible(), "hidden behind the full player");
        press(&mut app, Key::Back);
        settle(&mut app);
        assert!(app.mini_bar_visible());
        assert!(texts(&app).contains(&"Title1".to_string()), "bar draws the playing title on Home");
    }

    #[test]
    fn reselecting_the_playing_track_does_not_restart_it() {
        let (mut app, be, _p) = home_app(&[1]);
        press(&mut app, Key::Ok);
        press(&mut app, Key::Back);
        let before: Vec<Cmd> = be.rx.try_iter().collect();
        assert_eq!(before.iter().filter(|c| matches!(c, Cmd::Play { .. })).count(), 1);

        // Let the position advance, then pick the same card again.
        for _ in 0..120 {
            app.update(1.0 / 60.0, &[]);
        }
        let pos = app.player.now_playing().unwrap().pos;
        assert!(pos > 1.0);
        press(&mut app, Key::Ok);
        assert_eq!(app.route(), Route::Player);
        assert!(!be.rx.try_iter().any(|c| matches!(c, Cmd::Play { .. } | Cmd::LoadLyrics(_))), "no reload");
        assert!(app.player.now_playing().unwrap().pos >= pos, "position kept");
    }

    #[test]
    fn selecting_another_track_replaces_the_current_one() {
        let (mut app, be, _p) = home_app(&[1, 2]);
        press(&mut app, Key::Ok);
        press(&mut app, Key::Back);
        let _ = be.rx.try_iter().count();
        press(&mut app, Key::Right);
        press(&mut app, Key::Ok);
        let cmds: Vec<Cmd> = be.rx.try_iter().collect();
        assert!(cmds.iter().any(|c| matches!(c, Cmd::Play { track_id: 2, .. })));
        assert_eq!(app.player.track_id(), Some(2));
    }

    #[test]
    fn play_pause_key_works_on_home_and_toggles() {
        let (mut app, _be, player) = home_app(&[1]);
        press(&mut app, Key::Ok);
        press(&mut app, Key::Back);
        let _ = player_cmds(&player);
        press(&mut app, Key::PlayPause);
        assert!(matches!(player_cmds(&player)[..], [PlayerCmd::Pause]));
        assert!(!app.player.now_playing().unwrap().playing);
        press(&mut app, Key::PlayPause);
        assert!(matches!(player_cmds(&player)[..], [PlayerCmd::Resume]));
    }

    #[test]
    fn play_pause_with_nothing_loaded_does_nothing() {
        let (mut app, be, player) = home_app(&[1]);
        press(&mut app, Key::PlayPause);
        assert!(player_cmds(&player).is_empty());
        assert!(be.rx.try_iter().all(|c| !matches!(c, Cmd::Play { .. })));
    }

    #[test]
    fn chevron_opens_the_full_player_and_back_returns_to_the_same_page() {
        let (mut app, _be, _p) = home_app(&[1]);
        press(&mut app, Key::Ok);
        press(&mut app, Key::Back);
        // Down off the last row focuses the bar (it does not open the player).
        press(&mut app, Key::Down);
        assert_eq!(app.route(), Route::Home);
        assert_eq!(app.focus, Focus::Bar);
        // Up goes back to the page; Down again, then Right to the chevron, OK opens the player.
        press(&mut app, Key::Up);
        assert_eq!(app.focus, Focus::Content);
        press(&mut app, Key::Down);
        press(&mut app, Key::Right);
        press(&mut app, Key::Ok);
        assert_eq!(app.route(), Route::Player);
        press(&mut app, Key::Back);
        assert_eq!(app.route(), Route::Home);
        assert_eq!(app.focus, Focus::Content);
    }

    #[test]
    fn bar_play_button_toggles_playback() {
        let (mut app, _be, player) = home_app(&[1]);
        press(&mut app, Key::Ok);
        press(&mut app, Key::Back);
        let _ = player_cmds(&player);
        press(&mut app, Key::Down);
        press(&mut app, Key::Ok);
        assert!(matches!(player_cmds(&player)[..], [PlayerCmd::Pause]));
    }

    #[test]
    fn down_does_not_focus_the_bar_when_nothing_is_playing() {
        let (mut app, _be, _p) = home_app(&[1]);
        press(&mut app, Key::Down);
        assert_eq!(app.focus, Focus::Content);
    }

    #[test]
    fn sidebar_now_playing_opens_the_player() {
        let (mut app, _be, _p) = home_app(&[1]);
        press(&mut app, Key::Ok);
        press(&mut app, Key::Back);
        press(&mut app, Key::Left); // first card: Left moves to the sidebar
        assert_eq!(app.focus, Focus::Sidebar);
        press(&mut app, Key::Down);
        press(&mut app, Key::Down);
        press(&mut app, Key::Ok);
        assert_eq!(app.route(), Route::Player);
        press(&mut app, Key::Back);
        assert_eq!(app.route(), Route::Home, "returns to the page it was opened from");
    }

    #[test]
    fn sidebar_now_playing_with_nothing_playing_only_toasts() {
        let (mut app, _be, _p) = home_app(&[1]);
        press(&mut app, Key::Left);
        press(&mut app, Key::Down);
        press(&mut app, Key::Down);
        press(&mut app, Key::Ok);
        assert_eq!(app.route(), Route::Home);
        assert_eq!(app.focus, Focus::Sidebar);
        assert!(app.toast.is_some());
    }

    #[test]
    fn a_finished_track_is_played_again_from_the_bar_key_and_the_player() {
        let (mut app, be, player) = home_app(&[1]);
        press(&mut app, Key::Ok);
        press(&mut app, Key::Back);
        let _ = be.rx.try_iter().count();
        be.tx.send(Evt::TrackEnded).unwrap();
        settle(&mut app);
        assert!(app.mini_bar_visible(), "bar stays after the track ends");
        let _ = player_cmds(&player);

        press(&mut app, Key::PlayPause);
        assert!(be.rx.try_iter().any(|c| matches!(c, Cmd::Play { track_id: 1, .. })), "reloads instead of a dead Resume");
        assert!(player_cmds(&player).is_empty());
        assert!(app.player.now_playing().unwrap().playing);
    }

    // ---- audio quality ----------------------------------------------------------------

    fn quality_app() -> (App, BackendSide) {
        let (app_side, backend) = channel_pair();
        let app = App::new(app_side, Theme::default(), Start::Settings);
        (app, backend)
    }

    #[test]
    fn the_settings_quality_select_sends_and_keeps_the_choice() {
        let (mut app, be) = quality_app();
        settle(&mut app);
        let _ = be.rx.try_iter().count();

        press(&mut app, Key::Ok); // open the list, on the current choice (Max)
        press(&mut app, Key::Up); // Lossless
        press(&mut app, Key::Ok);
        assert!(be.rx.try_iter().any(|c| matches!(c, Cmd::SetQuality(q) if q == Quality::Lossless)));
        assert_eq!(app.quality(), Quality::Lossless);

        // Re-picking the same value sends nothing.
        press(&mut app, Key::Ok);
        press(&mut app, Key::Ok);
        assert!(!be.rx.try_iter().any(|c| matches!(c, Cmd::SetQuality(_))));
    }

    #[test]
    fn the_player_quality_key_opens_settings_on_the_quality_field() {
        let (mut app, be, _p) = home_app(&[1]);
        press(&mut app, Key::Ok); // play, go to the player
        assert_eq!(app.route(), Route::Player);

        press(&mut app, Key::Quality);
        assert_eq!(app.route(), Route::Settings);

        // The focus is on the quality field: OK opens its list and Up + OK picks Lossless.
        let _ = be.rx.try_iter().count();
        press(&mut app, Key::Ok);
        press(&mut app, Key::Up);
        press(&mut app, Key::Ok);
        assert!(be.rx.try_iter().any(|c| matches!(c, Cmd::SetQuality(q) if q == Quality::Lossless)));
    }

    #[test]
    fn selecting_a_finished_track_again_reloads_it() {
        let (mut app, be, _p) = home_app(&[1]);
        press(&mut app, Key::Ok);
        press(&mut app, Key::Back);
        be.tx.send(Evt::TrackEnded).unwrap();
        settle(&mut app);
        let _ = be.rx.try_iter().count();
        press(&mut app, Key::Ok);
        assert!(be.rx.try_iter().any(|c| matches!(c, Cmd::Play { track_id: 1, .. })));
    }
    // ---- sign-in: PKCE first, device-code fallback ---------------------------------------

    /// A login screen that already asked the backend for a sign-in (`Start::Login` sends `Boot`).
    fn login_app() -> (App, BackendSide) {
        let (app_side, backend) = channel_pair();
        let app = App::new(app_side, Theme::default(), Start::Login);
        (app, backend)
    }

    fn pkce_event() -> Evt {
        Evt::PkceLogin {
            qr_url: "https://login.tidal.com/authorize?client_id=x&state=y".into(),
            paste_url: Some("https://tidal-relay.andregama.dev/paste/abc".into()),
            expires_in: 180,
        }
    }

    #[test]
    fn the_pkce_screen_walks_through_both_qr_codes_and_back_cancels_it() {
        let (mut app, be) = login_app();
        settle(&mut app);
        assert!(be.rx.try_iter().any(|c| matches!(c, Cmd::Boot)));

        be.tx.send(pkce_event()).unwrap();
        settle(&mut app);
        let shown = texts(&app);
        assert!(shown.iter().any(|s| s.contains("Scan the left code")));
        assert!(shown.iter().any(|s| s.contains("paste it there")));
        // No address on screen: a hand-off id is not something anyone types on a remote.
        assert!(!shown.iter().any(|s| s.contains("tidal-relay")), "{shown:?}");

        // Back on this screen hangs up the phone link instead of quitting the app.
        press(&mut app, Key::Back);
        assert!(!app.should_quit());
        assert!(be.rx.try_iter().any(|c| matches!(c, Cmd::CancelLogin)));

        // The backend confirms; OK starts a fresh attempt.
        be.tx.send(Evt::LoginCancelled).unwrap();
        press(&mut app, Key::Ok);
        assert!(be.rx.try_iter().any(|c| matches!(c, Cmd::StartLogin)));
    }

    #[test]
    fn a_failed_pkce_offers_the_device_code_fallback() {
        let (mut app, be) = login_app();
        settle(&mut app);
        let _ = be.rx.try_iter().count();

        be.tx.send(Evt::FallbackOffered("No phone opened the link in time.".into())).unwrap();
        settle(&mut app);
        assert!(texts(&app).iter().any(|s| s.contains("Could not finish signing in")));

        press(&mut app, Key::Right); // focus "Use the common QR code"
        press(&mut app, Key::Ok);
        assert!(be.rx.try_iter().any(|c| matches!(c, Cmd::StartLoginFallback)));
    }

    #[test]
    fn a_reduced_quality_sign_in_warns_before_the_library() {
        let (mut app, be) = login_app();
        settle(&mut app);

        be.tx.send(Evt::LoggedIn { max_quality: Some("LOSSLESS".into()) }).unwrap();
        settle(&mut app);
        assert_eq!(app.route(), Route::Login);
        assert!(texts(&app).iter().any(|s| s.contains("Lossless CD")));

        press(&mut app, Key::Ok); // "Continue anyway"
        assert_eq!(app.route(), Route::Home);
    }

    #[test]
    fn a_hires_sign_in_goes_straight_to_the_library() {
        let (mut app, be) = login_app();
        settle(&mut app);
        be.tx.send(Evt::LoggedIn { max_quality: Some("HI_RES_LOSSLESS".into()) }).unwrap();
        settle(&mut app);
        assert_eq!(app.route(), Route::Home);
    }

    #[test]
    fn an_unknown_quality_does_not_block_the_library() {
        let (mut app, be) = login_app();
        settle(&mut app);
        be.tx.send(Evt::LoggedIn { max_quality: None }).unwrap();
        settle(&mut app);
        assert_eq!(app.route(), Route::Home);
    }
}
