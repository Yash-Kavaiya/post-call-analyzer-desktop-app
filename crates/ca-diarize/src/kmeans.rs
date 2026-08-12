//! k-means clustering with k-means++ seeding.
//!
//! Deterministic on purpose: the seeding uses a fixed pseudo-random sequence
//! rather than a system RNG, so re-running a batch produces identical speaker
//! assignments. A diarization that shuffles speaker numbers between runs would
//! make stored analyses and their citations disagree with each other.

/// Standardise each dimension to zero mean and unit variance.
///
/// Without this, pitch (measured in hundreds of hertz) would dominate every
/// distance and the spectral features would contribute nothing.
fn standardise(points: &[Vec<f64>]) -> Vec<Vec<f64>> {
    if points.is_empty() {
        return vec![];
    }
    let dims = points[0].len();
    let n = points.len() as f64;

    let mut means = vec![0.0; dims];
    for p in points {
        for (d, v) in p.iter().enumerate() {
            means[d] += v;
        }
    }
    for m in means.iter_mut() {
        *m /= n;
    }

    let mut sds = vec![0.0; dims];
    for p in points {
        for (d, v) in p.iter().enumerate() {
            sds[d] += (v - means[d]).powi(2);
        }
    }
    for s in sds.iter_mut() {
        *s = (*s / n).sqrt();
        if *s < 1e-9 {
            *s = 1.0; // constant dimension contributes nothing
        }
    }

    points
        .iter()
        .map(|p| {
            p.iter()
                .enumerate()
                .map(|(d, v)| (v - means[d]) / sds[d])
                .collect()
        })
        .collect()
}

fn dist2(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum()
}

/// Cluster `points` into `k` groups, returning one label per point.
pub fn cluster(points: &[Vec<f64>], k: usize) -> Vec<usize> {
    if points.is_empty() || k == 0 {
        return vec![];
    }
    if k == 1 || points.len() <= k {
        return (0..points.len()).map(|i| i.min(k - 1)).collect();
    }

    let pts = standardise(points);

    // k-means++ seeding: first centre is the point furthest from the mean,
    // each subsequent centre the point furthest from those already chosen.
    // Choosing deterministically (rather than sampling) keeps runs reproducible.
    let mut centres: Vec<Vec<f64>> = Vec::with_capacity(k);
    let origin = vec![0.0; pts[0].len()];
    let first = pts
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| {
            dist2(a, &origin)
                .partial_cmp(&dist2(b, &origin))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .unwrap_or(0);
    centres.push(pts[first].clone());

    while centres.len() < k {
        let next = pts
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| {
                let da = centres.iter().map(|c| dist2(a, c)).fold(f64::MAX, f64::min);
                let db = centres.iter().map(|c| dist2(b, c)).fold(f64::MAX, f64::min);
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
            .unwrap_or(0);
        centres.push(pts[next].clone());
    }

    let mut labels = vec![0usize; pts.len()];
    for _ in 0..50 {
        let mut changed = false;

        for (i, p) in pts.iter().enumerate() {
            let best = centres
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    dist2(p, a)
                        .partial_cmp(&dist2(p, b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(j, _)| j)
                .unwrap_or(0);
            if labels[i] != best {
                labels[i] = best;
                changed = true;
            }
        }

        let dims = pts[0].len();
        let mut sums = vec![vec![0.0; dims]; k];
        let mut counts = vec![0usize; k];
        for (i, p) in pts.iter().enumerate() {
            counts[labels[i]] += 1;
            for (d, v) in p.iter().enumerate() {
                sums[labels[i]][d] += v;
            }
        }
        for j in 0..k {
            if counts[j] > 0 {
                for d in 0..dims {
                    centres[j][d] = sums[j][d] / counts[j] as f64;
                }
            }
        }

        if !changed {
            break;
        }
    }

    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_no_labels() {
        assert!(cluster(&[], 2).is_empty());
    }

    #[test]
    fn k_of_one_puts_everything_together() {
        let pts = vec![vec![0.0], vec![100.0], vec![50.0]];
        assert_eq!(cluster(&pts, 1), vec![0, 0, 0]);
    }

    #[test]
    fn separates_two_obvious_groups() {
        let pts = vec![
            vec![0.0, 0.0],
            vec![0.5, 0.2],
            vec![100.0, 100.0],
            vec![100.5, 99.8],
        ];
        let labels = cluster(&pts, 2);
        assert_eq!(labels[0], labels[1], "near points should share a cluster");
        assert_eq!(labels[2], labels[3]);
        assert_ne!(labels[0], labels[2], "far points should not");
    }

    #[test]
    fn is_deterministic_across_runs() {
        // Speaker numbering must be stable, or stored citations stop agreeing
        // with re-analysed transcripts.
        let pts = vec![
            vec![1.0, 2.0],
            vec![1.1, 2.1],
            vec![9.0, 8.0],
            vec![9.2, 8.3],
            vec![5.0, 5.0],
        ];
        let a = cluster(&pts, 2);
        let b = cluster(&pts, 2);
        assert_eq!(a, b);
    }

    #[test]
    fn a_dimension_with_a_much_larger_scale_does_not_dominate() {
        // Dimension 0 spans thousands, dimension 1 spans single digits, but
        // the real grouping is in dimension 1. Standardisation must expose it.
        let pts = vec![
            vec![1000.0, 0.0],
            vec![2000.0, 0.1],
            vec![1500.0, 9.0],
            vec![2500.0, 9.1],
        ];
        let labels = cluster(&pts, 2);
        assert_eq!(labels[0], labels[1], "labels: {labels:?}");
        assert_eq!(labels[2], labels[3], "labels: {labels:?}");
        assert_ne!(labels[0], labels[2], "labels: {labels:?}");
    }

    #[test]
    fn more_clusters_than_points_is_handled() {
        let pts = vec![vec![1.0], vec![2.0]];
        let labels = cluster(&pts, 5);
        assert_eq!(labels.len(), 2);
    }
}
