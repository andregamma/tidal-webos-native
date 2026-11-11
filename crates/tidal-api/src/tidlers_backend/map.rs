//! Pure mappings from `tidlers` models to `tidal-core` models (no I/O).

use tidal_core::i18n;
use tidal_core::model::{LyricLine, Plan, Track};
use tidlers::client::models::subscription::UserSubscriptionResponse;
use tidlers::client::models::track::Track as TTrack;
use tidlers::client::models::user::User;
use tidlers::resources::uuid_to_url_with_size;

/// Cover URL template: the UI fills `{px}` with the size it needs.
/// 160/320/640 are assumed to be sizes the image CDN serves: verify against the service.
pub(super) fn art_template(uuid: &str) -> String {
    uuid_to_url_with_size(uuid, 320).replace("320x320", "{px}x{px}")
}

/// App language (`pt-BR`) -> the spelling the Tidal client sends as `locale` (`pt_BR`; the library's
/// own default is `en_US`).
pub(super) fn tidal_locale(code: &str) -> String {
    i18n::resolve(code).replace('-', "_")
}

/// Best name the account has: full name, first + last, nickname, finally the username.
pub(super) fn display_name(u: &User) -> String {
    let clean = |s: &Option<String>| s.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    if let Some(n) = clean(&u.full_name) {
        return n;
    }
    let both = [clean(&u.first_name), clean(&u.last_name)].into_iter().flatten().collect::<Vec<_>>().join(" ");
    if !both.is_empty() {
        return both;
    }
    clean(&u.nickname).unwrap_or_else(|| u.username.clone())
}

/// `HIFI_PLUS` -> `HiFi Plus`. Known plan types get their marketing name, anything else is
/// title-cased so a new type still reads sensibly.
pub(super) fn plan_label(raw: &str) -> String {
    match raw.trim().to_ascii_uppercase().as_str() {
        "FREE" => "Free".into(),
        "PREMIUM" => "Premium".into(),
        "PREMIUM_PLUS" => "Premium Plus".into(),
        "HIFI" => "HiFi".into(),
        "HIFIPLUS" | "HIFI_PLUS" => "HiFi Plus".into(),
        "" => "—".into(),
        other => other
            .split('_')
            .filter(|w| !w.is_empty())
            .map(|w| {
                let mut c = w.chars();
                c.next().map_or_else(String::new, |f| f.to_string() + &c.as_str().to_ascii_lowercase())
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// `2026-12-01T00:00:00.000+0000` -> `2026-12-01`.
fn date_only(s: &str) -> Option<String> {
    let d: String = s.chars().take(10).collect();
    let b = d.as_bytes();
    (b.len() == 10 && b[4] == b'-' && b[7] == b'-').then_some(d)
}

pub(super) fn map_plan(s: &UserSubscriptionResponse) -> Plan {
    Plan {
        name: plan_label(&s.subscription.subscription_type),
        status: s.status.clone(),
        valid_until: date_only(&s.valid_until),
        max_quality: Some(s.highest_sound_quality.clone()).filter(|q| !q.is_empty()),
    }
}

pub(super) fn seed(s: &str) -> u64 {
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
