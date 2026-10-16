//! Lyrics from files: synced formats keep their timing, plain ones are
//! cleaned up (chords, section labels, directives removed) and then timed
//! like pasted lyrics.
//!
//! * Synced: LRC (incl. enhanced word tags), SubRip `.srt`, WebVTT `.vtt`
//! * Plain:  text, ChordPro (`.cho`, `.chopro`, `.chordpro`, `.crd`, `.pro`),
//!   OpenLyrics XML (OpenLP, Worship Extreme…), OpenSong

use crate::model::LyricLine;

/// File extensions offered in the open dialog.
pub const EXTENSIONS: &[&str] = &["lrc", "srt", "vtt", "txt", "cho", "chopro", "chordpro", "crd", "pro", "xml", "song", "text"];

#[derive(Clone, Debug, Default)]
pub struct Imported {
    /// One lyric line per row (what goes in the Lyrics box).
    pub text: String,
    /// Timed lines when the file had timing.
    pub lines: Option<Vec<LyricLine>>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub format: &'static str,
}

/// Text from raw file bytes: UTF-8 (with or without BOM), UTF-16 with a BOM,
/// else Latin-1 (old Windows lyric files).
pub fn decode(bytes: &[u8]) -> String {
    let utf16 = |be: bool| {
        let units: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| if be { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) }).collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        [0xFF, 0xFE, ..] => utf16(false),
        [0xFE, 0xFF, ..] => utf16(true),
        _ => match std::str::from_utf8(bytes) {
            Ok(s) => s.to_owned(),
            Err(_) => bytes.iter().map(|&b| b as char).collect(),
        },
    }
}

/// Parse a lyrics file. `duration` (seconds) bounds the last synced line.
pub fn parse(file_name: &str, content: &str, duration: f64) -> Result<Imported, String> {
    let ext = file_name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    let head = content.trim_start();
    let lower = head.chars().take(4000).collect::<String>().to_lowercase();
    let mut out = if head.starts_with("WEBVTT") || ext == "vtt" {
        subtitles(content, "WebVTT")
    } else if ext == "srt" || content.lines().any(|l| l.contains("-->") && parse_clock(l.split("-->").next().unwrap_or("")).is_some()) {
        subtitles(content, "SubRip")
    } else if lower.contains("<song") && lower.contains("openlyrics") {
        openlyrics(content)
    } else if lower.contains("<song") && lower.contains("<lyrics") {
        opensong(content)
    } else if ext == "lrc" || content.lines().filter(|l| !lrc_times(l).0.is_empty()).count() >= 2 {
        lrc(content)
    } else if matches!(ext.as_str(), "cho" | "chopro" | "chordpro" | "crd" | "pro") || lower.contains("{title") || lower.contains("{start_of_chorus") || lower.contains("{soc}") {
        chordpro(content)
    } else {
        plain(content)
    };
    if let Some(lines) = &mut out.lines {
        finish_timing(lines, duration);
        if lines.is_empty() {
            out.lines = None;
        }
    }
    if out.text.trim().is_empty() {
        return Err("No lyrics found in that file".into());
    }
    Ok(out)
}

// --------------------------------------------------------------------------- synced

/// `[mm:ss.xx]` stamps at the start of an LRC line, and the text after them.
fn lrc_times(line: &str) -> (Vec<f64>, &str) {
    let mut rest = line.trim();
    let mut times = Vec::new();
    while let Some(r) = rest.strip_prefix('[') {
        let Some((tag, after)) = r.split_once(']') else { break };
        match parse_clock(tag) {
            Some(t) => times.push(t),
            None => break,
        }
        rest = after;
    }
    (times, rest)
}

