pub fn fold_ident(name: &str) -> String {
    name.chars()
        .map(|c| {
            let mut it = c.to_lowercase();
            match (it.next(), it.next()) {
                (Some(l), None) => l,
                _ => c,
            }
        })
        .collect()
}
