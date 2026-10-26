//! agglomeratives clustering mit centroid-linkage und cannot-link-randbedingungen.

use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct Embedding {
    /// index des 10-s-fensters (für cannot-link-paare).
    pub window: usize,
    pub local: usize,
    pub start_ms: u64,
    pub vector: Vec<f32>,
    pub duration_ms: u64,
}

pub fn normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for value in vector {
            *value /= norm;
        }
    }
}

pub fn cosine(left: &[f32], right: &[f32]) -> f32 {
    let mut dot = 0.0;
    let mut left_norm = 0.0;
    let mut right_norm = 0.0;
    for (a, b) in left.iter().zip(right) {
        dot += a * b;
        left_norm += a * a;
        right_norm += b * b;
    }
    if left_norm <= f32::EPSILON || right_norm <= f32::EPSILON {
        return 0.0;
    }
    dot / (left_norm.sqrt() * right_norm.sqrt())
}

pub fn distance(left: &[f32], right: &[f32]) -> f32 {
    1.0 - cosine(left, right)
}

pub fn mean(vectors: &[&[f32]]) -> Vec<f32> {
    let Some(first) = vectors.first() else {
        return Vec::new();
    };
    let mut out = vec![0.0; first.len()];
    for vector in vectors {
        for (slot, value) in out.iter_mut().zip(*vector) {
            *slot += value;
        }
    }
    let count = vectors.len() as f32;
    for value in &mut out {
        *value /= count;
    }
    normalize(&mut out);
    out
}

pub fn centroids(items: &[Embedding], assignment: &[usize]) -> Vec<Vec<f32>> {
    let count = assignment.iter().copied().max().map(|id| id + 1).unwrap_or(0);
    let mut groups = vec![Vec::new(); count];
    for (item, &cluster) in items.iter().zip(assignment) {
        if let Some(group) = groups.get_mut(cluster) {
            group.push(item.vector.as_slice());
        }
    }
    groups.iter().map(|group| mean(group)).collect()
}

pub fn nearest(vector: &[f32], centroids: &[Vec<f32>]) -> Option<usize> {
    centroids
        .iter()
        .enumerate()
        .map(|(index, centroid)| (index, distance(vector, centroid)))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(index, _)| index)
}

struct Cluster {
    members: Vec<usize>,
    /// fenster -> lokaler sprecher, für die cannot-link-randbedingung.
    seen: HashMap<usize, usize>,
    centroid: Vec<f32>,
    active: bool,
    duration_ms: u64,
}

pub fn cluster(items: &[Embedding], threshold: f32, max_speakers: usize) -> Vec<usize> {
    if items.is_empty() {
        return Vec::new();
    }
    let mut clusters: Vec<Cluster> = items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let mut seen = HashMap::new();
            seen.insert(item.window, item.local);
            Cluster {
                members: vec![index],
                seen,
                centroid: item.vector.clone(),
                active: true,
                duration_ms: item.duration_ms,
            }
        })
        .collect();

    // nearest[i] ist der nächste aktive cluster und die aktuelle cosine-distanz.
    let mut nearest: Vec<Option<(usize, f32)>> =
        (0..clusters.len()).map(|index| nearest_for(index, &clusters)).collect();

    loop {
        let mut best: Option<(usize, usize, f32)> = None;
        for i in 0..clusters.len() {
            if !clusters[i].active {
                continue;
            }
            if let Some((j, d)) = nearest[i]
                && d <= threshold
                && best.map(|(_, _, current)| d < current).unwrap_or(true)
            {
                best = Some((i, j, d));
            }
        }
        let Some((i, j, _)) = best else {
            break;
        };
        merge(i, j, &mut clusters);
        // nur abstände zum verschmolzenen cluster haben sich geändert.
        for (k, slot) in nearest.iter_mut().enumerate() {
            if k == i || !clusters[k].active {
                continue;
            }
            if slot.map(|(n, _)| n == i || n == j).unwrap_or(false) {
                *slot = nearest_for(k, &clusters);
            } else if let Some((_, current)) = *slot {
                if blocked(&clusters[k], &clusters[i]) {
                    continue;
                }
                let d = distance(&clusters[k].centroid, &clusters[i].centroid);
                if d < current {
                    *slot = Some((i, d));
                }
            }
        }
        nearest[i] = nearest_for(i, &clusters);
    }

    absorb_small(&mut clusters);
    while active_count(&clusters) > max_speakers {
        let Some((i, j)) = closest_pair(&clusters) else {
            break;
        };
        merge(i, j, &mut clusters);
    }

    let mut ids = vec![0usize; clusters.len()];
    let mut sorted = (0..clusters.len()).filter(|i| clusters[*i].active).collect::<Vec<_>>();
    sorted.sort_by_key(|i| clusters[*i].members.iter().copied().min().unwrap_or(usize::MAX));
    for (id, cluster_index) in sorted.into_iter().enumerate() {
        ids[cluster_index] = id;
    }
    let mut out = vec![0usize; items.len()];
    for (index, cluster) in clusters.iter().enumerate() {
        if cluster.active {
            for member in &cluster.members {
                out[*member] = ids[index];
            }
        }
    }
    out
}

