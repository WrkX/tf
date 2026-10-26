
use crate::cluster::{cosine, distance, mean};

const ITERATIONS: usize = 8;

/// teilt embeddings in zwei index-gruppen, die größere zuerst. gibt `None` zurück, wenn es weniger als zwei punkte gibt oder eine gruppe leer bleibt; dann sind die stimmen nicht unterscheidbar.
pub fn split_two(vectors: &[Vec<f32>]) -> Option<[Vec<usize>; 2]> {
    let (a, b) = farthest_pair(vectors)?;
    let mut centroids = [vectors[a].clone(), vectors[b].clone()];
    let mut groups = [Vec::new(), Vec::new()];
    for _ in 0..ITERATIONS {
        groups[0].clear();
        groups[1].clear();
        for (index, vector) in vectors.iter().enumerate() {
            let closer_to_second = distance(vector, &centroids[1]) < distance(vector, &centroids[0]);
            groups[usize::from(closer_to_second)].push(index);
        }
        for (slot, group) in groups.iter().enumerate() {
            let members = group.iter().map(|&index| vectors[index].as_slice()).collect::<Vec<_>>();
            if !members.is_empty() {
                centroids[slot] = mean(&members);
            }
        }
    }
    if groups[0].is_empty() || groups[1].is_empty() {
        return None;
    }
    groups.sort_by_key(|group| std::cmp::Reverse(group.len()));
    Some(groups)
}

fn farthest_pair(vectors: &[Vec<f32>]) -> Option<(usize, usize)> {
    let first = vectors.first()?;
    let far_from = |reference: &[f32], skip: Option<usize>| {
        vectors
            .iter()
            .enumerate()
            .filter(|(index, _)| Some(*index) != skip)
            .min_by(|a, b| cosine(reference, a.1).total_cmp(&cosine(reference, b.1)))
            .map(|(index, _)| index)
    };
    let a = far_from(first, None)?;
    let b = far_from(&vectors[a], Some(a))?;
    Some((a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_two_voices_and_puts_the_larger_first() {
        let vectors =
            vec![vec![1.0, 0.0], vec![0.0, 1.0], vec![0.98, 0.1], vec![0.99, 0.05], vec![0.05, 0.99]];
        let [first, second] = split_two(&vectors).unwrap();
        assert_eq!(first, vec![0, 2, 3]);
        assert_eq!(second, vec![1, 4]);
    }

    #[test]
    fn one_voice_cannot_be_split() {
        let vectors = vec![vec![1.0, 0.0]; 4];
        assert!(split_two(&vectors).is_none());
        assert!(split_two(&vectors[..1]).is_none());
    }
}
