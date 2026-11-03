//! Pure mappings from `tidlers` models to `tide-core` models (no I/O).

use tide_core::model::{Card, LyricLine, Shelf, Track};
use tidlers::client::models::home::{HomeFeedWeb, HomeItem, HomeListItem};
use tidlers::client::models::track::Track as TTrack;
use tidlers::resources::uuid_to_url_with_size;

/// Cover URL template: the UI fills `{px}` with the size it needs.
/// 160/320/640 are assumed to be sizes the image CDN serves: verify against the service.
fn art_template(uuid: &str) -> String {
    uuid_to_url_with_size(uuid, 320).replace("320x320", "{px}x{px}")
}

fn seed(s: &str) -> u64 {
    s.bytes().fold(14695981039346656037u64, |h, b| (h ^ b as u64).wrapping_mul(1099511628211))
}

fn join_names<'a>(names: impl Iterator<Item = &'a str>) -> String {
    names.collect::<Vec<_>>().join(", ")
}

pub fn map_track(t: &TTrack) -> Track {
    let mut artist = join_names(t.artists.iter().map(|a| a.name.as_str()));
    if artist.is_empty() {
        artist = t.artist.name.clone();
    }
    let title = match t.version.as_deref() {
        Some(v) if !v.is_empty() => format!("{} ({v})", t.title),
        _ => t.title.clone(),
    };
    Track {
        id: t.id,
        title,
        artist,
        album: t.album.as_ref().map(|a| a.title.clone()).unwrap_or_default(),
        duration: t.duration as f32,
        art: t.album.as_ref().and_then(|a| a.cover.as_deref()).map(art_template),
    }
}

/// Parse lyrics text. Returns `(lines, synced)`: `synced` is true when most
/// lines carry LRC timestamps like `[01:23.45]`.
pub fn parse_lyrics(text: &str) -> (Vec<LyricLine>, bool) {
    let mut timed: Vec<LyricLine> = Vec::new();
    let (mut with_ts, mut total) = (0usize, 0usize);

    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut stamps: Vec<f32> = Vec::new();
        while let Some(stripped) = rest.strip_prefix('[') {
            let Some(end) = stripped.find(']') else { break };
            match parse_stamp(&stripped[..end]) {
                Some(t) => stamps.push(t),
                None if stamps.is_empty() && stripped[..end].contains(':') => {
                    // Metadata tag such as [ar:Artist]: drop the whole line.
                    rest = "";
                    stamps.clear();
                    break;
                }
                None => break,
            }
            rest = stripped[end + 1..].trim_start();
        }
        if raw.trim().starts_with('[') && rest.is_empty() && stamps.is_empty() {
            continue; // metadata-only line
        }
        total += 1;
        if !stamps.is_empty() {
            with_ts += 1;
            for t in stamps {
                timed.push(LyricLine { t, text: rest.to_string() });
            }
        }
    }

    if total > 0 && with_ts * 2 >= total {
        timed.sort_by(|a, b| a.t.total_cmp(&b.t));
        return (timed, true);
    }
    // Unsynced: keep stanza breaks (single blank lines), trim the ends.
    let mut lines: Vec<LyricLine> = Vec::new();
    for l in text.lines().map(str::trim) {
        let l = l.to_string();
        if l.is_empty() && lines.last().is_none_or(|p| p.text.is_empty()) {
            continue;
        }
        lines.push(LyricLine { t: 0.0, text: l });
    }
    while lines.last().is_some_and(|l| l.text.is_empty()) {
        lines.pop();
    }
    (lines, false)
}

fn parse_stamp(s: &str) -> Option<f32> {
    let (m, rest) = s.split_once(':')?;
    let m: f32 = m.trim().parse().ok()?;
    let sec: f32 = rest.trim().replace(',', ".").parse().ok()?;
    Some(m * 60.0 + sec)
}

/// Build Home shelves from the feed. Track cards are playable; album,
/// playlist, mix and artist cards have `play: None` until M2 adds a
/// collection screen. NOT unit-tested (the feed models need very large
/// fixtures): verify with a real response.
pub fn map_home(feed: &HomeFeedWeb) -> Vec<Shelf> {
    let mut shelves = Vec::new();
    for item in &feed.items {
        let (title, items) = match item {
            HomeItem::HomeHorizontalList { inner } => (&inner.title, &inner.items),
            HomeItem::HomeHorizontalListWithContext { inner } => (&inner.title, &inner.items),
            _ => continue,
        };
        let cards: Vec<Card> = items.iter().filter_map(map_home_item).collect();
        if !cards.is_empty() {
            shelves.push(Shelf { title: title.clone(), cards });
        }
    }
    shelves
}

