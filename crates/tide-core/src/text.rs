/// Text measurement, implemented by the renderer (it owns the font) so that
/// layout in the core uses exactly the advances the GPU path will draw.
pub trait TextMetrics {
    fn text_width(&self, text: &str, size: f32) -> f32;
}

/// Truncate with an ellipsis so the result fits in `max_w`.
///
/// PERF NOTE: allocates and measures prefixes; fine for a few dozen visible
/// strings. If it shows up in profiles, cache fitted strings when data loads.
pub fn fit_text(m: &dyn TextMetrics, text: &str, size: f32, max_w: f32) -> String {
    if m.text_width(text, size) <= max_w {
        return text.to_string();
    }
    let mut out = String::new();
    for ch in text.chars() {
        let mut probe = out.clone();
        probe.push(ch);
        probe.push('…');
        if m.text_width(&probe, size) > max_w {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

/// Greedy word wrap into at most `max_lines` lines; the last line is
/// ellipsized if the text does not fit.
pub fn wrap_text(m: &dyn TextMetrics, text: &str, size: f32, max_w: f32, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let probe = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if m.text_width(&probe, size) <= max_w || cur.is_empty() {
            cur = probe;
        } else {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        let last = lines.pop().unwrap_or_default();
        lines.push(fit_text(m, &format!("{last}…"), size, max_w));
    }
    lines.iter().map(|l| fit_text(m, l, size, max_w)).collect()
}

/// Fixed-advance metrics for tests (no font needed).
pub struct MonoMetrics;

impl TextMetrics for MonoMetrics {
    fn text_width(&self, text: &str, size: f32) -> f32 {
        text.chars().count() as f32 * size * 0.5
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_words_and_limits_lines() {
        let m = MonoMetrics; // 5px per char at size 10
        let lines = wrap_text(&m, "aaa bbb ccc ddd", 10.0, 20.0, 5); // 4 chars per line
        assert_eq!(lines, vec!["aaa", "bbb", "ccc", "ddd"]);
        let lines = wrap_text(&m, "aaa bbb ccc ddd", 10.0, 20.0, 2);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].ends_with('…'));
    }

    #[test]
    fn fits_or_ellipsizes() {
        let m = MonoMetrics;
        assert_eq!(fit_text(&m, "abc", 10.0, 100.0), "abc");
        let s = fit_text(&m, "abcdefghij", 10.0, 30.0); // 6 chars max
        assert!(s.ends_with('…'));
        assert!(m.text_width(&s, 10.0) <= 30.0);
    }
}
