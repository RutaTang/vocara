//! Lyrics: the panel's modes and draft, the text edits and imports, and
//! tap-to-time sync with its cursor.

use std::time::Duration;

use vocara_model::models::lyric_line::LyricLine;
use vocara_model::util::dart_string;
use vocara_model::util::lyrics_format::{
    align_lines_default, is_lrc_document, looks_like_lrc, lyrics_to_plain_text, parse_lrc,
    parse_plain_lyrics,
};

use super::jobs::{self, Failure, LyricsRead, Output};
use super::persist::{Pending, SaveKind, SaveThen};
use super::{Call, Core, Effect, JobKind, LyricsMode, Reply, SongPracticeDetailViewModel, Started};

/// What a lyrics write's caller does with its answer.
pub(super) enum LyricsThen {
    /// Nothing awaits it (a stamp, a clear).
    Nothing,
    /// A call awaits whether it landed.
    Reply(Call),
    /// A dropped file's import awaits its end.
    FileDone(Call),
}

/// A dropped lyrics file is read no further than this.
const LYRICS_FILE_CAP: u64 = 512 * 1024;

fn any_timed(lines: &[LyricLine]) -> bool {
    lines.iter().any(LyricLine::is_synced)
}

fn same_words(a: Option<&[LyricLine]>, b: &[LyricLine]) -> bool {
    let Some(a) = a else {
        return false;
    };
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.text == y.text)
}

impl Core {
    pub(super) fn lyrics(&self) -> &[LyricLine] {
        self.practice
            .as_deref()
            .map_or(&[], |p| p.lyrics.as_slice())
    }

    // Tap-to-time sync. The cursor indexes `sync_words`, the lyrics it was
    // placed on, and is read through `cursor`, which re-seeds it whenever the
    // words under it changed — however they changed (a peer's merged lines
    // left it at 5 of 2). Two rules keep it honest through failed writes,
    // with no per-path bookkeeping (each such patch opened the next
    // interleaving):
    //  * a failed write that reverts the lyrics to the disk's re-seeds the
    //    cursor on them, from the earliest line stamped since they were last
    //    saved (see `revert_to`): the stamps, the cleared or imported text it
    //    was placed on, are gone, and a cursor left there overwrote a good
    //    time or skipped a lost one;
    //  * it never rests past the last line while one is still untimed (Space
    //    did nothing).
    pub(super) fn cursor(&self) -> usize {
        let now = self.lyrics();
        let changed = self.sync_words.borrow().as_deref() != Some(now);
        if changed {
            let was = self.sync_words.borrow().clone();
            if !same_words(was.as_deref(), now) {
                self.sync_index.set(self.first_unsynced_index());
            } else if was.as_deref().is_some_and(any_timed) && !any_timed(now) {
                // Every time gone from the same words (a peer's clear): start
                // over on the first words, as a clear here does. Other new
                // times (a stamp, a peer's) leave it where it is.
                self.sync_index.set(self.words_from(0));
            }
            *self.sync_words.borrow_mut() = Some(now.to_vec());
        }
        if self.words_from(self.sync_index.get()) >= now.len() {
            if let Some(untimed) = self.first_untimed_from(0) {
                self.sync_index.set(untimed);
            }
        }
        self.sync_index.get()
    }

    pub(super) fn place_cursor(&self, index: usize) {
        self.sync_index.set(index);
        *self.sync_words.borrow_mut() = Some(self.lyrics().to_vec());
    }

    /// The first line from `from` on that carries words and no time; `None`
    /// when none does.
    pub(super) fn first_untimed_from(&self, from: usize) -> Option<usize> {
        let lyrics = self.lyrics();
        (from..lyrics.len()).find(|&i| !lyrics[i].is_blank() && !lyrics[i].is_synced())
    }

    fn first_unsynced_index(&self) -> usize {
        // Every line is timed: start over from the first WORDS, not line 0 —
        // pasted lyrics often open on a blank line, and stamping it shifted
        // every time one line late.
        self.first_untimed_from(0)
            .unwrap_or_else(|| self.words_from(0))
    }

