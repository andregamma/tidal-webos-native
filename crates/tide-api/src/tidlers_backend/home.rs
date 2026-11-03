//! The Home feed, fetched as raw JSON and read field by field.
//!
//! `tidlers`' typed `get_home_feed` parses the whole feed into one enum and fails as a whole when
//! the service adds or renames a single item type (it did: `SHORTCUT_LIST`). Here every feed item
//! and every card is isolated: whatever is not understood is skipped and logged, the rest is shown.
//! Only the few fields the UI needs are read, and only `id`/`title` are required for a card.

use super::{describe, map};
use crate::diag;
use http::{HeaderMap, HeaderValue, Method};
use serde_json::Value;
use std::collections::HashMap;
use tide_core::model::{Card, Shelf, Track};
use tidlers::{
    requests::{RequestClient, TidalRequest},
    urls::WEB_API_V2_LOCATION,
    TidalClient,
};

/// Same browser-ish identity `tidlers` uses for its own requests.
const USER_AGENT: &str = "Mozilla/5.0 (Linux; Android 12; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/91.0.4472.114 Safari/537.36";
const CLIENT_VERSION: &str = "2026.1.6";

pub struct Parsed {
    pub shelves: Vec<Shelf>,
    /// Items in the feed.
    pub seen: usize,
    /// One line per feed item that produced no shelf (type + reason), for the log.
    pub skipped: Vec<String>,
}

/// Same request as `TidalClient::get_home_feed`, minus the typed parse.
pub async fn fetch(client: &TidalClient, limit: u32) -> Result<Parsed, String> {
    let country = client.user_info.as_ref().map(|u| u.country_code.clone()).ok_or("not signed in")?;
    let token = client.session.auth.access_token.clone().ok_or("not signed in")?;

    let mut params = HashMap::new();
    params.insert("countryCode".to_string(), country);
    params.insert("locale".to_string(), client.session.locale.clone());
    params.insert("limit".to_string(), limit.to_string());
    params.insert("deviceType".to_string(), "BROWSER".to_string());
    params.insert("platform".to_string(), "WEB".to_string());
    params.insert("timeOffset".to_string(), client.session.time_offset.clone());

    let mut headers = HeaderMap::new();
    headers.insert("x-tidal-client-version", HeaderValue::from_static(CLIENT_VERSION));
    headers.insert("User-Agent", HeaderValue::from_static(USER_AGENT));

    let req = TidalRequest {
        method: Method::GET,
        path: "/home/feed/static".to_string(),
        form: None,
        params: Some(params),
        basic_auth: None,
        access_token: Some(token),
        data: None,
        headers: Some(headers),
        base_url: Some(WEB_API_V2_LOCATION.to_string()),
        send_params_as_form: false,
    };
    let res = RequestClient::default().request(req).await.map_err(|e| describe(&e))?;
    let body = res.text().await.map_err(|e| describe(&e))?;
    parse_feed(&body)
}

/// Pure: JSON text -> shelves. Fails only when the body is not a feed at all.
pub fn parse_feed(body: &str) -> Result<Parsed, String> {
    let root: Value = serde_json::from_str(body).map_err(|e| format!("invalid JSON: {e}"))?;
    let items = root.get("items").and_then(Value::as_array).ok_or("the feed has no `items` list")?;
    let mut out = Parsed { shelves: Vec::new(), seen: items.len(), skipped: Vec::new() };
    for item in items {
        let ty = item.get("type").and_then(Value::as_str).unwrap_or("?");
        // The service has used both `HOME_HORIZONTAL_LIST` and unprefixed names.
        let kind = ty.strip_prefix("HOME_").unwrap_or(ty);
        if !matches!(kind, "HORIZONTAL_LIST" | "HORIZONTAL_LIST_WITH_CONTEXT" | "TRACK_LIST" | "VERTICAL_LIST_CARD" | "SHORTCUT_LIST" | "COMPACT_GRID_CARD") {
            out.skipped.push(format!("item type {ty}"));
            continue;
        }
        let title = text(item, "title").unwrap_or_default();
        let cards: Vec<Card> = item
            .get("items")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(card).collect())
            .unwrap_or_default();
        if cards.is_empty() {
            out.skipped.push(format!("{ty} \"{title}\" (no usable cards)"));
            continue;
        }
        out.shelves.push(Shelf { title, cards });
    }
    Ok(out)
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

