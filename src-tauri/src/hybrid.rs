use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct HybridResult {
    pub out_txt: PathBuf,
    pub out_srt: PathBuf,
    pub out_flagged: Option<PathBuf>,
    pub replacements: usize,
    pub flagged_segments: usize,
}

#[derive(Debug, Clone)]
struct SrtBlock {
    index: String,
    timing: String,
    start_ms: u64,
    end_ms: u64,
    text_lines: Vec<String>,
}

#[derive(Debug, Clone)]
struct WhisperToken {
    leading_ws: String,
    raw: String,
    word_lower: String,
    line_idx: usize,
}

#[derive(Debug, Clone)]
struct YtToken {
    word_lower: String,
    start_ms: u64,
    end_ms: u64,
}

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "been", "being", "but", "by", "can", "could", "did",
    "do", "does", "for", "from", "had", "has", "have", "he", "her", "here", "hers", "him", "his",
    "how", "i", "if", "in", "into", "is", "it", "its", "just", "let", "lets", "me", "mine", "my",
    "no", "not", "now", "of", "on", "or", "our", "ours", "out", "over", "own", "she", "should",
    "so", "some", "than", "that", "the", "their", "theirs", "them", "then", "there", "these",
    "they", "this", "those", "through", "to", "too", "under", "until", "up", "us", "very", "was",
    "we", "were", "what", "when", "where", "which", "while", "who", "whom", "why", "will", "with",
    "would", "yes", "you", "your", "yours",
];

pub fn build_hybrid(
    whisper_srt: &Path,
    youtube_srt: &Path,
    info_json: Option<&Path>,
    out_txt: &Path,
    out_srt: &Path,
    out_flagged: &Path,
) -> Result<HybridResult, String> {
    let whisper_content = fs::read_to_string(whisper_srt)
        .map_err(|e| format!("Could not read whisper SRT: {}", e))?;
    let youtube_content = fs::read_to_string(youtube_srt)
        .map_err(|e| format!("Could not read youtube SRT: {}", e))?;

    let mut whisper_blocks = parse_srt(&whisper_content);
    let youtube_blocks = parse_srt(&youtube_content);

    if whisper_blocks.is_empty() {
        return Err("Whisper SRT had no usable blocks".to_string());
    }

    let yt_tokens = collect_yt_tokens(&youtube_blocks);
    let metadata_vocab = info_json
        .and_then(|p| fs::read_to_string(p).ok())
        .map(|s| metadata_proper_nouns(&s))
        .unwrap_or_default();

    let stopwords: HashSet<&str> = STOPWORDS.iter().copied().collect();

    let mut replacements = 0usize;
    let mut flagged: Vec<String> = Vec::new();

    for block in whisper_blocks.iter_mut() {
        let merged_lines = merge_block(
            block,
            &yt_tokens,
            &metadata_vocab,
            &stopwords,
            &mut replacements,
            &mut flagged,
        );
        block.text_lines = merged_lines;
    }

    let srt_out = render_srt(&whisper_blocks);
    let txt_out = render_txt(&whisper_blocks);

    fs::write(out_srt, &srt_out).map_err(|e| format!("Could not write hybrid SRT: {}", e))?;
    fs::write(out_txt, &txt_out).map_err(|e| format!("Could not write hybrid TXT: {}", e))?;

    let flagged_path = if flagged.is_empty() {
        None
    } else {
        let body = format!(
            "Potential hallucinations or low-confidence merges.\n\
             Each section shows a run of Whisper words with no aligned YouTube caption.\n\
             ---\n\n{}\n",
            flagged.join("\n\n")
        );
        fs::write(out_flagged, body)
            .map_err(|e| format!("Could not write flagged segments: {}", e))?;
        Some(out_flagged.to_path_buf())
    };

    Ok(HybridResult {
        out_txt: out_txt.to_path_buf(),
        out_srt: out_srt.to_path_buf(),
        out_flagged: flagged_path,
        replacements,
        flagged_segments: flagged.len(),
    })
}

fn parse_srt(content: &str) -> Vec<SrtBlock> {
    let mut blocks = Vec::new();
    let mut current: Vec<&str> = Vec::new();

    let normalized = content.replace('\r', "");
    for line in normalized.split('\n').chain(std::iter::once("")) {
        if line.is_empty() {
            if !current.is_empty() {
                if let Some(block) = build_block(&current) {
                    blocks.push(block);
                }
                current.clear();
            }
        } else {
            current.push(line);
        }
    }
    blocks
}