    /// The first line from `from` on that carries words (a blank line takes
    /// no time); the line count when none does.
    pub(super) fn words_from(&self, from: usize) -> usize {
        let lyrics = self.lyrics();
        let mut i = from;
        while i < lyrics.len() && lyrics[i].is_blank() {
            i += 1;
        }
        i
    }
}

impl SongPracticeDetailViewModel {
    pub fn lyrics(&self) -> &[LyricLine] {
        self.c.lyrics()
    }

    pub fn has_lyrics(&self) -> bool {
        !self.c.lyrics().is_empty()
    }

    pub fn lyrics_mode(&self) -> LyricsMode {
        self.c.lyrics_mode
    }

    /// Bumped each time the lyrics editor opens, so a Save whose write lands
    /// late closes only the editing session it was tapped in — not one the
    /// user opened again in the meantime.
    pub fn lyrics_edit_session(&self) -> u64 {
        self.c.lyrics_edit_session
    }

    pub fn set_lyrics_mode(&mut self, mode: LyricsMode) {
        if self.c.lyrics_mode == mode {
            return;
        }
        self.c.lyrics_mode = mode;
        if mode == LyricsMode::Edit {
            self.c.lyrics_edit_session += 1;
        }
        if mode != LyricsMode::Edit {
            self.c.lyrics_draft = None;
        }
        if mode == LyricsMode::Sync {
            let at = self.c.first_unsynced_index();
            self.c.place_cursor(at);
        }
        self.c.notify();
    }

    /// The open lyrics editor's unsaved text. Held here, not in the editor:
    /// a switch to another right-panel tab or a collapse of the panel
    /// disposes the editor, which then reopened on the saved lyrics and the
    /// edit was gone. Memory only — the editor's Save is the one thing that
    /// applies it — and dropped when the editor closes (Save, Cancel) or the
    /// screen does.
    pub fn lyrics_draft(&self) -> Option<&str> {
        self.c.lyrics_draft.as_deref()
    }

    pub fn set_lyrics_draft(&mut self, draft: Option<String>) {
        self.c.lyrics_draft = draft;
    }

    pub fn lyrics_focus(&self) -> bool {
        self.c.lyrics_focus
    }

    pub fn set_lyrics_focus(&mut self, on: bool) {
        if self.c.lyrics_focus == on {
            return;
        }
        self.c.lyrics_focus = on;
        self.c.notify();
    }

    /// Index of the line highlighted at `position_ms` (the synced line with
    /// the LATEST time at or before the playhead), or `None`. Picking by max
    /// time rather than highest index keeps the karaoke highlight correct
    /// even if the user stamped lines out of time order (the times need not
    /// be monotonic with the line order).
    pub fn current_lyric_index(&self, position_ms: i64) -> Option<usize> {
        let mut idx = None;
        let mut best = -1;
        for (i, line) in self.c.lyrics().iter().enumerate() {
            if let Some(t) = line.time_ms {
                if t <= position_ms && t >= best {
                    best = t;
                    idx = Some(i);
                }
            }
        }
        idx
    }

    pub fn seek_to_lyric(&mut self, line: &LyricLine) {
        if let Some(t) = line.time_ms {
            self.seek(Duration::from_millis(u64::try_from(t).unwrap_or(0)));
        }
    }

    pub fn lyrics_as_text(&self) -> String {
        lyrics_to_plain_text(self.c.lyrics())
    }

    /// The tap-to-time cursor.
    pub fn sync_index(&self) -> usize {
        self.c.cursor()
    }

    /// Replaces lyrics from edited plain text, keeping existing line times
    /// by MATCHING TEXT (not position): inserting, deleting, or reordering a
    /// line in the editor must not pin an old timestamp onto different words
    /// — or onto a blank line. Answers whether the write landed (the editor
    /// keeps the draft if not).
    pub fn set_lyrics_text(&mut self, text: &str) -> Started {
        let call = self.next_call();
        self.set_lyrics_text_inner(text, LyricsThen::Reply(call));
        self.started(call)
    }