fn nearest_for(index: usize, clusters: &[Cluster]) -> Option<(usize, f32)> {
    let mut best: Option<(usize, f32)> = None;
    for (other, cluster) in clusters.iter().enumerate() {
        if other == index || !cluster.active {
            continue;
        }
        if blocked(&clusters[index], cluster) {
            continue;
        }
        let d = distance(&clusters[index].centroid, &cluster.centroid);
        if best.map(|(_, current)| d < current).unwrap_or(true) {
            best = Some((other, d));
        }
    }
    best
}

/// zwei cluster dürfen nicht verschmolzen werden, wenn sie verschiedene lokale sprecher aus demselben fenster enthalten.
fn blocked(left: &Cluster, right: &Cluster) -> bool {
    left.seen.iter().any(|(window, local)| right.seen.get(window).is_some_and(|other| other != local))
}

fn merge(into: usize, from: usize, clusters: &mut [Cluster]) {
    if into == from {
        return;
    }
    let from_members = std::mem::take(&mut clusters[from].members);
    let from_seen = std::mem::take(&mut clusters[from].seen);
    let from_duration = clusters[from].duration_ms;
    let from_centroid = clusters[from].centroid.clone();
    clusters[from].active = false;

    let into_cluster = &mut clusters[into];
    let total = into_cluster.members.len() + from_members.len();
    let weight_into = into_cluster.members.len() as f32 / total as f32;
    let weight_from = from_members.len() as f32 / total as f32;
    for (slot, value) in into_cluster.centroid.iter_mut().enumerate() {
        *value = *value * weight_into + from_centroid.get(slot).copied().unwrap_or(0.0) * weight_from;
    }
    normalize(&mut into_cluster.centroid);
    into_cluster.members.extend(from_members);
    into_cluster.seen.extend(from_seen);
    into_cluster.duration_ms += from_duration;
}

