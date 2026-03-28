fn count_eng_vowels(word: &str) -> usize {
    word.chars().filter(|c| "aeiouy".contains(*c)).count()
}

fn count_rus_vowels(word: &str) -> usize {
    word.chars().filter(|c| "ft`bjes'.".contains(*c)).count()
}

fn is_likely_english(word: &str) -> bool {
    let clean_word = word.trim_end_matches(&['.', ',', ';', '\'', '`', '[', ']'][..]);
    if clean_word.len() < 3 { return true; }
    
    let eng_vowels = clean_word.chars().filter(|c| "aeiouy".contains(*c)).count();
    if eng_vowels >= 2 && clean_word.len() <= 5 { return true; }
    
    false
}

fn should_switch(word: &str) -> bool {
    if word.len() < 3 { return false; }
    if is_likely_english(word) { return false; }

    let mut score = 0;
    
    for (i, k) in word.chars().enumerate() {
        if "[]:;',`".contains(k) {
            score += 15;
        }
        if ".,".contains(k) {
            if i < word.len() - 1 {
                score += 15;
            } else {
                score += 5;
            }
        }
    }
    
    let rus_vowels = count_rus_vowels(word);
    let eng_vowels = count_eng_vowels(word);

    if rus_vowels > eng_vowels { score += 10; }
    if eng_vowels == 0 { score += 12; }

    score >= 10
}

fn main() {
    let test_word = "ghbdtn1"; // 'Ghbdtn!' without shift
    println!("Word: {}", test_word);
    println!("is_likely_english: {}", is_likely_english(test_word));
    println!("should_switch: {}", should_switch(test_word));
}
