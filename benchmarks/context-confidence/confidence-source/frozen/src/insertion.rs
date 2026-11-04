//! Local, deterministic use of text surrounding the insertion/replacement range.
//! Never modifies the existing document or sends its contents to the cleanup LLM.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SurroundingText {
    pub before: String,
    pub after: String,
    /// True only for a nonempty selection; its contents are never needed.
    pub is_replacement: bool,
}

/// Supply nearby vocabulary to Whisper, keeping the user's explicit hint last
/// (Whisper retains the end of long prompts). No instructions are inferred from
/// document text. Limits apply to both token count and Unicode character count.
pub fn recognition_prompt(configured: &str, context: Option<&SurroundingText>) -> String {
    let Some(context) = context else {
        return configured.to_owned();
    };
    let words = |text: &str| {
        text.split(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    ',' | ';'
                        | '!'
                        | '?'
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | '"'
                        | '\''
                        | '“'
                        | '”'
                        | '„'
                        | '«'
                        | '»'
                )
        })
        .map(|word| {
            word.trim_start_matches(|c: char| {
                !c.is_alphanumeric() && !matches!(c, '+' | '#' | '.' | '_' | '$' | '@' | '-')
            })
            .trim_end_matches(|c: char| !c.is_alphanumeric() && !matches!(c, '+' | '#' | '_'))
        })
        .filter(|word| {
            !word.is_empty()
                && word.chars().count() <= 64
                && !word.chars().any(char::is_control)
                && !common_context_word(word)
        })
        .map(str::to_owned)
        .collect::<Vec<_>>()
    };
    // Alternate the nearest words on either side so long text on one side
    // cannot crowd out vocabulary immediately on the other side.
    let before = words(&context.before);
    let after = words(&context.after);
    let mut candidates = Vec::new();
    for i in 0..before.len().max(after.len()) {
        for word in [
            before.len().checked_sub(i + 1).and_then(|j| before.get(j)),
            after.get(i),
        ]
        .into_iter()
        .flatten()
        {
            if !candidates
                .iter()
                .any(|(_, _, seen): &(u8, usize, String)| seen == word)
            {
                let score = if word.chars().any(char::is_uppercase) {
                    4
                } else if word.chars().any(|c| {
                    c.is_numeric()
                        || matches!(c, '_' | '+' | '#' | '.' | '$' | '@' | '-' | '/' | ':')
                }) {
                    3
                } else {
                    1
                };
                candidates.push((score, i, word.clone()));
            }
        }
    }
    // Names and identifiers must not be crowded out by nearby ordinary prose.
    // Distance breaks ties within a vocabulary class, rather than deciding
    // which terms are admitted before we know their usefulness.
    candidates.sort_by_key(|(score, distance, _)| (std::cmp::Reverse(*score), *distance));
    let mut hints = Vec::new();
    let mut chars = 0;
    for (_, _, word) in candidates {
        let size = word.chars().count() + 2;
        if hints.len() < 40 && chars + size <= 320 {
            chars += size;
            hints.push(word);
        }
    }
    if hints.is_empty() {
        return configured.to_owned();
    }
    // Whisper retains the prompt suffix: put the nearest vocabulary there,
    // followed by the user's explicit hint, which has highest priority.
    hints.reverse();
    let mut prompt = hints.join(", ");
    if !configured.is_empty() {
        prompt.push('\n');
        prompt.push_str(configured);
    }
    prompt
}

fn common_context_word(word: &str) -> bool {
    const WORDS: &str = "a an the this that these those it its we you he she they our your my his her their there here and but or so because if when while with without for from to in on at is are was were be been being have has had can could would should will please use uses using do does did not as of all some any each every one then than now yet own about how what which who where i ale bo czy że to ten ta te jest są był była będzie będą w z na do od po dla przez przed się nie tak oraz albo żeby aby więc jeśli kiedy który która które jak gdzie już jeszcze proszę użyj używa";
    let lower = word.to_lowercase();
    WORDS.split_ascii_whitespace().any(|known| known == lower)
}

