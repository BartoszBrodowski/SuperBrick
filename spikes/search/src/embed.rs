//! Text encoders: a model2vec static table (potion) and a small BERT (MiniLM) on candle.
//! Both return unit vectors, so cosine similarity is a dot product.

use std::{error::Error, path::Path};

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

type Res<T> = Result<T, Box<dyn Error + Send + Sync>>;

pub use ::search::embed::Encoder;

fn unit(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
    v.iter_mut().for_each(|x| *x /= n);
    v
}

/// model2vec: tokenize without special tokens, average the token rows, normalize.
pub struct Potion {
    name: String,
    tok: Tokenizer,
    table: Vec<f32>,
    rows: usize,
    dim: usize,
    unk: Option<u32>,
}

impl Potion {
    pub fn load(name: &str, dir: &Path) -> Res<Potion> {
        let mut tok = Tokenizer::from_file(dir.join("tokenizer.json"))?;
        tok.with_padding(None);
        tok.with_truncation(None)?;
        let tensors = candle_core::safetensors::load(dir.join("model.safetensors"), &Device::Cpu)?;
        let t = tensors.get("embeddings").ok_or("no `embeddings` tensor")?.to_dtype(DType::F32)?;
        let (rows, dim) = t.dims2()?;
        let table = t.flatten_all()?.to_vec1::<f32>()?;
        let unk = tok.token_to_id("[UNK]");
        Ok(Potion { name: name.to_string(), tok, table, rows, dim, unk })
    }
}

impl Encoder for Potion {
    fn name(&self) -> &str {
        &self.name
    }
    fn dim(&self) -> usize {
        self.dim
    }
    fn encode(&self, texts: &[String]) -> Res<Vec<Vec<f32>>> {
        let encs = self.tok.encode_batch(texts.to_vec(), false)?;
        Ok(encs
            .iter()
            .map(|e| {
                let mut v = vec![0f32; self.dim];
                let mut n = 0;
                for &id in e.get_ids() {
                    if Some(id) == self.unk || id as usize >= self.rows {
                        continue;
                    }
                    let row = &self.table[id as usize * self.dim..(id as usize + 1) * self.dim];
                    v.iter_mut().zip(row).for_each(|(a, b)| *a += b);
                    n += 1;
                }
                if n > 0 {
                    v.iter_mut().for_each(|a| *a /= n as f32);
                }
                unit(v)
            })
            .collect())
    }
}

/// sentence-transformers BERT: masked mean pooling over the last hidden state.
pub struct Bert {
    name: String,
    tok: Tokenizer,
    model: BertModel,
    dim: usize,
    dev: Device,
}

impl Bert {
    pub fn load(name: &str, dir: &Path) -> Res<Bert> {
        let dev = Device::Cpu;
        let cfg: Config = serde_json::from_str(&std::fs::read_to_string(dir.join("config.json"))?)?;
        let dim = serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(dir.join("config.json"))?)?["hidden_size"]
            .as_u64()
            .unwrap_or(384) as usize;
        // SAFETY: the weights file is not modified while mapped.
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[dir.join("model.safetensors")], DType::F32, &dev)? };
        let model = BertModel::load(vb, &cfg)?;
        let mut tok = Tokenizer::from_file(dir.join("tokenizer.json"))?;
        tok.with_padding(Some(PaddingParams { strategy: PaddingStrategy::BatchLongest, ..Default::default() }));
        tok.with_truncation(Some(TruncationParams { max_length: 128, ..Default::default() }))?;
        Ok(Bert { name: name.to_string(), tok, model, dim, dev })
    }
}

impl Encoder for Bert {
    fn name(&self) -> &str {
        &self.name
    }
    fn dim(&self) -> usize {
        self.dim
    }
    fn encode(&self, texts: &[String]) -> Res<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(64) {
            let encs = self.tok.encode_batch(chunk.to_vec(), true)?;
            let (b, t) = (encs.len(), encs[0].get_ids().len());
            let ids: Vec<u32> = encs.iter().flat_map(|e| e.get_ids().iter().copied()).collect();
            let mask: Vec<u32> = encs.iter().flat_map(|e| e.get_attention_mask().iter().copied()).collect();
            let ids = Tensor::from_vec(ids, (b, t), &self.dev)?;
            let mask = Tensor::from_vec(mask, (b, t), &self.dev)?;
            let types = ids.zeros_like()?;
            let h = self.model.forward(&ids, &types, Some(&mask))?;
            let m = mask.to_dtype(DType::F32)?.unsqueeze(2)?;
            let mean = h.broadcast_mul(&m)?.sum(1)?.broadcast_div(&m.sum(1)?)?;
            out.extend(mean.to_vec2::<f32>()?.into_iter().map(unit));
        }
        Ok(out)
    }
}

pub fn load(name: &str, models: &Path) -> Res<Box<dyn Encoder>> {
    let dir = models.join(name);
    if !dir.join("model.safetensors").exists() {
        return Err(format!("no model at {}", dir.display()).into());
    }
    let cfg = std::fs::read_to_string(dir.join("config.json")).unwrap_or_default();
    if cfg.contains("model2vec") || cfg.contains("StaticModel") {
        Ok(Box::new(Potion::load(name, &dir)?))
    } else {
        Ok(Box::new(Bert::load(name, &dir)?))
    }
}
