// =============================================================================
// hybrid.rs — the "Hybrid Transcript" merge engine.
//
// WHAT THIS FILE DOES (in plain English)
// --------------------------------------
// This file builds a single "best of both worlds" transcript from two source
// transcripts:
//
//   1. Whisper's transcript — clean prose, proper punctuation, mixed case.
//      Whisper is great at general speech but often mistranscribes proper
//      nouns (people's names, places, brand names).
//
//   2. YouTube's auto-captions — usually all-caps, lightly punctuated, full
//      of duplicate "rolling caption" frames. YouTube is rough as a finished
//      transcript, but it tends to spell proper nouns correctly because
//      YouTube has access to channel metadata and a current name dictionary.
//
// The "merge" walks both transcripts side-by-side and, wherever the two
// disagree on a word, decides which one to trust. Whisper wins by default
// (its prose is cleaner); YouTube wins for words that look like names or
// brand-specific vocabulary. The output is a new .srt and .txt file, plus
// an optional sidecar file listing any spots Whisper might have invented
// words out of thin air (a "hallucination"), so a human can double-check
// those segments later.
//
// HOW THE STEPS LINE UP
// ---------------------
//   build_hybrid()      Top-level entry. Reads both files, runs the merge,
//                       writes the new transcript.
//   parse_srt()         Splits an .srt file into numbered blocks.
//   collect_yt_tokens() Turns YouTube blocks into a flat word stream and
//                       throws away duplicates from rolling captions.
//   tokenize_line()     Turns a Whisper line into individual words while
//                       remembering the spaces and punctuation around each.
//   needleman_wunsch()  The classic "alignment" algorithm: lines up two
//                       sequences of words even when one has extra/missing
//                       words. Used everywhere from biology (DNA alignment)
//                       to spell-checkers.
//   edit_distance()     Counts how many letter edits it takes to turn one
//                       word into another ("fuel" -> "field" = 2 edits).
//   merge_block()       For one Whisper block, decides word-by-word whether
//                       to use Whisper's word or YouTube's word, and flags
//                       suspicious-looking runs of Whisper-only text.
//   render_srt/txt()    Reassembles the corrected blocks into output files.
// =============================================================================

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// What the merge function gives back to whoever called it.
///
/// - `out_txt` / `out_srt` — paths to the two output files just written.
/// - `out_flagged` — path to the sidecar file listing suspicious segments,
///   or `None` if nothing was suspicious enough to flag.
/// - `replacements` — how many Whisper words got swapped out for YouTube's.
/// - `flagged_segments` — how many runs of suspicious Whisper-only text
///   ended up in the flagged sidecar.
#[derive(Debug)]
pub struct HybridResult {
    pub out_txt: PathBuf,
    pub out_srt: PathBuf,
    pub out_flagged: Option<PathBuf>,
    pub replacements: usize,
    pub flagged_segments: usize,
}

/// One numbered chunk of an .srt subtitle file. An .srt file is just a
/// repeating pattern of: a number, a time range, and one or more text lines,
/// separated by blank lines. This struct holds one such chunk.
#[derive(Debug, Clone)]
struct SrtBlock {
    index: String,            // e.g. "42" (the block number as written)
    timing: String,           // e.g. "00:01:23,400 --> 00:01:25,800"
    start_ms: u64,            // when this block starts, in milliseconds
    end_ms: u64,              // when this block ends, in milliseconds
    text_lines: Vec<String>,  // the actual subtitle text, one entry per line
}

/// One word from the Whisper transcript, along with the formatting around it
/// so we can put it back exactly as we found it (or with one word swapped).
#[derive(Debug, Clone)]
struct WhisperToken {
    leading_ws: String,  // the whitespace that came before this word
    raw: String,         // the word as it appeared, including punctuation
    word_lower: String,  // a lowercase, punctuation-free copy for comparison
    line_idx: usize,     // which line within the block this word lived on
}

/// One word from the YouTube captions, with the approximate time window
/// during which YouTube showed it on screen.
#[derive(Debug, Clone)]
struct YtToken {
    word_lower: String,
    start_ms: u64,
    end_ms: u64,
}

/// Common "filler" words we never want to substitute. If YouTube and Whisper
/// disagree about whether the next word was "on" vs. "in", we don't want the
/// merge to randomly flip every preposition — those small words are too easy
/// to mis-hear and rarely matter for accuracy.
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