fn map_home_item(item: &HomeListItem) -> Option<Card> {
    Some(match item {
        HomeListItem::Track(t) => {
            let d = &t.data;
            let artist = join_names(d.artists.iter().map(|a| a.name.as_str()));
            Card {
                title: d.title.clone(),
                subtitle: artist.clone(),
                art_seed: d.id,
                art: Some(art_template(&d.album.cover)),
                play: Some(Track {
                    id: d.id,
                    title: d.title.clone(),
                    artist,
                    album: d.album.title.clone(),
                    duration: d.duration as f32,
                    art: Some(art_template(&d.album.cover)),
                }),
            }
        }
        HomeListItem::Album(a) => Card {
            title: a.data.title.clone(),
            subtitle: join_names(a.data.artists.iter().map(|x| x.name.as_str())),
            art_seed: a.data.id,
            art: Some(art_template(&a.data.cover)),
            play: None,
        },
        HomeListItem::Playlist(p) => Card {
            title: p.data.title.clone(),
            subtitle: format!("{} tracks", p.data.number_of_tracks),
            art_seed: seed(&p.data.uuid),
            art: p
                .data
                .square_image
                .as_deref()
                .or(p.data.image.as_deref())
                .map(|u| art_template(u)),
            play: None,
        },
        HomeListItem::Mix(m) => Card {
            title: m.data.title_text_info.text.clone().unwrap_or_else(|| "Mix".into()),
            subtitle: m.data.short_subtitle_text_info.text.clone().unwrap_or_default(),
            art_seed: seed(&m.data.id),
            art: m.data.mix_images.first().map(|i| i.url.clone()),
            play: None,
        },
        HomeListItem::Artist(a) => Card {
            title: a.data.name.clone(),
            subtitle: "Artist".into(),
            art_seed: a.data.id,
            art: a.data.picture.as_deref().map(|u| art_template(u)),
            play: None,
        },
        HomeListItem::DeepLink(_) | HomeListItem::Unknown(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lrc_is_parsed_sorted_and_flagged_synced() {
        let (l, synced) = parse_lyrics("[ar:Someone]\n[00:12.50] second\n[00:01.00]first\n[01:02.5]third");
        assert!(synced);
        let got: Vec<_> = l.iter().map(|x| (x.t, x.text.as_str())).collect();
        assert_eq!(got, vec![(1.0, "first"), (12.5, "second"), (62.5, "third")]);
    }

    #[test]
    fn multiple_timestamps_on_one_line_repeat_the_text() {
        let (l, synced) = parse_lyrics("[00:10.00][00:30.00]chorus\n[00:20.00]verse");
        assert!(synced);
        let got: Vec<_> = l.iter().map(|x| (x.t as u32, x.text.as_str())).collect();
        assert_eq!(got, vec![(10, "chorus"), (20, "verse"), (30, "chorus")]);
    }

    #[test]
    fn plain_text_is_unsynced_with_stanza_breaks_kept() {
        let (l, synced) = parse_lyrics("\n one\n two \n\n\n three\n\n");
        assert!(!synced);
        let got: Vec<_> = l.iter().map(|x| x.text.as_str()).collect();
        assert_eq!(got, vec!["one", "two", "", "three"]);
    }

    #[test]
    fn mostly_untimed_text_is_not_treated_as_synced() {
        let (_, synced) = parse_lyrics("[00:01.00]a\nb\nc\nd");
        assert!(!synced);
    }

    #[test]
    fn empty_lyrics() {
        let (l, _) = parse_lyrics("");
        assert!(l.is_empty());
    }

    const TRACK_JSON: &str = r#"{
      "id": 123, "title": "Song", "duration": 215, "replayGain": -7.5, "peak": 0.98,
      "allowStreaming": true, "streamReady": true, "payToStream": false,
      "adSupportedStreamReady": true, "djReady": true, "stemReady": false,
      "premiumStreamingOnly": false, "trackNumber": 1, "volumeNumber": 1,
      "version": "Live", "popularity": 50, "url": "http://x", "editable": false,
      "explicit": false, "audioQuality": "LOSSLESS", "audioModes": ["STEREO"], "upload": false,
      "artist": {"id": 1, "name": "Main"},
      "artists": [{"id": 1, "name": "Main"}, {"id": 2, "name": "Guest"}],
      "album": {"id": 9, "title": "The Album", "cover": "aaaa-bbbb-cccc"}
    }"#;

    #[test]
    fn track_mapping_joins_artists_and_adds_version() {
        let t: TTrack = serde_json::from_str(TRACK_JSON).expect("fixture matches the tidlers Track model");
        let m = map_track(&t);
        assert_eq!((m.id, m.duration), (123, 215.0));
        assert_eq!(m.title, "Song (Live)");
        assert_eq!(m.artist, "Main, Guest");
        assert_eq!(m.album, "The Album");
        assert_eq!(m.art.as_deref(), Some("https://resources.tidal.com/images/aaaa/bbbb/cccc/{px}x{px}.jpg"));
    }
}
