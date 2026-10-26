use std::collections::HashMap;
use std::path::Path;

use crate::Error;

const WORD_BOUNDARY: char = '▁';

pub(crate) struct Vocab(Vec<String>);

impl Vocab {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let raw = std::fs::read(path).map_err(|_| Error::MissingModel(path.into()))?;
        let map: HashMap<String, String> =
            serde_json::from_slice(&raw).map_err(|e| Error::InvalidModel(format!("vocab: {e}")))?;
        let mut pieces = vec![String::new(); map.len()];
        for (id, piece) in map {
            let id: usize = id.parse().map_err(|_| Error::InvalidModel(format!("vocab id `{id}`")))?;
            *pieces
                .get_mut(id)
                .ok_or_else(|| Error::InvalidModel(format!("vocab id {id} out of range")))? = piece;
        }
        Ok(Self(pieces))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn decode(&self, tokens: &[u32]) -> String {
        let text: String = tokens
            .iter()
            .filter_map(|&t| self.0.get(t as usize))
            .filter(|p| !is_special(p))
            .flat_map(|p| p.chars())
            .map(|c| if c == WORD_BOUNDARY { ' ' } else { c })
            .collect();
        text.trim().to_owned()
    }
}

fn is_special(piece: &str) -> bool {
    piece.starts_with('<') && piece.ends_with('>')
}