    fn set_lyrics_text_inner(&mut self, text: &str, then: LyricsThen) {
        let prev = self.c.lyrics().to_vec();
        // Saved unchanged: nothing to write. The plain text cannot carry a
        // time on a blank line (an LRC end-of-line or instrumental marker),
        // and the round trip dropped every one, leaving the line before it
        // lit through the gap. Only with nothing pending, though: unchanged
        // from a write still queued (a second Save) said "saved" at once, the
        // editor closed, and that write then failed and reverted the text
        // with the draft gone.
        if self.c.unsaved_from.is_none() && text == lyrics_to_plain_text(&prev) {
            return self.lyrics_saved(then, Ok(true));
        }
        let parsed = parse_plain_lyrics(text);
        // A line keeps its time only from the line it continues, in order:
        // matched on text alone and handed out first come first served, a
        // fixed typo in the first chorus gave the second chorus the first
        // one's time.
        let prev_texts: Vec<&str> = prev.iter().map(|l| dart_string::trim(&l.text)).collect();
        let next_texts: Vec<&str> = parsed.iter().map(|l| dart_string::trim(&l.text)).collect();
        let from = align_lines_default(&prev_texts, &next_texts);
        let mut next: Vec<LyricLine> = parsed
            .iter()
            .enumerate()
            .map(|(i, line)| match from.get(i).copied().flatten() {
                Some(j) if prev[j].time_ms.is_some() => LyricLine {
                    time_ms: prev[j].time_ms,
                    ..line.clone()
                },
                _ => line.clone(), // a new line carries no time
            })
            .collect();
        // The timed blank lines that closed the old last line (its end
        // marker) still close it, when it is still the last.
        if !parsed.is_empty() {
            if let Some(last) = from.get(parsed.len() - 1).copied().flatten() {
                let tail = &prev[last + 1..];
                if tail.iter().all(LyricLine::is_blank) {
                    next.extend(tail.iter().filter(|l| l.is_synced()).cloned());
                }
            }
        }
        self.replace_lyrics(next, then);
    }

    /// Imports pasted/dropped lyrics — `.lrc` if it has time tags, else
    /// plain. Answers whether the write landed.
    pub fn import_lyrics(&mut self, content: &str) -> Started {
        let call = self.next_call();
        self.import_lyrics_inner(content, LyricsThen::Reply(call));
        self.started(call)
    }

    fn import_lyrics_inner(&mut self, content: &str, then: LyricsThen) {
        let lines = if looks_like_lrc(content) {
            match parse_lrc(content) {
                Ok(lines) => lines,
                Err(e) => {
                    let message = match e {
                        vocara_model::Error::Format { message, .. } => message,
                        other => other.to_string(),
                    };
                    self.c.toast(&message);
                    return self.lyrics_saved(then, Ok(false));
                }
            }
        } else {
            parse_plain_lyrics(content)
        };
        self.import_lyric_lines(lines, then);
    }

    fn import_lyric_lines(&mut self, lines: Vec<LyricLine>, then: LyricsThen) {
        if lines.iter().all(LyricLine::is_blank) {
            self.c.toast("That file didn't contain any lyrics.");
            return self.lyrics_saved(then, Ok(false));
        }
        self.replace_lyrics(lines, then);
    }

    /// Persists a NEW text (an import, an edit), shown at once, and re-seeds
    /// the sync cursor on it. Forced, not left to the cursor's words check:
    /// the same words re-imported without their times are a new text too,
    /// and the cursor left at line d stamped it while a–c had lost theirs.
    /// (Should the write fail, the revert re-seeds on the disk's text: see
    /// `revert_to`.)
    fn replace_lyrics(&mut self, lines: Vec<LyricLine>, then: LyricsThen) {
        let enqueued = self.set_lyrics(lines, then);
        *self.c.sync_words.borrow_mut() = None;
        self.c.unsaved_from = Some(0);
        self.c.notify();
        if let Some(then) = enqueued {
            self.lyrics_saved(then, Ok(false));
        }
    }