/// Adapt only the dictated text. Missing context preserves the old output
/// behavior exactly; existing spaces, punctuation and selected text are never
/// deleted via synthesized keys.
pub fn format(text: &str, context: Option<&SurroundingText>, append_space: bool) -> String {
    let Some(context) = context else {
        let mut result = text.to_owned();
        if append_space && !result.ends_with(char::is_whitespace) {
            result.push(' ');
        }
        return result;
    };
    let mut result = text.trim_matches(horizontal_space).to_owned();
    if result.is_empty() {
        return result;
    }
    if literal_context(context, &result) {
        return text.to_owned();
    }
    let left = context.before.chars().next_back();
    let right = context.after.chars().next();
    // A cursor inside a word or a structured token is not a prose boundary.
    // Preserve literal dictation there, without inserting separator spaces.
    let left_token = context.before.split_whitespace().next_back().unwrap_or("");
    let right_token = context.after.split_whitespace().next().unwrap_or("");
    let structured =
        |token: &str| token.contains(['/', '@', '_', '=', '\\', '`']) || token.contains("::");
    let literal_fragment =
        !result.chars().any(char::is_whitespace) && !result.ends_with(['.', '!', '?']);
    let completed_token = left.is_some_and(|c| {
        matches!(
            c,
            '.' | ',' | ';' | '!' | '?' | ')' | ']' | '}' | '”' | '»' | '`'
        )
    });
    let apostrophe_suffix = left.is_some_and(|c| matches!(c, '\'' | '’'))
        && context
            .before
            .chars()
            .rev()
            .nth(1)
            .is_some_and(char::is_alphanumeric)
        && matches!(
            result
                .split(|c: char| !c.is_alphabetic())
                .next()
                .unwrap_or(""),
            "s" | "t" | "re" | "ve" | "ll" | "d" | "m"
        );
    let explicit_line_break = result.starts_with(['\n', '\r']);
    if !explicit_line_break
        && ((!context.is_replacement
            && left.is_some_and(char::is_alphanumeric)
            && right.is_some_and(char::is_alphanumeric))
            || (left.is_some_and(|c| !c.is_whitespace())
                && structured(left_token)
                && ((!completed_token && literal_fragment) || left_token.ends_with("::")))
            || (!context.is_replacement
                && left.is_some_and(|c| !c.is_whitespace())
                && right.is_some_and(|c| !c.is_whitespace())
                && structured(right_token))
            || apostrophe_suffix)
    {
        return result;
    }
    let left_content = context
        .before
        .trim_end()
        .trim_end_matches([
            '”', '’', '»', '"', '\'', '“', '‘', '«', '„', '(', '[', '{', ')', ']', '}',
        ])
        .trim_end();
    let right_content = context.after.trim_start();
    let left_line_break = context.before[context.before.trim_end().len()..].contains(['\n', '\r']);
    let right_line_break =
        context.after[..context.after.len() - right_content.len()].contains(['\n', '\r']);
    let starts_sentence = left_content.is_empty()
        || left_content.ends_with(['!', '?', '…', '\n', '\r'])
        || (left_content.ends_with('.')
            && !abbreviation(left_content.split_whitespace().next_back().unwrap_or("")))
        || left_line_break
        || explicit_line_break;
    adjust_case(&mut result, starts_sentence);

    // Whisper tends to add a full stop to standalone utterances. In the
    // middle of existing prose it would split the sentence unintentionally.
    // Preserve emphatic punctuation, ellipses and dotted abbreviations.
    if result.ends_with('.') && !result.ends_with("..") {
        let last_word = result.split_whitespace().next_back().unwrap_or("");
        let dotted = abbreviation(last_word);
        let needs_complement = matches!(
            last_word.trim_end_matches('.').to_lowercase().as_str(),
            "new"
                | "the"
                | "a"
                | "an"
                | "to"
                | "for"
                | "with"
                | "of"
                | "and"
                | "or"
                | "nowy"
                | "nowe"
                | "nowa"
                | "dla"
                | "na"
                | "do"
                | "z"
        );
        if !dotted
            && right_content.chars().next().is_some_and(|c| {
                c.is_lowercase()
                    || c.is_numeric()
                    || (needs_complement && c.is_alphabetic())
                    || matches!(
                        c,
                        ',' | ';' | ':' | '.' | '!' | '?' | '…' | ')' | ']' | '}' | '`'
                    )
            })
            && !right_line_break
        {
            result.pop();
        }
    }
    // Avoid duplicating punctuation already immediately at the cursor.
    if let Some(right) = right
        && matches!(right, '.' | ',' | ';' | ':' | '!' | '?' | '…')
        && result.ends_with(right)
    {
        result.pop();
    }
    if result.is_empty() {
        return result;
    }
    if let Some(left) = left
        && matches!(left, ',' | ';' | ':')
        && result.starts_with(left)
    {
        result = result[left.len_utf8()..].trim_start().to_owned();
    }
    if result.is_empty() {
        return result;
    }
    let first = result.chars().next().unwrap();
    if left.is_some_and(|c| {
        !c.is_whitespace()
            && !opening_delimiter(c, &context.before)
            && !first.is_whitespace()
            && !closing_at_start(first, &result)
    }) {
        result.insert(0, ' ');
    }
    let last = result.chars().next_back().unwrap();
    let needs_space = match right {
        Some(c) => {
            !c.is_whitespace()
                && !closing_at_start(c, &context.after)
                && !opening_delimiter(last, &result)
        }
        None => append_space,
    };
    if needs_space && !last.is_whitespace() {
        result.push(' ');
    }
    result
}

