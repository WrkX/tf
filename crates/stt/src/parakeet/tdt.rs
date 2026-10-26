use crate::Error;
use tf_coreml::{Model, Tensor};

pub(crate) const ENCODER_DIM: usize = 1024;
const DECODER_DIM: usize = 640;
const DECODER_LAYERS: usize = 2;
const MAX_SYMBOLS_PER_FRAME: usize = 10;

/// hält den zustand des prediction-network über encoder-fenster hinweg, damit eine lange aufnahme als eine durchgehende sequenz dekodiert wird.
pub(crate) struct TdtDecoder<'m> {
    decoder: &'m Model,
    joint: &'m Model,
    blank: u32,
    hidden: Tensor,
    cell: Tensor,
    prediction: Tensor,
    encoder_step: Tensor,
    target: Tensor,
    target_len: Tensor,
    frame: usize,
    emitted_on_frame: usize,
    tokens: Vec<u32>,
}

impl<'m> TdtDecoder<'m> {
    pub fn new(decoder: &'m Model, joint: &'m Model, blank: u32) -> Result<Self, Error> {
        let state = [DECODER_LAYERS, 1, DECODER_DIM];
        let mut this = Self {
            decoder,
            joint,
            blank,
            hidden: Tensor::zeros_f32(&state)?,
            cell: Tensor::zeros_f32(&state)?,
            prediction: Tensor::zeros_f32(&[1, DECODER_DIM, 1])?,
            encoder_step: Tensor::zeros_f32(&[1, ENCODER_DIM, 1])?,
            target: Tensor::i32(&[1, 1], 0)?,
            target_len: Tensor::i32(&[1], 1)?,
            frame: 0,
            emitted_on_frame: 0,
            tokens: Vec::new(),
        };
        this.advance_prediction(blank)?;
        Ok(this)
    }

    pub fn decode(&mut self, frames: &[f32], offset: usize, end: usize) -> Result<(), Error> {
        let available = offset + frames.len() / ENCODER_DIM;
        let end = end.min(available);
        while self.frame < end {
            let local = self.frame - offset;
            self.encoder_step.write_f32(&frames[local * ENCODER_DIM..(local + 1) * ENCODER_DIM]);
            let (token, duration) = self.joint_step()?;

            if token == self.blank {
                self.move_forward(duration.max(1));
                continue;
            }

            self.tokens.push(token);
            self.advance_prediction(token)?;
            self.emitted_on_frame += 1;

            if duration > 0 {
                self.move_forward(duration);
            } else if self.emitted_on_frame >= MAX_SYMBOLS_PER_FRAME {
                self.move_forward(1);
            }
        }
        Ok(())
    }

    pub fn into_tokens(self) -> Vec<u32> {
        self.tokens
    }

    fn move_forward(&mut self, frames: usize) {
        self.frame += frames;
        self.emitted_on_frame = 0;
    }

    fn joint_step(&self) -> Result<(u32, usize), Error> {
        let out = self
            .joint
            .predict(&[("encoder_step", &self.encoder_step), ("decoder_step", &self.prediction)])?;
        let token = out.get("token_id")?.first_i32();
        let duration = out.get("duration")?.first_i32();
        Ok((token.max(0) as u32, duration.max(0) as usize))
    }

    fn advance_prediction(&mut self, token: u32) -> Result<(), Error> {
        self.target.write_i32(&[token as i32]);
        let out = self.decoder.predict(&[
            ("targets", &self.target),
            ("target_length", &self.target_len),
            ("h_in", &self.hidden),
            ("c_in", &self.cell),
        ])?;
        self.prediction.copy_from(&out.get("decoder")?);
        self.hidden.copy_from(&out.get("h_out")?);
        self.cell.copy_from(&out.get("c_out")?);
        Ok(())
    }
}
