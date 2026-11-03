use std::{thread::sleep, time::Duration};
use crate::settings;
use tide_core::model::{Account, Card, LyricLine, Plan, Shelf, Track};
use tide_core::services::{BackendSide, Cmd, Evt, PlayerCmd, StreamSource, StreamSpec};

/// Blocking loop: handles one command at a time, sleeping to mimic latency.
pub fn run(be: BackendSide) {
    while let Ok(cmd) = be.rx.recv() {
        let send = |e: Evt| {
            let _ = be.tx.send(e);
        };
        match cmd {
            Cmd::Boot => {
                sleep(Duration::from_millis(150));
                send(Evt::LoginRequired);
            }
            Cmd::StartLogin => {
                sleep(Duration::from_millis(400));
                send(Evt::LoginCode {
                    user_code: "XK7M2".into(),
                    verify_url: "link.example.com".into(),
                    qr_url: "https://link.example.com/XK7M2".into(),
                    expires_in: 300,
                });
                sleep(Duration::from_millis(4500));
                send(Evt::LoggedIn);
            }
            Cmd::Logout => {}
            Cmd::Play { track_id, duration } => {
                let source = StreamSource::Simulated;
                let _ = be.player.send(PlayerCmd::Load(StreamSpec { track_id, mime: String::new(), duration, source, tier: None }));
            }
            Cmd::LoadHome => {
                sleep(Duration::from_millis(300));
                send(Evt::Home(home()));
            }
            Cmd::LoadLiked => {
                sleep(Duration::from_millis(200));
                send(Evt::Liked(catalog()));
            }
            Cmd::LoadAccount => {
                sleep(Duration::from_millis(200));
                send(Evt::Account(account()));
            }
            Cmd::SetLocale(code) => {
                // Same persistence as the real backend, so the simulator remembers the language too.
                settings::save_locale(&code);
            }
            Cmd::LoadLyrics(id) => {
                sleep(Duration::from_millis(250));
                let dur = catalog().into_iter().find(|t| t.id == id).map_or(180.0, |t| t.duration);
                send(Evt::Lyrics { track_id: id, lines: lyrics(dur), synced: true });
            }
        }
    }
}

/// Fictional account for the Settings page.
pub(crate) fn account() -> Account {
    Account {
        name: "Maria Silva".into(),
        email: "maria.silva@example.com".into(),
        plan: Some(Plan {
            name: "HiFi Plus".into(),
            status: "ACTIVE".into(),
            valid_until: Some("2027-01-15".into()),
            max_quality: Some("HI_RES_LOSSLESS".into()),
        }),
    }
}

pub(crate) fn catalog() -> Vec<Track> {
    // Fictional titles/artists.
    let rows: [(&str, &str, &str, f32); 14] = [
        ("Neon Harbor", "Violet Static", "Night Shift", 214.0),
        ("Slow Orbit", "The Paper Moons", "Low Gravity", 187.0),
        ("Glass Cathedral", "Mara Quinn", "Echo Rooms", 243.0),
        ("Midnight Freight", "Tall Pines", "Long Way Home", 198.0),
        ("Static Bloom", "Violet Static", "Night Shift", 176.0),
        ("A Very Long Track Title That Should Get Truncated Nicely", "Orchid & The Foxes", "Compilation Vol. 3", 305.0),
        ("Paper Planes at Dawn", "Mara Quinn", "Echo Rooms", 221.0),
        ("Tidewater", "Salt Lines", "Harbor Songs", 202.0),
        ("Copper Sky", "Tall Pines", "Long Way Home", 189.0),
        ("Lantern Parade", "The Paper Moons", "Low Gravity", 232.0),
        ("Afterglow Drive", "Violet Static", "Night Shift", 207.0),
        ("Quiet Machines", "Salt Lines", "Harbor Songs", 259.0),
        ("Northbound", "Orchid & The Foxes", "Compilation Vol. 3", 195.0),
        ("Velvet Hours", "Mara Quinn", "Echo Rooms", 228.0),
    ];
    rows.iter()
        .enumerate()
        .map(|(i, (t, a, al, d))| Track {
            id: 100 + i as u64,
            title: (*t).into(),
            artist: (*a).into(),
            album: (*al).into(),
            duration: *d,
            art: None,
        })
        .collect()
}

pub(crate) fn home() -> Vec<Shelf> {
    let cat = catalog();
    let titles = ["Made for you", "Recently played", "New releases", "Your mixes", "Popular playlists"];
    titles
        .iter()
        .enumerate()
        .map(|(si, title)| Shelf {
            title: (*title).into(),
            cards: (0..12)
                .map(|ci| {
                    let tr = cat[(si * 3 + ci) % cat.len()].clone();
                    Card {
                        title: tr.title.clone(),
                        subtitle: tr.artist.clone(),
                        art_seed: tr.id * 31 + si as u64,
                        art: None,
                        play: Some(tr),
                    }
                })
                .collect(),
        })
        .collect()
}

/// Placeholder lyrics (not real songs) spread evenly over the track.
pub(crate) fn lyrics(duration: f32) -> Vec<LyricLine> {
    let n = 28usize;
    (0..n)
        .map(|i| {
            let text = if i % 7 == 6 {
                format!("Line {:02}: a much longer placeholder line, used to check truncation in the lyrics view", i + 1)
            } else {
                format!("Placeholder lyric line {:02}", i + 1)
            };
            LyricLine { t: 6.0 + (duration - 12.0) * i as f32 / n as f32, text }
        })
        .collect()
}
