//! Case-insensitive symbol search: exact/substring, subsequence, then bounded typos.
pub fn fuzzy_score(candidate: &str, query: &str) -> Option<usize> {
    let query = query
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>();
    let candidate = candidate.to_lowercase();
    if query.is_empty() || candidate == query {
        return Some(0);
    }
    let q = query.chars().collect::<Vec<_>>();
    let text = candidate.chars().collect::<Vec<_>>();
    if let Some(start) = candidate.find(&query) {
        let start = candidate[..start].chars().count();
        return Some(
            if start == 0 { 20 } else { 60 + start * 2 } + text.len().saturating_sub(q.len()),
        );
    }
    let mut at = 0;
    let mut gap = 0;
    for (index, ch) in text.iter().enumerate() {
        if q.get(at) == Some(ch) {
            at += 1;
            gap += index.saturating_sub(at - 1);
        }
        if at == q.len() {
            return Some(120 + gap + text.len().saturating_sub(q.len()));
        }
    }
    if !(4..=64).contains(&q.len()) {
        return None;
    }
    let budget = if q.len() >= 8 { 2 } else { 1 };
    candidate
        .split(|c: char| !c.is_alphanumeric())
        .filter_map(|word| {
            let word = word.chars().collect::<Vec<_>>();
            if word.len().abs_diff(q.len()) > budget || word.len() > 64 {
                return None;
            }
            let mut previous_previous = vec![];
            let mut previous = (0..=word.len()).collect::<Vec<_>>();
            for (i, ch) in q.iter().enumerate() {
                let mut current = vec![i + 1];
                for (j, other) in word.iter().enumerate() {
                    let mut cost = (previous[j + 1] + 1)
                        .min(current[j] + 1)
                        .min(previous[j] + usize::from(ch != other));
                    if i > 0 && j > 0 && *ch == word[j - 1] && q[i - 1] == *other {
                        cost = cost.min(previous_previous[j - 1] + 1);
                    }
                    current.push(cost);
                }
                previous_previous = previous;
                previous = current;
            }
            let distance = previous[word.len()];
            (distance <= budget).then_some(300 + distance * 40)
        })
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranks_exact_names_and_handles_abbreviations_typos_and_unicode() {
        assert!(fuzzy_score("render", "render") < fuzzy_score("renderGraph", "render"));
        assert!(fuzzy_score("app.Box.run", "bxrn").is_some());
        assert!(fuzzy_score("app.render", "rendre").is_some());
        assert!(fuzzy_score("app.Загрузить", "Згрз").is_some());
        assert!(fuzzy_score("app.Box.run", "unrelated").is_none());
    }
}
