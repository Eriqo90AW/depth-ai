//! Official Sherpa-ONNX CPU backend. No network access at runtime.
use crate::{
    config::Config,
    speakers::{
        normalize,
        worker::{Detection, Diarizer, LocalTurn},
    },
};
use anyhow::{Context, Result};
use sherpa_onnx::{
    OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig, SpeakerEmbeddingExtractor,
    SpeakerEmbeddingExtractorConfig,
};
use std::collections::BTreeMap;
pub struct LocalDiarizer {
    diarizer: OfflineSpeakerDiarization,
    embeddings: SpeakerEmbeddingExtractor,
}
impl LocalDiarizer {
    pub fn load(config: &Config) -> Result<Self> {
        let hint = "Install speaker models with python scripts/fetch_assets.py speakers.";
        let segmentation = config
            .resolve_model(
                &config.speaker_segmentation_model,
                "Speaker segmentation",
                hint,
            )
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let embedding = config
            .resolve_model(&config.speaker_embedding_model, "Speaker embedding", hint)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let embedding_config = SpeakerEmbeddingExtractorConfig {
            model: Some(embedding.to_string_lossy().into_owned()),
            num_threads: 1,
            ..Default::default()
        };
        let mut cfg = OfflineSpeakerDiarizationConfig {
            embedding: embedding_config.clone(),
            ..Default::default()
        };
        cfg.segmentation.pyannote.model = Some(segmentation.to_string_lossy().into_owned());
        cfg.segmentation.num_threads = 1;
        let diarizer = OfflineSpeakerDiarization::create(&cfg)
            .context("Could not load speaker segmentation")?;
        anyhow::ensure!(
            diarizer.sample_rate() == 16000,
            "Speaker models require 16 kHz audio"
        );
        let embeddings = SpeakerEmbeddingExtractor::create(&embedding_config)
            .context("Could not load speaker embeddings")?;
        Ok(Self {
            diarizer,
            embeddings,
        })
    }
}
impl Diarizer for LocalDiarizer {
    fn analyze(&mut self, samples: &[f32]) -> Result<Detection> {
        if samples.is_empty()
            || samples.iter().map(|v| v * v).sum::<f32>() / (samples.len() as f32) < 1e-8
        {
            return Ok(Detection::default());
        }
        // Segmentation expects its full ten-second model window, including at Stop.
        let mut padded = samples.to_vec();
        padded.resize(padded.len().max(160000), 0.0);
        let result = self
            .diarizer
            .process(&padded)
            .context("Speaker segmentation failed")?;
        let mut segments = result.sort_by_start_time();
        let duration = samples.len() as f32 / 16000.0;
        segments.retain(|s| {
            s.start.is_finite() && s.end.is_finite() && s.end > s.start && s.start < duration
        });
        let turns: Vec<_> = segments
            .iter()
            .map(|s| LocalTurn {
                start_ms: (s.start.max(0.0) * 1000.0) as u64,
                end_ms: (s.end.min(duration) * 1000.0) as u64,
                cluster: s.speaker,
            })
            .collect();
        // Extract voice profiles only from speech without simultaneous speakers.
        let mut speech: BTreeMap<i32, Vec<f32>> = BTreeMap::new();
        for (i, s) in segments.iter().enumerate() {
            let a = (s.start.max(0.0) * 16000.0) as usize;
            let b = ((s.end.min(duration) * 16000.0) as usize).min(samples.len());
            for at in (a..b).step_by(320) {
                let end = (at + 320).min(b);
                let overlap = segments.iter().enumerate().any(|(j, t)| {
                    j != i
                        && t.speaker != s.speaker
                        && t.start * 16000.0 < end as f32
                        && t.end * 16000.0 > at as f32
                });
                if !overlap {
                    let v = speech.entry(s.speaker).or_default();
                    if v.len() < 80000 {
                        v.extend_from_slice(&samples[at..end]);
                    }
                }
            }
        }
        let mut embeddings = BTreeMap::new();
        for (cluster, audio) in speech {
            if audio.len() < 16000 {
                continue;
            }
            let stream = self
                .embeddings
                .create_stream()
                .context("Could not create speaker embedding stream")?;
            stream.accept_waveform(16000, &audio);
            stream.input_finished();
            if self.embeddings.is_ready(&stream) {
                if let Some(mut embedding) = self.embeddings.compute(&stream) {
                    if normalize(&mut embedding) {
                        embeddings.insert(cluster, embedding);
                    }
                }
            }
        }
        Ok(Detection { turns, embeddings })
    }
}
