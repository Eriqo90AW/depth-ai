//! Timestamped segmentation of 16 kHz mono audio.
//!
//! The gate is a per-frame RMS threshold rather than a neural VAD: desktop loopback is digital
//! silence whenever nothing is playing, so an energy gate is both sufficient and free. Frames
//! are binary speech/silence, and a small state machine turns them into utterances.

/// A closed utterance: silence-trimmed audio ready for the engine.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub start_sample: u64,
    pub end_sample: u64,
}

impl Segment {
    pub fn duration_secs(&self) -> f32 {
        self.samples.len() as f32 / self.sample_rate as f32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// No utterance in progress; recent frames are kept as pre-roll so the onset is not clipped.
    Idle,
    /// Collecting an utterance.
    Speech,
}

/// Tunables derived from [`crate::config::Config`].
#[derive(Debug, Clone, Copy)]
pub struct SegmenterConfig {
    pub sample_rate: u32,
    pub frame_ms: u32,
    pub threshold_db: f32,
    pub speech_start_ms: u32,
    pub silence_close_ms: u32,
    pub min_segment_ms: u32,
    pub max_segment_secs: f32,
    pub tail_keep_ms: u32,
}

impl Default for SegmenterConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16_000,
            frame_ms: 20,
            threshold_db: -45.0,
            speech_start_ms: 300,
            silence_close_ms: 1000,
            min_segment_ms: 400,
            max_segment_secs: 25.0,
            tail_keep_ms: 200,
        }
    }
}

/// Turns a stream of fixed-size frames into utterances.
///
/// Feed whatever block sizes the capture layer produces to [`Segmenter::push`]; it re-blocks
/// internally. A returned [`Segment`] means one utterance is complete and ready to transcribe.
pub struct Segmenter {
    cfg: SegmenterConfig,
    frame_len: usize,
    open_frames: usize,
    close_frames: usize,
    min_frames: usize,
    max_frames: usize,
    tail_frames: usize,
    state: State,
    buf: Vec<f32>,
    /// Frames currently in `buf`, so callers never have to divide lengths.
    buf_frames: usize,
    voiced_run: usize,
    silent_run: usize,
    pending: Vec<f32>,
    processed_samples: u64,
}

impl Segmenter {
    pub fn new(cfg: SegmenterConfig) -> Self {
        let frame_len = ((cfg.sample_rate as u64 * cfg.frame_ms as u64) / 1000).max(1) as usize;
        let frames_for =
            |ms: u32| ((ms as u64 + cfg.frame_ms as u64 - 1) / cfg.frame_ms as u64).max(1) as usize;
        let tail_frames = (frames_for(cfg.tail_keep_ms)).min(frames_for(cfg.silence_close_ms));
        Self {
            frame_len,
            open_frames: frames_for(cfg.speech_start_ms),
            close_frames: frames_for(cfg.silence_close_ms),
            min_frames: frames_for(cfg.min_segment_ms),
            max_frames: frames_for((cfg.max_segment_secs * 1000.0) as u32),
            tail_frames,
            cfg,
            state: State::Idle,
            buf: Vec::with_capacity(frame_len * 512),
            buf_frames: 0,
            voiced_run: 0,
            silent_run: 0,
            pending: Vec::new(),
            processed_samples: 0,
        }
    }

    /// The frame size the segmenter works in, for callers that want to align their blocks.
    pub fn frame_len(&self) -> usize {
        self.frame_len
    }

    /// True while an utterance is being collected.
    pub fn in_speech(&self) -> bool {
        self.state == State::Speech
    }

    /// Read a draft snapshot without consuming the active utterance.
    pub fn snapshot(&self) -> Option<Segment> {
        if !self.in_speech() || self.buf_frames < self.min_frames {
            return None;
        }
        Some(Segment {
            samples: self.buf.clone(),
            sample_rate: self.cfg.sample_rate,
            start_sample: self.processed_samples.saturating_sub(self.buf.len() as u64),
            end_sample: self.processed_samples,
        })
    }