/// **The top-level "do the merge" function.**
///
/// Reads the Whisper .srt and YouTube .srt files, optionally reads the
/// video's .info.json (for extra proper-noun hints from the title /
/// description / channel name), runs the merge, and writes the corrected
/// .srt and .txt files to disk. Returns a `HybridResult` summarizing what
/// happened.
pub fn build_hybrid(
    whisper_srt: &Path,
    youtube_srt: &Path,
    info_json: Option<&Path>,
    out_txt: &Path,
    out_srt: &Path,
    out_flagged: &Path,
) -> Result<HybridResult, String> {
    // Step 1: load both subtitle files from disk into memory as text.
    let whisper_content = fs::read_to_string(whisper_srt)
        .map_err(|e| format!("Could not read whisper SRT: {}", e))?;
    let youtube_content = fs::read_to_string(youtube_srt)
        .map_err(|e| format!("Could not read youtube SRT: {}", e))?;

    // Step 2: parse the raw text into structured "blocks" (numbered chunks
    // with time ranges and text).
    let mut whisper_blocks = parse_srt(&whisper_content);
    let youtube_blocks = parse_srt(&youtube_content);

    // If the Whisper file was empty or unparseable, there's nothing to merge.
    if whisper_blocks.is_empty() {
        return Err("Whisper SRT had no usable blocks".to_string());
    }

    // Step 3: flatten YouTube's blocks into a single stream of words tagged
    // with their approximate timestamps (so we can match them against the
    // right Whisper block later).
    let yt_tokens = collect_yt_tokens(&youtube_blocks);

    // Step 4: if we were given the video's metadata file (.info.json), pull
    // out any words from the title/description/channel that look like proper
    // nouns. Those words become "strong-signal" replacements during the
    // merge — if YouTube spells one of them differently from Whisper, trust
    // YouTube even when the difference is small.
    let metadata_vocab = info_json
        .and_then(|p| fs::read_to_string(p).ok())
        .map(|s| metadata_proper_nouns(&s))
        .unwrap_or_default();

    // Step 5: build a fast lookup of the stopwords list.
    let stopwords: HashSet<&str> = STOPWORDS.iter().copied().collect();

    let mut replacements = 0usize;
    let mut flagged: Vec<String> = Vec::new();

    // Step 6: run the merge one Whisper block at a time. `merge_block`
    // rewrites the block's text lines in place with the merged result.
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

    // Step 7: turn the merged blocks back into .srt and plain .txt files.
    let srt_out = render_srt(&whisper_blocks);
    let txt_out = render_txt(&whisper_blocks);

    fs::write(out_srt, &srt_out).map_err(|e| format!("Could not write hybrid SRT: {}", e))?;
    fs::write(out_txt, &txt_out).map_err(|e| format!("Could not write hybrid TXT: {}", e))?;

    // Step 8: if any suspicious runs of Whisper-only text were flagged,
    // write a separate sidecar file the user can review.
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

/// Reads an .srt file's text and splits it into numbered blocks.
///
/// .srt files separate blocks with blank lines, so this function walks the
/// content line-by-line and starts a new block every time it hits a blank.
fn parse_srt(content: &str) -> Vec<SrtBlock> {
    let mut blocks = Vec::new();
    let mut current: Vec<&str> = Vec::new();

    // Strip Windows-style "\r" line endings so we only have to handle "\n".
    let normalized = content.replace('\r', "");
    // Walk every line, plus one extra empty line at the end to flush the
    // final block if the file didn't end with a blank line.
    for line in normalized.split('\n').chain(std::iter::once("")) {
        if line.is_empty() {
            // Blank line = end of current block. Try to turn what we
            // accumulated into a proper SrtBlock.
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

/// Turns a list of consecutive non-blank .srt lines into one `SrtBlock`.
/// First line is the block number, second line is the timing, the rest is
/// the subtitle text.
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

/// Parses an .srt timing line ("HH:MM:SS,mmm --> HH:MM:SS,mmm") into a pair
/// of millisecond values (start, end). Returns `None` if the line doesn't
/// look like a real timing line.
fn parse_timing(line: &str) -> Option<(u64, u64)> {
    let parts: Vec<&str> = line.split("-->").collect();
    if parts.len() != 2 {
        return None;
    }
    Some((parse_ts(parts[0].trim())?, parse_ts(parts[1].trim())?))
}

/// Parses a single .srt timestamp like "00:01:23,456" into milliseconds.
/// .srt uses a comma; .vtt and some sloppy SRTs use a period — we accept
/// either.
fn parse_ts(value: &str) -> Option<u64> {
    let value = value.trim();
    // Split "HH:MM:SS,mmm" into "HH:MM:SS" and "mmm".
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

/// Walks every YouTube caption block and produces a flat list of words,
/// each tagged with the time range during which YouTube showed it.
///
/// YouTube's auto-captions usually arrive as "rolling captions": every new
/// line repeats most of the previous line plus one new word. This function
/// also drops *consecutive identical* words to undo most of that rolling
/// duplication.
fn collect_yt_tokens(blocks: &[SrtBlock]) -> Vec<YtToken> {
    let mut tokens: Vec<YtToken> = Vec::new();
    let mut last_word: Option<String> = None;

    for block in blocks {
        for line in &block.text_lines {
            // Some YouTube captions include HTML-like tags (<c> ... </c>).
            // Strip those so we only deal with the actual words.
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

/// Removes any "<...>" tags from a caption line (YouTube sometimes wraps
/// individual words in tags for per-word highlighting). Also decodes a few
/// common HTML entities.
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

/// Strips away anything that isn't a letter, a digit, or an apostrophe,
/// leaving just the "core" of a word. So "hello," -> "hello" and
/// "they're." -> "they're".
fn strip_to_word(token: &str) -> String {
    token
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .collect()
}

/// Splits one line of Whisper text into individual word tokens, remembering
/// the whitespace that came before each word so we can rebuild the line
/// exactly as it was (possibly with one word swapped).
fn tokenize_line(line: &str, line_idx: usize) -> Vec<WhisperToken> {
    let mut tokens = Vec::new();
    let mut idx = 0usize;
    let chars: Vec<char> = line.chars().collect();
    while idx < chars.len() {
        // Capture any run of whitespace that comes before the next word.
        let ws_start = idx;
        while idx < chars.len() && chars[idx].is_whitespace() {
            idx += 1;
        }
        let leading_ws: String = chars[ws_start..idx].iter().collect();
        if idx >= chars.len() {
            // Trailing whitespace at end of line — store it as a token with
            // no word so the rebuild step keeps it.
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
        // Now capture the actual word (everything up to the next whitespace).
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

/// For a given Whisper block (with its start/end time), returns the slice of
/// YouTube tokens whose timestamps roughly overlap that window — plus 4
/// seconds of padding on each side to absorb any clock drift between the
/// two transcripts.
///
/// We do this windowing because the alignment algorithm is expensive on long
/// inputs: it's much faster to align "this Whisper block (20 words)" against
/// "the 30 nearby YouTube words" than to align the entire transcripts at
/// once.
fn yt_window<'a>(tokens: &'a [YtToken], start_ms: u64, end_ms: u64) -> Vec<&'a YtToken> {
    let padding = 4_000u64;
    let lo = start_ms.saturating_sub(padding);
    let hi = end_ms.saturating_add(padding);
    tokens
        .iter()
        .filter(|t| t.end_ms >= lo && t.start_ms <= hi)
        .collect()
}

/// The four possible outcomes of comparing one Whisper word to one YouTube
/// word during alignment:
///
/// - `Match`   — same word, no action needed
/// - `Sub`     — both sources have a word here but they disagree (consider
///               replacing Whisper's with YouTube's)
/// - `InsertA` — Whisper has a word here that YouTube doesn't (might be a
///               hallucination)
/// - `InsertB` — YouTube has a word here that Whisper doesn't (ignore;
///               we only keep what Whisper said)
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum AlignOp {
    Match(usize, usize),
    Sub(usize, usize),
    InsertA(usize), // whisper-only (a side)
    InsertB(usize), // youtube-only (b side)
}

/// The classic Needleman-Wunsch sequence alignment algorithm.
///
/// Given two lists of words `a` and `b`, this figures out the "best" way to
/// line them up — accounting for words that match, words that differ, and
/// gaps where one side has an extra word. The output is a list of
/// `AlignOp`s describing the alignment step-by-step.
///
/// This is the same algorithm bioinformatics uses to align DNA strands.
/// We're using it on words instead of nucleotides, but the math is identical.
fn needleman_wunsch(a: &[&str], b: &[&str]) -> Vec<AlignOp> {
    let n = a.len();
    let m = b.len();
    // Build a 2D scoring grid (n+1 rows by m+1 columns). Each cell stores
    // the best alignment score reachable up to that point.
    let mut score = vec![vec![0i32; m + 1]; n + 1];
    let gap: i32 = -1;        // penalty for skipping a word (insert/delete)
    let match_score: i32 = 2; // reward for a word that matches
    let mismatch: i32 = -1;   // penalty for two words that don't match
    // Initialize the top row and left column with cumulative gap penalties.
    for i in 0..=n {
        score[i][0] = (i as i32) * gap;
    }
    for j in 0..=m {
        score[0][j] = (j as i32) * gap;
    }
    // Fill the grid. Each cell looks at three predecessors (diagonal, up,
    // left) and picks the highest-scoring path.
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

    // "Trace back" from the bottom-right corner of the grid to the top-left,
    // recording the operation that produced each step. This reconstructs the
    // alignment itself.
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
    // Traceback collected the steps in reverse order, so flip them back.
    ops.reverse();
    ops
}

/// Counts the smallest number of single-letter edits (insert one letter,
/// delete one letter, or change one letter) needed to turn `a` into `b`.
///
/// This is the standard "Levenshtein distance". We use it to decide whether
/// two disagreeing words are *close* spellings of the same thing (probably
/// just a typo, leave Whisper alone) or *far* apart (probably a real
/// proper-noun mistake, swap in YouTube's spelling).
fn edit_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    if a_chars.is_empty() {
        return b_chars.len();
    }
    if b_chars.is_empty() {
        return a_chars.len();
    }
    // Use two rolling rows of a dynamic-programming table to compute the
    // distance without allocating the full n*m grid.
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

/// Decides whether we should swap Whisper's word for YouTube's word.
///
/// The rules, in order:
///   1. If either word is empty or they're already the same, no swap.
///   2. If YouTube's word is a stopword (a, the, in, on, ...) — never swap;
///      common filler words are too easy to mis-hear.
///   3. If YouTube's word contains digits — leave Whisper alone (digit
///      handling is too noisy to mess with here).
///   4. If YouTube's word is shorter than 3 letters — leave Whisper alone;
///      tiny words aren't worth the risk.
///   5. If the video metadata (title, description, channel name) explicitly
///      contains YouTube's word — trust YouTube even for tiny spelling
///      differences. The video says this is a real name.
///   6. Otherwise: swap only when the two spellings differ by more than one
///      letter. One-letter differences are usually typos, not name fixes.
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

/// Capitalizes just the first letter of a word and lowercases the rest.
/// So "BEIJING" -> "Beijing".
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

/// Takes Whisper's original "raw" word (which might be wrapped in quotes,
/// followed by a comma, etc.) and produces a new string with the word part
/// replaced — keeping all the surrounding punctuation intact.
///
/// Example: replace_word_in_raw("\"beejing,\"", "Beijing") -> "\"Beijing,\""
fn replace_word_in_raw(raw: &str, replacement: &str) -> String {
    // Preserve leading/trailing punctuation around the word in `raw`.
    let mut leading = String::new();
    let mut trailing = String::new();
    let bytes: Vec<char> = raw.chars().collect();
    // Walk forward from the start, grabbing any punctuation before the word.
    let mut i = 0usize;
    while i < bytes.len() && !bytes[i].is_alphanumeric() {
        leading.push(bytes[i]);
        i += 1;
    }
    let content_start = i;
    // Walk backward from the end, finding where the word ends.
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

/// **The core merge step for a single Whisper block.**
///
/// 1. Break the block's text into Whisper tokens (words + punctuation).
/// 2. Grab the YouTube words that line up with this block's time range.
/// 3. Run alignment between the two streams.
/// 4. For each aligned pair where they disagree, decide whether to swap.
/// 5. Note any 3+ word run of Whisper-only text as a possible hallucination.
/// 6. Reassemble the block's text lines with the corrected words.
fn merge_block(
    block: &SrtBlock,
    yt_tokens: &[YtToken],
    metadata_vocab: &HashSet<String>,
    stopwords: &HashSet<&str>,
    replacements: &mut usize,
    flagged: &mut Vec<String>,
) -> Vec<String> {
    // 1. Tokenize every line in the block.
    let mut whisper_tokens: Vec<WhisperToken> = Vec::new();
    for (line_idx, line) in block.text_lines.iter().enumerate() {
        whisper_tokens.extend(tokenize_line(line, line_idx));
    }
    let original_lines = block.text_lines.clone();

    // 2. Pick the relevant YouTube words by time window.
    let yt_slice: Vec<&YtToken> = yt_window(yt_tokens, block.start_ms, block.end_ms);
    // If either side has nothing to compare, leave the block alone.
    if yt_slice.is_empty() || whisper_tokens.is_empty() {
        return original_lines;
    }

    // 3. Align the two streams of lowercased words.
    let a: Vec<&str> = whisper_tokens
        .iter()
        .map(|t| t.word_lower.as_str())
        .collect();
    let b: Vec<&str> = yt_slice.iter().map(|t| t.word_lower.as_str()).collect();

    let ops = needleman_wunsch(&a, &b);

    // `new_raw` starts as a copy of Whisper's words; we'll overwrite entries
    // when we decide to replace one. `whisper_only_run` tracks current streak
    // of Whisper words that YouTube has no counterpart for; `runs` collects
    // the streaks that are long enough to flag as suspicious.
    let mut new_raw: Vec<String> = whisper_tokens.iter().map(|t| t.raw.clone()).collect();
    let mut whisper_only_run: Vec<usize> = Vec::new();
    let mut runs: Vec<Vec<usize>> = Vec::new();

    // 4. Walk the alignment operations one by one.
    for op in &ops {
        match op {
            AlignOp::Match(_, _) => {
                // Both sources said the same word — nothing to do. End any
                // running streak of Whisper-only words (it's "anchored" now).
                if whisper_only_run.len() >= 3 {
                    runs.push(whisper_only_run.clone());
                }
                whisper_only_run.clear();
            }
            AlignOp::Sub(ai, bi) => {
                // Both sources have a word here but they disagree. Decide
                // whether YouTube's word is more trustworthy.
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
                // Whisper has a word here that YouTube doesn't. Add it to
                // the current "no-YouTube-coverage" streak.
                if !whisper_tokens[*ai].word_lower.is_empty() {
                    whisper_only_run.push(*ai);
                }
            }
            AlignOp::InsertB(_) => {
                // Caption-only word; ignore.
            }
        }
    }
    // 5. Handle a streak that extended to the very end of the block.
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

    // Turn every flagged streak into a human-readable snippet so the user
    // can find the spot in the original transcript and double-check it.
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

    // 6. Reconstruct text lines from corrected tokens, preserving original line breaks.
    rebuild_lines(&whisper_tokens, &new_raw, block.text_lines.len())
}

/// Glues the (possibly corrected) tokens back into the same number of text
/// lines we started with, putting each token back on the line it originally
/// came from along with its leading whitespace.
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
    // Trim any trailing whitespace from each line so the .srt output stays clean.
    for line in lines.iter_mut() {
        *line = line.trim_end().to_string();
    }
    lines
}

/// Formats a millisecond timestamp as "HH:MM:SS.mmm" — used for the
/// human-readable headers in the flagged-segments sidecar file.
fn format_ts(ms: u64) -> String {
    let h = ms / 3_600_000;
    let m = (ms % 3_600_000) / 60_000;
    let s = (ms % 60_000) / 1_000;
    let frac = ms % 1_000;
    format!("{:02}:{:02}:{:02}.{:03}", h, m, s, frac)
}

/// Turns the corrected blocks back into the raw text of an .srt file —
/// block number, timing line, the text lines, then a blank line, repeating
/// for every block. This is the inverse of `parse_srt`.
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

/// Produces the plain-text version of the transcript — just the spoken
/// content, no timestamps, no block numbers, one line per subtitle line.
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

/// Scans the video's metadata JSON for likely proper nouns. We look at the
/// title, description, channel name, and uploader name, then pick out any
/// word that starts with a capital letter and is at least 3 characters long.
/// These become "strong signal" replacement candidates during the merge.
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

/// A tiny, hand-rolled extractor for one named string field out of a JSON
/// document. We don't need a full JSON parser here — we only ever read four
/// or five known fields from the .info.json file — so this avoids pulling
/// in a dependency just to read a title or description.
///
/// Returns the field's value if found, or `None` if the field is missing or
/// not a string.
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
            // Walk character by character, handling JSON escape sequences
            // (\n, \t, \", \\, etc.) until we hit the closing quote.
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

// =============================================================================
// Unit tests — these only run when you do `cargo test`, not during normal use.
// They give us confidence that the merge logic behaves the way we expect by
// running it against small, hand-crafted examples.
// =============================================================================
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
