/// Prints an inspirational quote to stdout.
pub fn quote() {
    println!("The only way to do great work is to love what you do. - Steve Jobs");
}

/// Returns an inspirational quote as a string.
pub fn get_quote() -> &'static str {
    "The only way to do great work is to love what you do. - Steve Jobs"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_quote() {
        let q = get_quote();
        assert!(!q.is_empty());
        assert!(q.contains("Steve Jobs"));
    }
}