fn names(v: Option<&Value>) -> String {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.get("name").and_then(Value::as_str)).collect::<Vec<_>>().join(", "))
        .unwrap_or_default()
}

/// `{ "text": "..." }` blocks used by mixes.
fn info_text(data: &Value, key: &str) -> Option<String> {
    data.get(key).and_then(|i| i.get("text")).and_then(Value::as_str).map(str::to_string)
}

/// One card, or `None` when its type is unknown or it lacks the minimum (id/title).
fn card(item: &Value) -> Option<Card> {
    let data = item.get("data")?;
    match item.get("type")?.as_str()? {
        "TRACK" => {
            let id = data.get("id")?.as_u64()?;
            let title = text(data, "title")?;
            let artist = names(data.get("artists"));
            let album = data.get("album");
            let art = album.and_then(|a| a.get("cover")).and_then(Value::as_str).map(map::art_template);
            Some(Card {
                title: title.clone(),
                subtitle: artist.clone(),
                art_seed: id,
                art: art.clone(),
                play: Some(Track {
                    id,
                    title,
                    artist,
                    album: album.and_then(|a| text(a, "title")).unwrap_or_default(),
                    duration: data.get("duration").and_then(Value::as_f64).unwrap_or(0.0) as f32,
                    art,
                }),
            })
        }
        "ALBUM" => Some(Card {
            title: text(data, "title")?,
            subtitle: names(data.get("artists")),
            art_seed: data.get("id").and_then(Value::as_u64)?,
            art: data.get("cover").and_then(Value::as_str).map(map::art_template),
            play: None,
        }),
        "PLAYLIST" => {
            let n = data.get("numberOfTracks").and_then(Value::as_u64);
            let img = data.get("squareImage").or_else(|| data.get("image")).and_then(Value::as_str);
            Some(Card {
                title: text(data, "title")?,
                subtitle: n.map(|n| format!("{n} tracks")).unwrap_or_else(|| "Playlist".into()),
                art_seed: map::seed(data.get("uuid").and_then(Value::as_str).unwrap_or("playlist")),
                art: img.map(map::art_template),
                play: None,
            })
        }
        "MIX" => Some(Card {
            title: info_text(data, "titleTextInfo").unwrap_or_else(|| "Mix".into()),
            subtitle: info_text(data, "shortSubtitleTextInfo").unwrap_or_default(),
            art_seed: map::seed(data.get("id").and_then(Value::as_str)?),
            art: data
                .get("mixImages")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(|i| i.get("url"))
                .and_then(Value::as_str)
                .map(str::to_string),
            play: None,
        }),
        "ARTIST" => Some(Card {
            title: text(data, "name")?,
            subtitle: "Artist".into(),
            art_seed: data.get("id").and_then(Value::as_u64)?,
            art: data.get("picture").and_then(Value::as_str).map(map::art_template),
            play: None,
        }),
        // "DEEP_LINK" => Some(Card {
        //     title: text(data, "title")?,
        //     subtitle: text(data, "subtitle").unwrap_or_default(),
        //     art_seed: map::seed(data.get("id").and_then(Value::as_str).unwrap_or("deeplink")),
        //     art: None,
        //     play: None,
        // }),
        other => {
            diag::log(&format!("home: unknown card type {other}"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Deliberately small: only the fields the app reads, plus noise it must ignore.
    const FEED: &str = r#"{
      "uuid": "u", "page": {"cursor": null}, "header": {"vibes": {"items": []}},
      "items": [
        {"type": "SHORTCUT_LIST", "moduleId": "s", "title": "Shortcuts", "icons": [], "items": []},
        {"type": "BRAND_NEW_MODULE", "title": "Future thing", "whatever": [1, 2, 3]},
        {"type": "HORIZONTAL_LIST", "moduleId": "a", "title": "Made for you", "items": [
          {"type": "TRACK", "following": false, "data": {
             "id": 123, "title": "Song", "duration": 215.0, "someNewField": {"x": 1},
             "artists": [{"id": 1, "name": "Main"}, {"id": 2, "name": "Guest"}],
             "album": {"id": 9, "title": "The Album", "cover": "aaaa-bbbb-cccc"}}},
          {"type": "ALBUM", "data": {"id": 5, "title": "Alb", "cover": "1111-2222-3333", "artists": [{"name": "Band"}]}},
          {"type": "SOMETHING_NEW", "data": {"id": 1}},
          {"type": "TRACK", "data": {"title": "no id: dropped"}}
        ]},
        {"type": "HOME_HORIZONTAL_LIST_WITH_CONTEXT", "title": "Because you listened", "header": {"type": "ARTIST"}, "items": [
          {"type": "PLAYLIST", "data": {"uuid": "p-1", "title": "Chill", "numberOfTracks": 40, "squareImage": "4444-5555-6666"}},
          {"type": "MIX", "data": {"id": "mix1", "titleTextInfo": {"text": "Daily Mix"}, "shortSubtitleTextInfo": {"text": "Your picks"},
             "mixImages": [{"size": "SMALL", "url": "https://img/x.jpg"}]}},
          {"type": "ARTIST", "data": {"id": 7, "name": "Someone", "picture": "7777-8888-9999"}}
        ]},
        {"type": "HOME_TRACK_LIST", "title": "Empty after filtering", "items": [{"type": "NOPE", "data": {}}]}
      ]
    }"#;

    #[test]
    fn unknown_items_and_cards_are_skipped_not_fatal() {
        let p = parse_feed(FEED).expect("the feed with SHORTCUT_LIST must parse");
        assert_eq!(p.seen, 5);
        let titles: Vec<_> = p.shelves.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, vec!["Made for you", "Because you listened"]);
        assert!(p.skipped.iter().any(|s| s.contains("SHORTCUT_LIST")));
        assert!(p.skipped.iter().any(|s| s.contains("BRAND_NEW_MODULE")));
        assert!(p.skipped.iter().any(|s| s.contains("Empty after filtering")));
        // Unknown card types and cards without id/title are dropped; the others stay.
        assert_eq!(p.shelves[0].cards.len(), 2);
        assert_eq!(p.shelves[1].cards.len(), 3);
    }

    #[test]
    fn track_card_is_playable_with_joined_artists_and_cover_template() {
        let p = parse_feed(FEED).unwrap();
        let c = &p.shelves[0].cards[0];
        assert_eq!((c.title.as_str(), c.subtitle.as_str()), ("Song", "Main, Guest"));
        let t = c.play.as_ref().expect("tracks are playable");
        assert_eq!((t.id, t.duration, t.album.as_str()), (123, 215.0, "The Album"));
        assert_eq!(t.art.as_deref(), Some("https://resources.tidal.com/images/aaaa/bbbb/cccc/{px}x{px}.jpg"));
        assert!(p.shelves[0].cards[1].play.is_none(), "albums are not playable yet");
    }

    #[test]
    fn mix_playlist_and_artist_cards() {
        let p = parse_feed(FEED).unwrap();
        let s = &p.shelves[1].cards;
        assert_eq!((s[0].title.as_str(), s[0].subtitle.as_str()), ("Chill", "40 tracks"));
        assert!(s[0].art.as_deref().unwrap().contains("4444/5555/6666"));
        assert_eq!((s[1].title.as_str(), s[1].subtitle.as_str()), ("Daily Mix", "Your picks"));
        assert_eq!(s[1].art.as_deref(), Some("https://img/x.jpg"));
        assert_eq!((s[2].title.as_str(), s[2].subtitle.as_str()), ("Someone", "Artist"));
    }

    #[test]
    fn prefixed_and_unprefixed_type_names_both_work() {
        let a = parse_feed(r#"{"items":[{"type":"HOME_HORIZONTAL_LIST","title":"A","items":[{"type":"ALBUM","data":{"id":1,"title":"x"}}]}]}"#).unwrap();
        let b = parse_feed(r#"{"items":[{"type":"HORIZONTAL_LIST","title":"A","items":[{"type":"ALBUM","data":{"id":1,"title":"x"}}]}]}"#).unwrap();
        assert_eq!((a.shelves.len(), b.shelves.len()), (1, 1));
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(parse_feed("<html>").is_err());
        assert!(parse_feed(r#"{"nope": 1}"#).is_err());
        let empty = parse_feed(r#"{"items": []}"#).unwrap();
        assert!(empty.shelves.is_empty() && empty.seen == 0);
    }
}
