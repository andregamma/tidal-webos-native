use crate::anim::Smooth;
use crate::draw::DrawList;
use crate::geom::{Color, Rect};
use crate::images::Images;
use crate::input::Key;
use crate::screens::{
    home::HomeScreen, liked::LikedScreen, login::LoginScreen, player::PlayerScreen,
    sidebar::Sidebar, Action,
};
use crate::services::{AppSide, Cmd, Evt, PlayerCmd};
use crate::text::{fit_text, TextMetrics};
use crate::theme::Theme;
use crate::{ui, Cx, H, W};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Login,
    Home,
    Liked,
    Player,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Sidebar,
    Content,
}

/// Where the app begins (handy for the simulator / screenshots).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    Login,
    Home,
    Liked,
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
    svc: AppSide,
    time: f32,
    fade: Smooth,
    toast: Option<(String, f32)>,
    quit: bool,
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
            svc,
            time: 0.0,
            fade: Smooth::new(0.0).with_rate(10.0),
            toast: None,
            quit: false,
            back_guard: 0.0,
        };
        match start {
            Start::Login => {
                app.fade.snap(1.0);
                app.send(Cmd::Boot);
            }
            Start::Home => app.enter_main(Route::Home),
            Start::Liked => app.enter_main(Route::Liked),
        }
        app
    }

    pub fn route(&self) -> Route {
        self.route
    }

    /// Set when the user pressed Back on the root of the app.
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    fn send(&self, c: Cmd) {
        let _ = self.svc.tx.send(c);
    }

    fn enter_main(&mut self, route: Route) {
        self.send(Cmd::LoadHome);
        self.send(Cmd::LoadLiked);
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
            _ => {}
        }
    }

    fn pump_events(&mut self) {
        while let Ok(evt) = self.svc.rx.try_recv() {
            match evt {
                Evt::LoginRequired => {
                    self.login.set_checking();
                    self.send(Cmd::StartLogin);
                }
                Evt::LoginCode { user_code, verify_url, qr_url, expires_in } => {
                    self.login.set_code(user_code, verify_url, &qr_url, expires_in, self.time)
                }
                Evt::LoggedIn => self.enter_main(Route::Home),
                Evt::LoginFailed(msg) => {
                    if self.route != Route::Login {
                        self.goto(Route::Login);
                    }
                    self.login.set_failed(msg);
                }
                Evt::Home(shelves) => self.home.set_shelves(shelves),
                Evt::Liked(tracks) => self.liked.set_tracks(tracks),
                Evt::Lyrics { track_id, lines, synced } => self.player.set_lyrics(track_id, lines, synced),
                Evt::StreamInfo(i) => self.player.set_stream_info(i),
                Evt::Position(s) => self.player.set_position(s),
                Evt::TrackEnded => self.player.on_ended(),
                Evt::PlaybackError(m) => {
                    self.player.set_playing(false);
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
        match self.route {
            Route::Login => match self.login.on_key(k) {
                Action::Back => self.quit = true,
                Action::RetryLogin => self.send(Cmd::StartLogin),
                _ => {}
            },
            Route::Player => {
                match self.player.on_key(k) {
                    Action::Back => {
                        let _ = self.svc.player.send(PlayerCmd::Stop);
                        let r = self.return_to;
                        self.goto(r);
                        self.focus = Focus::Content;
                    }
                    Action::Transport(c) => {
                        let _ = self.svc.player.send(c);
                    }
                    _ => {}
                }
            }
            Route::Home | Route::Liked => match self.focus {
                Focus::Sidebar => match k {
                    Key::Up => self.sidebar.move_sel(-1),
                    Key::Down => self.sidebar.move_sel(1),
                    Key::Ok | Key::Right => {
                        let r = if self.sidebar.sel == 0 { Route::Home } else { Route::Liked };
                        if r != self.route {
                            self.goto(r);
                        }
                        self.focus = Focus::Content;
                    }
                    Key::Back => self.quit = true,
                    _ => {}
                },
                Focus::Content => {
                    let action = if self.route == Route::Home {
                        self.home.on_key(k)
                    } else {
                        self.liked.on_key(k)
                    };
                    match action {
                        Action::None | Action::Back | Action::RetryLogin | Action::Transport(_) => {}
                        Action::Toast(m) => self.toast = Some((m, 3.0)),
                        Action::FocusSidebar => self.focus = Focus::Sidebar,
                        Action::Play(track) => {
                            self.send(Cmd::LoadLyrics(track.id));
                            self.send(Cmd::Play { track_id: track.id, duration: track.duration });
                            self.player.open(track);
                            self.return_to = self.route;
                            self.goto(Route::Player);
                        }
                    }
                }
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
        match self.route {
            Route::Home => self.home.update(dt),
            Route::Liked => self.liked.update(dt),
            Route::Player => self.player.update(dt),
            Route::Login => {}
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
        }
        out.set_alpha(1.0);

        if matches!(self.route, Route::Home | Route::Liked) {
            let current = if self.route == Route::Home { 0 } else { 1 };
            self.sidebar.view(out, &cx, current);
        }

        if let Some((msg, _)) = &self.toast {
            let s = fit_text(m, msg, 28.0, 1200.0);
            let w = m.text_width(&s, 28.0) + 64.0;
            let r = Rect::new((W - w) / 2.0, H - 140.0, w, 64.0);
            out.rect(r, Color::BLACK.with_alpha(0.85), 20.0);
            ui::text_centered(out, m, r, 28.0, self.theme.text, &s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Card, Shelf, Track};
    use crate::services::channel_pair;
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
}
