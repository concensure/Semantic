pub fn levenshtein(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let a_chars: Vec<char> = a.chars().collect();
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    for (i, ca) in a_chars.iter().enumerate() {
        let mut curr = vec![i + 1; b_chars.len() + 1];
        for (j, cb) in b_chars.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (curr[j] + 1).min(prev[j + 1] + 1).min(prev[j] + cost);
        }
        prev = curr;
    }
    prev[b_chars.len()]
}

pub fn similarity_score(query: &str, candidate: &str) -> f64 {
    let q = query.to_lowercase();
    let c = candidate.to_lowercase();
    let dist = levenshtein(&q, &c) as f64;
    let max_len = q.len().max(c.len()) as f64;
    if max_len == 0.0 {
        1.0
    } else {
        1.0 - (dist / max_len)
    }
}
