//! Ephemeral capture health and revisable text. Never serialized or exported.
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct Draft {
    pub revision: u64,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct LiveState {
    pub recording_id: String,
    pub drafts: BTreeMap<u64, Draft>,
    retired: BTreeSet<u64>,
    pub source: String,
    pub capture_error: Option<String>,
    pub notice: String,
    pub capture_notice: String,
    pub packets: u64,
    pub peak_db: f32,
    pub speech: bool,
    pub queued: usize,
    started: Instant,
    last_signal: Option<Instant>,
}

impl Default for LiveState {
    fn default() -> Self {
        Self {
            recording_id: String::new(),
            drafts: BTreeMap::new(),
            retired: BTreeSet::new(),
            source: String::new(),
            capture_error: None,
            notice: String::new(),
            capture_notice: String::new(),
            packets: 0,
            peak_db: f32::NEG_INFINITY,
            speech: false,
            queued: 0,
            started: Instant::now(),
            last_signal: None,
        }
    }
}

impl LiveState {
    pub fn begin(&mut self, id: String) {
        *self = Self {
            recording_id: id,
            ..Self::default()
        };
    }
    pub fn update_draft(&mut self, chunk: u64, revision: u64, text: String) {
        if self.retired.contains(&chunk)
            || self
                .drafts
                .get(&chunk)
                .is_some_and(|d| d.revision > revision)
        {
            return;
        }
        self.drafts.insert(chunk, Draft { revision, text });
    }
    pub fn update_draft_for(
        &mut self,
        recording_id: &str,
        chunk: u64,
        revision: u64,
        text: String,
    ) {
        if self.recording_id == recording_id {
            self.update_draft(chunk, revision, text);
        }
    }
    pub fn finish_chunk(&mut self, chunk: u64) {
        self.retired.insert(chunk);
        self.drafts.remove(&chunk);
    }
    pub fn chunk_finished(&self, chunk: u64) -> bool {
        self.retired.contains(&chunk)
    }
    pub fn finish(&mut self) {
        self.drafts.clear();
        self.queued = 0;
    }
    pub fn draft_text(&self) -> String {
        self.drafts
            .values()
            .map(|d| d.text.as_str())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
    pub fn level(&mut self, packets: u64, peak_db: f32) {
        self.packets = packets;
        self.peak_db = peak_db;
        if peak_db > -60.0 {
            self.last_signal = Some(Instant::now());
        }
    }
    pub fn waiting_for_audio(&self) -> bool {
        self.last_signal.unwrap_or(self.started).elapsed() >= Duration::from_secs(5)
    }
    pub fn guidance(&self) -> String {
        self.guidance_for(true)
    }
    pub fn guidance_for(&self, capturing: bool) -> String {
        if let Some(error) = &self.capture_error {
            return error.clone();
        }
        let mut messages = Vec::new();
        if capturing && self.waiting_for_audio() {
            messages.push("Waiting for desktop audio. Check browser playback, the player and tab mute settings, and Windows per-app output routing.".to_string());
        }
        if !self.notice.is_empty() {
            messages.push(self.notice.clone());
        }
        if !self.capture_notice.is_empty() {
            messages.push(self.capture_notice.clone());
        }
        messages.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn revisions_replace_drafts_and_final_text_wins() {
        let mut s = LiveState::default();
        s.update_draft(0, 2, "Halo dunia".into());
        s.update_draft(0, 1, "Halo".into());
        assert_eq!(s.draft_text(), "Halo dunia");
        s.finish_chunk(0);
        s.update_draft(0, 3, "Late result".into());
        assert!(s.draft_text().is_empty());
    }
    #[test]
    fn pending_chunks_remain_ordered_and_recordings_are_isolated() {
        let mut s = LiveState::default();
        s.begin("first".into());
        s.update_draft(96000, 1, "Kedua".into());
        s.update_draft(0, 1, "Pertama".into());
        assert_eq!(s.draft_text(), "Pertama\n\nKedua");
        s.finish_chunk(0);
        assert_eq!(s.draft_text(), "Kedua");
        s.begin("second".into());
        assert!(s.drafts.is_empty());
        s.update_draft_for(
            "first",
            0,
            9,
            "Late result from the previous recording".into(),
        );
        assert!(s.drafts.is_empty());
        s.update_draft(0, 1, "Baru".into());
        assert_eq!(s.draft_text(), "Baru");
    }
    #[test]
    fn silence_has_guidance_and_signal_clears_it() {
        let mut s = LiveState::default();
        s.started = Instant::now() - Duration::from_secs(6);
        assert!(s.waiting_for_audio());
        s.level(3, -20.0);
        assert!(!s.waiting_for_audio());
        s.capture_error = Some("Selected output is disconnected".into());
        assert!(s.guidance().contains("disconnected"));
    }
}