    /// Feed captured audio. Returns a segment for each utterance that completes.
    pub fn push(&mut self, audio: &[f32]) -> Vec<Segment> {
        let mut out = Vec::new();
        if self.pending.is_empty() && audio.len() < self.frame_len {
            self.pending.extend_from_slice(audio);
            return out;
        }
        let mut all: Vec<f32>;
        let input: &[f32] = if self.pending.is_empty() {
            audio
        } else {
            all = std::mem::take(&mut self.pending);
            all.extend_from_slice(audio);
            &all
        };

        let mut offset = 0;
        while offset + self.frame_len <= input.len() {
            let frame = &input[offset..offset + self.frame_len];
            if let Some(segment) = self.push_frame(frame) {
                out.push(segment);
            }
            offset += self.frame_len;
        }
        if offset < input.len() {
            self.pending.extend_from_slice(&input[offset..]);
        }
        out
    }

    /// Align blocks to the capture clock, including gaps when loopback stops sending silence.
    pub fn push_at(&mut self, audio: &[f32], start_sample: u64) -> Vec<Segment> {
        let accounted = self.processed_samples + self.pending.len() as u64;
        let gap = start_sample.saturating_sub(accounted);
        let mut result = Vec::new();
        // Ignore the small jitter from resampling blocks. Long gaps close speech with silence.
        if gap > (self.frame_len * 2) as u64 {
            let silence_len = gap.min((self.close_frames * self.frame_len + self.frame_len) as u64);
            result.extend(self.push(&vec![0.0; silence_len as usize]));
            // A short packet gap belongs to the current utterance. Resetting here
            // used to discard its audio and orphan its preview. Large gaps need
            // only enough silence to close speech before advancing the clock.
            if gap > silence_len {
                if let Some(segment) = self.flush() {
                    result.push(segment);
                }
                self.reset();
                self.processed_samples = start_sample;
            }
        }
        result.extend(self.push(audio));
        result
    }
    /// Close any utterance in progress, e.g. on pause or shutdown.
    pub fn flush(&mut self) -> Option<Segment> {
        let segment = self.take_segment(0);
        self.reset();
        segment
    }

    fn push_frame(&mut self, frame: &[f32]) -> Option<Segment> {
        self.processed_samples += frame.len() as u64;
        let voiced = rms_db(frame) > self.cfg.threshold_db;
        match self.state {
            State::Idle => {
                // Keep a rolling pre-roll so the utterance starts a little before the trigger.
                self.buf.extend_from_slice(frame);
                self.buf_frames += 1;
                while self.buf_frames > self.open_frames {
                    self.buf.drain(..self.frame_len);
                    self.buf_frames -= 1;
                }
                if voiced {
                    self.voiced_run += 1;
                } else {
                    self.voiced_run = 0;
                }
                if self.voiced_run >= self.open_frames {
                    self.state = State::Speech;
                    self.silent_run = 0;
                }
                None
            }
            State::Speech => {
                self.buf.extend_from_slice(frame);
                self.buf_frames += 1;
                if voiced {
                    self.silent_run = 0;
                } else {
                    self.silent_run += 1;
                }
                if self.buf_frames >= self.max_frames {
                    // Hard split: stay in Speech so the following audio is not lost.
                    let segment = self.take_segment(0);
                    self.buf.clear();
                    self.buf_frames = 0;
                    self.silent_run = 0;
                    return segment;
                }
                if self.silent_run >= self.close_frames {
                    let segment = self.take_segment(self.tail_frames);
                    self.reset();
                    return segment;
                }
                None
            }
        }
    }

    /// Slice `keep_tail_frames` of the trailing silence back off, then apply the minimum length.
    fn take_segment(&mut self, keep_tail_frames: usize) -> Option<Segment> {
        let drop_frames = self.silent_run.saturating_sub(keep_tail_frames);
        let keep_frames = self.buf_frames.saturating_sub(drop_frames);
        if keep_frames < self.min_frames {
            return None;
        }
        let keep_samples = keep_frames * self.frame_len;
        let samples = self.buf[..keep_samples.min(self.buf.len())].to_vec();
        Some(Segment {
            samples,
            sample_rate: self.cfg.sample_rate,
            start_sample: self
                .processed_samples
                .saturating_sub((self.buf_frames * self.frame_len) as u64),
            end_sample: self
                .processed_samples
                .saturating_sub((drop_frames * self.frame_len) as u64),
        })
    }