    /// Imports lyrics from a DROPPED file: validates the type + size, reads,
    /// and replaces the lyrics — with feedback on every failure path so the
    /// drop never silently does nothing, OOMs on a huge file, or overwrites
    /// the lyrics with the contents of an unrelated file.
    pub fn import_lyrics_from_file(&mut self, path: &str) -> Started {
        let call = self.next_call();
        // No take or edit guard, as for pasted lyrics: the text goes onto the
        // live model and its write queues behind any other. Refused, the same
        // lines dropped mid-take were turned away while pasted ones were
        // taken.
        let ext = path
            .rfind('.')
            .map_or(String::new(), |dot| path[dot..].to_lowercase());
        if !matches!(ext.as_str(), ".lrc" | ".txt" | ".text") {
            self.c.toast("Drop a .lrc or .txt lyrics file.");
            self.c.reply(Reply::Done(call));
            return self.started(call);
        }
        let path = path.to_string();
        self.c.job(
            JobKind::ReadLyrics,
            Pending::LyricsFile {
                call,
                lrc: ext == ".lrc",
            },
            move || Output::Lyrics(jobs::read_lyrics_file(&path, LYRICS_FILE_CAP)),
        );
        self.started(call)
    }

    pub(super) fn lyrics_file_read(&mut self, call: Call, lrc: bool, output: Output) {
        match output {
            Output::Lyrics(LyricsRead::Text(text)) => {
                // A .lrc file is LRC wherever its time tags are; a text file
                // only when every line is timed (see is_lrc_document):
                // parse_lrc drops untimed lines, so one "Guitar solo [2:30]"
                // kept that line of a .txt alone.
                if lrc || is_lrc_document(&text) {
                    self.import_lyrics_inner(&text, LyricsThen::FileDone(call));
                } else {
                    self.import_lyric_lines(parse_plain_lyrics(&text), LyricsThen::FileDone(call));
                }
            }
            Output::Lyrics(LyricsRead::TooLarge) => {
                self.c.toast("That file is too large to be lyrics.");
                self.c.reply(Reply::Done(call));
            }
            _ => {
                self.c.toast("Couldn't read that file as lyrics.");
                self.c.reply(Reply::Done(call));
            }
        }
    }

    /// Applies text saved from the editor: parsed as `.lrc` when the WHOLE
    /// text reads as an LRC document (so pasting LRC just works), otherwise
    /// as plain text (keeping times). The whole-document test matters here
    /// and not on the import path: `parse_lrc` drops untimed lines by
    /// design, so routing on a single stray `[04:33]` would delete every
    /// other line the user typed. Answers whether it persisted — a failed
    /// write reverts the model to what's on disk, so the editor must stay
    /// open on false or the draft is lost.
    pub fn apply_edited_lyrics(&mut self, text: &str) -> Started {
        let call = self.next_call();
        if is_lrc_document(text) {
            self.import_lyrics_inner(text, LyricsThen::Reply(call));
        } else {
            self.set_lyrics_text_inner(text, LyricsThen::Reply(call));
        }
        self.started(call)
    }

    /// Stamps the next line at the playhead, then advances past blank lines.
    #[must_use]
    pub fn stamp_next_lyric(&mut self) -> Vec<Effect> {
        let at = self.c.cursor();
        // A cursor on a blank line stamps the words it stands before.
        let index = self.c.words_from(at);
        self.stamp_and_advance(index);
        self.finish()
    }

    /// Stamps a specific line at the current playhead (a re-time, or a line
    /// the taps passed), then carries on from the line after it, as a tap
    /// would: parked on the clicked line, the next Space re-stamped it with
    /// the following line's time.
    #[must_use]
    pub fn stamp_lyric_at(&mut self, index: usize) -> Vec<Effect> {
        if index < self.c.lyrics().len() {
            self.stamp_and_advance(index);
        }
        self.finish()
    }

