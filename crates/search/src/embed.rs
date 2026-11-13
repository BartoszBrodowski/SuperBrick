//! Production model2vec encoder. No BERT runtime or dynamic inference library is needed.
use std::{error::Error, path::Path};
use tokenizers::Tokenizer;
pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

pub trait Encoder: Send + Sync {
    fn name(&self) -> &str;
    fn dim(&self) -> usize;
    fn encode(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}

pub struct Potion {
    tok: Tokenizer,
    table: Vec<f32>,
    dim: usize,
    unk: Option<u32>,
}
impl Potion {
    pub fn load(dir: &Path) -> Result<Self> {
        let mut tok = Tokenizer::from_file(dir.join("tokenizer.json"))?;
        tok.with_padding(None);
        tok.with_truncation(None)?;
        let bytes = std::fs::read(dir.join("model.safetensors"))?;
        let data = safetensors::SafeTensors::deserialize(&bytes)?;
        let tensor = data.tensor("embeddings")?;
        if tensor.dtype() != safetensors::Dtype::F32 || tensor.shape().len() != 2 {
            return Err("expected a two-dimensional F32 embeddings table".into());
        }
        let dim = tensor.shape()[1];
        let table = tensor
            .data()
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        let unk = tok.token_to_id("[UNK]");
        Ok(Self {
            tok,
            table,
            dim,
            unk,
        })
    }
}
impl Encoder for Potion {
    fn name(&self) -> &str {
        "potion-base-8m"
    }
    fn dim(&self) -> usize {
        self.dim
    }
    fn encode(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.tok
            .encode_batch(texts.to_vec(), false)?
            .iter()
            .map(|tokens| {
                let mut v = vec![0.0f32; self.dim];
                for &id in tokens.get_ids() {
                    if Some(id) == self.unk {
                        continue;
                    }
                    if let Some(row) = self
                        .table
                        .get(id as usize * self.dim..(id as usize + 1) * self.dim)
                    {
                        for (a, b) in v.iter_mut().zip(row) {
                            *a += b;
                        }
                    }
                }
                let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
                v.iter_mut().for_each(|x| *x /= norm);
                Ok(v)
            })
            .collect()
    }
}