fn lrc(content: &str) -> Imported {
    let mut out = Imported { format: "LRC", ..Default::default() };
    let mut offset = 0.0;
    let mut timed: Vec<LyricLine> = Vec::new();
    for raw in content.lines() {
        let (times, text) = lrc_times(raw);
        if times.is_empty() {
            // Metadata: [ti:Title] [ar:Artist] [offset:+250]
            if let Some((key, value)) = raw.trim().strip_prefix('[').and_then(|r| r.strip_suffix(']')).and_then(|r| r.split_once(':')) {
                let value = value.trim().to_owned();
                match key.trim().to_lowercase().as_str() {
                    "ti" if !value.is_empty() => out.title = Some(value),
                    "ar" if !value.is_empty() => out.artist = Some(value),
                    "offset" => offset = value.parse::<f64>().unwrap_or(0.0) / 1000.0,
                    _ => {}
                }
            }
            continue;
        }
        let text = clean_inline(text);
        for t in times {
            // An empty stamped line marks where the previous line stops.
            timed.push(LyricLine::new(text.clone(), (t - offset).max(0.0), 0.0));
        }
    }
    timed.sort_by(|a, b| a.start.total_cmp(&b.start));
    out.lines = Some(timed);
    out.text = out.lines.as_ref().map(|l| text_of(l)).unwrap_or_default();
    out
}

/// SRT and WebVTT cues.
fn subtitles(content: &str, format: &'static str) -> Imported {
    let mut lines = Vec::new();
    let mut cue: Option<(f64, f64, Vec<String>)> = None;
    let flush = |cue: &mut Option<(f64, f64, Vec<String>)>, lines: &mut Vec<LyricLine>| {
        if let Some((start, end, text)) = cue.take() {
            let text = clean_inline(&text.join(" "));
            if !text.is_empty() {
                lines.push(LyricLine::new(text, start, end));
            }
        }
    };
    let mut skipping_block = false;
    for raw in content.replace("\r\n", "\n").lines() {
        let line = raw.trim();
        if line.is_empty() {
            flush(&mut cue, &mut lines);
            skipping_block = false;
            continue;
        }
        if let Some((a, b)) = line.split_once("-->")
            && let (Some(start), Some(end)) = (parse_clock(a), parse_clock(b.split_whitespace().next().unwrap_or("")))
        {
            flush(&mut cue, &mut lines);
            cue = Some((start, end, Vec::new()));
            continue;
        }
        if cue.is_none() && (line.starts_with("NOTE") || line.starts_with("STYLE") || line.starts_with("REGION")) {
            skipping_block = true;
        }
        match &mut cue {
            Some((_, _, text)) if !skipping_block => text.push(line.to_owned()),
            // Cue numbers / ids, WEBVTT header, NOTE blocks.
            _ => {}
        }
    }
    flush(&mut cue, &mut lines);
    lines.sort_by(|a, b| a.start.total_cmp(&b.start));
    Imported { text: text_of(&lines), lines: Some(lines), format, ..Default::default() }
}

/// `1:02.5`, `01:02:03,450`, `00:01.000`, `1:02:30` (LRC centiseconds).
fn parse_clock(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() || !s.chars().next()?.is_ascii_digit() {
        return None;
    }
    let s = s.replace(',', ".");
    let parts: Vec<&str> = s.split(':').collect();
    let num = |p: &str| p.parse::<f64>().ok().filter(|v| *v >= 0.0);
    match parts.as_slice() {
        [m, sec] => Some(num(m)? * 60.0 + num(sec)?),
        // h:m:s.f, or LRC's m:s:cc
        [a, b, c] if c.contains('.') || c.len() != 2 || num(b)? >= 60.0 => Some(num(a)? * 3600.0 + num(b)? * 60.0 + num(c)?),
        [a, b, c] => Some(num(a)? * 60.0 + num(b)? + num(c)? / 100.0),
        _ => None,
    }
}

/// Ends for synced lines: up to the next line (gaps marked by empty lines
/// stay gaps); the last runs at most 8 s.
fn finish_timing(lines: &mut Vec<LyricLine>, duration: f64) {
    for i in 0..lines.len() {
        if lines[i].end <= lines[i].start {
            let next = lines.get(i + 1).map(|l| l.start);
            let limit = if duration > 0.0 { duration } else { f64::INFINITY };
            lines[i].end = next.unwrap_or((lines[i].start + 8.0).min(limit)).max(lines[i].start + 0.2);
        }
    }
    lines.retain(|l| !l.text.is_empty());
}

fn text_of(lines: &[LyricLine]) -> String {
    lines.iter().filter(|l| !l.text.is_empty()).map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n")
}

// --------------------------------------------------------------------------- plain