fn build_block(lines: &[&str]) -> Option<SrtBlock> {
    let mut iter = lines.iter().peekable();
    let index = iter.next()?.to_string();
    let timing_line = iter.next()?.to_string();
    let (start_ms, end_ms) = parse_timing(&timing_line)?;
    let text_lines: Vec<String> = iter.map(|s| s.to_string()).collect();
    Some(SrtBlock {
        index,
        timing: timing_line,
        start_ms,
        end_ms,
        text_lines,
    })
}

fn parse_timing(line: &str) -> Option<(u64, u64)> {
    let parts: Vec<&str> = line.split("-->").collect();
    if parts.len() != 2 {
        return None;
    }
    Some((parse_ts(parts[0].trim())?, parse_ts(parts[1].trim())?))
}

fn parse_ts(value: &str) -> Option<u64> {
    let value = value.trim();
    let (hms_part, ms_part) = value
        .split_once(',')
        .or_else(|| value.split_once('.'))
        .unwrap_or((value, "0"));
    let hms: Vec<&str> = hms_part.split(':').collect();
    if hms.len() != 3 {
        return None;
    }
    let h: u64 = hms[0].parse().ok()?;
    let m: u64 = hms[1].parse().ok()?;
    let s: u64 = hms[2].parse().ok()?;
    let ms: u64 = ms_part.parse().ok()?;
    Some(((h * 3600 + m * 60 + s) * 1000) + ms)
}

fn collect_yt_tokens(blocks: &[SrtBlock]) -> Vec<YtToken> {
    let mut tokens: Vec<YtToken> = Vec::new();
    let mut last_word: Option<String> = None;

    for block in blocks {
        for line in &block.text_lines {
            let cleaned = strip_caption_tags(line);
            for chunk in cleaned.split_whitespace() {
                let word_lower = strip_to_word(chunk).to_lowercase();
                if word_lower.is_empty() {
                    continue;
                }
                // Skip rolling-caption frame duplicates: consecutive identical words.
                if last_word.as_deref() == Some(word_lower.as_str()) {
                    continue;
                }
                last_word = Some(word_lower.clone());
                tokens.push(YtToken {
                    word_lower,
                    start_ms: block.start_ms,
                    end_ms: block.end_ms,
                });
            }
        }
    }
    tokens
}