/// cluster mit weniger als 3 mitgliedern oder unter 3 s sprache werden dem nächsten großen cluster zugeordnet.
fn absorb_small(clusters: &mut [Cluster]) {
    let large = clusters.iter().filter(|cluster| cluster.active && !is_small(cluster)).count();
    if large == 0 {
        return;
    }
    for index in 0..clusters.len() {
        if !clusters[index].active || !is_small(&clusters[index]) {
            continue;
        }
        let target = (0..clusters.len())
            .filter(|other| {
                *other != index
                    && clusters[*other].active
                    && !is_small(&clusters[*other])
                    && !blocked(&clusters[index], &clusters[*other])
            })
            .map(|other| (other, distance(&clusters[index].centroid, &clusters[other].centroid)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(other, _)| other);
        if let Some(target) = target {
            merge(target, index, clusters);
        }
    }
}

fn is_small(cluster: &Cluster) -> bool {
    cluster.members.len() < 3 || cluster.duration_ms < 3_000
}

fn active_count(clusters: &[Cluster]) -> usize {
    clusters.iter().filter(|cluster| cluster.active).count()
}

fn closest_pair(clusters: &[Cluster]) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize, f32)> = None;
    for i in 0..clusters.len() {
        if !clusters[i].active {
            continue;
        }
        for j in (i + 1)..clusters.len() {
            if !clusters[j].active || blocked(&clusters[i], &clusters[j]) {
                continue;
            }
            let d = distance(&clusters[i].centroid, &clusters[j].centroid);
            if best.map(|(_, _, current)| d < current).unwrap_or(true) {
                best = Some((i, j, d));
            }
        }
    }
    best.map(|(i, j, _)| (i, j))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(window: usize, local: usize, vector: &[f32]) -> Embedding {
        Embedding {
            window,
            local,
            start_ms: window as u64 * 2_500,
            vector: vector.to_vec(),
            duration_ms: 1_500,
        }
    }

    #[test]
    fn separates_well_formed_blobs() {
        let mut items = Vec::new();
        for window in 0..6 {
            items.push(item(window, 0, &[1.0, 0.0, 0.0]));
            items.push(item(window, 1, &[0.0, 1.0, 0.0]));
        }
        let assignment = cluster(&items, 0.5, 8);
        assert_eq!(assignment.iter().copied().max(), Some(1));
        assert_ne!(assignment[0], assignment[1]);
    }

    #[test]
    fn cannot_link_survives_merges_of_other_clusters() {
        // lokale sprecher 0 und 1 aus fenster 0 sind identisch mit einer dritten stimme, die sonst allein auftritt; egal wer zuerst verschmilzt, 0 und 1 müssen getrennt bleiben.
        let items = vec![
            item(0, 0, &[1.0, 0.0]),
            item(0, 1, &[0.98, 0.2]),
            item(1, 0, &[0.99, 0.1]),
            item(2, 0, &[1.0, 0.01]),
            item(3, 0, &[0.97, 0.15]),
        ];
        let assignment = cluster(&items, 0.9, 8);
        assert_ne!(assignment[0], assignment[1]);
    }

    #[test]
    fn a_single_blob_stays_one_cluster() {
        let items = (0..6).map(|window| item(window, 0, &[1.0, 0.0])).collect::<Vec<_>>();
        let assignment = cluster(&items, 0.5, 8);
        assert_eq!(assignment, vec![0; 6]);
    }

    #[test]
    fn cannot_link_keeps_simultaneous_speakers_apart() {
        let items = vec![
            item(0, 0, &[1.0, 0.0]),
            item(0, 1, &[1.0, 0.0]),
            item(1, 0, &[1.0, 0.0]),
            item(1, 1, &[1.0, 0.0]),
        ];
        let assignment = cluster(&items, 0.9, 8);
        assert_ne!(assignment[0], assignment[1]);
        assert_ne!(assignment[2], assignment[3]);
    }

    #[test]
    fn small_clusters_are_absorbed() {
        let mut items = Vec::new();
        for window in 0..6 {
            items.push(item(window, 0, &[1.0, 0.0]));
        }
        items.push(item(6, 1, &[0.99, 0.01]));
        let assignment = cluster(&items, 0.9, 8);
        assert_eq!(assignment.iter().copied().max(), Some(0));
    }

    #[test]
    fn the_cap_holds() {
        let mut items = Vec::new();
        for window in 0..12 {
            let angle = window as f32 * 0.7;
            items.push(item(window, 0, &[angle.cos(), angle.sin()]));
        }
        let assignment = cluster(&items, 0.9, 2);
        assert_eq!(assignment.iter().copied().max(), Some(1));
    }

    #[test]
    fn centroids_are_normalised() {
        let items = vec![item(0, 0, &[2.0, 0.0]), item(1, 0, &[2.0, 0.0])];
        let centroids = centroids(&items, &[0, 0]);
        assert!((cosine(&centroids[0], &[1.0, 0.0]) - 1.0).abs() < 1e-5);
    }
}