fn plain(content: &str) -> Imported {
    let text = content.lines().filter(|l| !is_chord_line(l) && !is_section_label(l)).map(clean_inline).collect::<Vec<_>>().join("\n");
    Imported { text: tidy(&text), format: "Text", ..Default::default() }
}

fn chordpro(content: &str) -> Imported {
    let mut out = Imported { format: "ChordPro", ..Default::default() };
    let mut text = Vec::new();
    let mut in_tab = false;
    for raw in content.lines() {
        let line = raw.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some(d) = line.strip_prefix('{').and_then(|d| d.strip_suffix('}')) {
            let (key, value) = d.split_once(':').map_or((d, ""), |(k, v)| (k, v.trim()));
            match key.trim().to_lowercase().as_str() {
                "title" | "t" if !value.is_empty() => out.title = Some(value.to_owned()),
                "artist" | "subtitle" | "st" | "composer" if out.artist.is_none() && !value.is_empty() => out.artist = Some(value.to_owned()),
                "start_of_tab" | "sot" | "start_of_grid" | "sog" => in_tab = true,
                "end_of_tab" | "eot" | "end_of_grid" | "eog" => in_tab = false,
                _ => text.push(String::new()),
            }
            continue;
        }
        if in_tab || is_chord_line(line) || is_section_label(line) {
            continue;
        }
        // Inline chords: "[G]Amazing [C]grace"
        let mut s = String::new();
        let mut depth = 0;
        for c in line.chars() {
            match c {
                '[' => depth += 1,
                ']' if depth > 0 => depth -= 1,
                _ if depth == 0 => s.push(c),
                _ => {}
            }
        }
        text.push(clean_inline(&s));
    }
    out.text = tidy(&text.join("\n"));
    out
}

/// OpenLyrics XML (OpenLP, Worship Extreme, …), verses in `verseOrder`.
fn openlyrics(xml: &str) -> Imported {
    let mut out = Imported { format: "OpenLyrics", ..Default::default() };
    out.title = tag_text(xml, "title").map(|t| entities(&t));
    out.artist = tag_text(xml, "author").map(|t| entities(&strip_tags(&t)));
    let mut verses: Vec<(String, String)> = Vec::new();
    let mut rest = xml;
    while let Some(i) = find_tag(rest, "verse") {
        let after = &rest[i..];
        let Some(open_end) = after.find('>') else { break };
        let open = &after[..open_end];
        let name = attr(open, "name").unwrap_or_default().to_lowercase();
        let Some(close) = after.find("</verse>") else { break };
        let body = &after[open_end + 1..close];
        let mut lines = Vec::new();
        let mut b = body;
        while let Some(j) = find_tag(b, "lines") {
            let s = &b[j..];
            let (Some(gt), Some(end)) = (s.find('>'), s.find("</lines>")) else { break };
            let inner = &s[gt + 1..end];
            let inner = remove_element(inner, "comment");
            for l in inner.replace("<br/>", "\n").replace("<br />", "\n").replace("<br>", "\n").lines() {
                lines.push(clean_inline(&entities(&strip_tags(l))));
            }
            b = &s[end + 8..];
        }
        verses.push((name, lines.join("\n")));
        rest = &after[close + 8..];
    }
    let order = tag_text(xml, "verseOrder").map(|o| o.split_whitespace().map(str::to_lowercase).collect::<Vec<_>>()).unwrap_or_default();
    out.text = tidy(&in_order(&verses, &order));
    out
}