fn strip_caption_tags(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_tag = false;
    for ch in line.chars() {
        if in_tag {
            if ch == '>' {
                in_tag = false;
            }
            continue;
        }
        if ch == '<' {
            in_tag = true;
            continue;
        }
        out.push(ch);
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

fn strip_to_word(token: &str) -> String {
    token
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .collect()
}

fn tokenize_line(line: &str, line_idx: usize) -> Vec<WhisperToken> {
    let mut tokens = Vec::new();
    let mut idx = 0usize;
    let chars: Vec<char> = line.chars().collect();
    while idx < chars.len() {
        let ws_start = idx;
        while idx < chars.len() && chars[idx].is_whitespace() {
            idx += 1;
        }
        let leading_ws: String = chars[ws_start..idx].iter().collect();
        if idx >= chars.len() {
            if !leading_ws.is_empty() {
                tokens.push(WhisperToken {
                    leading_ws,
                    raw: String::new(),
                    word_lower: String::new(),
                    line_idx,
                });
            }
            break;
        }
        let word_start = idx;
        while idx < chars.len() && !chars[idx].is_whitespace() {
            idx += 1;
        }
        let raw: String = chars[word_start..idx].iter().collect();
        let word_lower = strip_to_word(&raw).to_lowercase();
        tokens.push(WhisperToken {
            leading_ws,
            raw,
            word_lower,
            line_idx,
        });
    }
    tokens
}

fn yt_window<'a>(tokens: &'a [YtToken], start_ms: u64, end_ms: u64) -> Vec<&'a YtToken> {
    let padding = 4_000u64;
    let lo = start_ms.saturating_sub(padding);
    let hi = end_ms.saturating_add(padding);
    tokens
        .iter()
        .filter(|t| t.end_ms >= lo && t.start_ms <= hi)
        .collect()
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum AlignOp {
    Match(usize, usize),
    Sub(usize, usize),
    InsertA(usize), // whisper-only (a side)
    InsertB(usize), // youtube-only (b side)
}

fn needleman_wunsch(a: &[&str], b: &[&str]) -> Vec<AlignOp> {
    let n = a.len();
    let m = b.len();
    let mut score = vec![vec![0i32; m + 1]; n + 1];
    let gap: i32 = -1;
    let match_score: i32 = 2;
    let mismatch: i32 = -1;
    for i in 0..=n {
        score[i][0] = (i as i32) * gap;
    }
    for j in 0..=m {
        score[0][j] = (j as i32) * gap;
    }
    for i in 1..=n {
        for j in 1..=m {
            let diag = score[i - 1][j - 1]
                + if a[i - 1] == b[j - 1] {
                    match_score
                } else {
                    mismatch
                };
            let up = score[i - 1][j] + gap;
            let left = score[i][j - 1] + gap;
            score[i][j] = diag.max(up).max(left);
        }
    }

    let mut ops = Vec::new();
    let mut i = n;
    let mut j = m;
    while i > 0 || j > 0 {
        if i > 0 && j > 0 {
            let diag = score[i - 1][j - 1]
                + if a[i - 1] == b[j - 1] {
                    match_score
                } else {
                    mismatch
                };
            if score[i][j] == diag {
                if a[i - 1] == b[j - 1] {
                    ops.push(AlignOp::Match(i - 1, j - 1));
                } else {
                    ops.push(AlignOp::Sub(i - 1, j - 1));
                }
                i -= 1;
                j -= 1;
                continue;
            }
        }
        if i > 0 && score[i][j] == score[i - 1][j] + gap {
            ops.push(AlignOp::InsertA(i - 1));
            i -= 1;
            continue;
        }
        ops.push(AlignOp::InsertB(j - 1));
        j -= 1;
    }
    ops.reverse();
    ops
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    if a_chars.is_empty() {
        return b_chars.len();
    }
    if b_chars.is_empty() {
        return a_chars.len();
    }
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    let mut cur = vec![0usize; b_chars.len() + 1];
    for i in 1..=a_chars.len() {
        cur[0] = i;
        for j in 1..=b_chars.len() {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b_chars.len()]
}

fn should_replace_with_youtube(
    whisper_word: &str,
    yt_word: &str,
    metadata_vocab: &HashSet<String>,
    stopwords: &HashSet<&str>,
) -> bool {
    if yt_word.is_empty() || whisper_word.is_empty() || whisper_word == yt_word {
        return false;
    }
    if stopwords.contains(yt_word) {
        return false;
    }
    if yt_word.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    if yt_word.chars().count() < 3 {
        return false;
    }
    // Strong signal: metadata vocabulary contains the YT token.
    if metadata_vocab.contains(yt_word) {
        return true;
    }
    let dist = edit_distance(whisper_word, yt_word);
    dist > 1
}

fn title_case(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => {
            let rest: String = chars.collect();
            format!("{}{}", first.to_uppercase(), rest.to_lowercase())
        }
        None => String::new(),
    }
}

fn replace_word_in_raw(raw: &str, replacement: &str) -> String {
    // Preserve leading/trailing punctuation around the word in `raw`.
    let mut leading = String::new();
    let mut trailing = String::new();
    let bytes: Vec<char> = raw.chars().collect();
    let mut i = 0usize;
    while i < bytes.len() && !bytes[i].is_alphanumeric() {
        leading.push(bytes[i]);
        i += 1;
    }
    let content_start = i;
    let mut j = bytes.len();
    while j > content_start && !bytes[j - 1].is_alphanumeric() {
        j -= 1;
    }
    let content_end = j;
    for ch in &bytes[content_end..] {
        trailing.push(*ch);
    }
    // Preserve apostrophe-suffix like "'s" if present.
    let middle: String = bytes[content_start..content_end].iter().collect();
    if let Some(idx) = middle.find('\'') {
        let suffix = &middle[idx..];
        format!("{}{}{}{}", leading, replacement, suffix, trailing)
    } else {
        format!("{}{}{}", leading, replacement, trailing)
    }
}

fn merge_block(
    block: &SrtBlock,
    yt_tokens: &[YtToken],
    metadata_vocab: &HashSet<String>,
    stopwords: &HashSet<&str>,
    replacements: &mut usize,
    flagged: &mut Vec<String>,
) -> Vec<String> {
    let mut whisper_tokens: Vec<WhisperToken> = Vec::new();
    for (line_idx, line) in block.text_lines.iter().enumerate() {
        whisper_tokens.extend(tokenize_line(line, line_idx));
    }
    let original_lines = block.text_lines.clone();

    let yt_slice: Vec<&YtToken> = yt_window(yt_tokens, block.start_ms, block.end_ms);
    if yt_slice.is_empty() || whisper_tokens.is_empty() {
        return original_lines;
    }

    let a: Vec<&str> = whisper_tokens
        .iter()
        .map(|t| t.word_lower.as_str())
        .collect();
    let b: Vec<&str> = yt_slice.iter().map(|t| t.word_lower.as_str()).collect();

    let ops = needleman_wunsch(&a, &b);

    let mut new_raw: Vec<String> = whisper_tokens.iter().map(|t| t.raw.clone()).collect();
    let mut whisper_only_run: Vec<usize> = Vec::new();
    let mut runs: Vec<Vec<usize>> = Vec::new();

    for op in &ops {
        match op {
            AlignOp::Match(_, _) => {
                if whisper_only_run.len() >= 3 {
                    runs.push(whisper_only_run.clone());
                }
                whisper_only_run.clear();
            }
            AlignOp::Sub(ai, bi) => {
                let w_word = &whisper_tokens[*ai].word_lower;
                let y_word = &yt_slice[*bi].word_lower;
                if should_replace_with_youtube(w_word, y_word, metadata_vocab, stopwords) {
                    let cased = title_case(y_word);
                    new_raw[*ai] = replace_word_in_raw(&whisper_tokens[*ai].raw, &cased);
                    *replacements += 1;
                }
                if whisper_only_run.len() >= 3 {
                    runs.push(whisper_only_run.clone());
                }
                whisper_only_run.clear();
            }
            AlignOp::InsertA(ai) => {
                if !whisper_tokens[*ai].word_lower.is_empty() {
                    whisper_only_run.push(*ai);
                }
            }
            AlignOp::InsertB(_) => {
                // Caption-only word; ignore.
            }
        }
    }
    if whisper_only_run.len() >= 3 {
        // Only flag if there is aligned context before AND after; if the run is at
        // the boundary, it likely just reflects missing YT coverage.
        let has_before = ops
            .iter()
            .position(|op| matches!(op, AlignOp::InsertA(_)))
            .map(|first_run_pos| {
                ops.iter()
                    .take(first_run_pos)
                    .any(|op| matches!(op, AlignOp::Match(_, _) | AlignOp::Sub(_, _)))
            })
            .unwrap_or(false);
        if has_before {
            runs.push(whisper_only_run.clone());
        }
        whisper_only_run.clear();
    }

    for run in runs {
        let snippet: String = run
            .iter()
            .map(|ai| whisper_tokens[*ai].raw.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        flagged.push(format!(
            "[{} --> {}] {}",
            format_ts(block.start_ms),
            format_ts(block.end_ms),
            snippet
        ));
    }

    // Reconstruct text lines from corrected tokens, preserving original line breaks.
    rebuild_lines(&whisper_tokens, &new_raw, block.text_lines.len())
}

fn rebuild_lines(
    tokens: &[WhisperToken],
    new_raw: &[String],
    line_count: usize,
) -> Vec<String> {
    let mut lines: Vec<String> = vec![String::new(); line_count.max(1)];
    let max_idx = lines.len() - 1;
    for (token, raw) in tokens.iter().zip(new_raw.iter()) {
        let line = &mut lines[token.line_idx.min(max_idx)];
        line.push_str(&token.leading_ws);
        line.push_str(raw);
    }
    for line in lines.iter_mut() {
        *line = line.trim_end().to_string();
    }
    lines
}

fn format_ts(ms: u64) -> String {
    let h = ms / 3_600_000;
    let m = (ms % 3_600_000) / 60_000;
    let s = (ms % 60_000) / 1_000;
    let frac = ms % 1_000;
    format!("{:02}:{:02}:{:02}.{:03}", h, m, s, frac)
}

fn render_srt(blocks: &[SrtBlock]) -> String {
    let mut out = String::new();
    for block in blocks {
        out.push_str(&block.index);
        out.push('\n');
        out.push_str(&block.timing);
        out.push('\n');
        for line in &block.text_lines {
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

fn render_txt(blocks: &[SrtBlock]) -> String {
    let mut out = String::new();
    for block in blocks {
        for line in &block.text_lines {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            out.push_str(trimmed);
            out.push('\n');
        }
    }
    out
}

fn metadata_proper_nouns(info_json: &str) -> HashSet<String> {
    let mut out: HashSet<String> = HashSet::new();
    for field in ["title", "description", "channel", "uploader", "uploader_id"] {
        if let Some(value) = extract_string_field(info_json, field) {
            for word in value.split(|c: char| !c.is_alphanumeric() && c != '\'') {
                let lower = word.to_lowercase();
                if lower.chars().count() >= 3 && word.chars().next().map_or(false, |c| c.is_uppercase()) {
                    out.insert(lower);
                }
            }
        }
    }
    out
}

fn extract_string_field(json: &str, field: &str) -> Option<String> {
    // Minimal best-effort JSON string extraction so we don't have to depend on serde_json here.
    let needle = format!("\"{}\"", field);
    let mut idx = 0usize;
    while let Some(pos) = json[idx..].find(&needle) {
        let abs = idx + pos + needle.len();
        let rest = json.get(abs..)?;
        // skip whitespace and colon
        let rest = rest.trim_start();
        let rest = rest.strip_prefix(':')?.trim_start();
        if let Some(stripped) = rest.strip_prefix('"') {
            let mut value = String::new();
            let mut escape = false;
            for ch in stripped.chars() {
                if escape {
                    match ch {
                        'n' => value.push('\n'),
                        't' => value.push('\t'),
                        'r' => value.push('\r'),
                        '"' => value.push('"'),
                        '\\' => value.push('\\'),
                        '/' => value.push('/'),
                        other => value.push(other),
                    }
                    escape = false;
                    continue;
                }
                if ch == '\\' {
                    escape = true;
                    continue;
                }
                if ch == '"' {
                    return Some(value);
                }
                value.push(ch);
            }
            return Some(value);
        }
        idx = abs;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_srt() {
        let src = "1\n00:00:01,000 --> 00:00:03,000\nHello world\n\n2\n00:00:03,000 --> 00:00:05,000\nSecond line\n";
        let blocks = parse_srt(src);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].text_lines, vec!["Hello world".to_string()]);
        assert_eq!(blocks[0].start_ms, 1000);
        assert_eq!(blocks[1].end_ms, 5000);
    }

    #[test]
    fn aligns_identical_streams() {
        let a = vec!["hello", "world"];
        let b = vec!["hello", "world"];
        let ops = needleman_wunsch(&a, &b);
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], AlignOp::Match(0, 0)));
    }

    #[test]
    fn detects_substitution() {
        let a = vec!["the", "beejing", "report"];
        let b = vec!["the", "beijing", "report"];
        let ops = needleman_wunsch(&a, &b);
        assert!(matches!(ops[1], AlignOp::Sub(1, 1)));
    }

    #[test]
    fn replaces_proper_noun_when_edit_distance_high() {
        let stopwords: HashSet<&str> = STOPWORDS.iter().copied().collect();
        let meta = HashSet::new();
        // Real-world example from the design doc: Whisper heard "field affordability",
        // YouTube heard "fuel affordability". field <-> fuel is two edits.
        assert!(should_replace_with_youtube(
            "field", "fuel", &meta, &stopwords
        ));
    }

    #[test]
    fn keeps_whisper_for_single_character_typo_without_metadata() {
        let stopwords: HashSet<&str> = STOPWORDS.iter().copied().collect();
        let meta = HashSet::new();
        // Only one edit apart: too risky to replace without a vocabulary signal.
        assert!(!should_replace_with_youtube(
            "beejing", "beijing", &meta, &stopwords
        ));
    }

    #[test]
    fn keeps_whisper_when_word_is_stopword() {
        let stopwords: HashSet<&str> = STOPWORDS.iter().copied().collect();
        let meta = HashSet::new();
        assert!(!should_replace_with_youtube(
            "in", "on", &meta, &stopwords
        ));
    }

    #[test]
    fn metadata_vocab_overrides_low_edit_distance() {
        let stopwords: HashSet<&str> = STOPWORDS.iter().copied().collect();
        let mut meta = HashSet::new();
        meta.insert("kasie".to_string());
        // Only one edit apart, but metadata says this is a real name.
        assert!(should_replace_with_youtube(
            "casey", "kasie", &meta, &stopwords
        ));
    }

    #[test]
    fn extracts_string_field_from_json() {
        let json = r#"{ "title": "Sec. Rubio meets Kasie Hunt", "view_count": 12 }"#;
        let title = extract_string_field(json, "title").unwrap();
        assert_eq!(title, "Sec. Rubio meets Kasie Hunt");
        let vocab = metadata_proper_nouns(json);
        assert!(vocab.contains("rubio"));
        assert!(vocab.contains("kasie"));
        assert!(vocab.contains("hunt"));
    }

    #[test]
    fn replaces_word_preserving_punctuation() {
        assert_eq!(replace_word_in_raw("beejing,", "Beijing"), "Beijing,");
        assert_eq!(replace_word_in_raw("\"beejing\"", "Beijing"), "\"Beijing\"");
    }

    #[test]
    fn dedupes_consecutive_yt_words() {
        let blocks = vec![SrtBlock {
            index: "1".into(),
            timing: "00:00:00,000 --> 00:00:02,000".into(),
            start_ms: 0,
            end_ms: 2000,
            text_lines: vec!["hello hello world".into()],
        }];
        let tokens = collect_yt_tokens(&blocks);
        let words: Vec<&str> = tokens.iter().map(|t| t.word_lower.as_str()).collect();
        assert_eq!(words, vec!["hello", "world"]);
    }
}