fn literal_context(context: &SurroundingText, text: &str) -> bool {
    let before = &context.before;
    if before.matches("```").count() % 2 == 1
        || before.matches("~~~").count() % 2 == 1
        || before.chars().filter(|c| *c == '`').count() % 2 == 1
    {
        return true;
    }
    let keyword = text.split_whitespace().next().unwrap_or("");
    matches!(
        keyword,
        "let" | "const" | "var" | "fn" | "def" | "class" | "import" | "pub"
    ) && text.contains(['=', ';', '{', '(', ':'])
}

fn horizontal_space(c: char) -> bool {
    c.is_whitespace() && !matches!(c, '\n' | '\r')
}

fn opening_delimiter(c: char, prefix: &str) -> bool {
    matches!(c, '(' | '[' | '{' | '“' | '‘' | '«' | '„')
        || (matches!(c, '"' | '\'')
            && prefix.chars().rev().nth(1).is_none_or(|previous| {
                previous.is_whitespace() || matches!(previous, '(' | '[' | '{' | ':' | '=')
            }))
}

fn closing_at_start(c: char, suffix: &str) -> bool {
    if matches!(c, '"' | '\'') {
        return suffix
            .chars()
            .nth(1)
            .is_none_or(|next| next.is_whitespace() || closing_punctuation(next));
    }
    closing_punctuation(c)
}

fn abbreviation(word: &str) -> bool {
    matches!(
        word.to_lowercase().as_str(),
        "dr."
            | "mr."
            | "mrs."
            | "ms."
            | "prof."
            | "sr."
            | "jr."
            | "np."
            | "itd."
            | "itp."
            | "tzn."
            | "ul."
            | "godz."
            | "r."
    ) || word.strip_suffix('.').is_some_and(|stem| {
        stem.contains('.')
            && stem
                .split('.')
                .all(|part| part.chars().count() == 1 && part.chars().all(char::is_alphabetic))
    })
}

fn closing_punctuation(c: char) -> bool {
    matches!(
        c,
        '.' | ',' | ';' | ':' | '!' | '?' | '…' | ')' | ']' | '}' | '”' | '’' | '»' | '"' | '\''
    )
}