/// OpenSong: `[V1]` headers, `.` chord lines, `;` comments, `|` line breaks.
fn opensong(xml: &str) -> Imported {
    let mut out = Imported { format: "OpenSong", ..Default::default() };
    out.title = tag_text(xml, "title").map(|t| entities(&t));
    out.artist = tag_text(xml, "author").map(|t| entities(&t)).filter(|a| !a.trim().is_empty());
    let body = entities(&tag_text(xml, "lyrics").unwrap_or_default());
    let mut verses: Vec<(String, String)> = Vec::new();
    let mut current = String::new();
    for raw in body.lines() {
        if let Some(h) = raw.trim().strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
            current = h.trim().to_lowercase();
            continue;
        }
        if raw.starts_with('.') || raw.starts_with(';') || raw.trim().is_empty() {
            continue;
        }
        // A leading digit numbers verses sharing one chord block: "1 Amazing grace".
        let (name, line) = match raw.chars().next() {
            Some(d) if d.is_ascii_digit() => (format!("{current}{d}"), &raw[1..]),
            _ => (current.clone(), raw),
        };
        let line = line.replace("||", "\n").replace('|', "\n").replace('_', "");
        match verses.iter_mut().find(|(n, _)| *n == name) {
            Some((_, text)) => {
                text.push('\n');
                text.push_str(line.trim());
            }
            None => verses.push((name, line.trim().to_owned())),
        }
    }
    let order = tag_text(xml, "presentation").map(|o| o.split_whitespace().map(str::to_lowercase).collect::<Vec<_>>()).unwrap_or_default();
    out.text = tidy(&in_order(&verses, &order));
    out
}

/// Verses in the song's order (repeats included), else as written.
fn in_order(verses: &[(String, String)], order: &[String]) -> String {
    let picked: Vec<&str> = order.iter().filter_map(|o| verses.iter().find(|(n, _)| n == o).map(|(_, t)| t.as_str())).collect();
    let parts: Vec<&str> = if picked.is_empty() { verses.iter().map(|(_, t)| t.as_str()).collect() } else { picked };
    parts.join("\n\n")
}

// --------------------------------------------------------------------------- helpers

/// "G  D/F#  Em7   C" — a line of only chords.
fn is_chord_line(line: &str) -> bool {
    let tokens: Vec<&str> = line.split_whitespace().filter(|t| !matches!(*t, "|" | "-" | "/" | "x2" | "x3" | "x4" | "(x2)")).collect();
    !tokens.is_empty() && tokens.iter().all(|t| is_chord(t.trim_matches(|c| c == '(' || c == ')')))
}

fn is_chord(t: &str) -> bool {
    let (main, bass) = t.split_once('/').unwrap_or((t, ""));
    let root = |s: &str| -> Option<usize> {
        let mut c = s.chars();
        if !matches!(c.next()?, 'A'..='G') {
            return None;
        }
        Some(if matches!(c.next(), Some('#' | 'b')) { 2 } else { 1 })
    };
    let Some(n) = root(main) else { return false };
    if !bass.is_empty() && root(bass) != Some(bass.len()) {
        return false;
    }
    let mut q = main[n..].to_owned();
    for word in ["maj", "min", "dim", "aug", "sus", "add", "m", "M", "°", "+", "ø"] {
        q = q.replace(word, "");
    }
    q.chars().all(|c| c.is_ascii_digit() || matches!(c, '#' | 'b' | '(' | ')'))
}

/// "[Chorus]", "Verse 2:", "(Bridge)", "PRE-CHORUS".
fn is_section_label(line: &str) -> bool {
    let t = line.trim().trim_matches(|c| matches!(c, '[' | ']' | '(' | ')' | ':' | '*' | '#')).trim().to_lowercase();
    const LABELS: &[&str] = &[
        "verse", "chorus", "pre-chorus", "pre chorus", "prechorus", "bridge", "intro", "outro", "tag", "refrain", "ending", "interlude", "instrumental", "hook", "vamp", "coda", "turnaround", "breakdown",
    ];
    LABELS.iter().any(|l| t == *l || t.strip_prefix(l).is_some_and(|r| r.trim().chars().all(|c| c.is_ascii_digit() || c == 'x' || c == ' ')))
}