    fn stamp_and_advance(&mut self, index: usize) {
        if index >= self.c.lyrics().len() {
            return;
        }
        // Advance the cursor BEFORE awaiting the write. Two taps land faster
        // than one save round-trip, and computing the advance afterwards
        // reads a cursor the other tap already moved: both would stamp this
        // line and the cursor would jump a line past them, leaving it
        // unstamped. A write that fails reverts the lyrics and re-seeds the
        // cursor (see revert_to): back on this line, or on the earliest stamp
        // of a run that write took with it.
        self.c.unsaved_from = Some(self.c.unsaved_from.unwrap_or(index).min(index));
        let next = self.c.words_from(index + 1);
        self.c.place_cursor(next);
        self.c.notify();
        self.stamp_at(index);
    }

    /// Writes the playhead onto `index` WITHOUT touching the sync cursor —
    /// its callers own the cursor.
    fn stamp_at(&mut self, index: usize) {
        let at = i64::try_from(self.playback.position().as_millis()).unwrap_or(i64::MAX);
        let lines: Vec<LyricLine> = self
            .c
            .lyrics()
            .iter()
            .enumerate()
            .map(|(i, l)| {
                if i == index {
                    LyricLine {
                        time_ms: Some(at),
                        ..l.clone()
                    }
                } else {
                    l.clone()
                }
            })
            .collect();
        if let Some(then) = self.set_lyrics(lines, LyricsThen::Nothing) {
            self.lyrics_saved(then, Ok(false));
        }
    }

    #[must_use]
    pub fn clear_lyric_times(&mut self) -> Vec<Effect> {
        // Every line is untimed now: the cursor starts on the first words.
        let at = self.c.words_from(0);
        self.c.place_cursor(at);
        self.c.unsaved_from = Some(0);
        let lines: Vec<LyricLine> = self
            .c
            .lyrics()
            .iter()
            .map(|l| LyricLine {
                time_ms: None,
                ..l.clone()
            })
            .collect();
        if let Some(then) = self.set_lyrics(lines, LyricsThen::Nothing) {
            self.lyrics_saved(then, Ok(false));
        }
        self.finish()
    }

    /// Persists a lyrics change without re-syncing the audio engine (lyrics
    /// don't affect playback), so tap-to-time while playing doesn't glitch
    /// the audio — unless the save merged a concurrent change. Runs on the
    /// same save chain as every save. Hands `then` back when there is no
    /// practice to write to (the caller answers it: not landed).
    fn set_lyrics(&mut self, lines: Vec<LyricLine>, then: LyricsThen) -> Option<LyricsThen> {
        let Some(practice) = self.c.practice.as_deref() else {
            return Some(then);
        };
        let next = vocara_model::models::song_practice::SongPractice {
            lyrics: lines,
            ..practice.clone()
        };
        self.c.set_practice(next);
        // Landed only if no revert came first. A queued write sends the model
        // as it is when it runs, so a save failing ahead of it sent these
        // lines too, and its revert threw them away: the write then saved the
        // reverted model and said it landed, and the editor closed with its
        // draft gone. (A false "not landed" only keeps the editor open: an
        // unchanged Save closes it.)
        let reverts = self.c.reverts;
        self.enqueue_save(SaveKind::Lyrics { reverts }, SaveThen::Lyrics(then));
        None
    }

    pub(super) fn lyrics_saved(&mut self, then: LyricsThen, result: Result<bool, Failure>) {
        let landed = result.unwrap_or(false);
        match then {
            LyricsThen::Nothing => {}
            LyricsThen::Reply(call) => self.c.reply(Reply::LyricsSaved { call, landed }),
            LyricsThen::FileDone(call) => self.c.reply(Reply::Done(call)),
        }
    }
}
