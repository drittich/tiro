//! Voice activity detection: how likely each short frame of audio is to be speech.

use std::sync::OnceLock;

use ort::session::Session;
use ort::value::Tensor;

/// Scores fixed-size frames of 16 kHz mono audio, in order. Implementations may keep state between frames.
pub trait VoiceDetector: Send {
    /// Samples per frame.
    fn frame_len(&self) -> usize;
    /// The probability (0–1) that `frame` (exactly `frame_len` samples) is speech.
    fn speech_probability(&mut self, frame: &[i16]) -> f32;
}

/// Silero VAD v6 (MIT, github.com/snakers4/silero-vad), bundled into the binary.
const SILERO_MODEL: &[u8] = include_bytes!("../models/silero_vad.onnx");
const FRAME_LEN: usize = 512;
/// Silero expects each frame preceded by the last 64 samples of the previous one.
const CONTEXT_LEN: usize = 64;
const STATE_LEN: usize = 2 * 128;

#[derive(Debug, thiserror::Error)]
#[error("voice detection is unavailable: {0}")]
pub struct VadError(String);

/// Silero VAD at 16 kHz on ONNX Runtime. One per Track: it carries the model's recurrent state.
pub struct SileroVad {
    session: Session,
    state: Vec<f32>,
    input: Vec<f32>,
}

impl SileroVad {
    pub fn new() -> Result<Self, VadError> {
        static ENVIRONMENT: OnceLock<Result<ort::environment::Environment, String>> = OnceLock::new();
        let environment = ENVIRONMENT
            .get_or_init(|| ort::init().with_name("tiro").build().map_err(|e| e.to_string()))
            .as_ref()
            .map_err(|e| VadError(e.clone()))?;
        let session = Session::builder(environment)
            .and_then(|b| b.with_intra_threads(1).map_err(Into::into))
            .and_then(|mut b| b.commit_from_memory(SILERO_MODEL))
            .map_err(|e| VadError(e.to_string()))?;
        Ok(Self { session, state: vec![0.0; STATE_LEN], input: vec![0.0; CONTEXT_LEN + FRAME_LEN] })
    }

    fn run(&mut self) -> ort::Result<f32> {
        let outputs = self.session.run(ort::inputs![
            "input" => Tensor::from_array(([1usize, CONTEXT_LEN + FRAME_LEN], self.input.clone()))?,
            "state" => Tensor::from_array(([2usize, 1, 128], std::mem::take(&mut self.state)))?,
            "sr" => Tensor::from_array(([0usize; 0], vec![16_000i64]))?,
        ])?;
        self.state = outputs["stateN"].try_extract_tensor::<f32>()?.1.to_vec();
        Ok(outputs["output"].try_extract_tensor::<f32>()?.1[0])
    }
}

impl VoiceDetector for SileroVad {
    fn frame_len(&self) -> usize {
        FRAME_LEN
    }

    fn speech_probability(&mut self, frame: &[i16]) -> f32 {
        self.input.copy_within(FRAME_LEN.., 0);
        for (x, &s) in self.input[CONTEXT_LEN..].iter_mut().zip(frame) {
            *x = s as f32 / 32768.0;
        }
        match self.run() {
            Ok(p) => p,
            Err(e) => {
                // Treat a failed frame as speech so audio is never silently dropped; reset the recurrent state.
                tracing::warn!("voice detection failed on a frame: {e}");
                self.state = vec![0.0; STATE_LEN];
                1.0
            }
        }
    }
}