/// Remove markup inside a line: HTML-ish tags, enhanced-LRC word stamps.
fn clean_inline(line: &str) -> String {
    let s = strip_tags(line);
    let s = entities(&s);
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn entities(s: &str) -> String {
    let mut s = s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&#39;", "'").replace("&nbsp;", " ");
    // Numeric: &#8217; &#x2019;
    while let Some(i) = s.find("&#") {
        let Some(len) = s[i..].find(';') else { break };
        let code = &s[i + 2..i + len];
        let ch = if let Some(hex) = code.strip_prefix(['x', 'X']) { u32::from_str_radix(hex, 16).ok() } else { code.parse().ok() };
        let ch = ch.and_then(char::from_u32).map(String::from).unwrap_or_default();
        s.replace_range(i..i + len + 1, &ch);
    }
    s.replace("&amp;", "&")
}

/// Byte offset of the first `<name>` / `<name …>` (not `<names>`).
fn find_tag(xml: &str, name: &str) -> Option<usize> {
    let pat = format!("<{name}");
    let mut from = 0;
    while let Some(i) = xml[from..].find(&pat) {
        let at = from + i;
        match xml[at + pat.len()..].chars().next() {
            Some('>' | '/' | ' ' | '\t' | '\n' | '\r') => return Some(at),
            _ => from = at + pat.len(),
        }
    }
    None
}

/// Inner text of the first `<name …>…</name>`.
fn tag_text(xml: &str, name: &str) -> Option<String> {
    let start = find_tag(xml, name)?;
    let after = &xml[start..];
    let gt = after.find('>')?;
    if after[..gt].ends_with('/') {
        return None;
    }
    let end = after.find(&format!("</{name}>"))?;
    let inner = after[gt + 1..end].trim();
    let inner = inner.strip_prefix("<![CDATA[").and_then(|s| s.strip_suffix("]]>")).unwrap_or(inner);
    Some(inner.to_owned())
}

fn attr(open_tag: &str, name: &str) -> Option<String> {
    let i = open_tag.find(&format!("{name}="))?;
    let rest = &open_tag[i + name.len() + 1..];
    let quote = rest.chars().next()?;
    let rest = &rest[1..];
    Some(rest[..rest.find(quote)?].to_owned())
}

fn remove_element(s: &str, name: &str) -> String {
    let mut out = s.to_owned();
    while let (Some(a), Some(b)) = (find_tag(&out, name), out.find(&format!("</{name}>"))) {
        if b < a {
            break;
        }
        out.replace_range(a..b + name.len() + 3, "");
    }
    out
}

/// Trim lines and collapse runs of blank lines.
fn tidy(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for l in text.lines().map(str::trim) {
        if l.is_empty() && out.last().is_none_or(|p| p.is_empty()) {
            continue;
        }
        out.push(l);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn times(i: &Imported) -> Vec<(f64, f64, &str)> {
        i.lines.as_ref().unwrap().iter().map(|l| ((l.start * 100.0).round() / 100.0, (l.end * 100.0).round() / 100.0, l.text.as_str())).collect()
    }

    #[test]
    fn lrc_with_metadata_repeats_gaps_and_word_stamps() {
        let lrc = "[ti:Oceans]\n[ar:Hillsong UNITED]\n[offset:+500]\n[00:10.50]You call me out\n[00:14.00][01:00.00]Upon the <00:15.20>waters\n[00:20.00]\n[00:25.00]Where feet may fail\n";
        let i = parse("oceans.lrc", lrc, 90.0).unwrap();
        assert_eq!((i.title.as_deref(), i.artist.as_deref(), i.format), (Some("Oceans"), Some("Hillsong UNITED"), "LRC"));
        assert_eq!(
            times(&i),
            vec![(10.0, 13.5, "You call me out"), (13.5, 19.5, "Upon the waters"), (24.5, 59.5, "Where feet may fail"), (59.5, 67.5, "Upon the waters")]
        );
        assert_eq!(i.text, "You call me out\nUpon the waters\nWhere feet may fail\nUpon the waters");
    }

    #[test]
    fn srt_and_vtt_cues() {
        let srt = "\u{feff}1\r\n00:00:01,000 --> 00:00:04,500\r\n<i>Amazing grace</i>\r\nhow sweet the sound\r\n\r\n2\r\n00:00:05,000 --> 00:00:08,000\r\nThat saved a wretch like me\r\n";
        let i = parse("grace.srt", &decode(srt.as_bytes()), 0.0).unwrap();
        assert_eq!(times(&i), vec![(1.0, 4.5, "Amazing grace how sweet the sound"), (5.0, 8.0, "That saved a wretch like me")]);

        let vtt = "WEBVTT\n\nNOTE made by hand\nnot a lyric\n\nintro\n00:01.000 --> 00:03.000 align:start\n<v Singer>Way maker</v>\n\n01:02:03.500 --> 01:02:05.000\nMiracle &amp; worker\n";
        let i = parse("x.vtt", vtt, 0.0).unwrap();
        assert_eq!(times(&i), vec![(1.0, 3.0, "Way maker"), (3723.5, 3725.0, "Miracle & worker")]);
        assert_eq!(i.format, "WebVTT");
    }

    #[test]
    fn chordpro_drops_chords_and_directives() {
        let cho = "{title: Amazing Grace}\n{artist: John Newton}\n# comment\n{soc}\n[G]Amazing [G7]grace, how [C]sweet the [G]sound\n{eoc}\n{sot}\ne|---3---|\n{eot}\n[Verse 2]\nG     D/F#   Em\n'Twas grace that taught\n";
        let i = parse("grace.cho", cho, 0.0).unwrap();
        assert_eq!((i.title.as_deref(), i.artist.as_deref(), i.lines.is_none()), (Some("Amazing Grace"), Some("John Newton"), true));
        assert_eq!(i.text, "Amazing grace, how sweet the sound\n\n'Twas grace that taught");
    }

    #[test]
    fn plain_text_from_a_lyrics_site() {
        let txt = "[Verse 1]\nYou are the way maker\n\n\n[Chorus]\nC        G       Am   F\nMiracle worker, promise keeper\nChorus x2\n";
        let i = parse("song.txt", txt, 0.0).unwrap();
        assert_eq!(i.text, "You are the way maker\n\nMiracle worker, promise keeper");
        assert_eq!(i.format, "Text");
    }

    #[test]
    fn openlyrics_follows_verse_order() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<song xmlns="http://openlyrics.info/namespace/2009/song" version="0.9">
  <properties><titles><title>Amazing Grace</title></titles><authors><author>John Newton</author></authors>
  <verseOrder>v1 c v1</verseOrder></properties>
  <lyrics>
    <verse name="c"><lines>Praise <chord name="D"/>Him<br/>Praise &amp; worship</lines></verse>
    <verse name="v1"><lines><comment>soft</comment>Amazing grace<br />how sweet the sound</lines></verse>
  </lyrics>
</song>"#;
        let i = parse("grace.xml", xml, 0.0).unwrap();
        assert_eq!((i.title.as_deref(), i.artist.as_deref(), i.format), (Some("Amazing Grace"), Some("John Newton"), "OpenLyrics"));
        assert_eq!(i.text, "Amazing grace\nhow sweet the sound\n\nPraise Him\nPraise & worship\n\nAmazing grace\nhow sweet the sound");
    }

    #[test]
    fn opensong_sections_and_presentation() {
        let xml = "<?xml version=\"1.0\"?>\n<song><title>Grace</title><author>Newton</author><presentation>V1 C V2</presentation><lyrics>[C]\n. G   C\n Praise|the Lord\n[V]\n;comment\n1 Amazing grace\n2 Through many dangers\n</lyrics></song>";
        let i = parse("Grace", xml, 0.0).unwrap();
        assert_eq!(i.format, "OpenSong");
        assert_eq!(i.text, "Amazing grace\n\nPraise\nthe Lord\n\nThrough many dangers");
    }

    #[test]
    fn decodes_utf16_and_latin1_and_rejects_empty() {
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend("[00:01.00]Siyahamba".encode_utf16().flat_map(|u| u.to_le_bytes()));
        assert_eq!(decode(&utf16), "[00:01.00]Siyahamba");
        assert_eq!(decode(&[b'J', 0xE9, b's', b'u', b's']), "Jésus");
        assert!(parse("empty.txt", "\n[Chorus]\n G  C \n", 0.0).is_err());
    }

    #[test]
    fn chords_and_labels_are_recognised() {
        for c in ["G", "D/F#", "Em7", "Cmaj7", "Bbsus4", "F#m7b5", "Asus2", "C#dim", "Gadd9"] {
            assert!(is_chord(c), "{c}");
        }
        for w in ["Grace", "I", "Am I", "Be", "Ages"] {
            assert!(!is_chord_line(w), "{w}");
        }
        assert!(is_section_label("Pre-Chorus 2:") && is_section_label("(Bridge)") && !is_section_label("Chorus of angels"));
    }
}