fn adjust_case(text: &mut String, sentence_start: bool) {
    let first_token = text
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches(['.', ',', ':', ';', '!', '?']);
    if (first_token.starts_with('[') && first_token.ends_with(']'))
        || (first_token.starts_with('(') && first_token.ends_with(')'))
    {
        return;
    }
    // Skip opening quotes/brackets, but preserve intentional identifiers and
    // acronyms. A small set of ordinary sentence starters can safely be
    // decapitalized; unknown words may be names (Alice, Kubernetes, Warszawa).
    let Some((index, first)) = text.char_indices().find(|(_, c)| c.is_alphabetic()) else {
        return;
    };
    if text[..index].chars().any(|c| {
        !c.is_whitespace() && !matches!(c, '"' | '\'' | '(' | '[' | '{' | '“' | '‘' | '«' | '„')
    }) {
        return;
    }
    let word: String = text[index..]
        .chars()
        .take_while(|c| c.is_alphabetic())
        .collect();
    // Check the complete token, rather than treating an identifier's alphabetic
    // prefix (is_enabled, i18n, The_widget) as an ordinary sentence starter.
    let suffix = &text[index + word.len()..];
    if suffix.starts_with(|c: char| {
        c.is_alphanumeric() || matches!(c, '_' | '-' | '/' | '@' | '=' | '+' | '#' | '\\')
    }) || suffix
        .strip_prefix('.')
        .is_some_and(|rest| rest.starts_with(char::is_alphanumeric))
    {
        return;
    }
    let lower = word.to_lowercase();
    let ordinary_start = matches!(
        lower.as_str(),
        "a" | "an"
            | "the"
            | "this"
            | "that"
            | "these"
            | "those"
            | "it"
            | "its"
            | "we"
            | "you"
            | "he"
            | "she"
            | "they"
            | "our"
            | "your"
            | "my"
            | "his"
            | "her"
            | "their"
            | "there"
            | "here"
            | "and"
            | "but"
            | "or"
            | "so"
            | "because"
            | "if"
            | "when"
            | "while"
            | "with"
            | "without"
            | "for"
            | "from"
            | "to"
            | "in"
            | "on"
            | "at"
            | "is"
            | "are"
            | "was"
            | "were"
            | "have"
            | "has"
            | "had"
            | "can"
            | "could"
            | "would"
            | "should"
            | "please"
            | "hello"
            | "thanks"
            | "thank"
            | "testing"
            | "test"
            | "fix"
            | "run"
            | "then"
            | "add"
            | "remove"
            | "update"
            | "check"
            | "show"
            | "change"
            | "write"
            | "build"
            | "implement"
            | "move"
            | "keep"
            | "read"
            | "call"
            | "tell"
            | "use"
            | "set"
            | "make"
            | "create"
            | "start"
            | "stop"
            | "try"
            | "popraw"
            | "uruchom"
            | "dodaj"
            | "usuń"
            | "sprawdź"
            | "zmień"
            | "napisz"
            | "zrób"
            | "some"
            | "another"
            | "new"
            | "more"
            | "i"
            | "ale"
            | "bo"
            | "czy"
            | "że"
            | "ten"
            | "ta"
            | "te"
            | "jest"
            | "są"
            | "był"
            | "była"
            | "będzie"
            | "będą"
            | "mam"
            | "mamy"
            | "chcę"
            | "chcemy"
            | "możemy"
            | "można"
            | "proszę"
            | "dziękuję"
            | "cześć"
            | "tak"
            | "nie"
            | "jutro"
            | "dzisiaj"
            | "teraz"
            | "potem"
            | "więc"
            | "jeśli"
            | "kiedy"
            | "który"
            | "która"
            | "które"
            | "nowy"
            | "nowa"
            | "nowe"
            | "w"
            | "z"
            | "na"
            | "do"
            | "od"
            | "po"
            | "przed"
            | "przez"
            | "dla"
    );
    let next_word = text[index + word.len()..]
        .split_whitespace()
        .next()
        .unwrap_or("");
    let likely_name = matches!(lower.as_str(), "new" | "more" | "my" | "your")
        && next_word.chars().next().is_some_and(char::is_uppercase)
        && next_word
            .trim_end_matches(|c: char| !c.is_alphabetic())
            .chars()
            .skip(1)
            .all(char::is_lowercase);
    let replacement =
        if sentence_start && first.is_lowercase() && word.chars().all(char::is_lowercase) {
            first.to_uppercase().collect::<String>()
        } else if !sentence_start
            && first.is_uppercase()
            && ordinary_start
            && word != "I"
            && !likely_name
            && !word.chars().skip(1).any(char::is_uppercase)
        {
            first.to_lowercase().collect::<String>()
        } else {
            return;
        };
    text.replace_range(index..index + first.len_utf8(), &replacement);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(before: &str, text: &str, after: &str) -> String {
        format(
            text,
            Some(&SurroundingText {
                before: before.into(),
                after: after.into(),
                is_replacement: false,
            }),
            true,
        )
    }

    #[test]
    fn sentence_boundaries_and_unicode() {
        assert_eq!(at("", "hello world.", ""), "Hello world. ");
        assert_eq!(at("Done. ", "this works.", ""), "This works. ");
        assert_eq!(at("Lista:\n", "że działa.", ""), "Że działa. ");
        assert_eq!(at("Myślę, że ", "To działa.", ""), "to działa. ");
        assert_eq!(at("say", "Hello", " again"), " hello");
        assert_eq!(at("Title\n    ", "The example.", ""), "The example. ");
        assert_eq!(at("", "Hello.", " \nnext paragraph"), "Hello.");
    }

    #[test]
    fn merges_existing_punctuation_and_spacing() {
        assert_eq!(at("We use ", "The new API.", " daily."), "the new API");
        assert_eq!(at("Hello", "World.", ", friend"), " World");
        assert_eq!(at("(", "The example.", ")"), "The example");
        assert_eq!(at("We use (", "The example.", ")"), "the example");
        assert_eq!(at("Done. ", "Hello.", ". Next"), "Hello");
        assert_eq!(at("", "Really?", "?"), "Really");
        assert_eq!(at("", "Wait...", ""), "Wait... ");
        assert_eq!(at("in ", "The U.S.", " today"), "the U.S.");
        assert_eq!(at("", "Hello.", "\nNext"), "Hello.");
        assert_eq!(at("He said \"", "Hello", "\"."), "hello");
        assert_eq!(at("“Done.” ", "this works.", ""), "This works. ");
        assert_eq!(at("", "Hello.", "!"), "Hello");
        assert_eq!(at("Hello,", ", everyone.", ""), " everyone. ");
    }

    #[test]
    fn preserves_names_acronyms_and_identifiers() {
        for word in [
            "Alice",
            "Warszawa",
            "Kubernetes",
            "NASA",
            "iPhone",
            "camelCase",
            "I",
            "I'm",
        ] {
            assert_eq!(at("ask ", word, ""), std::format!("{word} "));
        }
        assert_eq!(at("Ask ", "Dr.", " Smith"), "Dr.");
        assert_eq!(at("", "Hello.", " Next sentence."), "Hello.");
        assert_eq!(at("hel", "lo", "world"), "lo");
        assert_eq!(at("https://", "Example", ".com"), "Example");
        assert_eq!(at("user@", "Example", ".com"), "Example");
        for identifier in [
            "is_enabled",
            "i18n",
            "The_widget",
            "the-widget",
            "the/example",
            "the@example.com",
            "The.widget",
            "isEnabled",
        ] {
            assert_eq!(at("", identifier, ""), std::format!("{identifier} "));
            assert_eq!(at("use ", identifier, ""), std::format!("{identifier} "));
        }
    }

    #[test]
    fn replacement_uses_only_text_outside_the_selection() {
        assert_eq!(at("We need ", "A new plan.", " by Friday."), "a new plan");
        assert_eq!(at("Hello", "", "world"), "");
        let context = SurroundingText {
            before: "".into(),
            after: "".into(),
            is_replacement: false,
        };
        assert_eq!(format("hello", Some(&context), false), "Hello");
        assert_eq!(format("hello", None, true), "hello ");
        assert_eq!(format("hello ", None, true), "hello ");
        assert_eq!(format("hello", None, false), "hello");
    }

    #[test]
    fn recognition_preserves_manual_hint_and_nearby_terms() {
        let context = SurroundingText {
            before: "Kuba uses Kubernetes and C++".into(),
            after: " with PostgreSQL.".into(),
            is_replacement: false,
        };
        let prompt = recognition_prompt("Qwen2.5, localwispr", Some(&context));
        for term in ["Kuba", "Kubernetes", "C++", "PostgreSQL"] {
            assert!(prompt.contains(term), "{term}: {prompt}");
        }
        assert!(prompt.ends_with("Qwen2.5, localwispr"));
        assert_eq!(recognition_prompt("manual", None), "manual");
    }

    #[test]
    fn recognition_hints_are_bounded_and_have_no_control_characters() {
        let context = SurroundingText {
            before: (0..500).map(|i| std::format!("żółw{i} ")).collect(),
            after: "\0secret\nTerm".into(),
            is_replacement: false,
        };
        let hint = recognition_prompt("", Some(&context));
        assert!(hint.chars().count() <= 320);
        assert!(!hint.chars().any(char::is_control));
        assert!(hint.contains("żółw499"));
        assert!(hint.contains("Term"));
    }

    #[test]
    fn prose_after_structured_tokens_and_dotted_words_keeps_boundaries() {
        for before in [
            "See https://example.com.",
            "Email user@example.com.",
            "Use snake_case,",
        ] {
            assert!(at(before, "This works.", "").starts_with(' '), "{before}");
            assert!(at(before, "This works.", "").ends_with(' '));
        }
        assert_eq!(
            at("Edit ", "The config.toml.", " daily."),
            "the config.toml"
        );
        assert_eq!(at("Value: ", "3.14.", ", approximately."), "3.14");
        assert_eq!(at("Version ", "1.2.3.", " is current."), "1.2.3");
        assert_eq!(at("in ", "The U.S.", " today"), "the U.S.");
    }

    #[test]
    fn nonempty_selection_restores_word_separators_without_reading_its_contents() {
        let context = SurroundingText {
            before: "We".into(),
            after: "today.".into(),
            is_replacement: true,
        };
        assert_eq!(
            format("Have new tools.", Some(&context), true),
            " have new tools "
        );
        assert_eq!(at("hel", "lo", "world"), "lo");
    }

    #[test]
    fn proper_names_quotes_apostrophes_and_code_are_preserved() {
        assert_eq!(at("I visited ", "New York.", ""), "New York. ");
        assert_eq!(at("We use ", "New Relic.", ""), "New Relic. ");
        assert_eq!(at("Use `foo`", "And continue.", ""), " and continue. ");
        assert_eq!(at("", "[value]", ""), "[value] ");
        assert_eq!(at("", "(x)", ""), "(x) ");
        assert_eq!(at("He said \"hello\"", "And left.", ""), " and left. ");
        assert_eq!(at("Powiedział: „", "To działa.", "”"), "to działa.");
        assert_eq!(at("John’", "s", " book"), "s");
        assert_eq!(at("don'", "t", " worry"), "t");
        assert_eq!(at("To jest np. ", "Nowy model.", ""), "nowy model. ");
        assert_eq!(at("`", "the", "`"), "the");
        assert_eq!(at("std::", "vector", "<int>"), "vector");
    }

    #[test]
    fn explicit_paragraph_breaks_and_sentence_starts_are_preserved() {
        assert_eq!(
            at("First paragraph.", "\n\nthis is next.", ""),
            "\n\nThis is next. "
        );
        assert_eq!(
            at("First paragraph.", "\n\nthis is next.\n", ""),
            "\n\nThis is next.\n"
        );
        assert_eq!(at("Done. ", "good morning.", ""), "Good morning. ");
    }

    #[test]
    fn technical_prefixes_and_nearest_hint_priority_survive_extraction() {
        let context = SurroundingText {
            before: "Use .NET _private C++ C#".into(),
            ..SurroundingText::default()
        };
        let prompt = recognition_prompt("", Some(&context));
        for word in [".NET", "_private", "C++", "C#"] {
            assert!(prompt.contains(word), "{word}: {prompt}");
        }
        let context = SurroundingText {
            before: "far middle nearest".into(),
            after: " right further distant".into(),
            ..SurroundingText::default()
        };
        assert_eq!(
            recognition_prompt("Manual", Some(&context)),
            "distant, far, further, middle, right, nearest\nManual"
        );
    }

    #[test]
    fn instructions_and_prose_beside_code_join_naturally() {
        assert_eq!(at("Please ", "Fix the spacing.", ""), "fix the spacing. ");
        assert_eq!(at("Next, ", "Run cargo test.", ""), "run cargo test. ");
        assert_eq!(
            at("Fix the parser and ", "Then update the tests.", ""),
            "then update the tests. "
        );
        assert_eq!(
            at("We use ", "The PostgreSQL driver.", ""),
            "the PostgreSQL driver. "
        );
        assert_eq!(at("We use ", "The new.", " PostgreSQL driver."), "the new");
        assert_eq!(at("Use ", "The option.", "`is_enabled`."), "the option ");
        assert_eq!(
            at("Set ", "The option.", "is_enabled = true."),
            "the option "
        );
        assert_eq!(
            at("```rust\n", "let enabled = true;", ""),
            "let enabled = true;"
        );
        assert_eq!(at("```sh\n", "cargo test --lib", ""), "cargo test --lib");
        assert_eq!(at("We use ", "New Relic.", ""), "New Relic. ");
    }

    #[test]
    fn names_and_jargon_survive_crowded_prose() {
        let context = SurroundingText {
            before: "Marta Qdrant PostgreSQL you are on the same page and all of this is for our own work but not yet so now use that one then go to my next task or get more help from your team as they will see".into(),
            after: "Please call Marta,then use Qdrant;PostgreSQL".into(),
            ..SurroundingText::default()
        };
        let prompt = recognition_prompt("Manual", Some(&context));
        for term in ["Marta", "Qdrant", "PostgreSQL"] {
            assert!(prompt.contains(term), "{term}: {prompt}");
        }
        assert!(!prompt.contains("Marta,then"));
        assert!(!prompt.contains("Qdrant;PostgreSQL"));
        assert!(!prompt.contains(" the"));
        assert!(prompt.ends_with("Manual"));
    }
}
