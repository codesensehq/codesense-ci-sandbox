// SPDX-License-Identifier: MIT
use serde_json::Value;
use std::collections::HashMap;

/// Keep the longest unchanged order and allocate positions only to moved or new rows.
/// Renumbering every neighbour would turn a one-row move into a full snapshot delta.
pub(super) fn positions(rows: &mut [Value], previous: &[Value]) {
    let old: HashMap<_, _> = previous
        .iter()
        .filter_map(|row| Some((row["id"].as_str()?, row["order"].as_f64()?)))
        .collect();
    let values: Vec<_> = rows
        .iter()
        .map(|row| row["id"].as_str().and_then(|id| old.get(id)).copied())
        .collect();
    let mut lengths = vec![0; rows.len()];
    let mut predecessor = vec![None; rows.len()];
    for i in 0..rows.len() {
        if let Some(value) = values[i] {
            lengths[i] = 1;
            for j in 0..i {
                if values[j].is_some_and(|v| v < value) && lengths[j] + 1 > lengths[i] {
                    lengths[i] = lengths[j] + 1;
                    predecessor[i] = Some(j);
                }
            }
        }
    }
    let mut anchors = Vec::new();
    let mut at = lengths
        .iter()
        .enumerate()
        .filter(|(_, n)| **n > 0)
        .max_by_key(|(_, n)| **n)
        .map(|(i, _)| i);
    while let Some(i) = at {
        anchors.push(i);
        at = predecessor[i];
    }
    anchors.reverse();
    let mut from = 0;
    let mut left =
        anchors.first().and_then(|i| values[*i]).unwrap_or(0.0) - (rows.len() + 1) as f64 * 1024.0;
    for end in anchors.into_iter().chain(std::iter::once(rows.len())) {
        let right = if end < rows.len() {
            values[end].unwrap()
        } else {
            left + (end - from + 1) as f64 * 1024.0
        };
        let step = (right - left) / (end - from + 1) as f64;
        for (offset, row) in rows[from..end].iter_mut().enumerate() {
            row["order"] = (left + step * (offset + 1) as f64).into();
        }
        if end < rows.len() {
            rows[end]["order"] = right.into();
        }
        from = end + 1;
        left = right;
    }
    if rows
        .windows(2)
        .any(|w| w[0]["order"].as_f64() >= w[1]["order"].as_f64())
    {
        for (i, row) in rows.iter_mut().enumerate() {
            row["order"] = (i as f64 * 1024.0).into();
        }
    }
}
