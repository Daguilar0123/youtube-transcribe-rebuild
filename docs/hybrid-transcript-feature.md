# Hybrid Transcript Feature

## Summary

Hybrid Transcript is a new mode that combines the strengths of two existing outputs to produce a single, higher-quality transcript than either source can produce alone:

- **Whisper output** — clean prose, mixed case, proper punctuation, paragraph flow.
- **YouTube captions** — strong proper noun coverage (people, places, organizations) thanks to YouTube's broadcast metadata and current public-figure vocabulary.

The feature uses Whisper as the structural base and applies a targeted find-and-replace pass against the YouTube captions to correct proper nouns and other vocabulary YouTube transcribed correctly but Whisper got wrong.

## Problem It Solves

In side-by-side testing, neither source is reliably "best":

- Whisper produces clean, readable prose but consistently mistranscribes contemporary names, network correspondents, government officials, and brand names. It also occasionally hallucinates plausible-sounding content in noisy audio (e.g., inserting "in Beijing" into a question that contained no such phrase).
- YouTube captions get those proper nouns right but are delivered in all-caps, lightly punctuated, and full of duplicated rolling-caption frames with mid-stream typos. They are nearly unusable as a clean transcript without heavy post-processing.

The current "Captions + Whisper" mode saves both files for manual comparison. Users still have to merge them by hand. Hybrid Transcript automates that merge.

## How It Works

The pipeline runs after both source transcripts exist (i.e., the user is in or upgrades from Captions + Whisper mode).

1. **Normalize the YouTube captions.** Convert to mixed case, deduplicate the rolling-caption frames, strip mid-stream typos that don't appear in any settled frame, and tokenize into a clean word list with positions.
2. **Tokenize the Whisper transcript** into a parallel word list, preserving punctuation and timestamps from the SRT.
3. **Align the two streams** using a sequence alignment algorithm (Needleman-Wunsch or similar) over normalized word forms. The alignment tolerates insertions, deletions, and substitutions.
4. **Identify proper noun mismatches.** For every aligned position where the two sources disagree, classify the disagreement:
   - Both tokens look like common words → keep Whisper (Whisper's case and punctuation are usually better).
   - YouTube token is capitalized in its source and looks like a proper noun (name, place, organization, title) → use the YouTube token.
   - YouTube token matches a known vocabulary list of names that appear in the video metadata, channel description, or video title → use the YouTube token.
5. **Flag potential hallucinations.** Where Whisper has a multi-word phrase that has no aligned counterpart in YouTube, mark it for user review rather than silently keeping it.
6. **Re-emit** the corrected transcript as both `.txt` and `.srt`, preserving Whisper's timestamps.

## Output Naming

Following the existing source-labeled naming convention:

```text
video-title.hybrid.txt
video-title.hybrid.srt
```

The two source files (`.whisper.*` and `.youtube-captions.*`) are still saved alongside so users can audit the merge.

## UX

A new YouTube workflow mode in the existing mode picker:

### Hybrid Transcript

The app saves YouTube captions, runs Whisper locally, then merges the two into a single corrected transcript.

If YouTube captions are not available for the video, the mode falls back to Whisper-only with a clear status message in the live log explaining that no merge was possible.

A "Review flagged segments" button appears in the output panel after a successful run if any potential hallucinations or low-confidence merges were flagged. Clicking it opens a side-by-side diff view showing each flagged segment with the Whisper text, the aligned YouTube text, and the chosen output, with a one-click toggle to accept the alternative.

## Implementation Notes

- The alignment step is the only non-trivial new code. A pure-TypeScript Needleman-Wunsch over short word streams is fine for transcripts under ~50,000 words. For longer videos, chunk the alignment by SRT timestamp boundaries to keep memory bounded.
- Proper noun classification can start simple: any YouTube token that is capitalized in the source AND differs from the aligned Whisper token by more than a small edit distance is treated as a likely proper noun replacement. A second pass against video metadata (title, description, channel name) refines this.
- Punctuation and casing for non-replaced tokens always come from Whisper. YouTube's all-caps source is only consulted for token *content*, never for case or punctuation.
- The hallucination flag should fire when Whisper has 3+ consecutive words with no alignment in YouTube AND the YouTube source has audio coverage for the same time window (i.e., YouTube wasn't just missing captions there).

## Edge Cases

- **No YouTube captions exist.** Fall back to Whisper-only and surface a clear status message.
- **YouTube captions exist but are auto-generated and very rough.** The merge may introduce more noise than it removes. Add a confidence score per alignment block and skip merging blocks where YouTube confidence is below a threshold.
- **Speaker name appears spelled differently across the YouTube file** (rolling-caption typos baked into different frames). Use the most-frequent settled spelling, not the first occurrence.
- **Numbers and units.** Whisper tends to spell out numbers in some places and use digits in others. YouTube is more consistent with digits. Decide a policy (probably: prefer the form Whisper used unless YouTube clearly differs in value).
- **Brand and product names with intentional camelCase or unusual punctuation** (e.g., "iPhone", "ChatGPT"). YouTube is more likely to preserve these correctly.

## Future Web App Considerations

When this becomes a web app:

- The whisper.cpp step moves to a server-side worker. Consider offering both fast (small) and accurate (large-v3) Whisper tiers, since model size has a much larger impact on hallucination rate than on word-level accuracy.
- The proper noun vocabulary list could be enriched from a Wikidata or YouTube channel API lookup at job time.
- The "Review flagged segments" UI is a strong candidate for a shareable diff link, so editors can collaborate on cleanup before exporting the final transcript.

## Origin

The feature was proposed after manual side-by-side comparison of Whisper and YouTube outputs for a noisy-audio news clip (helicopter background). The Whisper output mistranscribed five proper nouns (correspondent names, a cabinet secretary, and a White House spokesperson), got "real fuel affordability" as "real field affordability," and hallucinated "in Beijing" into a quoted question. YouTube got every one of those right but was unusable as-is due to all-caps formatting and rolling-caption duplicates. A manual merge produced a transcript better than either source.