    fn reset(&mut self) {
        self.state = State::Idle;
        self.buf.clear();
        self.buf_frames = 0;
        self.voiced_run = 0;
        self.silent_run = 0;
        self.pending.clear();
    }
}

/// Level of one frame in dBFS; digital silence is negative infinity.
pub fn rms_db(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return f32::NEG_INFINITY;
    }
    let sum_sq: f32 = frame.iter().map(|s| s * s).sum();
    let rms = (sum_sq / frame.len() as f32).sqrt();
    if rms <= f32::EPSILON {
        f32::NEG_INFINITY
    } else {
        20.0 * rms.log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    fn cfg() -> SegmenterConfig {
        SegmenterConfig {
            sample_rate: RATE,
            ..Default::default()
        }
    }

    fn tone(ms: u32, amplitude: f32) -> Vec<f32> {
        let n = (RATE as u64 * ms as u64 / 1000) as usize;
        (0..n)
            .map(|i| amplitude * (i as f32 * 0.05).sin())
            .collect()
    }

    fn silence(ms: u32) -> Vec<f32> {
        vec![0.0; (RATE as u64 * ms as u64 / 1000) as usize]
    }

    #[test]
    fn one_utterance_between_silences() {
        let mut s = Segmenter::new(cfg());
        let mut segments = Vec::new();
        segments.extend(s.push(&silence(500)));
        segments.extend(s.push(&tone(2000, 0.5)));
        segments.extend(s.push(&silence(1500)));
        assert_eq!(segments.len(), 1, "expected exactly one utterance");
        let secs = segments[0].duration_secs();
        assert!(
            (1.7..=2.4).contains(&secs),
            "utterance should be about 2 s of speech plus a little tail, got {secs}"
        );
        assert_eq!(segments[0].sample_rate, RATE);
    }

    #[test]
    fn pure_silence_never_opens_an_utterance() {
        let mut s = Segmenter::new(cfg());
        let mut segments = Vec::new();
        for _ in 0..100 {
            segments.extend(s.push(&silence(100)));
        }
        assert!(segments.is_empty());
        assert!(!s.in_speech());
    }

    #[test]
    fn white_noise_below_threshold_is_ignored() {
        let mut s = Segmenter::new(cfg());
        // -60 dBFS noise: quiet enough that the gate stays shut.
        let noisy: Vec<f32> = (0..RATE as usize * 3)
            .map(|i| 0.001 * (i as f32 * 0.7).sin())
            .collect();
        assert!(s.push(&noisy).is_empty());
    }

    #[test]
    fn continuous_speech_is_split_at_the_cap() {
        let mut s = Segmenter::new(cfg());
        let mut segments = Vec::new();
        // 30 s of unbroken speech with a 25 s cap must yield a split.
        for _ in 0..300 {
            segments.extend(s.push(&tone(100, 0.5)));
        }
        assert_eq!(segments.len(), 1, "25 s cap should fire once in 30 s");
        assert!(segments[0].duration_secs() >= 24.0);
        assert!(s.in_speech(), "must keep collecting after a hard split");
        segments.extend(s.flush());
        assert_eq!(segments.len(), 2);
    }

    #[test]
    fn short_blips_are_dropped() {
        let mut s = Segmenter::new(cfg());
        let mut segments = Vec::new();
        segments.extend(s.push(&tone(100, 0.5)));
        segments.extend(s.push(&silence(1500)));
        assert!(segments.is_empty(), "a 100 ms blip is below min_segment_ms");
    }

    #[test]
    fn onset_is_not_clipped_thanks_to_preroll() {
        let mut s = Segmenter::new(cfg());
        let mut segments = Vec::new();
        segments.extend(s.push(&silence(200)));
        segments.extend(s.push(&tone(1000, 0.5)));
        segments.extend(s.push(&silence(1500)));
        assert_eq!(segments.len(), 1);
        // Pre-roll keeps 300 ms before the trigger, so the utterance is longer than the tone.
        assert!(
            segments[0].samples.len() > (RATE as usize),
            "preroll should extend the utterance past 1 s"
        );
    }

    #[test]
    fn arbitrary_block_sizes_are_reblocked() {
        let mut s = Segmenter::new(cfg());
        let audio: Vec<f32> = (0..RATE as usize * 2)
            .map(|i| 0.4 * (i as f32 * 0.05).sin())
            .collect();
        // Odd sizes that do not divide the frame length.
        let mut segments = Vec::new();
        for chunk in audio.chunks(333) {
            segments.extend(s.push(chunk));
        }
        segments.extend(s.push(&silence(1500)));
        assert_eq!(segments.len(), 1);
    }

    #[test]
    fn flush_closes_a_partial_utterance() {
        let mut s = Segmenter::new(cfg());
        assert!(s.push(&tone(1000, 0.5)).is_empty());
        assert!(s.in_speech());
        let closed = s.flush().expect("flush should close the utterance");
        assert!(closed.duration_secs() >= 0.9);
        assert!(!s.in_speech());
    }

    #[test]
    fn rms_of_digital_silence_is_negative_infinity() {
        assert_eq!(rms_db(&[0.0; 320]), f32::NEG_INFINITY);
        assert!(rms_db(&[0.5; 320]) > -10.0);
    }
}
#[cfg(test)]
mod clock_tests {
    use super::*;
    #[test]
    fn short_packet_gap_preserves_audio_and_the_draft_chunk_id() {
        let mut gate = Segmenter::new(SegmenterConfig::default());
        gate.push_at(&vec![0.5; 32000], 0);
        let draft = gate.snapshot().unwrap();
        assert!(gate.push_at(&vec![0.5; 32000], 32960).is_empty());
        let closed = gate.flush().unwrap();
        assert_eq!(closed.start_sample, draft.start_sample);
        assert_eq!(closed.end_sample, 64960);
        assert_eq!(closed.samples.iter().filter(|&&s| s == 0.5).count(), 64000);
    }
    #[test]
    fn live_snapshots_preserve_chunk_identity_and_six_second_continuity() {
        let mut gate = Segmenter::new(SegmenterConfig {
            max_segment_secs: 6.0,
            ..Default::default()
        });
        let mut closed = Vec::new();
        for second in 0..14 {
            closed.extend(gate.push_at(&vec![0.5; 16000], second * 16000));
            if second == 1 || second == 3 {
                let snapshot = gate.snapshot().unwrap();
                assert_eq!(snapshot.start_sample, 0);
                assert_eq!(snapshot.end_sample, (second + 1) * 16000);
                assert_eq!(snapshot.samples.len(), snapshot.end_sample as usize);
            }
        }
        closed.push(gate.flush().unwrap());
        assert_eq!(closed.len(), 3);
        assert_eq!(closed[0].start_sample, 0);
        assert_eq!(closed[0].end_sample, 96000);
        assert_eq!(closed[1].start_sample, closed[0].end_sample);
        assert_eq!(closed[2].start_sample, closed[1].end_sample);
        assert_eq!(closed[2].end_sample, 14 * 16000);
        assert_eq!(
            closed.iter().map(|s| s.samples.len()).sum::<usize>(),
            14 * 16000
        );
    }
    #[test]
    fn capture_gaps_advance_timestamps_and_close_speech() {
        let mut gate = Segmenter::new(SegmenterConfig::default());
        assert!(gate.push_at(&vec![0.5; 16000], 0).is_empty());
        let first = gate.push_at(&vec![0.5; 16000], 160000);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].start_sample, 0);
        let second = gate.flush().unwrap();
        assert_eq!(second.start_sample, 160000);
        assert_eq!(second.end_sample, 176000);
    }
}
