//! Streaming quality: what the user asks for, and how what is playing is labelled.
//!
//! The preference is stored by its id (`low`, `high`, `lossless`, `max`) in `tide-settings.json`; the
//! backend maps it to the service's own levels and caps it by the account's ceiling. Ids, labels and
//! the badge colour live here so the UI and the backend cannot drift apart.
//!
//! Colours follow the official client: low is grey, high cyan, max amber. Lossless is white (the
//! official client folds Lossless and HiRes into MAX; here they stay separate for now).

use crate::geom::Color;
use crate::services::StreamInfo;
use crate::ui::fmt_khz;

/// What the user asks the service for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Quality {
    Low,
    High,
    Lossless,
    /// HiRes lossless: FLAC up to 24-bit/192 kHz. The official client calls this MAX.
    #[default]
    Max,
}

impl Quality {
    /// Settings order, lowest first.
    pub const ALL: [Quality; 4] = [Quality::Low, Quality::High, Quality::Lossless, Quality::Max];

    /// Stable id: the settings file and `Cmd::SetQuality` use it.
    pub fn id(self) -> &'static str {
        match self {
            Quality::Low => "low",
            Quality::High => "high",
            Quality::Lossless => "lossless",
            Quality::Max => "max",
        }
    }

    /// What the user reads in Settings.
    pub fn label(self) -> &'static str {
        match self {
            Quality::Low => "Low",
            Quality::High => "High",
            Quality::Lossless => "Lossless",
            Quality::Max => "Max",
        }
    }

    /// From an id or any reasonable spelling (`MAX`, `hires`, `best`). Unknown values are `None`.
    pub fn parse(s: &str) -> Option<Quality> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" => Some(Quality::Low),
            "high" => Some(Quality::High),
            "lossless" | "cd" => Some(Quality::Lossless),
            "max" | "hires" | "hi_res" | "hi-res" | "best" => Some(Quality::Max),
            _ => None,
        }
    }

    fn rank(self) -> u8 {
        match self {
            Quality::Low => 0,
            Quality::High => 1,
            Quality::Lossless => 2,
            Quality::Max => 3,
        }
    }

    /// `self` when the account can stream it, otherwise the ceiling (`None` = unknown, no cap).
    pub fn capped(self, ceiling: Option<Quality>) -> Quality {
        match ceiling {
            Some(c) if c.rank() < self.rank() => c,
            _ => self,
        }
    }

    /// The level of a stream, for its badge: the service's own label when it sent one, else what the
    /// decoder measured (a `--play-url` stream carries no label).
    pub fn of_stream(i: &StreamInfo) -> Option<Quality> {
        if let Some(tier) = &i.tier {
            let t = tier.to_ascii_uppercase();
            // Before `LOSSLESS`: `HI_RES_LOSSLESS` contains both.
            if t.contains("HI_RES") || t.contains("HIRES") {
                return Some(Quality::Max);
            }
            if t.contains("LOSSLESS") {
                return Some(Quality::Lossless);
            }
            if t.contains("HIGH") {
                return Some(Quality::High);
            }
            if t.contains("LOW") {
                return Some(Quality::Low);
            }
        }
        match i.src_bits {
            Some(b) if b >= 24 => Some(Quality::Max),
            Some(_) => Some(Quality::Lossless),
            None if i.codec.eq_ignore_ascii_case("FLAC") => Some(Quality::Lossless),
            None if !i.codec.is_empty() => Some(Quality::High),
            None => None,
        }
    }

    /// Colour of the tier's badge, as in the official client.
    pub fn color(self) -> Color {
        match self {
            Quality::Low => Color::rgba(0.545, 0.576, 0.655, 1.0), // #8b93a7
            Quality::High => Color::rgba(0.180, 0.902, 0.816, 1.0), // #2ee6d0
            Quality::Lossless => Color::rgba(0.949, 0.957, 0.973, 1.0), // #f2f4f8
            Quality::Max => Color::rgba(0.957, 0.698, 0.243, 1.0), // #f4b23e
        }
    }

    /// Background of a chip in this tier's colour.
    pub fn chip_bg(self) -> Color {
        self.color().with_alpha(0.18)
    }

    /// `FLAC · 24-bit / 96 kHz`, `AAC · 44.1 kHz`: the specs the official client shows instead of a
    /// tier name. `None` until the decoder knows the codec.
    pub fn specs(i: &StreamInfo) -> Option<String> {
        if i.codec.is_empty() {
            return None;
        }
        let rate = if i.src_rate > 0 { Some(fmt_khz(i.src_rate)) } else { None };
        Some(match (i.src_bits, rate) {
            (Some(b), Some(r)) => format!("{} · {}-bit / {}", i.codec, b, r),
            (Some(b), None) => format!("{} · {}-bit", i.codec, b),
            (None, Some(r)) => format!("{} · {}", i.codec, r),
            (None, None) => i.codec.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_labels_round_trip() {
        for q in Quality::ALL {
            assert_eq!(Quality::parse(q.id()), Some(q));
            assert_eq!(Quality::parse(q.label()), Some(q));
        }
        assert_eq!(Quality::parse(" HIRES "), Some(Quality::Max));
        assert_eq!(Quality::parse("ultra"), None);
    }

    #[test]
    fn the_default_is_the_best_available() {
        assert_eq!(Quality::default(), Quality::Max);
        // Capped by what the account can stream; unknown ceilings do not cap.
        assert_eq!(Quality::Max.capped(None), Quality::Max);
        assert_eq!(Quality::Max.capped(Some(Quality::Lossless)), Quality::Lossless);
        assert_eq!(Quality::High.capped(Some(Quality::Max)), Quality::High);
        assert_eq!(Quality::Low.capped(Some(Quality::Lossless)), Quality::Low);
    }

    #[test]
    fn stream_tiers_map_to_badge_levels() {
        let info = |tier: &str, bits: Option<u32>| StreamInfo {
            codec: "FLAC".into(),
            src_rate: 44_100,
            src_bits: bits,
            tier: Some(tier.into()),
            ..Default::default()
        };
        assert_eq!(Quality::of_stream(&info("HI_RES", Some(24))), Some(Quality::Max));
        assert_eq!(Quality::of_stream(&info("HI_RES_LOSSLESS", Some(24))), Some(Quality::Max));
        assert_eq!(Quality::of_stream(&info("LOSSLESS", Some(16))), Some(Quality::Lossless));
        assert_eq!(Quality::of_stream(&info("HIGH", None)), Some(Quality::High));
        assert_eq!(Quality::of_stream(&info("LOW", None)), Some(Quality::Low));
        // No label (`--play-url`): the decoder's numbers decide.
        let mut measured = StreamInfo { codec: "FLAC".into(), src_rate: 96_000, src_bits: Some(24), ..Default::default() };
        assert_eq!(Quality::of_stream(&measured), Some(Quality::Max));
        measured.codec = "AAC".into();
        measured.src_bits = None;
        assert_eq!(Quality::of_stream(&measured), Some(Quality::High));
    }

    #[test]
    fn specs_are_the_codec_bits_and_rate() {
        let mut i = StreamInfo { codec: "FLAC".into(), src_rate: 96_000, src_bits: Some(24), ..Default::default() };
        assert_eq!(Quality::specs(&i).as_deref(), Some("FLAC · 24-bit / 96 kHz"));
        i.src_bits = None;
        i.src_rate = 44_100;
        assert_eq!(Quality::specs(&i).as_deref(), Some("FLAC · 44.1 kHz"));
        i.codec.clear();
        assert_eq!(Quality::specs(&i), None);
    }
}
