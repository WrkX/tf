
use crate::cluster::distance;

const EXACT_LIMIT: usize = 8;

/// ordnet jeden neuen cluster einem früheren sprecher-index zu. sucht das matching mit der kleinsten gesamtdistanz; paare weiter als `threshold` werden nie gematcht, ein ungematchter cluster kostet `threshold`. möglichst viele cluster zu matchen gewinnt also gegen auslassen. ungematchte cluster sind `None`, damit der aufrufer einen neuen sprecher anlegt.
pub fn match_clusters(centroids: &[Vec<f32>], previous: &[Vec<f32>], threshold: f32) -> Vec<Option<usize>> {
    if previous.is_empty() || centroids.is_empty() {
        return vec![None; centroids.len()];
    }
    if centroids.len() <= EXACT_LIMIT && previous.len() <= EXACT_LIMIT {
        return exact(centroids, previous, threshold);
    }
    greedy(centroids, previous, threshold)
}

/// dynamische programmierung über die menge der schon belegten früheren sprecher.
fn exact(centroids: &[Vec<f32>], previous: &[Vec<f32>], threshold: f32) -> Vec<Option<usize>> {
    let states = 1usize << previous.len();
    let mut cost = vec![vec![f32::INFINITY; states]; centroids.len() + 1];
    let mut choice = vec![vec![None; states]; centroids.len()];
    cost[0][0] = 0.0;
    for (row, centroid) in centroids.iter().enumerate() {
        for taken in 0..states {
            let so_far = cost[row][taken];
            if !so_far.is_finite() {
                continue;
            }
            let mut relax = |next: usize, extra: f32, slot: Option<usize>| {
                if so_far + extra < cost[row + 1][next] {
                    cost[row + 1][next] = so_far + extra;
                    choice[row][next] = Some((taken, slot));
                }
            };
            relax(taken, threshold, None);
            for (slot, speaker) in previous.iter().enumerate() {
                let d = distance(centroid, speaker);
                if taken & (1 << slot) == 0 && d <= threshold {
                    relax(taken | (1 << slot), d, Some(slot));
                }
            }
        }
    }
    let Some(mut state) =
        (0..states).min_by(|a, b| cost[centroids.len()][*a].total_cmp(&cost[centroids.len()][*b]))
    else {
        return vec![None; centroids.len()];
    };
    let mut out = vec![None; centroids.len()];
    for row in (0..centroids.len()).rev() {
        let Some((before, slot)) = choice[row][state] else {
            break;
        };
        out[row] = slot;
        state = before;
    }
    out
}

/// fallback für sehr große eingaben: den nächsten freien sprecher nehmen.
fn greedy(centroids: &[Vec<f32>], previous: &[Vec<f32>], threshold: f32) -> Vec<Option<usize>> {
    let mut used = vec![false; previous.len()];
    centroids
        .iter()
        .map(|centroid| {
            let best = previous
                .iter()
                .enumerate()
                .filter(|(index, _)| !used[*index])
                .map(|(index, speaker)| (index, distance(centroid, speaker)))
                .min_by(|a, b| a.1.total_cmp(&b.1));
            match best {
                Some((index, distance)) if distance <= threshold => {
                    used[index] = true;
                    Some(index)
                }
                _ => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permuted_clusters_map_back_to_the_right_speakers() {
        let centroids = vec![vec![0.0, 1.0], vec![1.0, 0.0]];
        let previous = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
        assert_eq!(match_clusters(&centroids, &previous, 0.3), vec![Some(1), Some(0)]);
    }

    #[test]
    fn an_unmatched_cluster_becomes_new() {
        let centroids = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
        let previous = vec![vec![1.0, 0.0]];
        assert_eq!(match_clusters(&centroids, &previous, 0.3), vec![Some(0), None]);
    }

    #[test]
    fn more_clusters_than_speakers_keeps_the_closest_match() {
        let centroids = vec![vec![0.0, 1.0], vec![1.0, 0.0], vec![0.7, 0.7]];
        let previous = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
        assert_eq!(match_clusters(&centroids, &previous, 0.3), vec![Some(1), Some(0), None]);
    }

    #[test]
    fn fewer_clusters_than_speakers_still_match() {
        let centroids = vec![vec![0.0, 1.0]];
        let previous = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
        assert_eq!(match_clusters(&centroids, &previous, 0.3), vec![Some(1)]);
    }

    #[test]
    fn empty_previous_means_new_speakers() {
        assert_eq!(match_clusters(&[vec![1.0]], &[], 0.3), vec![None]);
    }
}
